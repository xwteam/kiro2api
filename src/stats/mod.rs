//! 统计持久化层(Phase 1)。
//!
//! 提供三类存储:用量记录、失败日志(401/403)、限流日志(429)。全部 JSON 文件落盘于
//! 配置数据目录(由 `credentials_path` 的父目录推断),约 5s 异步批量刷盘、每凭据 LRU 上限、
//! 注入时钟(所有落库入口取 `now_unix` 参数,业务逻辑内不直接读系统时钟)、原子写(tmp+rename)。
//!
//! 线程安全:内部 `tokio::RwLock`;对外经 `Arc` 共享给 relay(record_*)与 admin(query_*)。
//!
//! 本阶段只建持久化层,不改 relay/admin;instrumentation 由后续阶段接线。

pub mod eventlog;
pub mod model;
pub mod persist;
pub mod pricing;
pub mod usage;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::stats::eventlog::{EVENT_CAP_PER_CREDENTIAL, EventLog};
use crate::stats::model::{FailureEvent, ThrottleEvent, truncate_chars};
use crate::stats::usage::UsageTracker;

/// failure 响应体截断上限(字符)。
pub const FAILURE_BODY_MAX: usize = 2000;
/// throttle 响应体截断上限(字符)。
pub const THROTTLE_BODY_MAX: usize = 200;

/// 三存储容器。`Arc<StatsManager>` 注入 relay 与 admin state。
pub struct StatsManager {
    pub usage: Arc<UsageTracker>,
    pub failure_log: Arc<EventLog<FailureEvent>>,
    pub throttle_log: Arc<EventLog<ThrottleEvent>>,
}

impl StatsManager {
    /// 由 `credentials_path` 推断数据目录(取其父目录;无父目录时用当前目录),
    /// 在其下载入/初始化三个 JSON 文件并启动各自后台刷盘。
    pub fn load_from_credentials_path(credentials_path: &str) -> Arc<Self> {
        let dir = data_dir_from(credentials_path);
        Self::load_from_dir(&dir)
    }

    /// 直接指定数据目录载入。
    pub fn load_from_dir(dir: &Path) -> Arc<Self> {
        let usage = UsageTracker::load(dir.join("usage_records.json"));
        let failure_log =
            EventLog::<FailureEvent>::load(dir.join("failure_log.json"), EVENT_CAP_PER_CREDENTIAL);
        let throttle_log = EventLog::<ThrottleEvent>::load(
            dir.join("throttle_log.json"),
            EVENT_CAP_PER_CREDENTIAL,
        );
        Arc::new(Self {
            usage,
            failure_log,
            throttle_log,
        })
    }

    /// 停机时的最终刷盘:把**三个存储**(用量记录 + 每日汇总、失败日志、限流日志)同步
    /// 落盘(原子写),不等 5s 去抖定时器。
    ///
    /// 优雅停机(SIGTERM/Ctrl-C)与管理面重启端点都必须在退出前 `await` 本方法,否则最近
    /// 一拍之后的写只在内存里,重启即丢。失败只记 error 日志,不阻断停机。
    ///
    /// 口径为什么必须含事件日志:后台刷盘任务指望不上——进程退出时运行时随之销毁,那一拍
    /// 轮不到;重启端点更是直接 `std::process::exit`,连析构都不跑。丢的这几秒正是运营者
    /// 最想看的现场:刚出的 401/403 与 429 在失败/限流弹窗里查无此事,usage-summary 的
    /// errorRate 窗口计数(`EventLog::count_in_window`)也跟着少算。
    pub async fn flush_now(&self) {
        self.usage.flush_now().await;
        self.failure_log.flush_now().await;
        self.throttle_log.flush_now().await;
    }

    /// 清空某凭据的全部用量记录与失败/限流事件;返回删除的用量记录条数。
    ///
    /// 凭据被删除时调用:三类记录都按数值 id 键控,而 id 会被后续新增的凭据复用
    /// (高水位边车文件缺失/写失败时尤其容易),不清就等于让新账号一上线就继承前任的
    /// 用量、消费与报错明细——统计聚合、每凭据记录上限、失败/限流弹窗全被污染。
    pub async fn purge_credential(&self, credential_id: u32) -> usize {
        let removed = self.usage.reset_credential(credential_id).await;
        self.failure_log.remove_credential(credential_id).await;
        self.throttle_log.remove_credential(credential_id).await;
        removed
    }

    /// 记录一次失败(401/403)。响应体截断到 2000 字符。热路径 fire-and-forget。
    pub async fn record_failure(
        &self,
        credential_id: u32,
        request_type: impl Into<String>,
        status_code: u16,
        response_body: &str,
        now_unix: i64,
    ) {
        self.failure_log
            .push(FailureEvent {
                credential_id,
                request_type: request_type.into(),
                status_code,
                response_body: truncate_chars(response_body, FAILURE_BODY_MAX),
                created_at_unix: now_unix,
            })
            .await;
    }

