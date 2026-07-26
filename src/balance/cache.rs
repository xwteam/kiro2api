//! `BalanceCache`:按凭据 id 键控的余额缓存(5 分钟 TTL)+ 去抖异步落盘。
//!
//! 设计与 `stats` 层一致:热路径只改内存 + 置脏,后台任务约 5s 批量、原子写
//! (tmp+rename)落 `kiro_balance_cache.json`,复用 [`crate::stats::persist`]。
//! 线程安全:内部 `tokio::RwLock`,经 `Arc` 共享给 admin handler。
//! 注入纪律:TTL 判定用调用方传入的 `now_unix`,模块内不读系统时钟。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::balance::model::UsageLimitsResponse;
use crate::balance::{BALANCE_CACHE_FILE, BALANCE_TTL_SECS};
use crate::stats::persist::{self, DirtyFlag};

/// 单个凭据的余额快照(已从上游明细归约成扁平数值,camelCase 落盘/回读)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceSnapshot {
    /// 订阅标题(如 "KIRO PRO+");无则 None。
    #[serde(default)]
    pub subscription_title: Option<String>,
    /// 当前已用额度(积分)。
    pub current_usage: f64,
    /// 总限额(积分)。
    pub usage_limit: f64,
    /// 剩余额度(积分)。
    pub remaining: f64,
    /// 已用占比(0..=100)。
    pub usage_percentage: f64,
    /// 下次重置时刻(Unix 秒);无则 None。
    #[serde(default)]
    pub next_reset_at: Option<i64>,
    /// 本快照抓取时刻(Unix 秒),用于 TTL 判定;不外泄给前端契约。
    pub fetched_at_unix: u64,
}

impl BalanceSnapshot {
    /// 由上游响应 + 抓取时刻归约成快照。
    pub fn from_response(r: &UsageLimitsResponse, now_unix: u64) -> Self {
        Self {
            subscription_title: r.subscription_title().map(|s| s.to_string()),
            current_usage: r.current_usage(),
            usage_limit: r.usage_limit(),
            remaining: r.remaining(),
            usage_percentage: r.usage_percentage(),
            next_reset_at: r.next_reset_at(),
            fetched_at_unix: now_unix,
        }
    }

    /// 相对 `now_unix` 是否仍新鲜(未超过 TTL)。
    pub fn is_fresh(&self, now_unix: u64) -> bool {
        now_unix.saturating_sub(self.fetched_at_unix) < BALANCE_TTL_SECS
    }
}

/// 按凭据 id(String)键控的余额缓存。
pub struct BalanceCache {
    inner: RwLock<HashMap<String, BalanceSnapshot>>,
    dirty: DirtyFlag,
}

impl BalanceCache {
    /// 在数据目录下载入/初始化 `kiro_balance_cache.json` 并启动后台去抖刷盘。
    pub fn load_from_dir(dir: &Path) -> Arc<Self> {
        let path = dir.join(BALANCE_CACHE_FILE);
        let initial: HashMap<String, BalanceSnapshot> =
            persist::read_json(&path).ok().flatten().unwrap_or_default();
        Self::spawn(path, initial)
    }

    /// 由 credentials 文件路径推断数据目录(取父目录),与 stats 同目录。
    pub fn load_from_credentials_path(credentials_path: &str) -> Arc<Self> {
        let dir = data_dir_from(credentials_path);
        Self::load_from_dir(&dir)
    }

    fn spawn(path: PathBuf, initial: HashMap<String, BalanceSnapshot>) -> Arc<Self> {
        let (flag, handle) = persist::dirty_channel();
        let cache = Arc::new(Self {
            inner: RwLock::new(initial),
            dirty: flag,
        });
        // 快照闭包在 spawn_blocking 内被调用:用 blocking_read 拿只读快照序列化。
        let snap_src = cache.clone();
        persist::spawn_flush_loop(path, handle, persist::FLUSH_INTERVAL_SECS, move || {
            let map = snap_src.inner.blocking_read();
            serde_json::to_vec_pretty(&*map).unwrap_or_else(|_| b"{}".to_vec())
        });
        cache
    }

    /// 取某凭据仍新鲜的缓存;过期/缺失返回 None。
    pub async fn get_fresh(&self, id: &str, now_unix: u64) -> Option<BalanceSnapshot> {
        let map = self.inner.read().await;
        map.get(id).filter(|s| s.is_fresh(now_unix)).cloned()
    }

