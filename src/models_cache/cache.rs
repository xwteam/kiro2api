//! `ModelsCache`:按凭据 id 键控的模型清单缓存(TTL 判定用注入的 `now_unix`)。
//!
//! 纯内存(不落盘):模型清单廉价可随时重拉,故无需持久化;进程重启后按需惰性回填。
//! 线程安全:内部 `tokio::RwLock`,经 `Arc` 共享给 admin handler。
//! `get_union` 汇总所有账号缓存里仍新鲜的条目,按规范化 id 去重 + 排序;空时调用方回落静态目录。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::models_cache::MODELS_TTL_SECS;
use crate::models_cache::model::ModelInfo;

/// 单个凭据的模型清单快照(带抓取时刻,供 TTL 判定)。
#[derive(Debug, Clone)]
pub struct ModelsSnapshot {
    /// 归约后的模型信息(已去重,顺序即上游顺序)。
    pub models: Vec<ModelInfo>,
    /// 抓取时刻(Unix 秒)。
    pub fetched_at_unix: u64,
}

impl ModelsSnapshot {
    /// 相对 `now_unix` 是否仍新鲜(未超过 TTL)。
    pub fn is_fresh(&self, now_unix: u64) -> bool {
        now_unix.saturating_sub(self.fetched_at_unix) < MODELS_TTL_SECS
    }
}

/// 按凭据 id(String)键控的模型清单缓存。
#[derive(Default)]
pub struct ModelsCache {
    inner: RwLock<HashMap<String, ModelsSnapshot>>,
}

impl ModelsCache {
    /// 新建空缓存(经 `Arc` 共享)。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 写入/覆盖某凭据的模型清单快照。
    pub async fn put(&self, id: impl Into<String>, models: Vec<ModelInfo>, now_unix: u64) {
        let mut map = self.inner.write().await;
        map.insert(
            id.into(),
            ModelsSnapshot {
                models,
                fetched_at_unix: now_unix,
            },
        );
    }

    /// 取某凭据仍新鲜的快照;过期/缺失返回 None。
    pub async fn get_fresh(&self, id: &str, now_unix: u64) -> Option<ModelsSnapshot> {
        let map = self.inner.read().await;
        map.get(id).filter(|s| s.is_fresh(now_unix)).cloned()
    }

    /// 某凭据是否有仍新鲜的缓存(避免重复实拉)。
    pub async fn is_fresh(&self, id: &str, now_unix: u64) -> bool {
        let map = self.inner.read().await;
        map.get(id).is_some_and(|s| s.is_fresh(now_unix))
    }

    /// 所有账号仍新鲜缓存的并集:按规范化 id 去重(先出现者优先),再按 id 升序排序。
    /// 空缓存/全过期时返回空 Vec(调用方据此回落静态目录)。
    pub async fn get_union(&self, now_unix: u64) -> Vec<ModelInfo> {
        let map = self.inner.read().await;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<ModelInfo> = Vec::new();
        for snap in map.values() {
            if !snap.is_fresh(now_unix) {
                continue;
            }
            for m in &snap.models {
                if seen.insert(m.id.clone()) {
                    out.push(m.clone());
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, owner: &str, max: u32) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display_name: id.into(),
            owned_by: owner.into(),
            max_tokens: max,
            rate_multiplier: None,
        }
    }

    #[tokio::test]
    async fn put_then_get_fresh_within_ttl() {
        let cache = ModelsCache::new();
        assert!(cache.is_empty().await);
        cache
            .put(
                "7",
                vec![info("claude-sonnet-5", "anthropic", 200_000)],
                1000,
            )
            .await;
        assert_eq!(cache.len().await, 1);
        // 新鲜:1000 + (TTL-1) < 1000 + TTL
        assert!(cache.is_fresh("7", 1000 + MODELS_TTL_SECS - 1).await);
        let got = cache.get_fresh("7", 1000).await.unwrap();
        assert_eq!(got.models.len(), 1);
    }

    #[tokio::test]
    async fn expired_snapshot_not_returned() {
        let cache = ModelsCache::new();
        cache.put("7", vec![info("auto", "kiro", 0)], 1000).await;
        // 到点即视为过期:1000 + TTL
        assert!(cache.get_fresh("7", 1000 + MODELS_TTL_SECS).await.is_none());
        assert!(!cache.is_fresh("7", 1000 + MODELS_TTL_SECS).await);
        // 未知 id
        assert!(cache.get_fresh("999", 1000).await.is_none());
    }

    #[tokio::test]
    async fn union_dedupes_across_accounts_and_sorts() {
        let cache = ModelsCache::new();
        // 账号 A:auto + claude
        cache
            .put(
                "a",
                vec![
                    info("auto", "kiro", 0),
                    info("claude-sonnet-5", "anthropic", 200_000),
                ],
                1000,
            )
            .await;
        // 账号 B:claude(重复)+ gpt
        cache
            .put(
                "b",
                vec![
                    info("claude-sonnet-5", "anthropic", 200_000),
                    info("gpt-5.6-sol", "openai", 128_000),
                ],
                1000,
            )
            .await;
        let union = cache.get_union(1000).await;
        // 去重:auto / claude-sonnet-5 / gpt-5.6-sol,共 3
        assert_eq!(union.len(), 3);
        // 排序:按 id 升序 → auto, claude-sonnet-5, gpt-5.6-sol
        assert_eq!(union[0].id, "auto");
        assert_eq!(union[1].id, "claude-sonnet-5");
        assert_eq!(union[2].id, "gpt-5.6-sol");
    }

    #[tokio::test]
    async fn union_empty_when_all_stale() {
        let cache = ModelsCache::new();
        cache.put("a", vec![info("auto", "kiro", 0)], 1000).await;
        // 全过期 → 空并集(调用方回落静态目录)
        let union = cache.get_union(1000 + MODELS_TTL_SECS).await;
        assert!(union.is_empty());
    }

    #[tokio::test]
    async fn union_skips_stale_keeps_fresh() {
        let cache = ModelsCache::new();
        let now = 10 * MODELS_TTL_SECS;
        // a 过期(抓取于 now-TTL,恰好到点 → 过期)
        cache
            .put(
                "a",
                vec![info("stale-model", "unknown", 0)],
                now - MODELS_TTL_SECS,
            )
            .await;
        // b 新鲜(刚抓取)
        cache
            .put("b", vec![info("fresh-model", "unknown", 0)], now)
            .await;
        let union = cache.get_union(now).await;
        assert_eq!(union.len(), 1);
        assert_eq!(union[0].id, "fresh-model");
    }

    #[tokio::test]
    async fn empty_cache_union_is_empty() {
        let cache = ModelsCache::new();
        assert!(cache.get_union(1000).await.is_empty());
    }
}
