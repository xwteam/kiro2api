//! 事件日志通用存储:failure(401/403)与 throttle(429)共用此结构,差异仅在
//! 落盘文件名与响应体截断长度。按凭据 LRU 上限,降序分页查询。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;

use crate::stats::persist::{DirtyFlag, dirty_channel, read_json, spawn_flush_loop};
use crate::stats::usage::Page;

/// 每凭据事件上限(failure/throttle 均 500)。
pub const EVENT_CAP_PER_CREDENTIAL: usize = 500;

/// 事件须能取到凭据 id 与时刻,以便按凭据裁剪与降序排序。
pub trait Event: Clone + Serialize + DeserializeOwned + Send + Sync + 'static {
    fn credential_id(&self) -> u32;
    fn created_at_unix(&self) -> i64;
}

/// 泛型事件日志存储。`Arc<EventLog<E>>` 可在 relay 与 admin 间共享。
pub struct EventLog<E: Event> {
    events: RwLock<Vec<E>>,
    dirty: DirtyFlag,
    cap: usize,
    /// 落盘路径,供 [`flush_now`](Self::flush_now) 在停机/重启流程里显式刷盘。
    path: PathBuf,
}

impl<E: Event> EventLog<E> {
    /// 从 `path` 载入(不存在则空),启动后台刷盘。`cap` 为每凭据上限。
    pub fn load(path: PathBuf, cap: usize) -> Arc<Self> {
        let initial: Vec<E> = read_json(&path).ok().flatten().unwrap_or_default();
        let (dirty, handle) = dirty_channel();
        let store = Arc::new(Self {
            events: RwLock::new(initial),
            dirty,
            cap,
            path: path.clone(),
        });
        let snap = store.clone();
        spawn_flush_loop(
            path,
            handle,
            crate::stats::persist::FLUSH_INTERVAL_SECS,
            move || {
                let guard = snap.events.blocking_read();
                serde_json::to_vec_pretty(&*guard).unwrap_or_else(|_| b"[]".to_vec())
            },
        );
        store
    }

    /// 立即把当前事件全量同步落盘(原子写),不等后台约 5s 的去抖定时器。
    ///
    /// 停机/管理面重启前必须调用:`push` 只改内存 + 置脏,真正写盘要等下一拍;而进程退出
    /// (SIGTERM 或重启端点的 `std::process::exit`,后者连析构都不跑)时那一拍根本轮不到,
    /// 于是最近约 5s 内的 401/403 与 429 事件静默消失——失败/限流弹窗查不到刚出事的那几条,
    /// usage-summary 的 errorRate 窗口计数也跟着少算。失败只记 error 日志,不阻断停机。
    pub async fn flush_now(&self) {
        let snapshot = {
            let guard = self.events.read().await;
            match serde_json::to_vec_pretty(&*guard) {
                Ok(bytes) => bytes,
                Err(e) => {
                    // 序列化失败就整轮跳过:宁可磁盘停在上一版本,也不能拿坏快照覆盖。
                    tracing::error!(error = %e, path = %self.path.display(), "事件日志序列化失败,跳过停机刷盘");
                    return;
                }
            }
        };
        let path = self.path.clone();
        // 同步写 + fsync 可达数十毫秒,不占 async 工作线程。
        let res = tokio::task::spawn_blocking(move || {
            crate::stats::persist::write_bytes_atomic(&path, &snapshot)
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!(error = %e, path = %self.path.display(), "事件日志最终刷盘失败")
            }
            Err(e) => {
                tracing::error!(error = %e, path = %self.path.display(), "事件日志最终刷盘任务 join 失败")
            }
        }
    }

    /// 删除某凭据的全部事件;返回删除条数。有删除则置脏触发落盘。
    ///
    /// 凭据被删除时调用:事件按数值 id 键控,而 id 会被后续新增的凭据复用,残留事件会让
    /// 新账号的失败/限流弹窗里凭空出现上一位主人的报错明细(含上游响应体)。
    pub async fn remove_credential(&self, credential_id: u32) -> usize {
        let removed = {
            let mut guard = self.events.write().await;
            let before = guard.len();
            guard.retain(|e| e.credential_id() != credential_id);
            before - guard.len()
        };
        if removed > 0 {
            self.dirty.mark_dirty();
        }
        removed
    }

