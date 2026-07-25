//! 用量记录存储:按凭据分桶、每凭据 LRU 上限、CST 日聚合、分页查询。
//!
//! 内存布局:`RwLock<Vec<UsageRecord>>`(全量单向量,按 created_at 追加)。查询侧
//! 在读锁下过滤/排序/分页;写侧在写锁下追加 + 单次 `retain` 逐凭据裁到上限。
//! 落盘由 `persist` 层的脏标记 + 5s 定时器驱动,热路径不做 I/O。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::stats::model::{UsageRecord, cst_daykey};
use crate::stats::persist::{DirtyFlag, dirty_channel, read_json, spawn_flush_loop};

/// 每凭据用量记录上限(超出淘汰最旧)。
pub const USAGE_CAP_PER_CREDENTIAL: usize = 10_000;
/// 单日记录导出上限(端点 6 契约:max 2000)。
pub const DAILY_RECORDS_MAX: usize = 2_000;

/// 一页记录 + 分页元信息。
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub total_pages: usize,
}

/// 单凭据当日(CST)聚合。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DaySummary {
    pub total_requests: u64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    pub total_credits: f64,
}

/// 全局单日(CST)聚合(跨全部凭据)。
#[derive(Debug, Clone, PartialEq)]
pub struct DailyRollup {
    pub date: String,
    pub total_requests: u64,
    pub total_cost: f64,
    pub total_credits: f64,
}

/// 时间窗口用量聚合(跨全部凭据),供 `GET /api/admin/usage/summary?range=` 使用。
/// 全字段以 f64/i64 原始精度累加,不预先四舍五入;credits 沿用既有约定
/// (`credits_used` 缺失记 0,不做 cost→credits 反算)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RangeSummary {
    /// 命中窗口的记录条数。
    pub total_requests: u64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    pub total_credits: f64,
    /// 窗口内**带延迟记录**的 `latency_ms` 之和(用于算 `avgLatencyMs`)。
    pub latency_ms_sum: u64,
    /// 窗口内**带延迟记录**的条数(分母;可能 < total_requests,因旧记录无 latency)。
    pub latency_sample_count: u64,
}

/// 时间分桶的单桶聚合(供图表)。`bucket_start_unix` 为该桶起始 unix 秒(桶宽由调用侧决定)。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageBucket {
    pub bucket_start_unix: i64,
    pub total_requests: u64,
    pub total_cost: f64,
    pub total_credits: f64,
}

/// 单模型维度的用量聚合(供 API-KEY 汇总的 by_model 明细)。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelUsageAgg {
    pub model: String,
    pub requests: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
}

/// 单个 API-KEY 的用量聚合(跨全部凭据/模型)。credits 由 handler 层按需换算,
/// 本层只提供 cost/token/requests 原始聚合与 by_model 明细。
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKeyUsageSummary {
    pub api_key_id: u32,
    pub total_requests: u64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    /// 记录内已填的 credits_used 之和(未填则计 0)。
    pub total_credits: f64,
    pub by_model: Vec<ModelUsageAgg>,
}

/// 聚合过程中的按模型累加器(内部用)。
struct ModelAgg {
    requests: u64,
    input_tokens: i64,
    output_tokens: i64,
    cost: f64,
}

impl ApiKeyUsageSummary {
    fn new(api_key_id: u32) -> Self {
        Self {
            api_key_id,
            total_requests: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cost: 0.0,
            total_credits: 0.0,
            by_model: Vec::new(),
        }
    }

    /// 累加一条记录到总量与 by_model 明细。
    fn accumulate(&mut self, r: &UsageRecord, by_model: &mut HashMap<String, ModelAgg>) {
        self.total_requests += 1;
        self.total_input_tokens += r.input_tokens as i64;
        self.total_output_tokens += r.output_tokens as i64;
        self.total_cost += r.estimated_cost;
        self.total_credits += r.credits_used.unwrap_or(0.0);
        let m = by_model.entry(r.model.clone()).or_insert(ModelAgg {
            requests: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
        });
        m.requests += 1;
        m.input_tokens += r.input_tokens as i64;
        m.output_tokens += r.output_tokens as i64;
        m.cost += r.estimated_cost;
    }

