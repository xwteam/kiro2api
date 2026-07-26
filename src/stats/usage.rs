//! 用量记录存储:按凭据分桶、每凭据 LRU 上限、CST 日聚合、分页查询。
//!
//! 内存布局:`RwLock<UsageState>` —— 原始记录(全量单向量,按 created_at 追加)+ 每凭据
//! 存活计数 + **每日汇总**。查询侧在读锁下过滤/排序/分页;写侧在写锁下追加,超额记录累计
//! 到阈值才做一次全表压实(把逐次插入的 O(n) 扫描摊销掉)。
//!
//! 每日汇总(`daily`)记的是**已被淘汰**的原始记录在当日(CST)的累计:记录淘汰前先并入
//! 汇总,汇总自身按天保留(上限 [`DAILY_ROLLUP_MAX_DAYS`] 天)、不随原始记录淘汰,故
//! [`UsageTracker::daily_rollup`] = 存活记录聚合 + 已淘汰累计,长窗口统计才有真正的兜底。
//!
//! 落盘由 `persist` 层的脏标记 + 5s 定时器驱动,热路径不做 I/O;后台任务只持 `Weak`,
//! 停机时的最终刷盘由 [`UsageTracker::flush_now`] 显式驱动。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::stats::model::{UsageRecord, cst_daykey};
use crate::stats::persist::{
    DirtyFlag, dirty_channel, read_json, spawn_flush_loop_opt, write_bytes_atomic,
};

/// 每凭据用量记录上限(超出淘汰最旧)。
pub const USAGE_CAP_PER_CREDENTIAL: usize = 10_000;
/// 单日记录导出上限(端点 6 契约:max 2000)。
pub const DAILY_RECORDS_MAX: usize = 2_000;
/// 每日汇总保留天数上限(超出丢最旧的日)。每天一条定长累计(几十字节),
/// 400 天 ≈ 十几 KB,内存可忽略,且足够覆盖最长的 30d 窗口查询。
pub const DAILY_ROLLUP_MAX_DAYS: usize = 400;
/// 触发一次全表压实的超额条数:超过每凭据上限的记录先累计,攒够该阈值才压实一次,
/// 把"每次插入都全表扫描"摊销成每 `EVICT_BATCH` 次插入一次 O(n)。
/// 代价是记录数会短暂超出上限最多 `EVICT_BATCH` 条(纯内存冗余,查询口径不受影响)。
const EVICT_BATCH: usize = 256;

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

/// 已淘汰原始记录在某 CST 日的累计。当日总量 = 存活记录当日聚合 + 本累计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
struct DayArchive {
    requests: u64,
    cost: f64,
    credits: f64,
}

impl DayArchive {
    /// 并入一条即将被淘汰的记录。credits 缺失记 0(与其它聚合口径一致)。
    fn absorb(&mut self, r: &UsageRecord) {
        self.requests += 1;
        self.cost += r.estimated_cost;
        self.credits += r.credits_used.unwrap_or(0.0);
    }
}

/// 锁内状态:原始记录 + 每凭据存活计数 + 待压实超额数 + 每日汇总。
#[derive(Default)]
struct UsageState {
    records: Vec<UsageRecord>,
    /// credential_id → 存活记录条数。增量维护,免去每次插入的全表扫描。
    counts: HashMap<u32, usize>,
    /// 已超出每凭据上限、尚未压实的记录数。
    overflow: usize,
    /// CST 日键 → 已淘汰记录的当日累计(按键升序即按日期升序,便于裁最旧的日)。
    daily: BTreeMap<String, DayArchive>,
}

impl UsageState {
    /// 由落盘内容构造:重算计数,并把超限部分(旧文件可能是别的上限写的)立即压实。
    fn from_persisted(p: PersistedUsage, cap: usize) -> Self {
        let mut st = Self {
            records: p.records,
            counts: HashMap::new(),
            overflow: 0,
            daily: p.daily,
        };
        st.recount(cap);
        st.compact(cap);
        st.trim_daily();
        st
    }