    /// 写入/覆盖某凭据的快照并置脏(触发去抖落盘)。
    pub async fn put(&self, id: impl Into<String>, snap: BalanceSnapshot) {
        {
            let mut map = self.inner.write().await;
            map.insert(id.into(), snap);
        }
        self.dirty.mark_dirty();
    }

    /// 失效某凭据的缓存条目;返回是否确有条目被移除。
    ///
    /// 凭据被删除/替换时必须调用:凭据 id 会被后续新增的凭据复用,残留条目会让新账号
    /// 在 TTL 内(最长 5 分钟)显示已删账号的余额与订阅档位。移除后置脏,落盘同步跟进。
    pub async fn invalidate(&self, id: &str) -> bool {
        let removed = {
            let mut map = self.inner.write().await;
            map.remove(id).is_some()
        };
        if removed {
            self.dirty.mark_dirty();
        }
        removed
    }

    /// 只读读取某凭据缓存里的订阅标题(subscription tier),要求缓存条目仍新鲜。
    /// 纯缓存读、绝不触发上游(语义与 [`get_fresh`] 一致):
    /// - 条目缺失 / 已过期 → `None`;
    /// - 条目新鲜但 `subscription_title` 为空(上游未给标题)→ `None`;
    /// - 否则返回订阅标题字符串(如 "KIRO PRO+")。
    ///
    /// 供模型刷新按订阅档位分组(每档只刷一个代表账号)读取账号档位用。
    /// TTL 判定用调用方传入的 `now_unix`(模块内不读系统时钟)。
    pub async fn tier_of(&self, id: &str, now_unix: u64) -> Option<String> {
        let map = self.inner.read().await;
        map.get(id)
            .filter(|s| s.is_fresh(now_unix))
            .and_then(|s| s.subscription_title.clone())
    }

    /// 对给定 id 列表读取仍新鲜的缓存快照(只读、绝不触发上游)。
    /// 返回 `(id, snapshot)` 对,过期/缺失的 id 直接跳过——供全局积分聚合按池内
    /// 凭据集合读缓存求和用。TTL 判定用调用方传入的 `now_unix`(模块内不读系统时钟)。
    pub async fn get_fresh_for_ids(
        &self,
        ids: &[String],
        now_unix: u64,
    ) -> Vec<(String, BalanceSnapshot)> {
        let map = self.inner.read().await;
        ids.iter()
            .filter_map(|id| {
                map.get(id)
                    .filter(|s| s.is_fresh(now_unix))
                    .map(|s| (id.clone(), s.clone()))
            })
            .collect()
    }

    /// 当前缓存条数(测试/观测用)。
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// 是否为空(测试用)。
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
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

    fn snap(now: u64) -> BalanceSnapshot {
        BalanceSnapshot {
            subscription_title: Some("KIRO PRO+".into()),
            current_usage: 10.0,
            usage_limit: 100.0,
            remaining: 90.0,
            usage_percentage: 10.0,
            next_reset_at: Some(1_784_462_400),
            fetched_at_unix: now,
        }
    }