    /// 收尾:把 by_model map 转成按 model 名升序的稳定 Vec。
    fn finish(&mut self, by_model: HashMap<String, ModelAgg>) {
        let mut models: Vec<ModelUsageAgg> = by_model
            .into_iter()
            .map(|(model, a)| ModelUsageAgg {
                model,
                requests: a.requests,
                input_tokens: a.input_tokens,
                output_tokens: a.output_tokens,
                cost: a.cost,
            })
            .collect();
        models.sort_by(|a, b| a.model.cmp(&b.model));
        self.by_model = models;
    }
}

/// 用量追踪器。`Arc<UsageTracker>` 可在 relay 与 admin 间共享。
pub struct UsageTracker {
    records: RwLock<Vec<UsageRecord>>,
    dirty: DirtyFlag,
}

impl UsageTracker {
    /// 从 `path` 载入(不存在则空),并启动后台刷盘任务。
    pub fn load(path: PathBuf) -> Arc<Self> {
        let initial: Vec<UsageRecord> = read_json(&path).ok().flatten().unwrap_or_default();
        let (dirty, handle) = dirty_channel();
        let tracker = Arc::new(Self {
            records: RwLock::new(initial),
            dirty,
        });
        let snap = tracker.clone();
        spawn_flush_loop(
            path,
            handle,
            crate::stats::persist::FLUSH_INTERVAL_SECS,
            move || {
                // 同步取快照:阻塞式读锁(在 spawn_blocking 线程里,可接受)。
                let guard = snap.records.blocking_read();
                serde_json::to_vec_pretty(&*guard).unwrap_or_else(|_| b"[]".to_vec())
            },
        );
        tracker
    }

    /// 记录一条用量(热路径,fire-and-forget)。追加后按凭据裁到上限,置脏。
    /// 归属 api_key_id 恒为 0(全局/开放模式);需按 API-KEY 归属时用
    /// [`record_usage_with_api_key`](Self::record_usage_with_api_key)。
    #[allow(clippy::too_many_arguments)]
    pub async fn record_usage(
        &self,
        credential_id: u32,
        model: String,
        input_tokens: i32,
        output_tokens: i32,
        estimated_cost: f64,
        client_ip: Option<String>,
        cache_read_input_tokens: Option<i32>,
        cache_creation_input_tokens: Option<i32>,
        now_unix: i64,
    ) {
        self.record_usage_with_api_key(
            credential_id,
            0,
            model,
            input_tokens,
            output_tokens,
            estimated_cost,
            client_ip,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            now_unix,
        )
        .await;
    }

    /// 记录一条用量并归属到指定 API-KEY(`api_key_id`;0 = 无归属)。热路径,
    /// 追加后按凭据裁到上限,置脏。auth/relay 接线后由中转侧传入解析出的 key id。
    ///
    /// 本便捷签名不带 credits(`credits_used` 记 `None`);需落真实积分消耗
    /// (来自 meteringEvent)时用 [`record_usage_full`](Self::record_usage_full)。
    #[allow(clippy::too_many_arguments)]
    pub async fn record_usage_with_api_key(
        &self,
        credential_id: u32,
        api_key_id: u32,
        model: String,
        input_tokens: i32,
        output_tokens: i32,
        estimated_cost: f64,
        client_ip: Option<String>,
        cache_read_input_tokens: Option<i32>,
        cache_creation_input_tokens: Option<i32>,
        now_unix: i64,
    ) {
        self.record_usage_full(
            credential_id,
            api_key_id,
            model,
            input_tokens,
            output_tokens,
            estimated_cost,
            None,
            client_ip,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            None,
            now_unix,
        )
        .await;
    }