    /// 追加一条记录;仅当超额攒够 [`EVICT_BATCH`] 才做一次全表压实。
    fn push(&mut self, rec: UsageRecord, cap: usize) {
        let c = self.counts.entry(rec.credential_id).or_insert(0);
        *c += 1;
        if *c > cap {
            self.overflow += 1;
        }
        self.records.push(rec);
        if self.overflow >= EVICT_BATCH {
            self.compact(cap);
        }
    }

    /// 逐凭据裁到 `cap`:单次 O(n) 从后往前保留每凭据最新 `cap` 条,**被淘汰的记录先
    /// 并入当日汇总**再丢弃(汇总因此不随原始记录淘汰而丢失)。
    /// 记录整体按 created_at 追加(近似有序),从尾扫描按凭据计数即可。
    fn compact(&mut self, cap: usize) {
        if self.overflow == 0 {
            return;
        }
        let mut counts: HashMap<u32, usize> = HashMap::new();
        // 从最新(尾)往最旧(头)扫,标记每凭据超过 cap 的旧记录待删。
        let mut keep = vec![true; self.records.len()];
        for i in (0..self.records.len()).rev() {
            let c = counts.entry(self.records[i].credential_id).or_insert(0);
            *c += 1;
            if *c > cap {
                keep[i] = false;
            }
        }
        let old = std::mem::take(&mut self.records);
        let mut kept = Vec::with_capacity(old.len());
        for (i, r) in old.into_iter().enumerate() {
            if keep[i] {
                kept.push(r);
            } else {
                self.daily
                    .entry(cst_daykey(r.created_at_unix))
                    .or_default()
                    .absorb(&r);
            }
        }
        self.records = kept;
        self.trim_daily();
        self.recount(cap);
    }

    /// 每日汇总只保留最近 [`DAILY_ROLLUP_MAX_DAYS`] 天(BTreeMap 键序即日期序)。
    fn trim_daily(&mut self) {
        while self.daily.len() > DAILY_ROLLUP_MAX_DAYS {
            self.daily.pop_first();
        }
    }

    /// 全量重算每凭据计数与待压实超额数(压实后、批量删除后调用)。
    fn recount(&mut self, cap: usize) {
        self.counts.clear();
        for r in &self.records {
            *self.counts.entry(r.credential_id).or_insert(0) += 1;
        }
        self.overflow = self.counts.values().map(|c| c.saturating_sub(cap)).sum();
    }

    /// 锁内低成本快照:只克隆,序列化留到锁外做。
    fn to_persisted(&self) -> PersistedUsage {
        PersistedUsage {
            records: self.records.clone(),
            daily: self.daily.clone(),
        }
    }
}

/// 落盘结构(v1):原始记录 + 已淘汰记录的每日累计。
#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedUsage {
    records: Vec<UsageRecord>,
    #[serde(default)]
    daily: BTreeMap<String, DayArchive>,
}

/// 落盘文件的兼容外壳:v1 为对象,更早的版本落的是裸记录数组。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PersistedFile {
    Structured(PersistedUsage),
    /// 旧格式:`[UsageRecord, ...]`,无每日汇总。
    Legacy(Vec<UsageRecord>),
}

impl From<PersistedFile> for PersistedUsage {
    fn from(f: PersistedFile) -> Self {
        match f {
            PersistedFile::Structured(p) => p,
            PersistedFile::Legacy(records) => PersistedUsage {
                records,
                daily: BTreeMap::new(),
            },
        }
    }
}

/// 载入落盘文件:
/// - 不存在 → 空库(首次运行的正常路径,静默);
/// - 存在但读取/解析失败 → **error 日志 + 把损坏文件改名备份**后按空库继续,
///   绝不让随后的原子写静默覆盖掉可能可挽救的历史。
fn load_persisted(path: &Path) -> PersistedUsage {
    match read_json::<PersistedFile>(path) {
        Ok(Some(f)) => f.into(),
        Ok(None) => PersistedUsage::default(),
        Err(e) => {
            let backup = corrupt_backup_path(path);
            match std::fs::rename(path, &backup) {
                Ok(()) => tracing::error!(
                    path = %path.display(),
                    backup = %backup.display(),
                    error = %e,
                    "用量记录文件损坏或不兼容,已改名备份,按空库继续"
                ),
                Err(re) => tracing::error!(
                    path = %path.display(),
                    error = %e,
                    rename_error = %re,
                    "用量记录文件损坏或不兼容,且备份改名失败,按空库继续(下次刷盘将覆盖该文件)"
                ),
            }
            PersistedUsage::default()
        }
    }
}