    #[tokio::test]
    async fn put_then_get_fresh_within_ttl() {
        let dir = std::env::temp_dir().join(format!("kiro2api_bal_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cache = BalanceCache::load_from_dir(&dir);
        assert!(cache.is_empty().await);
        cache.put("7", snap(1000)).await;
        assert_eq!(cache.len().await, 1);
        // 新鲜:1000 + 299 < 1000 + 300
        let got = cache.get_fresh("7", 1299).await;
        assert!(got.is_some());
        assert_eq!(got.unwrap().remaining, 90.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn expired_snapshot_not_returned() {
        let dir = std::env::temp_dir().join(format!("kiro2api_bal2_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cache = BalanceCache::load_from_dir(&dir);
        cache.put("7", snap(1000)).await;
        // 到点即视为过期:1000 + 300 = 1300
        assert!(cache.get_fresh("7", 1300).await.is_none());
        // 未知 id
        assert!(cache.get_fresh("999", 1000).await.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn get_fresh_for_ids_sums_only_fresh_and_skips_misses() {
        let dir = std::env::temp_dir().join(format!("kiro2api_bal4_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let cache = BalanceCache::load_from_dir(&dir);
        // "7" 新鲜(fetched 1000), "8" 过期(fetched 100 → 100+300<=1299 已过期)。
        cache.put("7", snap(1000)).await;
        cache.put("8", snap(100)).await;
        // 查询 id 集合含一个缺失的 "9"。
        let got = cache
            .get_fresh_for_ids(&["7".to_string(), "8".to_string(), "9".to_string()], 1299)
            .await;
        // 只有 "7" 新鲜;"8" 过期、"9" 缺失都被跳过。
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "7");
        assert_eq!(got[0].1.remaining, 90.0);
        assert_eq!(got[0].1.fetched_at_unix, 1000);
        // 空列表 → 空结果。
        assert!(cache.get_fresh_for_ids(&[], 1299).await.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn invalidate_drops_entry_so_reused_id_starts_clean() {
        let dir = std::env::temp_dir().join(format!("kiro2api_bal_inv_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let cache = BalanceCache::load_from_dir(&dir);
        cache.put("7", snap(1000)).await;
        cache.put("8", snap(1000)).await;
        // 凭据 7 被删除 → 失效其缓存;同 id 的新凭据不得继承旧余额/订阅档位。
        assert!(cache.invalidate("7").await);
        assert!(cache.get_fresh("7", 1100).await.is_none());
        assert!(cache.tier_of("7", 1100).await.is_none());
        // 其它凭据不受影响
        assert!(cache.get_fresh("8", 1100).await.is_some());
        assert_eq!(cache.len().await, 1);
        // 重复失效/未知 id → false
        assert!(!cache.invalidate("7").await);
        assert!(!cache.invalidate("999").await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn tier_of_returns_cached_title_when_fresh() {
        let dir = std::env::temp_dir().join(format!("kiro2api_bal_tier_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let cache = BalanceCache::load_from_dir(&dir);
        // snap() 的 subscription_title = "KIRO PRO+",fetched_at=1000。
        cache.put("7", snap(1000)).await;
        // 新鲜:1000 + 299 < 1000 + 300 → 命中标题。
        assert_eq!(cache.tier_of("7", 1299).await.as_deref(), Some("KIRO PRO+"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn tier_of_none_on_expired_or_missing_or_untitled() {
        let dir = std::env::temp_dir().join(format!("kiro2api_bal_tier2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let cache = BalanceCache::load_from_dir(&dir);
        cache.put("7", snap(1000)).await;
        // 过期(1000 + 300 = 1300 到点即过期)→ None。
        assert!(cache.tier_of("7", 1300).await.is_none());
        // 缺失 id → None。
        assert!(cache.tier_of("999", 1000).await.is_none());
        // 新鲜但无订阅标题 → None。
        let mut untitled = snap(2000);
        untitled.subscription_title = None;
        cache.put("8", untitled).await;
        assert!(cache.tier_of("8", 2100).await.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_camel_case_roundtrip() {
        let s = snap(1000);
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"subscriptionTitle\""));
        assert!(j.contains("\"usageLimit\""));
        assert!(j.contains("\"nextResetAt\""));
        let back: BalanceSnapshot = serde_json::from_str(&j).unwrap();
        assert_eq!(back.remaining, 90.0);
    }

    #[test]
    fn from_response_reduces_fields() {
        let r: UsageLimitsResponse = serde_json::from_str(
            r#"{"subscriptionInfo":{"subscriptionTitle":"KIRO FREE"},
                "usageBreakdownList":[{"currentUsageWithPrecision":25.0,"usageLimitWithPrecision":100.0}]}"#,
        )
        .unwrap();
        let s = BalanceSnapshot::from_response(&r, 500);
        assert_eq!(s.usage_limit, 100.0);
        assert_eq!(s.current_usage, 25.0);
        assert_eq!(s.remaining, 75.0);
        assert_eq!(s.usage_percentage, 25.0);
        assert_eq!(s.subscription_title.as_deref(), Some("KIRO FREE"));
        assert_eq!(s.fetched_at_unix, 500);
    }

    #[tokio::test]
    async fn persists_across_reload() {
        let dir = std::env::temp_dir().join(format!("kiro2api_bal3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        {
            let cache = BalanceCache::load_from_dir(&dir);
            cache.put("7", snap(1000)).await;
            // 等一拍多让后台去抖落盘
            tokio::time::sleep(std::time::Duration::from_millis(
                persist::FLUSH_INTERVAL_SECS * 1000 + 400,
            ))
            .await;
        }
        // 重新载入应看到落盘的条目
        let cache2 = BalanceCache::load_from_dir(&dir);
        assert_eq!(cache2.len().await, 1);
        assert!(cache2.get_fresh("7", 1100).await.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