    /// 记录一条用量(全字段,含 `credits_used`)。credits 来自 `meteringEvent` 的真实
    /// 积分消耗(`usage`),`None` 表示上游未发 meteringEvent(回退字符估算,不落积分)。
    /// `latency_ms` 为本次请求端到端延迟(毫秒;`None`=未测量),供 usage-summary 计 `avgLatencyMs`。
    /// 其余同 [`record_usage_with_api_key`](Self::record_usage_with_api_key)。
    #[allow(clippy::too_many_arguments)]
    pub async fn record_usage_full(
        &self,
        credential_id: u32,
        api_key_id: u32,
        model: String,
        input_tokens: i32,
        output_tokens: i32,
        estimated_cost: f64,
        credits_used: Option<f64>,
        client_ip: Option<String>,
        cache_read_input_tokens: Option<i32>,
        cache_creation_input_tokens: Option<i32>,
        latency_ms: Option<u64>,
        now_unix: i64,
    ) {
        let rec = UsageRecord {
            credential_id,
            model,
            input_tokens,
            output_tokens,
            estimated_cost,
            credits_used,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            created_at_unix: now_unix,
            client_ip,
            api_key_id,
            latency_ms,
        };
        {
            let mut guard = self.records.write().await;
            guard.push(rec);
            evict_per_credential(&mut guard, USAGE_CAP_PER_CREDENTIAL);
        }
        self.dirty.mark_dirty();
    }

    /// 某凭据的记录分页(按 created_at 降序,最新在前)。
    pub async fn records_for_credential(
        &self,
        credential_id: u32,
        page: usize,
        page_size: usize,
    ) -> Page<UsageRecord> {
        let guard = self.records.read().await;
        let mut filtered: Vec<UsageRecord> = guard
            .iter()
            .filter(|r| r.credential_id == credential_id)
            .cloned()
            .collect();
        drop(guard);
        sort_desc(&mut filtered);
        paginate(filtered, page, page_size)
    }

    /// 某凭据当日(CST,以 `now_unix` 定"今天")聚合。
    pub async fn today_summary(&self, credential_id: u32, now_unix: i64) -> DaySummary {
        let today = cst_daykey(now_unix);
        let guard = self.records.read().await;
        let mut s = DaySummary::default();
        for r in guard.iter() {
            if r.credential_id == credential_id && cst_daykey(r.created_at_unix) == today {
                s.total_requests += 1;
                s.total_input_tokens += r.input_tokens as i64;
                s.total_output_tokens += r.output_tokens as i64;
                s.total_cost += r.estimated_cost;
                s.total_credits += r.credits_used.unwrap_or(0.0);
            }
        }
        s
    }

    /// 全部 CST 日的全局聚合,按日期降序(最新在前)。
    pub async fn daily_rollup(&self) -> Vec<DailyRollup> {
        let guard = self.records.read().await;
        let mut by_day: HashMap<String, DailyRollup> = HashMap::new();
        for r in guard.iter() {
            let day = cst_daykey(r.created_at_unix);
            let e = by_day.entry(day.clone()).or_insert(DailyRollup {
                date: day,
                total_requests: 0,
                total_cost: 0.0,
                total_credits: 0.0,
            });
            e.total_requests += 1;
            e.total_cost += r.estimated_cost;
            e.total_credits += r.credits_used.unwrap_or(0.0);
        }
        drop(guard);
        let mut out: Vec<DailyRollup> = by_day.into_values().collect();
        out.sort_by(|a, b| b.date.cmp(&a.date)); // 降序
        out
    }