    /// 追加一条事件,裁到每凭据上限,置脏(热路径)。
    pub async fn push(&self, event: E) {
        {
            let mut guard = self.events.write().await;
            guard.push(event);
            evict_per_credential(&mut guard, self.cap);
        }
        self.dirty.mark_dirty();
    }

    /// 某凭据事件降序分页。
    pub async fn records_for_credential(
        &self,
        credential_id: u32,
        page: usize,
        page_size: usize,
    ) -> Page<E> {
        let guard = self.events.read().await;
        let mut filtered: Vec<E> = guard
            .iter()
            .filter(|e| e.credential_id() == credential_id)
            .cloned()
            .collect();
        drop(guard);
        filtered.sort_by(|a, b| b.created_at_unix().cmp(&a.created_at_unix()));
        crate::stats::usage::paginate_pub(filtered, page, page_size)
    }

    /// 时间窗口 `[since_unix, until_unix]`(闭区间,unix 秒)内的事件条数。
    /// failure-log 用它数窗口内 401/403 数、throttle-log 数 429 数,供 usage-summary 算 errorRate。
    /// 注意:事件按凭据有 `EVENT_CAP_PER_CREDENTIAL` LRU 上限,极高频失败的账号最旧事件可能已被
    /// 淘汰,故长窗口下此计数是"未被淘汰事件"的下界(errorRate 因此偏保守,不会虚高)。
    pub async fn count_in_window(&self, since_unix: i64, until_unix: i64) -> u64 {
        let guard = self.events.read().await;
        guard
            .iter()
            .filter(|e| {
                let t = e.created_at_unix();
                t >= since_unix && t <= until_unix
            })
            .count() as u64
    }

    pub async fn len(&self) -> usize {
        self.events.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.events.read().await.is_empty()
    }
}

/// 逐凭据裁到 `cap`,保留每凭据最新 `cap` 条(与 usage 同策略)。
fn evict_per_credential<E: Event>(v: &mut Vec<E>, cap: usize) {
    if v.len() <= cap {
        return;
    }
    let mut counts: HashMap<u32, usize> = HashMap::new();
    let mut keep = vec![true; v.len()];
    for i in (0..v.len()).rev() {
        let c = counts.entry(v[i].credential_id()).or_insert(0);
        *c += 1;
        if *c > cap {
            keep[i] = false;
        }
    }
    let mut idx = 0;
    v.retain(|_| {
        let k = keep[idx];
        idx += 1;
        k
    });
}

// ---- Event 实现 ----

use crate::stats::model::{FailureEvent, ThrottleEvent};

impl Event for FailureEvent {
    fn credential_id(&self) -> u32 {
        self.credential_id
    }
    fn created_at_unix(&self) -> i64 {
        self.created_at_unix
    }
}

impl Event for ThrottleEvent {
    fn credential_id(&self) -> u32 {
        self.credential_id
    }
    fn created_at_unix(&self) -> i64 {
        self.created_at_unix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::model::{FailureEvent, truncate_chars};

    fn fail(cred: u32, at: i64) -> FailureEvent {
        FailureEvent {
            credential_id: cred,
            request_type: "api".into(),
            status_code: 401,
            response_body: "unauthorized".into(),
            created_at_unix: at,
        }
    }

    #[tokio::test]
    async fn push_and_query_desc_with_eviction() {
        let path = std::env::temp_dir().join(format!("kiro2api_evlog_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let log = EventLog::<FailureEvent>::load(path.clone(), 3);
        assert!(log.is_empty().await);
        // cred 1 塞 5 条 → 只留最新 3;cred 2 塞 1 条
        for i in 1..=5 {
            log.push(fail(1, 100 + i)).await;
        }
        log.push(fail(2, 50)).await;
        // cred 1 分页(降序)
        let p = log.records_for_credential(1, 1, 10).await;
        assert_eq!(p.total, 3); // 淘汰到 3
        assert_eq!(p.items[0].created_at_unix, 105);
        assert_eq!(p.items[2].created_at_unix, 103);
        let p2 = log.records_for_credential(2, 1, 10).await;
        assert_eq!(p2.total, 1);
        // 空凭据
        let pe = log.records_for_credential(999, 1, 10).await;
        assert_eq!(pe.total, 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn body_truncation_lengths() {
        let long = "x".repeat(3000);
        assert_eq!(truncate_chars(&long, 2000).len(), 2000);
        assert_eq!(truncate_chars(&long, 200).len(), 200);
    }
}