/// 损坏文件的备份路径:`{path}.corrupt.{unix秒}.{序号}`,取第一个不存在的候选,
/// 保证同秒内多次备份互不覆盖。全部候选都被占用时回落到最后一个。
fn corrupt_backup_path(path: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let with_suffix = |n: u32| {
        let mut s = path.as_os_str().to_os_string();
        s.push(format!(".corrupt.{secs}.{n}"));
        PathBuf::from(s)
    };
    for n in 0..100 {
        let cand = with_suffix(n);
        if !cand.exists() {
            return cand;
        }
    }
    with_suffix(99)
}

/// 序列化落盘快照:紧凑 JSON(该文件只供本程序回读,pretty 只是徒增写盘量)。
/// 序列化失败(如出现非有限浮点)返回 `None` —— 本轮跳过写盘,保留磁盘上一版本,
/// 而不是把空内容原子覆写上去。
fn serialize_snapshot(snap: &PersistedUsage) -> Option<Vec<u8>> {
    match serde_json::to_vec(snap) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::error!(error = %e, "用量记录序列化失败,本轮跳过刷盘");
            None
        }
    }
}

/// 用量追踪器。`Arc<UsageTracker>` 可在 relay 与 admin 间共享。
pub struct UsageTracker {
    state: RwLock<UsageState>,
    dirty: DirtyFlag,
    /// 落盘路径,供 [`flush_now`](Self::flush_now) 在停机流程里显式刷盘。
    path: PathBuf,
}

impl UsageTracker {
    /// 从 `path` 载入(不存在则空;损坏则备份后按空库继续),并启动后台刷盘任务。
    pub fn load(path: PathBuf) -> Arc<Self> {
        let initial = UsageState::from_persisted(load_persisted(&path), USAGE_CAP_PER_CREDENTIAL);
        let (dirty, handle) = dirty_channel();
        let tracker = Arc::new(Self {
            state: RwLock::new(initial),
            dirty,
            path: path.clone(),
        });
        // 后台任务只持 Weak:持 Arc 会与 tracker 构成自引用环,`DirtyFlag` 永不析构、
        // 通道永不关闭,"关闭时最终刷盘"分支不可达。停机刷盘走显式的 `flush_now`。
        let weak = Arc::downgrade(&tracker);
        spawn_flush_loop_opt(
            path,
            handle,
            crate::stats::persist::FLUSH_INTERVAL_SECS,
            move || {
                let tracker = weak.upgrade()?;
                // 同步取快照:阻塞式读锁(在 spawn_blocking 线程里,可接受)。
                // 锁内只克隆,序列化移到锁外,避免每 5s 在读锁里做全量 JSON 序列化。
                let snap = {
                    let guard = tracker.state.blocking_read();
                    guard.to_persisted()
                };
                drop(tracker);
                serialize_snapshot(&snap)
            },
        );
        tracker
    }