    /// 时间窗口 `[since_unix, until_unix]`(闭区间,unix 秒)内的全局用量聚合 +
    /// 可选的时间分桶序列(桶宽 `bucket_secs` 秒;传 0 则不产生桶,返回空 Vec)。
    ///
    /// 聚合与分桶都直接扫描**原始记录**,以 f64/i64 原始精度累加,不预先四舍五入。
    /// 桶按 `bucket_start = floor(created_at / bucket_secs) * bucket_secs` 归组,
    /// 返回时按桶起始升序排列(便于前端画时间轴)。空窗口 → 全零 summary + 空桶。
    ///
    /// 注意:原始记录按凭据有 `USAGE_CAP_PER_CREDENTIAL` 上限,极长窗口(如 30d)下
    /// 高流量凭据的最旧记录可能已被淘汰,故此方法只保证"未被淘汰的原始记录"精确求和;
    /// handler 侧对长窗口会用每日汇总(`daily_rollup`)交叉校验/兜底补齐(见 handler 注释)。
    pub async fn range_summary(
        &self,
        since_unix: i64,
        until_unix: i64,
        bucket_secs: i64,
    ) -> (RangeSummary, Vec<UsageBucket>) {
        let guard = self.records.read().await;
        let mut s = RangeSummary::default();
        // 桶:bucket_start_unix → (requests, cost, credits)
        let mut buckets: HashMap<i64, (u64, f64, f64)> = HashMap::new();
        for r in guard.iter() {
            if r.created_at_unix < since_unix || r.created_at_unix > until_unix {
                continue;
            }
            let credits = r.credits_used.unwrap_or(0.0);
            s.total_requests += 1;
            s.total_input_tokens += r.input_tokens as i64;
            s.total_output_tokens += r.output_tokens as i64;
            s.total_cost += r.estimated_cost;
            s.total_credits += credits;
            if let Some(lat) = r.latency_ms {
                s.latency_ms_sum += lat;
                s.latency_sample_count += 1;
            }
            if bucket_secs > 0 {
                let start = (r.created_at_unix.div_euclid(bucket_secs)) * bucket_secs;
                let e = buckets.entry(start).or_insert((0, 0.0, 0.0));
                e.0 += 1;
                e.1 += r.estimated_cost;
                e.2 += credits;
            }
        }
        drop(guard);
        let mut series: Vec<UsageBucket> = buckets
            .into_iter()
            .map(
                |(bucket_start_unix, (total_requests, total_cost, total_credits))| UsageBucket {
                    bucket_start_unix,
                    total_requests,
                    total_cost,
                    total_credits,
                },
            )
            .collect();
        series.sort_by_key(|b| b.bucket_start_unix);
        (s, series)
    }

    /// 窗口 `[since_unix, until_unix]` 内的原始记录按 CST 日聚合成
    /// `daykey → (requests, cost, credits)`,供 handler 侧长窗口的"每日汇总兜底"取 max 用。
    /// credits 缺失记 0(与其它聚合一致)。
    pub async fn raw_daily_agg_in_window(
        &self,
        since_unix: i64,
        until_unix: i64,
    ) -> HashMap<String, (u64, f64, f64)> {
        let guard = self.records.read().await;
        let mut by_day: HashMap<String, (u64, f64, f64)> = HashMap::new();
        for r in guard.iter() {
            if r.created_at_unix < since_unix || r.created_at_unix > until_unix {
                continue;
            }
            let e = by_day
                .entry(cst_daykey(r.created_at_unix))
                .or_insert((0, 0.0, 0.0));
            e.0 += 1;
            e.1 += r.estimated_cost;
            e.2 += r.credits_used.unwrap_or(0.0);
        }
        by_day
    }

    /// 某 CST 日的记录分页(降序);先裁到 `DAILY_RECORDS_MAX` 再分页。
    pub async fn records_for_day(
        &self,
        date: &str,
        page: usize,
        page_size: usize,
    ) -> Page<UsageRecord> {
        let guard = self.records.read().await;
        let mut filtered: Vec<UsageRecord> = guard
            .iter()
            .filter(|r| cst_daykey(r.created_at_unix) == date)
            .cloned()
            .collect();
        drop(guard);
        sort_desc(&mut filtered);
        filtered.truncate(DAILY_RECORDS_MAX);
        paginate(filtered, page, page_size)
    }

    // ------------------------------------------------------------------
    // 按 API-KEY 归属的过滤/聚合/重置(P2)。全部在读/写锁下线性扫描 records,
    // 与既有按凭据的查询同构;对外 camelCase 包装由 admin/user handler 后续完成。
    // ------------------------------------------------------------------