    /// 单个 API-KEY 的用量聚合(委托 usage 层;无记录返回全零)。admin/user 查询用。
    pub async fn get_summary_by_api_key(
        &self,
        api_key_id: u32,
    ) -> crate::stats::usage::ApiKeyUsageSummary {
        self.usage.summary_for_api_key(api_key_id).await
    }

    /// 各 API-KEY 的实时 RPM(近 60s 窗口内落库的请求数),委托 usage 层。
    /// 供 `GET /api/admin/rpm` 的 `byApiKey` 维度使用。
    pub async fn rpm_by_api_key(&self, now_unix: i64) -> std::collections::HashMap<u32, u32> {
        self.usage.rpm_by_api_key(now_unix).await
    }

    /// 所有出现过的 API-KEY 的用量聚合(按 id 升序,不含无归属 id=0)。
    pub async fn get_summaries_by_api_key(&self) -> Vec<crate::stats::usage::ApiKeyUsageSummary> {
        self.usage.summaries_by_api_key().await
    }

    /// 单个 API-KEY 的记录分页(降序)。
    pub async fn get_records_by_api_key(
        &self,
        api_key_id: u32,
        page: usize,
        page_size: usize,
    ) -> crate::stats::usage::Page<crate::stats::model::UsageRecord> {
        self.usage
            .records_for_api_key(api_key_id, page, page_size)
            .await
    }

    /// 清空某 API-KEY 的全部用量记录;返回删除条数。
    pub async fn reset_by_api_key(&self, api_key_id: u32) -> usize {
        self.usage.reset_api_key(api_key_id).await
    }

    /// 记录一次限流(429)。响应体截断到 200 字符。热路径 fire-and-forget。
    pub async fn record_throttle(
        &self,
        credential_id: u32,
        request_type: impl Into<String>,
        response_body: &str,
        now_unix: i64,
    ) {
        self.throttle_log
            .push(ThrottleEvent {
                credential_id,
                request_type: request_type.into(),
                status_code: 429,
                response_body: truncate_chars(response_body, THROTTLE_BODY_MAX),
                created_at_unix: now_unix,
            })
            .await;
    }
}

/// 从 credentials 文件路径推断数据目录:取父目录;为空/无父则用 "."。
fn data_dir_from(credentials_path: &str) -> PathBuf {
    let p = Path::new(credentials_path);
    match p.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_inference() {
        assert_eq!(
            data_dir_from("/data/credentials.json"),
            PathBuf::from("/data")
        );
        assert_eq!(data_dir_from("credentials.json"), PathBuf::from("."));
        assert_eq!(
            data_dir_from("/var/lib/kiro/creds.json"),
            PathBuf::from("/var/lib/kiro")
        );
    }

    #[tokio::test]
    async fn manager_records_route_to_right_store_and_truncate() {
        let dir = std::env::temp_dir().join(format!("kiro2api_statsmgr_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mgr = StatsManager::load_from_dir(&dir);
        // 空态
        assert!(mgr.usage.is_empty().await);
        assert!(mgr.failure_log.is_empty().await);
        assert!(mgr.throttle_log.is_empty().await);

        // usage
        mgr.usage
            .record_usage(1, "m".into(), 10, 20, 0.5, None, None, None, 1000)
            .await;
        assert_eq!(mgr.usage.len().await, 1);

        // failure 截断到 2000
        let big = "a".repeat(5000);
        mgr.record_failure(1, "api", 403, &big, 1000).await;
        let fp = mgr.failure_log.records_for_credential(1, 1, 10).await;
        assert_eq!(fp.total, 1);
        assert_eq!(fp.items[0].response_body.chars().count(), FAILURE_BODY_MAX);
        assert_eq!(fp.items[0].status_code, 403);

        // throttle 截断到 200,status 恒 429
        mgr.record_throttle(1, "mcp", &big, 1000).await;
        let tp = mgr.throttle_log.records_for_credential(1, 1, 10).await;
        assert_eq!(tp.total, 1);
        assert_eq!(tp.items[0].response_body.chars().count(), THROTTLE_BODY_MAX);
        assert_eq!(tp.items[0].status_code, 429);
        assert_eq!(tp.items[0].request_type, "mcp");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn manager_flush_now_persists_usage_for_shutdown() {
        let dir = std::env::temp_dir().join(format!("kiro2api_statsflush_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        {
            let mgr = StatsManager::load_from_dir(&dir);
            mgr.usage
                .record_usage(1, "m".into(), 10, 20, 0.5, None, None, None, 1000)
                .await;
            // 停机流程的最终刷盘:不等 5s 去抖定时器。
            mgr.flush_now().await;
        }
        // 重新载入应看到刚才那条(否则重启会丢最近几秒的用量/计费)。
        let reloaded = StatsManager::load_from_dir(&dir);
        assert_eq!(reloaded.usage.len().await, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