    /// 立即把当前用量记录与每日汇总同步落盘(原子写),不等后台去抖定时器。
    ///
    /// 停机流程的最终刷盘入口:后台任务只在每 5s 的拍子上落盘,进程退出时最近一拍
    /// 之后的记录尚在内存,须由停机流程显式调用本方法(`stats.usage.flush_now().await`)。
    /// 失败只记 error 日志,不 panic、不阻断停机。
    pub async fn flush_now(&self) {
        let snap = {
            let guard = self.state.read().await;
            guard.to_persisted()
        };
        let path = self.path.clone();
        let res = tokio::task::spawn_blocking(move || match serde_json::to_vec(&snap) {
            Ok(bytes) => write_bytes_atomic(&path, &bytes),
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!(error = %e, "用量记录最终刷盘失败"),
            Err(e) => tracing::error!(error = %e, "用量记录最终刷盘任务 join 失败"),
        }
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
            let mut guard = self.state.write().await;
            guard.push(rec, USAGE_CAP_PER_CREDENTIAL);
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
        let guard = self.state.read().await;
        let mut filtered: Vec<UsageRecord> = guard
            .records
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
        let guard = self.state.read().await;
        let mut s = DaySummary::default();
        for r in guard.records.iter() {
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
    ///
    /// 口径 = **存活原始记录**当日聚合 + **已淘汰记录**的当日累计(见模块头)。后者独立
    /// 留存、不随每凭据 LRU 淘汰而消失,故本汇总恒 ≥ 直接扫原始记录的结果,长窗口
    /// (7d/30d)的兜底补齐才真正生效。tokens 无当日累计,故此结构只给 requests/cost/credits。
    ///
    /// 注意:已并入汇总的历史累计不随 `reset_*` 回退(汇总是跨凭据/跨 key 的全局口径)。
    pub async fn daily_rollup(&self) -> Vec<DailyRollup> {
        let guard = self.state.read().await;
        let mut by_day: HashMap<String, DailyRollup> = HashMap::new();
        for (date, a) in guard.daily.iter() {
            by_day.insert(
                date.clone(),
                DailyRollup {
                    date: date.clone(),
                    total_requests: a.requests,
                    total_cost: a.cost,
                    total_credits: a.credits,
                },
            );
        }
        for r in guard.records.iter() {
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
    /// handler 侧对长窗口会用每日汇总(`daily_rollup`,含已淘汰记录的当日累计)交叉校验/
    /// 兜底补齐(见 handler 注释)。
    pub async fn range_summary(
        &self,
        since_unix: i64,
        until_unix: i64,
        bucket_secs: i64,
    ) -> (RangeSummary, Vec<UsageBucket>) {
        let guard = self.state.read().await;
        let mut s = RangeSummary::default();
        // 桶:bucket_start_unix → (requests, cost, credits)
        let mut buckets: HashMap<i64, (u64, f64, f64)> = HashMap::new();
        for r in guard.records.iter() {
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
        let guard = self.state.read().await;
        let mut by_day: HashMap<String, (u64, f64, f64)> = HashMap::new();
        for r in guard.records.iter() {
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
        let guard = self.state.read().await;
        let mut filtered: Vec<UsageRecord> = guard
            .records
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
        let guard = self.state.read().await;
        let mut s = ApiKeyUsageSummary::new(api_key_id);
        let mut by_model: HashMap<String, ModelAgg> = HashMap::new();
        for r in guard.records.iter() {
            if r.api_key_id == api_key_id {
                s.accumulate(r, &mut by_model);
            }
        }
        s.finish(by_model);
        s
    }

    /// 所有出现过的 API-KEY 的用量聚合(按 api_key_id 升序);不含 id=0 的无归属记录。
    pub async fn summaries_by_api_key(&self) -> Vec<ApiKeyUsageSummary> {
        let guard = self.state.read().await;
        let mut acc: HashMap<u32, (ApiKeyUsageSummary, HashMap<String, ModelAgg>)> = HashMap::new();
        for r in guard.records.iter() {
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
        let guard = self.state.read().await;
        let mut filtered: Vec<UsageRecord> = guard
            .records
            .iter()
            .filter(|r| r.api_key_id == api_key_id)
            .cloned()
            .collect();
        drop(guard);
        sort_desc(&mut filtered);
        paginate(filtered, page, page_size)
    }

    /// 删除某 API-KEY 的全部用量记录;返回删除条数。置脏触发落盘。
    /// 已并入每日汇总的历史累计(已淘汰部分)不回退,见 [`daily_rollup`](Self::daily_rollup)。
    pub async fn reset_api_key(&self, api_key_id: u32) -> usize {
        self.retain_records(|r| r.api_key_id != api_key_id).await
    }

    /// 删除某凭据的全部用量记录;返回删除条数。凭据被删除/替换时调用,避免数值 id
    /// 复用后新凭据继承已删账号的用量历史。已并入每日汇总的历史累计不回退。
    pub async fn reset_credential(&self, credential_id: u32) -> usize {
        self.retain_records(|r| r.credential_id != credential_id)
            .await
    }

    /// 按谓词保留记录,返回删除条数;有删除则重算计数并置脏。
    async fn retain_records(&self, keep: impl Fn(&UsageRecord) -> bool) -> usize {
        let removed;
        {
            let mut guard = self.state.write().await;
            let before = guard.records.len();
            guard.records.retain(|r| keep(r));
            removed = before - guard.records.len();
            if removed > 0 {
                guard.recount(USAGE_CAP_PER_CREDENTIAL);
            }
        }
        if removed > 0 {
            self.dirty.mark_dirty();
        }
        removed
    }

    /// 当前记录总条数(测试/观测用)。
    pub async fn len(&self) -> usize {
        self.state.read().await.records.len()
    }

    /// 是否为空。
    pub async fn is_empty(&self) -> bool {
        self.state.read().await.records.is_empty()
    }
}

/// 按 created_at 降序(最新在前);同刻按插入顺序稳定。
fn sort_desc(v: &mut [UsageRecord]) {
    v.sort_by(|a, b| b.created_at_unix.cmp(&a.created_at_unix));
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
    fn compact_keeps_newest_per_credential_and_archives_evicted() {
        // cred 1 放 5 条(at 1..=5,cost 1.0/条),cred 2 放 2 条;cap=3
        let mut st = UsageState::default();
        for r in [
            rec(1, 1, 1.0, 0),
            rec(1, 2, 1.0, 0),
            rec(2, 10, 1.0, 0),
            rec(1, 3, 1.0, 0),
            rec(1, 4, 1.0, 0),
            rec(2, 11, 1.0, 0),
            rec(1, 5, 1.0, 0),
        ] {
            st.push(r, 3);
        }
        // cred1 超限 2 条,但未到批量阈值 → 尚未压实
        assert_eq!(st.overflow, 2);
        assert_eq!(st.records.len(), 7);
        st.compact(3);
        let cred1: Vec<i64> = st
            .records
            .iter()
            .filter(|r| r.credential_id == 1)
            .map(|r| r.created_at_unix)
            .collect();
        let cred2: Vec<i64> = st
            .records
            .iter()
            .filter(|r| r.credential_id == 2)
            .map(|r| r.created_at_unix)
            .collect();
        // cred1 只留最新 3 条(3,4,5),旧的 1,2 淘汰
        assert_eq!(cred1, vec![3, 4, 5]);
        // cred2 未超限,全留
        assert_eq!(cred2, vec![10, 11]);
        // 被淘汰的 2 条先并入当日汇总,不再凭空消失
        let day = cst_daykey(1);
        assert_eq!(st.daily.get(&day).unwrap().requests, 2);
        assert!((st.daily.get(&day).unwrap().cost - 2.0).abs() < 1e-9);
        // 压实后计数归位、无待压实超额
        assert_eq!(st.overflow, 0);
        assert_eq!(st.counts.get(&1).copied(), Some(3));
        assert_eq!(st.counts.get(&2).copied(), Some(2));
    }

    #[test]
    fn push_defers_compaction_until_batch_threshold() {
        let cap = 2usize;
        let mut st = UsageState::default();
        // 攒到差一条触发阈值:此前一次全表压实都不做(避免每插一条扫全表)。
        for i in 0..(cap + EVICT_BATCH - 1) as i64 {
            st.push(rec(1, 1000 + i, 1.0, 0), cap);
        }
        assert_eq!(st.records.len(), cap + EVICT_BATCH - 1);
        assert_eq!(st.overflow, EVICT_BATCH - 1);
        assert!(st.daily.is_empty());
        // 再插一条达阈值 → 压实一次,超出上限的记录全部并入当日汇总
        st.push(rec(1, 1000 + (cap + EVICT_BATCH - 1) as i64, 1.0, 0), cap);
        assert_eq!(st.records.len(), cap);
        assert_eq!(st.overflow, 0);
        let day = cst_daykey(1000);
        assert_eq!(st.daily.get(&day).unwrap().requests, EVICT_BATCH as u64);
    }

    #[test]
    fn daily_archive_trims_to_max_days() {
        let mut st = UsageState::default();
        // 造 MAX+5 天的汇总(每天一条),裁剪后只留最近 MAX 天。
        for d in 0..(DAILY_ROLLUP_MAX_DAYS + 5) as i64 {
            st.daily.insert(
                cst_daykey(d * 86400),
                DayArchive {
                    requests: 1,
                    cost: 1.0,
                    credits: 0.0,
                },
            );
        }
        st.trim_daily();
        assert_eq!(st.daily.len(), DAILY_ROLLUP_MAX_DAYS);
        // 最旧的 5 天被丢弃,最新一天保留
        assert!(!st.daily.contains_key(&cst_daykey(0)));
        assert!(!st.daily.contains_key(&cst_daykey(4 * 86400)));
        assert!(st.daily.contains_key(&cst_daykey(5 * 86400)));
        assert!(
            st.daily
                .contains_key(&cst_daykey((DAILY_ROLLUP_MAX_DAYS as i64 + 4) * 86400))
        );
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

    #[tokio::test]
    async fn daily_rollup_includes_evicted_archive() {
        let path =
            std::env::temp_dir().join(format!("kiro2api_usage_arch_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let t = UsageTracker::load(path.clone());
        let d27 = chrono::DateTime::parse_from_rfc3339("2026-06-27T02:00:00Z")
            .unwrap()
            .timestamp(); // 27 CST
        t.record_usage(1, "m".into(), 1, 1, 2.0, None, None, None, d27)
            .await;
        // 模拟这一日另有 3 条记录早已被 LRU 淘汰(淘汰前已并入当日汇总)。
        {
            let mut guard = t.state.write().await;
            guard.daily.insert(
                "2026-06-27".into(),
                DayArchive {
                    requests: 3,
                    cost: 3.0,
                    credits: 1.5,
                },
            );
            // 另一天:该日记录全被淘汰,存活记录里已无此日 —— 汇总仍须给出该日。
            guard.daily.insert(
                "2026-06-20".into(),
                DayArchive {
                    requests: 5,
                    cost: 5.0,
                    credits: 0.0,
                },
            );
        }
        let roll = t.daily_rollup().await;
        assert_eq!(roll.len(), 2);
        // 降序:27 在前;当日 = 存活 1 条 + 已淘汰 3 条
        assert_eq!(roll[0].date, "2026-06-27");
        assert_eq!(roll[0].total_requests, 4);
        assert!((roll[0].total_cost - 5.0).abs() < 1e-9);
        assert!((roll[0].total_credits - 1.5).abs() < 1e-9);
        // 全被淘汰的那天不因原始记录消失而丢失(长窗口兜底的关键)
        assert_eq!(roll[1].date, "2026-06-20");
        assert_eq!(roll[1].total_requests, 5);
        // 原始记录聚合仍只看存活记录(兜底靠二者之差)
        let raw = t.raw_daily_agg_in_window(d27 - 86400, d27 + 86400).await;
        assert_eq!(raw.get("2026-06-27").copied().unwrap().0, 1);
        assert!(!raw.contains_key("2026-06-20"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn flush_now_persists_records_and_daily_across_reload() {
        let name = format!("kiro2api_usage_flushnow_{}.json", std::process::id());
        let path = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&path);
        {
            let t = UsageTracker::load(path.clone());
            t.record_usage(3, "m".into(), 10, 20, 0.25, None, None, None, 1000)
                .await;
            {
                let mut guard = t.state.write().await;
                guard.daily.insert(
                    "1970-01-01".into(),
                    DayArchive {
                        requests: 7,
                        cost: 7.0,
                        credits: 3.5,
                    },
                );
            }
            // 不等 5s 定时器:停机流程显式刷盘。
            t.flush_now().await;
            // 紧凑序列化(机器读文件),不含 pretty 的换行缩进。
            let raw = std::fs::read_to_string(&path).unwrap();
            assert!(!raw.contains("\n  "), "落盘应为紧凑 JSON: {raw}");
            drop(t);
        }
        // 重新载入:原始记录与每日汇总都要回来。
        let t2 = UsageTracker::load(path.clone());
        assert_eq!(t2.len().await, 1);
        let roll = t2.daily_rollup().await;
        assert_eq!(roll.len(), 1);
        assert_eq!(roll[0].date, "1970-01-01");
        // 1 条存活(1000 秒属 1970-01-01 CST)+ 7 条已淘汰
        assert_eq!(roll[0].total_requests, 8);
        assert!((roll[0].total_credits - 3.5).abs() < 1e-9);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn dropping_tracker_does_not_clobber_persisted_file() {
        let path =
            std::env::temp_dir().join(format!("kiro2api_usage_drop_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let t = UsageTracker::load(path.clone());
        t.record_usage(1, "m".into(), 1, 1, 1.0, None, None, None, 1000)
            .await;
        t.flush_now().await;
        // 丢弃最后一个强引用:后台任务只持 Weak → 通道关闭、任务退出,
        // 且最后一次刷盘拿不到快照时必须跳过写盘,而不是把空库覆写上去。
        drop(t);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let t2 = UsageTracker::load(path.clone());
        assert_eq!(t2.len().await, 1, "进程内丢弃 tracker 不得清空已落盘记录");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn corrupt_file_is_backed_up_not_silently_overwritten() {
        let dir =
            std::env::temp_dir().join(format!("kiro2api_usage_corrupt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("usage_records.json");
        std::fs::write(&path, b"{ this is not valid json").unwrap();

        let t = UsageTracker::load(path.clone());
        assert!(t.is_empty().await);
        // 原文件被改名备份走,内容原样保留;原路径不再存在(下次刷盘才会新建)。
        assert!(!path.exists(), "损坏文件必须改名,不能留在原地被静默覆盖");
        let backups: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".corrupt."))
            .collect();
        assert_eq!(backups.len(), 1, "应有且仅有一个备份文件");
        assert_eq!(
            std::fs::read(&backups[0]).unwrap(),
            b"{ this is not valid json".to_vec()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn loads_legacy_bare_array_file() {
        let path =
            std::env::temp_dir().join(format!("kiro2api_usage_legacy_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // 旧版本落的是裸数组(无每日汇总),必须仍能载入且不触发损坏备份。
        std::fs::write(
            &path,
            serde_json::to_vec(&vec![rec(1, 1000, 1.0, 5), rec(2, 1001, 2.0, 6)]).unwrap(),
        )
        .unwrap();
        let t = UsageTracker::load(path.clone());
        assert_eq!(t.len().await, 2);
        let roll = t.daily_rollup().await;
        assert_eq!(roll.len(), 1);
        assert_eq!(roll[0].total_requests, 2);
        assert!(path.exists(), "旧格式不是损坏文件,不应被改名备份");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn reset_credential_clears_only_that_credential() {
        let path =
            std::env::temp_dir().join(format!("kiro2api_usage_resetc_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let t = UsageTracker::load(path.clone());
        t.record_usage(1, "m".into(), 1, 1, 1.0, None, None, None, 1000)
            .await;
        t.record_usage(1, "m".into(), 1, 1, 1.0, None, None, None, 1001)
            .await;
        t.record_usage(2, "m".into(), 1, 1, 1.0, None, None, None, 1002)
            .await;
        // 凭据 1 被删除 → 清其用量历史,避免 id 复用后被新凭据继承。
        assert_eq!(t.reset_credential(1).await, 2);
        assert_eq!(t.records_for_credential(1, 1, 10).await.total, 0);
        assert_eq!(t.records_for_credential(2, 1, 10).await.total, 1);
        // 计数随之收敛,重复删除返回 0。
        assert_eq!(t.reset_credential(1).await, 0);
        assert_eq!(t.len().await, 1);
        let _ = std::fs::remove_file(&path);
    }
}