    /// 单个 API-KEY 的用量聚合;无任何记录时返回全零(非 None,便于 handler 直接输出)。
    pub async fn summary_for_api_key(&self, api_key_id: u32) -> ApiKeyUsageSummary {
        let guard = self.records.read().await;
        let mut s = ApiKeyUsageSummary::new(api_key_id);
        let mut by_model: HashMap<String, ModelAgg> = HashMap::new();
        for r in guard.iter() {
            if r.api_key_id == api_key_id {
                s.accumulate(r, &mut by_model);
            }
        }
        s.finish(by_model);
        s
    }

    /// 所有出现过的 API-KEY 的用量聚合(按 api_key_id 升序);不含 id=0 的无归属记录。
    pub async fn summaries_by_api_key(&self) -> Vec<ApiKeyUsageSummary> {
        let guard = self.records.read().await;
        let mut acc: HashMap<u32, (ApiKeyUsageSummary, HashMap<String, ModelAgg>)> = HashMap::new();
        for r in guard.iter() {
            if r.api_key_id == 0 {
                continue;
            }
            let e = acc
                .entry(r.api_key_id)
                .or_insert_with(|| (ApiKeyUsageSummary::new(r.api_key_id), HashMap::new()));
            e.0.accumulate(r, &mut e.1);
        }
        drop(guard);
        let mut out: Vec<ApiKeyUsageSummary> = acc
            .into_values()
            .map(|(mut s, by_model)| {
                s.finish(by_model);
                s
            })
            .collect();
        out.sort_by_key(|s| s.api_key_id);
        out
    }

    /// 单个 API-KEY 的记录分页(按 created_at 降序,最新在前)。
    pub async fn records_for_api_key(
        &self,
        api_key_id: u32,
        page: usize,
        page_size: usize,
    ) -> Page<UsageRecord> {
        let guard = self.records.read().await;
        let mut filtered: Vec<UsageRecord> = guard
            .iter()
            .filter(|r| r.api_key_id == api_key_id)
            .cloned()
            .collect();
        drop(guard);
        sort_desc(&mut filtered);
        paginate(filtered, page, page_size)
    }

    /// 删除某 API-KEY 的全部用量记录;返回删除条数。置脏触发落盘。
    pub async fn reset_api_key(&self, api_key_id: u32) -> usize {
        let removed;
        {
            let mut guard = self.records.write().await;
            let before = guard.len();
            guard.retain(|r| r.api_key_id != api_key_id);
            removed = before - guard.len();
        }
        if removed > 0 {
            self.dirty.mark_dirty();
        }
        removed
    }

    /// 当前记录总条数(测试/观测用)。
    pub async fn len(&self) -> usize {
        self.records.read().await.len()
    }

    /// 是否为空。
    pub async fn is_empty(&self) -> bool {
        self.records.read().await.is_empty()
    }
}

/// 按 created_at 降序(最新在前);同刻按插入顺序稳定。
fn sort_desc(v: &mut [UsageRecord]) {
    v.sort_by(|a, b| b.created_at_unix.cmp(&a.created_at_unix));
}

/// 逐凭据裁剪到 `cap`:单次 O(n) 从后往前保留每凭据最新 `cap` 条,删更旧的。
/// 记录整体按 created_at 追加(近似有序),从尾扫描按凭据计数即可。
fn evict_per_credential(v: &mut Vec<UsageRecord>, cap: usize) {
    // 快速路径:没有任何凭据可能超限时直接返回。
    if v.len() <= cap {
        return;
    }
    let mut counts: HashMap<u32, usize> = HashMap::new();
    // 从最新(尾)往最旧(头)扫,标记每凭据超过 cap 的旧记录待删。
    let mut keep = vec![true; v.len()];
    for i in (0..v.len()).rev() {
        let c = counts.entry(v[i].credential_id).or_insert(0);
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

/// 通用分页的公开封装,供 eventlog 等同层模块复用。
pub fn paginate_pub<T: Clone>(items: Vec<T>, page: usize, page_size: usize) -> Page<T> {
    paginate(items, page, page_size)
}

/// 通用分页:page 从 1 起,越界钳到 [1, total_pages];空集返回 page=1/total_pages=0。
fn paginate<T: Clone>(items: Vec<T>, page: usize, page_size: usize) -> Page<T> {
    let page_size = page_size.max(1);
    let total = items.len();
    let total_pages = total.div_ceil(page_size);
    let page = if total_pages == 0 {
        1
    } else {
        page.clamp(1, total_pages)
    };
    let start = (page - 1) * page_size;
    let end = (start + page_size).min(total);
    let slice = if start < total {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    Page {
        items: slice,
        total,
        page,
        page_size,
        total_pages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(cred: u32, at: i64, cost: f64, out: i32) -> UsageRecord {
        UsageRecord {
            credential_id: cred,
            model: "claude-sonnet-4.5".into(),
            input_tokens: 100,
            output_tokens: out,
            estimated_cost: cost,
            credits_used: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            created_at_unix: at,
            client_ip: Some("1.2.3.4".into()),
            api_key_id: 0,
            latency_ms: None,
        }
    }

    #[test]
    fn paginate_empty_state() {
        let p = paginate::<UsageRecord>(vec![], 1, 20);
        assert_eq!(p.total, 0);
        assert_eq!(p.total_pages, 0);
        assert_eq!(p.page, 1);
        assert!(p.items.is_empty());
    }

    #[test]
    fn paginate_clamps_out_of_range() {
        let items: Vec<u32> = (0..25).collect();
        // 3 页(20/页 → 2 页;这里 page_size=10 → 3 页)
        let p = paginate(items.clone(), 99, 10);
        assert_eq!(p.total, 25);
        assert_eq!(p.total_pages, 3);
        assert_eq!(p.page, 3); // 钳到最后一页
        assert_eq!(p.items, vec![20, 21, 22, 23, 24]);
        // page 0 → 钳到 1
        let p0 = paginate(items, 0, 10);
        assert_eq!(p0.page, 1);
        assert_eq!(p0.items.len(), 10);
    }

    #[test]
    fn evict_keeps_newest_per_credential() {
        // cred 1 放 5 条(at 1..=5),cred 2 放 2 条;cap=3
        let mut v = vec![
            rec(1, 1, 0.0, 0),
            rec(1, 2, 0.0, 0),
            rec(2, 10, 0.0, 0),
            rec(1, 3, 0.0, 0),
            rec(1, 4, 0.0, 0),
            rec(2, 11, 0.0, 0),
            rec(1, 5, 0.0, 0),
        ];
        evict_per_credential(&mut v, 3);
        let cred1: Vec<i64> = v
            .iter()
            .filter(|r| r.credential_id == 1)
            .map(|r| r.created_at_unix)
            .collect();
        let cred2: Vec<i64> = v
            .iter()
            .filter(|r| r.credential_id == 2)
            .map(|r| r.created_at_unix)
            .collect();
        // cred1 只留最新 3 条(3,4,5),旧的 1,2 淘汰
        assert_eq!(cred1, vec![3, 4, 5]);
        // cred2 未超限,全留
        assert_eq!(cred2, vec![10, 11]);
    }

    #[tokio::test]
    async fn record_and_paginate_desc() {
        let path =
            std::env::temp_dir().join(format!("kiro2api_usage_rec_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let t = UsageTracker::load(path.clone());
        assert!(t.is_empty().await);
        for i in 1..=5 {
            t.record_usage(7, "m".into(), 10, 20, 0.01, None, None, None, 1000 + i)
                .await;
        }
        assert_eq!(t.len().await, 5);
        let p = t.records_for_credential(7, 1, 2).await;
        assert_eq!(p.total, 5);
        assert_eq!(p.total_pages, 3);
        // 降序:最新 created_at 1005,1004 在第一页
        assert_eq!(p.items[0].created_at_unix, 1005);
        assert_eq!(p.items[1].created_at_unix, 1004);
        // 别的凭据 → 空
        let empty = t.records_for_credential(99, 1, 10).await;
        assert_eq!(empty.total, 0);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn today_summary_cst_bucketing() {
        let path =
            std::env::temp_dir().join(format!("kiro2api_usage_today_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let t = UsageTracker::load(path.clone());
        // "今天" now = 2026-06-27T02:00:00Z (== 10:00 CST, 属 27 日)
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-27T02:00:00Z")
            .unwrap()
            .timestamp();
        // 一条属今天(27 CST):UTC 2026-06-26T20:00:00Z == 04:00 CST 27 日
        let today_at = chrono::DateTime::parse_from_rfc3339("2026-06-26T20:00:00Z")
            .unwrap()
            .timestamp();
        // 一条属昨天(26 CST):UTC 2026-06-26T10:00:00Z == 18:00 CST 26 日
        let yday_at = chrono::DateTime::parse_from_rfc3339("2026-06-26T10:00:00Z")
            .unwrap()
            .timestamp();
        t.record_usage(5, "m".into(), 100, 200, 0.50, None, None, None, today_at)
            .await;
        t.record_usage(5, "m".into(), 100, 300, 0.30, None, None, None, today_at)
            .await;
        t.record_usage(5, "m".into(), 100, 999, 9.99, None, None, None, yday_at)
            .await;
        let s = t.today_summary(5, now).await;
        assert_eq!(s.total_requests, 2); // 只算今天的两条
        assert_eq!(s.total_output_tokens, 500);
        assert!((s.total_cost - 0.80).abs() < 1e-9);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn api_key_filter_summary_records_and_reset() {
        let path =
            std::env::temp_dir().join(format!("kiro2api_usage_apikey_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let t = UsageTracker::load(path.clone());

        // key 7:两条(model a + model b);key 8:一条;无归属(0):一条。
        t.record_usage_with_api_key(1, 7, "a".into(), 100, 200, 1.0, None, None, None, 1000)
            .await;
        t.record_usage_with_api_key(2, 7, "b".into(), 10, 20, 0.5, None, None, None, 1001)
            .await;
        t.record_usage_with_api_key(1, 8, "a".into(), 5, 5, 0.25, None, None, None, 1002)
            .await;
        // 走旧签名 → 归属 0(无归属),不应混入按 key 的聚合
        t.record_usage(3, "a".into(), 9, 9, 9.0, None, None, None, 1003)
            .await;

        // 单 key 聚合
        let s7 = t.summary_for_api_key(7).await;
        assert_eq!(s7.api_key_id, 7);
        assert_eq!(s7.total_requests, 2);
        assert_eq!(s7.total_input_tokens, 110);
        assert_eq!(s7.total_output_tokens, 220);
        assert!((s7.total_cost - 1.5).abs() < 1e-9);
        // by_model 按 model 名升序:a 在 b 前
        assert_eq!(s7.by_model.len(), 2);
        assert_eq!(s7.by_model[0].model, "a");
        assert_eq!(s7.by_model[0].requests, 1);
        assert_eq!(s7.by_model[1].model, "b");

        // 不存在的 key → 全零、空 by_model
        let s99 = t.summary_for_api_key(99).await;
        assert_eq!(s99.total_requests, 0);
        assert!(s99.by_model.is_empty());

        // 全部 key 聚合(升序,不含 id=0)
        let all = t.summaries_by_api_key().await;
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].api_key_id, 7);
        assert_eq!(all[1].api_key_id, 8);

        // 记录分页按 key 过滤(降序)
        let p7 = t.records_for_api_key(7, 1, 10).await;
        assert_eq!(p7.total, 2);
        assert_eq!(p7.items[0].created_at_unix, 1001); // 最新在前
        assert_eq!(p7.items[0].api_key_id, 7);

        // 重置某 key 只清该 key,其他不动
        let removed = t.reset_api_key(7).await;
        assert_eq!(removed, 2);
        assert_eq!(t.records_for_api_key(7, 1, 10).await.total, 0);
        assert_eq!(t.records_for_api_key(8, 1, 10).await.total, 1);
        // 无归属(0)记录仍在
        assert_eq!(t.len().await, 2);
        // 重置不存在的 key → 0
        assert_eq!(t.reset_api_key(12345).await, 0);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn range_summary_window_and_buckets() {
        let path =
            std::env::temp_dir().join(format!("kiro2api_usage_range_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let t = UsageTracker::load(path.clone());
        // 三条:1000(桶0)、1000+30=1030(同桶,桶宽3600)、5000(窗外)。
        t.record_usage_full(
            1,
            0,
            "m".into(),
            10,
            20,
            1.0,
            Some(2.0),
            None,
            None,
            None,
            Some(100),
            1000,
        )
        .await;
        t.record_usage_full(
            1,
            0,
            "m".into(),
            5,
            5,
            0.5,
            Some(1.0),
            None,
            None,
            None,
            Some(300),
            1030,
        )
        .await;
        t.record_usage_full(
            2,
            0,
            "m".into(),
            99,
            99,
            9.9,
            Some(9.9),
            None,
            None,
            None,
            Some(999),
            5000,
        )
        .await;
        // 窗口 [900, 2000] 只含前两条;桶宽 3600 → 都落在 bucket_start=0。
        let (s, series) = t.range_summary(900, 2000, 3600).await;
        assert_eq!(s.total_requests, 2);
        assert_eq!(s.total_input_tokens, 15);
        assert_eq!(s.total_output_tokens, 25);
        assert!((s.total_cost - 1.5).abs() < 1e-9);
        assert!((s.total_credits - 3.0).abs() < 1e-9);
        // 延迟:窗口内两条 latency=100+300,样本数 2 → 均值 200;窗外 999 不计入。
        assert_eq!(s.latency_ms_sum, 400);
        assert_eq!(s.latency_sample_count, 2);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].bucket_start_unix, 0);
        assert_eq!(series[0].total_requests, 2);
        assert!((series[0].total_cost - 1.5).abs() < 1e-9);
        // bucket_secs=0 → 不产桶。
        let (_s2, empty) = t.range_summary(900, 2000, 0).await;
        assert!(empty.is_empty());
        // 空窗口 → 全零。
        let (s3, series3) = t.range_summary(1_000_000, 2_000_000, 3600).await;
        assert_eq!(s3.total_requests, 0);
        assert!(series3.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn daily_rollup_desc_and_day_records() {
        let path =
            std::env::temp_dir().join(format!("kiro2api_usage_roll_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let t = UsageTracker::load(path.clone());
        let d26 = chrono::DateTime::parse_from_rfc3339("2026-06-26T02:00:00Z")
            .unwrap()
            .timestamp(); // 26 CST
        let d27 = chrono::DateTime::parse_from_rfc3339("2026-06-27T02:00:00Z")
            .unwrap()
            .timestamp(); // 27 CST
        t.record_usage(1, "m".into(), 1, 1, 1.0, None, None, None, d26)
            .await;
        t.record_usage(2, "m".into(), 1, 1, 2.0, None, None, None, d27)
            .await;
        t.record_usage(1, "m".into(), 1, 1, 3.0, None, None, None, d27)
            .await;
        let roll = t.daily_rollup().await;
        assert_eq!(roll.len(), 2);
        // 降序:27 在前
        assert_eq!(roll[0].date, "2026-06-27");
        assert_eq!(roll[0].total_requests, 2);
        assert!((roll[0].total_cost - 5.0).abs() < 1e-9);
        assert_eq!(roll[1].date, "2026-06-26");
        // 单日记录
        let recs = t.records_for_day("2026-06-27", 1, 10).await;
        assert_eq!(recs.total, 2);
        let _ = std::fs::remove_file(&path);
    }
}
