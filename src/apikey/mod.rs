//! API-KEY 存储(P2)。store-backed 的每用户 key:细粒度访问控制、用量归属、
//! 过期与消费上限。落盘为 `api_keys.json`(内部字段 snake_case,面向前端的
//! camelCase 转换在 admin/user handler 完成,本层只管存与取)。
//!
//! 线程安全:内部 `parking_lot::RwLock`;对外经 `Arc<ApiKeyStore>` 共享给
//! auth 闸 / relay / admin / user handler。落盘沿用 `stats::persist` 的
//! 原子写(tmp+rename)+ 约 5s 去抖批量刷盘,热路径不做 I/O。
//!
//! key 生成:`sk-` 前缀 + 128-bit 随机(getrandom → hex),无外部随机 id 库依赖。
//!
//! 时间:内部一律 `DateTime<Utc>`(chrono);过期/激活以注入的 `now` 计算,
//! 业务逻辑内不直接读系统时钟(与 stats 层同规约,便于单测钉死边界)。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::{DateTime, Duration, Utc};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::stats::persist::{
    DirtyFlag, FLUSH_INTERVAL_SECS, atomic_write_json, dirty_channel, read_json, spawn_flush_loop,
};

/// 单个 API-KEY(内部表示;落盘 snake_case)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKey {
    /// 1 起自增数值 id。
    pub id: u32,
    /// "sk-<hex>" 形式的密钥明文。
    pub key: String,
    /// 用户可见标签(常为数字流水号)。
    pub name: String,
    /// 运行期启用/停用开关。
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 创建时刻(UTC)。
    pub created_at: DateTime<Utc>,
    /// 绝对过期时刻(UTC);None = 不按绝对时间过期。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// 消费上限(按 limit_unit 计量);None = 不限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spending_limit: Option<f64>,
    /// 消费上限计量单位:"usd" | "credits"。
    #[serde(default = "default_limit_unit")]
    pub limit_unit: String,
    /// 惰性激活时长(天;首次使用时才据此算 expires_at)。None = 无惰性激活。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_days: Option<f64>,
    /// 首次使用时刻(UTC);None = 尚未激活(pending)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activated_at: Option<DateTime<Utc>>,
    /// 绑定的凭据 id 列表;Some 时该 key 仅可用这些账号(池过滤为后续阶段)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_credential_ids: Option<Vec<u64>>,
}

fn default_enabled() -> bool {
    true
}

fn default_limit_unit() -> String {
    "usd".to_string()
}

impl ApiKey {
    /// 是否已过期(仅按绝对 expires_at 判;pending 且 duration_days 未激活视为未过期)。
    ///
    /// 语义:
    /// - 若设 `duration_days` 但 `activated_at` 为 None → 惰性 key 未激活,永不过期。
    /// - 否则若设 `expires_at` 且 `now >= expires_at` → 过期。
    /// - 其余 → 未过期。
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        if self.duration_days.is_some() && self.activated_at.is_none() {
            return false;
        }
        matches!(self.expires_at, Some(exp) if now >= exp)
    }

    /// 是否可用(启用 且 未过期)。
    pub fn is_valid(&self, now: DateTime<Utc>) -> bool {
        self.enabled && !self.is_expired(now)
    }

    /// 惰性激活:置 activated_at=now,并据 duration_days 算 expires_at(幂等)。
    /// 已激活(activated_at 已有值)则不动,返回 false;本次真正激活返回 true。
    pub fn activate(&mut self, now: DateTime<Utc>) -> bool {
        if self.activated_at.is_some() {
            return false;
        }
        self.activated_at = Some(now);
        if let Some(days) = self.duration_days {
            self.expires_at = Some(expiry_from_duration_days(now, days));
        }
        true
    }
}

/// `duration_days` 的绝对值上限(约 1 万年)。超过即视为"实际上永不过期",
/// 用于把管理面传进来的任意 f64 收敛到 chrono 绝不会溢出的范围内。
/// 取值理由:3.65e6 天换算成毫秒约 3.2e14,远在 i64 与 `DateTime<Utc>` 的可表示范围内
/// (chrono 上限约公元 262143 年),而任何真实的授权时长都不会接近它,故对正常业务是纯粹的空操作。
const MAX_DURATION_DAYS: f64 = 3_650_000.0;

/// 把任意 f64 时长收敛到安全范围:NaN → 0(立即过期,失败方向取"更严"而非"无限期可用"),
/// 其余按 ±[`MAX_DURATION_DAYS`] 截断(±Inf 也在此被截断)。
///
/// 为什么必须有:`CreateApiKeyRequest.duration_days` 是裸 `Option<f64>`,从 HTTP body 一路
/// 原样存进 `ApiKey`,中间没有任何范围校验。管理员误填 `1000000000`(或 JSON 里的 `1e999`
/// 解析出的 Inf)本身不报错,坏账要到**该 key 第一次被使用**时才爆。
fn sanitize_duration_days(days: f64) -> f64 {
    if days.is_nan() {
        return 0.0;
    }
    days.clamp(-MAX_DURATION_DAYS, MAX_DURATION_DAYS)
}

/// 由惰性时长算绝对过期时刻。**任何取值都不得 panic**。
///
/// 为什么用 checked 算术而不是 `now + Duration::milliseconds(ms)`:后者的 `Add` 实现内部是
/// `.expect("`DateTime + TimeDelta` overflowed")`,溢出直接 panic;而本函数跑在鉴权中间件的
/// 热路径上(`auth::require_api_key` → `ApiKeyStore::activate_key`,每个 key 首次使用都会走),
/// 一条 `durationDays: 1e9` 的坏 key 就能让请求线程 panic —— 客户拿到 500/连接断开,而不是
/// 一个可解释的鉴权错误。先 clamp 再 checked,双保险:clamp 后理论上不可能溢出,真溢出也只是
/// 饱和到可表示边界(正时长 → 几乎永不过期,负时长 → 立即过期),绝不 panic。
fn expiry_from_duration_days(now: DateTime<Utc>, days: f64) -> DateTime<Utc> {
    let days = sanitize_duration_days(days);
    // 以毫秒精度换算,兼容 0.25 天(6 小时)这类小数时长。
    // 浮点→整数的 `as` 转换在 Rust 中是饱和的(NaN→0),不会 UB。
    let ms = (days * 86_400_000.0).round() as i64;
    match Duration::try_milliseconds(ms).and_then(|d| now.checked_add_signed(d)) {
        Some(t) => t,
        None if ms >= 0 => DateTime::<Utc>::MAX_UTC,
        None => DateTime::<Utc>::MIN_UTC,
    }
}

/// `validate` 的解析结果。Valid 携带 handler/relay 归属与后续限额检查所需的元信息。
#[derive(Debug, Clone, PartialEq)]
pub enum ApiKeyAuthResult {
    Valid {
        id: u32,
        name: String,
        spending_limit: Option<f64>,
        limit_unit: String,
        bound_credential_ids: Option<Vec<u64>>,
    },
    /// 命中但被停用。
    Disabled,
    /// 命中但已过期。
    Expired,
    /// 未命中任何 key。
    NotFound,
}

/// 落盘结构:除 key 列表外还持久化 `next_id`,使 id 分配在**删除后也不回收**。
/// 字段 snake_case,与 `ApiKey` 同规约。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApiKeyStoreFile {
    /// 下一个待分配的 id;单调递增,进程重启/删除 key 都不回退。
    #[serde(default)]
    next_id: u32,
    /// key 列表。**故意不加 `#[serde(default)]`**:写侧恒会写出该字段,故一份「是对象但没有
    /// keys 数组」的文件(被别的内容顶掉/手工编辑写坏字段名)只可能是坏文件。若给它加默认值,
    /// 这类坏文件会被 serde 悄悄解析成"空 store"而不报错,随后 5s 刷盘就把它原地覆写掉。
    keys: Vec<ApiKey>,
}

/// 磁盘上的两种形态:新版对象(含 `next_id`)与旧版裸数组(仅 key 列表)。
/// 旧文件按 `Legacy` 载入,`next_id` 由现存 id 推出,升级无需迁移脚本。
#[derive(Deserialize)]
#[serde(untagged)]
enum ApiKeyFile {
    Versioned(ApiKeyStoreFile),
    Legacy(Vec<ApiKey>),
}

/// 线程安全的 API-KEY 存储。持锁在内存改动,落盘经脏标记 + 后台刷盘去抖。
pub struct ApiKeyStore {
    keys: RwLock<Vec<ApiKey>>,
    /// 下一个待分配的 id。只增不减(删除的 id 永不回收),随 keys 一同落盘。
    /// 分配在 `keys` 写锁内完成,故无需自身的原子 CAS 循环。
    next_id: AtomicU32,
    dirty: DirtyFlag,
    file_path: PathBuf,
    /// 每 key 的“在途预留额”(按各 key 自身 limit_unit 计量,与消费上限同单位)。
    /// 用于把消费上限的“check-then-act”并发窗口收敛为原子的“reserve-then-reconcile”:
    /// 准入时原子地判 `(已花 + 在途预留 + 本次预估) <= 上限` 并累加预留,请求完成时释放。
    /// 不落盘(纯运行期内存态);进程重启后在途预留自然清零(此时也无在途请求)。
    reservations: Mutex<HashMap<u32, f64>>,
}

/// 一次在途请求对某 key 的预留额,持有期间计入该 key 的“在途预留”总额。
/// Drop 时自动把本次预留额从 store 的 reservations 里扣回(RAII 释放),
/// 从而无论请求成功/失败/panic 都不泄漏预留——释放点即 guard 离开作用域处。
#[must_use = "持有此 guard 才维持预留;提前 drop 会立即释放预留额"]
pub struct SpendReservation {
    store: Arc<ApiKeyStore>,
    id: u32,
    amount: f64,
}

/// 预留清理阈值:释放后残额 <= 此值即从 map 移除该项。
///
/// 单个 `f64::EPSILON`(≈2.22e-16)对“多次 est 累加/相减”的浮点残渣过紧:
/// 连续 reserve/release 若干个 `est = 1.0/0.72` 这类无法精确表示的量后,残额可能停在
/// 1e-16 ~ 1e-12 量级(既非精确 0、也可能落在 EPSILON 之上),用 EPSILON 判零会漏清、
/// 令 map 残留近零僵尸项无限累积。取一个远大于典型残渣、又远小于任何真实 est 的小阈值,
/// 使“已全部释放”稳定判为归零并移除;真实在途预留(est 量级 ≥ ~1)绝不会被误清。
/// 负残额(减多了的浮点抖动)天然 <= 阈值,一并按归零移除。
const RESERVATION_CLEANUP_EPSILON: f64 = 1e-9;

impl Drop for SpendReservation {
    fn drop(&mut self) {
        let mut guard = self.store.reservations.lock();
        if let Some(slot) = guard.get_mut(&self.id) {
            *slot -= self.amount;
            // 归零即移除,避免 map 无限增长残留近零僵尸项。浮点残渣兜底:<= 小阈值即清。
            if *slot <= RESERVATION_CLEANUP_EPSILON {
                guard.remove(&self.id);
            }
        }
    }
}

impl ApiKeyStore {
    /// 从 `path` 载入(不存在则空),并启动后台刷盘任务(约 5s 去抖批量落盘)。
    ///
    /// `next_id` 取 `max(文件里的 next_id, 现存最大 id + 1)`:旧版裸数组文件无该字段时
    /// 由现存 id 推出(平滑升级),文件被手工编辑过导致 next_id 落后时也不会退回去撞已用 id。
    pub fn load<P: AsRef<Path>>(path: P) -> Arc<Self> {
        let path = path.as_ref().to_path_buf();
        let (initial, stored_next_id) = load_keys_resilient(&path);
        let next_id = stored_next_id.max(max_id_plus_one(initial.as_slice()));
        let (dirty, handle) = dirty_channel();
        let store = Arc::new(Self {
            keys: RwLock::new(initial),
            next_id: AtomicU32::new(next_id),
            dirty,
            file_path: path.clone(),
            reservations: Mutex::new(HashMap::new()),
        });
        // 后台刷盘依赖 tokio 运行时(spawn)。有运行时才起后台任务;无运行时
        // (如纯同步单测)跳过——此时写不自动落盘,须显式 `save_now`。
        if tokio::runtime::Handle::try_current().is_ok() {
            let snap = store.clone();
            spawn_flush_loop(path, handle, FLUSH_INTERVAL_SECS, move || {
                serde_json::to_vec_pretty(&snap.snapshot_file())
                    .unwrap_or_else(|_| br#"{"next_id":1,"keys":[]}"#.to_vec())
            });
        }
        store
    }

    /// 当前内存态的落盘快照(key 列表 + 单调 next_id)。
    fn snapshot_file(&self) -> ApiKeyStoreFile {
        let keys = self.keys.read().clone();
        ApiKeyStoreFile {
            next_id: self.next_id.load(Ordering::SeqCst),
            keys,
        }
    }

    /// 同步原子落盘(测试/关键路径即时持久化用;常规写走脏标记后台刷盘)。
    pub fn save_now(&self) -> std::io::Result<()> {
        atomic_write_json(&self.file_path, &self.snapshot_file())
    }

    /// 校验明文 key:命中后按 启用 + 过期(注入 now)裁决。不检查消费上限
    /// (上限求和依赖 stats 层,由 relay/handler 另行处理)。
    ///
    /// 比较走常量时间(与全局 key 同规约),且**遍历全部候选**不在命中处提前退出:
    /// 否则耗时会随"命中的是第几条 key"/"与某条 key 前多少字节相同"变化,
    /// 给出可按字节试探的时序侧信道。
    pub fn validate(&self, key_str: &str, now: DateTime<Utc>) -> ApiKeyAuthResult {
        let guard = self.keys.read();
        let mut hit: Option<&ApiKey> = None;
        for k in guard.iter() {
            if ct_eq_key(&k.key, key_str) && hit.is_none() {
                hit = Some(k);
            }
        }
        match hit {
            None => ApiKeyAuthResult::NotFound,
            Some(k) if !k.enabled => ApiKeyAuthResult::Disabled,
            Some(k) if k.is_expired(now) => ApiKeyAuthResult::Expired,
            Some(k) => ApiKeyAuthResult::Valid {
                id: k.id,
                name: k.name.clone(),
                spending_limit: k.spending_limit,
                limit_unit: k.limit_unit.clone(),
                bound_credential_ids: k.bound_credential_ids.clone(),
            },
        }
    }

    /// 全量列出(克隆快照)。
    pub fn list(&self) -> Vec<ApiKey> {
        self.keys.read().clone()
    }

    /// 按 id 取(克隆)。
    pub fn get_by_id(&self, id: u32) -> Option<ApiKey> {
        self.keys.read().iter().find(|k| k.id == id).cloned()
    }

    /// 新建一个 key。id 由单调递增的 `next_id` 分配(从 1 起,已删除的 id 永不回收)。
    /// 生成 "sk-" 前缀随机明文。`created_at` 由注入的 `now` 决定。返回新建的 ApiKey 克隆。
    ///
    /// id 不回收是硬约束:用量记录、消费统计均以 api-key id 归属,复用已删除的 id 会让
    /// 新 key 直接继承前任的调用明细(IP/模型/费用跨用户泄露)与累计消费(可能一上来就 402)。
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        name: String,
        expires_at: Option<DateTime<Utc>>,
        spending_limit: Option<f64>,
        limit_unit: Option<String>,
        duration_days: Option<f64>,
        bound_credential_ids: Option<Vec<u64>>,
        now: DateTime<Utc>,
    ) -> ApiKey {
        let created = {
            let mut guard = self.keys.write();
            // 在 keys 写锁内分配:与现存最大 id + 1 取上确界,兼容外部直接改文件后
            // next_id 落后于实际 id 的情形;分配后立即推进,故并发 create 不会撞号。
            let next_id = self
                .next_id
                .load(Ordering::SeqCst)
                .max(max_id_plus_one(guard.as_slice()));
            self.next_id
                .store(next_id.saturating_add(1), Ordering::SeqCst);
            let key = ApiKey {
                id: next_id,
                key: generate_api_key(),
                name,
                enabled: true,
                created_at: now,
                expires_at,
                spending_limit,
                limit_unit: limit_unit.unwrap_or_else(default_limit_unit),
                // 入库即收敛到安全范围:HTTP 层的 `durationDays` 是裸 f64、无任何校验,
                // 让 1e9 这种值落到盘上,重启后每次读到它都得再防一遍溢出。
                duration_days: duration_days.map(sanitize_duration_days),
                activated_at: None,
                bound_credential_ids,
            };
            guard.push(key.clone());
            key
        };
        self.dirty.mark_dirty();
        created
    }

    /// 局部更新:每个参数 `Some(_)` 表示"改这一项",`None` 表示"不动"。
    /// 对可空字段(expires_at/spending_limit/duration_days/bound_credential_ids)
    /// 用 `Option<Option<_>>`:外层 Some 表示要改、内层决定设值或清空。
    /// 返回更新后的 ApiKey;id 不存在返回 None。
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        id: u32,
        name: Option<String>,
        enabled: Option<bool>,
        expires_at: Option<Option<DateTime<Utc>>>,
        spending_limit: Option<Option<f64>>,
        limit_unit: Option<String>,
        duration_days: Option<Option<f64>>,
        bound_credential_ids: Option<Option<Vec<u64>>>,
    ) -> Option<ApiKey> {
        let updated = {
            let mut guard = self.keys.write();
            let k = guard.iter_mut().find(|k| k.id == id)?;
            if let Some(v) = name {
                k.name = v;
            }
            if let Some(v) = enabled {
                k.enabled = v;
            }
            if let Some(v) = expires_at {
                k.expires_at = v;
            }
            if let Some(v) = spending_limit {
                k.spending_limit = v;
            }
            if let Some(v) = limit_unit {
                k.limit_unit = v;
            }
            if let Some(v) = duration_days {
                // 同 `create`:改时长也要收敛,别让越界值绕过创建口从更新口进来。
                k.duration_days = v.map(sanitize_duration_days);
            }
            if let Some(v) = bound_credential_ids {
                k.bound_credential_ids = v;
            }
            k.clone()
        };
        self.dirty.mark_dirty();
        Some(updated)
    }

    /// 删除;返回是否删掉了(false = id 不存在)。
    pub fn delete(&self, id: u32) -> bool {
        let removed = {
            let mut guard = self.keys.write();
            let before = guard.len();
            guard.retain(|k| k.id != id);
            before != guard.len()
        };
        if removed {
            self.dirty.mark_dirty();
        }
        removed
    }

    /// 惰性激活指定 key(首次使用)。已激活则幂等无副作用。
    /// 返回 Some(true)=本次激活、Some(false)=已激活/无需、None=id 不存在。
    pub fn activate_key(&self, id: u32, now: DateTime<Utc>) -> Option<bool> {
        let (found, changed) = {
            let mut guard = self.keys.write();
            match guard.iter_mut().find(|k| k.id == id) {
                Some(k) => (true, k.activate(now)),
                None => (false, false),
            }
        };
        if !found {
            return None;
        }
        if changed {
            self.dirty.mark_dirty();
        }
        Some(changed)
    }

    /// 停用指定 key(消费上限触发时用)。返回是否改动(已停用/不存在返回 false)。
    pub fn disable(&self, id: u32) -> bool {
        let changed = {
            let mut guard = self.keys.write();
            match guard.iter_mut().find(|k| k.id == id) {
                Some(k) if k.enabled => {
                    k.enabled = false;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.dirty.mark_dirty();
        }
        changed
    }

    /// 原子“预留”一次在途消费额,把消费上限的并发 check-then-act 收敛为 reserve-then-reconcile。
    ///
    /// 语义:在 `reservations` 锁**单一临界区**内原子地完成
    /// “读在途预留 + 判 `spent + 已有在途预留 + est_cost <= limit` + 累加预留”三步:
    /// - 通过 → 累加 `est_cost` 到该 key 的在途预留,返回 `Ok(SpendReservation)`(RAII,Drop 释放);
    /// - 不通过 → 不改动预留,返回 `Err(())`,调用方据此回 402。
    ///
    /// **并发准入的正确性关键(见 finding #7)**:`reserved` 的读取与新预留的写入必须在同一把锁内
    /// 一气呵成,中间不得释放锁、不得穿插 await。故本方法为同步方法、内部无 await:调用方先把
    /// `spent` 快照读好(stats 是 async 锁,无法与本 sync 锁合并),再把该快照传入本临界区;
    /// 由于 `reserved` 在锁内累加,N 个并发请求即便读到**同一个** under-limit 的 `spent`,后来者
    /// 也必然看到“`spent` + 前面所有在途预留之和”,不会两个请求都据同一 `spent` 各自准入 →
    /// 总越界被严格界定在“至多一次在途请求的 est_cost”之内(而非无界 over-admission)。
    ///
    /// `spent`、`limit`、`est_cost` 三者必须同单位(由调用方按 key 的 limit_unit 归一后传入,本层
    /// 不认单位、只做算术)。`spent`/`limit` 非有限(NaN/Inf)时保守拒绝(返回 Err),避免 NaN 比较
    /// 恒 false 而误放行;`est_cost` 负/非有限视为 0,避免把预留算成负数“扩容”上限。
    ///
    /// 需 `Arc<Self>` 以便 guard 持有 store 句柄在 Drop 时释放;调用点用 `Arc::clone`。
    pub fn try_reserve_spend(
        self: &Arc<Self>,
        id: u32,
        spent: f64,
        limit: f64,
        est_cost: f64,
    ) -> Result<SpendReservation, ()> {
        // 非有限的 spent/limit 保守拒绝:NaN 参与的 `>` 恒为 false 会误判“未越界”而放行。
        if !spent.is_finite() || !limit.is_finite() {
            return Err(());
        }
        // 负/非有限的预估视为 0,避免把预留额算成负数“扩容”上限。
        let est = if est_cost.is_finite() && est_cost > 0.0 {
            est_cost
        } else {
            0.0
        };
        // 单一临界区:读 reserved → 判越界 → 写 reserved,全程持锁、无 await。
        let mut guard = self.reservations.lock();
        let reserved = guard.get(&id).copied().unwrap_or(0.0);
        if spent + reserved + est > limit {
            return Err(());
        }
        *guard.entry(id).or_insert(0.0) += est;
        Ok(SpendReservation {
            store: Arc::clone(self),
            id,
            amount: est,
        })
    }

    /// 某 key 当前的在途预留总额(测试/观测用)。
    #[cfg(test)]
    pub fn reserved_amount(&self, id: u32) -> f64 {
        self.reservations.lock().get(&id).copied().unwrap_or(0.0)
    }

    /// 当前 key 数(测试/观测用)。
    pub fn len(&self) -> usize {
        self.keys.read().len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.keys.read().is_empty()
    }
}

/// 由 `credentials_path` 的父目录推断 `api_keys.json` 路径(与 stats 数据目录同规约)。
pub fn api_keys_path_from(credentials_path: &str) -> PathBuf {
    let p = Path::new(credentials_path);
    let dir = match p.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    dir.join("api_keys.json")
}

/// 逐条抢救坏文件时,最多逐条打印多少条坏 key 的明细(其余只计入总数)。
/// 上限的理由与凭据池同:线上 api_keys.json 有上千条客户 key,整体格式走样时不能让启动日志
/// 被刷爆(日志同时进实时日志面板的环形缓冲,刷爆等于把别的线索一起冲掉)。
const MAX_BAD_APIKEY_LOGS: usize = 20;

/// 载入落盘的 key 列表——**解析失败绝不静默当空 store**。返回 `(可用 key, next_id 下界)`。
///
/// 为什么不能再写 `read_json(..).ok().flatten()`:那会把「解析失败」和「文件根本不存在」
/// 折叠成同一个 `None`。线上这份 api_keys.json 装着全部客户 key,而 serde 是整份文件
/// all-or-nothing —— 任何**一条** key 写坏(`created_at` 不是 RFC3339、缺 `key` 字段、
/// 手工编辑多一个逗号、文件被截断)都会让整份文件解析失败。旧写法的后果是:进程带着 0 个 key
/// 启动、日志里一个字都没有,全部客户当场 401、管理面 API-KEY 页空空如也;运营者只能猜
/// 「那就重新发一个 key」——这一下 `create` 置脏,5s 去抖刷盘就把内存里仅有的那条新 key
/// 整份原子覆写上去,原本还能人工修好的上千条客户 key 当场蒸发且不可逆。
///
/// 故这里与 `server::load_credentials_resilient` / `stats::usage` 同规格做三件事:
/// 1. **原样备份**(复制而非改名):存成 `{path}.corrupt.{unix秒}.{序号}`,原文件留在原处供人工
///    修复,备份档任凭后续任何原子写都覆不掉;
/// 2. **逐条抢救**:文件本体还是 JSON(新版对象 / 旧版裸数组)时按条反序列化,坏条目跳过、
///    好 key 照常入库 —— 1 条坏 key 不再拖垮另外 999 条,客户继续能用;
/// 3. **大声报错**并抬高 `next_id` 下界:坏条目进不了内存,但它占用过的 id 仍要计入下界,
///    否则新建 key 会复用该 id、直接继承前任的用量明细与累计消费(跨客户串账,见 `create` 注释)。
fn load_keys_resilient(path: &Path) -> (Vec<ApiKey>, u32) {
    let err = match read_json::<ApiKeyFile>(path) {
        Ok(Some(ApiKeyFile::Versioned(f))) => return (f.keys, f.next_id),
        Ok(Some(ApiKeyFile::Legacy(keys))) => return (keys, 0),
        // 文件不存在 = 首次运行的正常路径,静默按空 store 起(不备份、不报错)。
        Ok(None) => return (Vec::new(), 0),
        Err(e) => e,
    };
    // 走到这里说明文件存在但读不动/解析不了。备份与逐条抢救都要用原始字节,只读一次。
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(read_err) => {
            tracing::error!(
                path = %path.display(),
                error = %err,
                read_error = %read_err,
                "api_keys.json 读取失败,API-KEY 按空载入;修好之前请勿在管理面增删 key——任何写入都会用空存储整份覆写该文件"
            );
            return (Vec::new(), 0);
        }
    };
    let backup = backup_corrupt_api_keys(path, &raw);
    let salvaged = salvage_api_keys(&raw);
    tracing::error!(
        path = %path.display(),
        error = %err,
        entries_total = salvaged.total,
        salvaged = salvaged.keys.len(),
        skipped = salvaged.skipped,
        next_id = salvaged.next_id,
        backup = backup
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<备份失败>".to_string()),
        "api_keys.json 解析失败:已原样备份并逐条抢救可用 key;请按上面的逐条报错修好坏条目后重启,期间在管理面增删 key 会用当前(可能不完整的)列表覆写该文件"
    );
    (salvaged.keys, salvaged.next_id)
}

/// 逐条抢救的产物。
struct SalvagedKeys {
    /// 解析成功、可继续鉴权的 key。
    keys: Vec<ApiKey>,
    /// `next_id` 下界:`max(文件里的 next_id, 全部条目(含坏条目)的最大 id + 1)`。
    next_id: u32,
    /// 文件里的条目总数(含坏条目)。
    total: usize,
    /// 跳过的坏条目数。
    skipped: usize,
}

/// 逐条抢救:文件本体仍是 JSON 时按条反序列化,坏条目跳过并报错。
/// 整份文件连 JSON 都解析不出(语法错/被截断)时全空——此时只剩备份档可人工修复。
fn salvage_api_keys(raw: &[u8]) -> SalvagedKeys {
    let empty = SalvagedKeys {
        keys: Vec::new(),
        next_id: 0,
        total: 0,
        skipped: 0,
    };
    let value: serde_json::Value = match serde_json::from_slice(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                error = %e,
                "api_keys.json 连 JSON 都解析不出(语法错误/文件被截断),无法逐条抢救;原文件已备份,请人工修复"
            );
            return empty;
        }
    };
    let (entries, stored_next_id) = match value {
        // 旧版裸数组形态。
        serde_json::Value::Array(a) => (a, 0u32),
        // 新版对象形态:`next_id` 单独抢救——它自己坏掉也不该连累 key 列表,反之亦然。
        serde_json::Value::Object(mut o) => {
            let next_id = o
                .get("next_id")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32;
            match o.remove("keys") {
                Some(serde_json::Value::Array(a)) => (a, next_id),
                _ => {
                    tracing::error!("api_keys.json 里没有可用的 keys 数组,无法逐条抢救 key");
                    (Vec::new(), next_id)
                }
            }
        }
        _ => {
            tracing::error!("api_keys.json 既不是对象也不是数组,无法逐条抢救");
            return empty;
        }
    };
    let total = entries.len();
    let mut keys = Vec::with_capacity(total);
    let mut skipped = 0usize;
    let mut max_id = 0u32;
    for (index, entry) in entries.into_iter().enumerate() {
        // 先取 id:坏条目也要计入 next_id 下界,否则新 key 复用它的 id 会继承前任的用量与消费。
        if let Some(id) = entry.get("id").and_then(|v| v.as_u64()) {
            max_id = max_id.max(id.min(u32::MAX as u64) as u32);
        }
        match serde_json::from_value::<ApiKey>(entry) {
            Ok(k) => {
                max_id = max_id.max(k.id);
                keys.push(k);
            }
            Err(e) => {
                skipped += 1;
                if skipped <= MAX_BAD_APIKEY_LOGS {
                    // 定位信息只取数组下标,**绝不打印 key 明文或 name**——这些日志会进实时日志
                    // 面板与日志文件,打出去等于把客户密钥泄给任何能看日志的人。
                    tracing::error!(
                        index = index,
                        error = %e,
                        "该 API-KEY 条目解析失败,已跳过(其余 key 照常载入)"
                    );
                }
            }
        }
    }
    SalvagedKeys {
        keys,
        next_id: stored_next_id.max(max_id.saturating_add(1)),
        total,
        skipped,
    }
}

/// 把当前(解析不了的)api_keys.json **原样复制**一份到 `{path}.corrupt.{unix秒}.{序号}`。
///
/// 为什么是复制而不是像 `stats::usage` 那样改名:这份文件是客户 key 的唯一真源。改名走人,
/// 运营者连"照着原文件改一个字符"都没得改,且下一次刷盘就会用抢救出来的残缺列表建一份新文件。
/// 复制则两头都保住:原文件留在原地供人工修复,备份档任凭后续原子写也覆不掉。
///
/// 幂等:同一份坏内容已经备份过(存在字节完全相同的 `.corrupt.*`)就不再重复备份,
/// 否则容器反复重启会在数据盘上堆出一摞副本。
fn backup_corrupt_api_keys(path: &Path, raw: &[u8]) -> Option<PathBuf> {
    if let Some(existing) = existing_identical_backup(path, raw) {
        return Some(existing);
    }
    let backup = corrupt_backup_path(path);
    let res = (|| -> std::io::Result<()> {
        // 文件里是全部客户 API-KEY 的明文:与 `persist::write_bytes_atomic` 同规按 0600 建,
        // 不能用默认权限(0644)把密钥副本变成全局可读。`create_new` 保证绝不顶掉已有备份。
        #[cfg(unix)]
        let f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&backup)?
        };
        #[cfg(not(unix))]
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)?;
        {
            use std::io::Write;
            let mut w = std::io::BufWriter::new(&f);
            w.write_all(raw)?;
            w.flush()?;
        }
        f.sync_all() // fsync:备份的意义就在于机器随后掉电也还在
    })();
    match res {
        Ok(()) => Some(backup),
        Err(e) => {
            tracing::error!(
                path = %path.display(),
                backup = %backup.display(),
                error = %e,
                "损坏的 api_keys.json 备份失败:原文件仍在原处,但下一次刷盘会覆盖它,请先手工另存一份"
            );
            None
        }
    }
}

/// 损坏文件的备份路径:`{path}.corrupt.{unix秒}.{序号}`,取第一个尚不存在的候选
/// (与 `server` / `stats::usage` 同规格),保证同秒内多次备份互不覆盖。
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

/// 找同目录下内容与 `raw` 逐字节相同的既有 `{basename}.corrupt.*` 备份(有则本次不必再备份)。
/// 先比长度再比内容,避免为每个候选都整份读一遍。
fn existing_identical_backup(path: &Path, raw: &[u8]) -> Option<PathBuf> {
    let (dir, file_name) = (
        path.parent()?,
        path.file_name()?.to_string_lossy().into_owned(),
    );
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{file_name}.corrupt.");
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let same_len = entry
            .metadata()
            .map(|m| m.len() == raw.len() as u64)
            .unwrap_or(false);
        if same_len
            && std::fs::read(entry.path())
                .map(|b| b == raw)
                .unwrap_or(false)
        {
            return Some(entry.path());
        }
    }
    None
}

/// 现存 key 里最大 id + 1(空列表 → 1),即"不撞已用 id"的下界。
/// id 已到 u32 上界时饱和(现实中不可达),不回绕成小 id。
fn max_id_plus_one(keys: &[ApiKey]) -> u32 {
    keys.iter()
        .map(|k| k.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

/// 明文 key 的常量时间比较:长度不同直接判否(长度非秘密),等长走 `subtle`,
/// 使耗时与"前多少字节相同"无关。
fn ct_eq_key(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// 生成 "sk-" 前缀 + 128-bit 随机(hex)明文 key。getrandom 失败极罕见,回退时
/// panic 优于产出弱随机 key。
fn generate_api_key() -> String {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).expect("getrandom failed generating api key");
    format!("sk-{}", hex::encode(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kiro2api_apikey_{}_{}.json",
            tag,
            std::process::id()
        ))
    }

    #[test]
    fn generated_keys_are_prefixed_and_unique() {
        let a = generate_api_key();
        let b = generate_api_key();
        assert!(a.starts_with("sk-"), "{a}");
        assert_eq!(a.len(), 3 + 32); // "sk-" + 16 bytes hex
        assert!(a.chars().skip(3).all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "两次生成不应相同");
        // 更强的唯一性抽样:1000 个互不相同
        let mut set = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(set.insert(generate_api_key()), "碰撞");
        }
    }

    #[test]
    fn crud_roundtrip_and_id_autoincrement() {
        let path = tmp_path("crud");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        assert!(store.is_empty());

        let now = ts("2026-07-20T00:00:00Z");
        let k1 = store.create(
            "0001".into(),
            None,
            Some(100.0),
            Some("usd".into()),
            None,
            None,
            now,
        );
        let k2 = store.create("0002".into(), None, None, None, None, None, now);
        assert_eq!(k1.id, 1);
        assert_eq!(k2.id, 2);
        assert_eq!(k2.limit_unit, "usd"); // 默认单位
        assert!(k1.enabled);
        assert!(k1.activated_at.is_none());
        assert_eq!(store.len(), 2);

        // get_by_id
        assert_eq!(store.get_by_id(1).unwrap().name, "0001");
        assert!(store.get_by_id(999).is_none());

        // update:改 name + 停用 + 清空 spending_limit
        let upd = store
            .update(
                1,
                Some("renamed".into()),
                Some(false),
                None,
                Some(None),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(upd.name, "renamed");
        assert!(!upd.enabled);
        assert!(upd.spending_limit.is_none());
        // 未指定的字段(name of k2)不受影响
        assert_eq!(store.get_by_id(2).unwrap().name, "0002");
        // update 不存在的 id → None
        assert!(
            store
                .update(999, Some("x".into()), None, None, None, None, None, None)
                .is_none()
        );

        // delete
        assert!(store.delete(2));
        assert!(!store.delete(2)); // 再删 → false
        assert_eq!(store.len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_enabled_disabled_notfound() {
        let path = tmp_path("validate");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let now = ts("2026-07-20T00:00:00Z");
        let k = store.create(
            "n".into(),
            None,
            Some(50.0),
            Some("credits".into()),
            None,
            Some(vec![3, 7]),
            now,
        );

        // 命中 + 启用 → Valid,携带元信息
        match store.validate(&k.key, now) {
            ApiKeyAuthResult::Valid {
                id,
                name,
                spending_limit,
                limit_unit,
                bound_credential_ids,
            } => {
                assert_eq!(id, k.id);
                assert_eq!(name, "n");
                assert_eq!(spending_limit, Some(50.0));
                assert_eq!(limit_unit, "credits");
                assert_eq!(bound_credential_ids, Some(vec![3, 7]));
            }
            other => panic!("expected Valid, got {other:?}"),
        }
        // 未命中
        assert_eq!(store.validate("sk-nope", now), ApiKeyAuthResult::NotFound);
        // 停用后 → Disabled
        store.update(k.id, None, Some(false), None, None, None, None, None);
        assert_eq!(store.validate(&k.key, now), ApiKeyAuthResult::Disabled);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_expiry_absolute() {
        let path = tmp_path("expiry");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let created = ts("2026-07-20T00:00:00Z");
        let expires = ts("2026-07-21T00:00:00Z");
        let k = store.create("e".into(), Some(expires), None, None, None, None, created);

        // 过期前 → Valid
        assert!(matches!(
            store.validate(&k.key, ts("2026-07-20T23:59:59Z")),
            ApiKeyAuthResult::Valid { .. }
        ));
        // 到点/过点 → Expired
        assert_eq!(store.validate(&k.key, expires), ApiKeyAuthResult::Expired);
        assert_eq!(
            store.validate(&k.key, ts("2026-07-22T00:00:00Z")),
            ApiKeyAuthResult::Expired
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lazy_activation_computes_expiry_and_is_idempotent() {
        let path = tmp_path("lazy");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let created = ts("2026-07-20T00:00:00Z");
        // duration_days=0.25(6 小时)的惰性 key,创建时无 expires_at、未激活
        let k = store.create("lazy".into(), None, None, None, Some(0.25), None, created);
        assert!(k.expires_at.is_none());
        assert!(k.activated_at.is_none());

        // 未激活的惰性 key → 永不过期(即便 now 远在未来)
        assert!(matches!(
            store.validate(&k.key, ts("2027-01-01T00:00:00Z")),
            ApiKeyAuthResult::Valid { .. }
        ));

        // 首次激活:置 activated_at,算 expires_at = now + 6h
        let use_at = ts("2026-07-20T10:00:00Z");
        assert_eq!(store.activate_key(k.id, use_at), Some(true));
        let after = store.get_by_id(k.id).unwrap();
        assert_eq!(after.activated_at, Some(use_at));
        assert_eq!(after.expires_at, Some(ts("2026-07-20T16:00:00Z")));

        // 幂等:再激活不改动、返回 Some(false)
        assert_eq!(
            store.activate_key(k.id, ts("2026-07-20T12:00:00Z")),
            Some(false)
        );
        assert_eq!(store.get_by_id(k.id).unwrap().activated_at, Some(use_at));

        // 激活后按算出的 expires_at 过期
        assert!(matches!(
            store.validate(&k.key, ts("2026-07-20T15:59:59Z")),
            ApiKeyAuthResult::Valid { .. }
        ));
        assert_eq!(
            store.validate(&k.key, ts("2026-07-20T16:00:00Z")),
            ApiKeyAuthResult::Expired
        );
        // activate 不存在的 id → None
        assert_eq!(store.activate_key(999, use_at), None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn disable_helper() {
        let path = tmp_path("disable");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let now = ts("2026-07-20T00:00:00Z");
        let k = store.create("d".into(), None, None, None, None, None, now);
        assert!(store.disable(k.id)); // 首次停用 → true
        assert!(!store.disable(k.id)); // 已停用 → false
        assert!(!store.get_by_id(k.id).unwrap().enabled);
        assert!(!store.disable(999)); // 不存在 → false
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn persist_roundtrip_via_save_now() {
        let path = tmp_path("persist");
        let _ = std::fs::remove_file(&path);
        let now = ts("2026-07-20T00:00:00Z");
        {
            let store = ApiKeyStore::load(&path);
            store.create(
                "keep".into(),
                None,
                Some(10.0),
                None,
                Some(1.0),
                Some(vec![1]),
                now,
            );
            store.save_now().unwrap();
        }
        // 重新载入应看到落盘内容
        let reloaded = ApiKeyStore::load(&path);
        assert_eq!(reloaded.len(), 1);
        let k = reloaded.get_by_id(1).unwrap();
        assert_eq!(k.name, "keep");
        assert_eq!(k.spending_limit, Some(10.0));
        assert_eq!(k.duration_days, Some(1.0));
        assert_eq!(k.bound_credential_ids, Some(vec![1]));
        assert!(k.key.starts_with("sk-"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deleted_ids_are_never_reused_across_reload() {
        let path = tmp_path("nextid");
        let _ = std::fs::remove_file(&path);
        let now = ts("2026-07-20T00:00:00Z");
        {
            let store = ApiKeyStore::load(&path);
            let k1 = store.create("0001".into(), None, None, None, None, None, now);
            let k2 = store.create("0002".into(), None, None, None, None, None, now);
            assert_eq!((k1.id, k2.id), (1, 2));
            // 删掉末条后新建:不得复用 id=2(否则新 key 继承前任的用量记录与累计消费)。
            assert!(store.delete(k2.id));
            let k3 = store.create("0003".into(), None, None, None, None, None, now);
            assert_eq!(k3.id, 3);
            // 全部删光后依然不回退到 1。
            assert!(store.delete(k1.id));
            assert!(store.delete(k3.id));
            assert!(store.is_empty());
            let k4 = store.create("0004".into(), None, None, None, None, None, now);
            assert_eq!(k4.id, 4);
            assert!(store.delete(k4.id));
            store.save_now().unwrap();
        }
        // 重启:key 列表已空,next_id 仍由落盘字段恢复,不从 1 重来。
        let reloaded = ApiKeyStore::load(&path);
        assert!(reloaded.is_empty());
        let k5 = reloaded.create("0005".into(), None, None, None, None, None, now);
        assert_eq!(k5.id, 5);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_array_file_loads_and_next_id_follows_max_id() {
        let path = tmp_path("legacy");
        let _ = std::fs::remove_file(&path);
        // 旧版落盘形态:裸数组、无 next_id 字段。
        let legacy = r#"[
          {"id":1,"key":"sk-aaa","name":"0001","enabled":true,"created_at":"2026-07-20T00:00:00Z"},
          {"id":7,"key":"sk-bbb","name":"0007","enabled":true,"created_at":"2026-07-20T00:00:00Z"}
        ]"#;
        std::fs::write(&path, legacy).unwrap();
        let now = ts("2026-07-21T00:00:00Z");

        let store = ApiKeyStore::load(&path);
        assert_eq!(store.len(), 2);
        assert_eq!(store.get_by_id(7).unwrap().name, "0007");
        // 无 next_id 字段 → 由现存最大 id 推出,新 key 不撞已用 id。
        let k = store.create("new".into(), None, None, None, None, None, now);
        assert_eq!(k.id, 8);
        // 升级后按新形态落盘;删掉最后一条再载入,next_id 依旧不回退。
        assert!(store.delete(k.id));
        store.save_now().unwrap();
        let reloaded = ApiKeyStore::load(&path);
        assert_eq!(reloaded.len(), 2);
        let k2 = reloaded.create("newer".into(), None, None, None, None, None, now);
        assert_eq!(k2.id, 9);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn constant_time_key_compare_accepts_exact_match_only() {
        assert!(ct_eq_key("sk-abc", "sk-abc"));
        assert!(!ct_eq_key("sk-abc", "sk-abd"));
        assert!(!ct_eq_key("sk-abc", "sk-ab")); // 前缀不算命中
        assert!(!ct_eq_key("sk-abc", "sk-abcd"));
        assert!(!ct_eq_key("", "sk-abc"));
        assert!(ct_eq_key("", ""));
    }

    #[test]
    fn validate_matches_exact_key_at_any_position() {
        let path = tmp_path("ctvalidate");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let now = ts("2026-07-20T00:00:00Z");
        let first = store.create("a".into(), None, None, None, None, None, now);
        let _mid = store.create("b".into(), None, None, None, None, None, now);
        let last = store.create("c".into(), None, None, None, None, None, now);

        // 首条与末条都须命中(遍历全部候选,不因位置靠后而漏判)。
        for k in [&first, &last] {
            match store.validate(&k.key, now) {
                ApiKeyAuthResult::Valid { id, .. } => assert_eq!(id, k.id),
                other => panic!("expected Valid, got {other:?}"),
            }
        }
        // 前缀/多一字节/大小写变体都不得命中。
        let prefix = &last.key[..last.key.len() - 1];
        assert_eq!(store.validate(prefix, now), ApiKeyAuthResult::NotFound);
        assert_eq!(
            store.validate(&format!("{}0", last.key), now),
            ApiKeyAuthResult::NotFound
        );
        assert_eq!(
            store.validate(&last.key.to_uppercase(), now),
            ApiKeyAuthResult::NotFound
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn try_reserve_spend_atomic_bound_and_release() {
        let path = tmp_path("reserve");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let id = 1u32;
        let limit = 10.0;

        // 已花 0,预估 1.0:连续预留直到 spent(0)+reserved 逼近 limit。
        // 第 1..=10 次每次 +1.0 应成功(0+9+1<=10 时第 10 次:0+9+1=10<=10 通过),
        // 第 11 次 0+10+1=11>10 应失败——把并发越界界定在“至多一次在途请求之内”。
        let mut guards = Vec::new();
        for _ in 0..10 {
            guards.push(
                store
                    .try_reserve_spend(id, 0.0, limit, 1.0)
                    .expect("应可预留"),
            );
        }
        assert_eq!(store.reserved_amount(id), 10.0);
        assert!(
            store.try_reserve_spend(id, 0.0, limit, 1.0).is_err(),
            "越界应拒绝"
        );

        // 释放一个 guard → 预留降到 9.0 → 又能预留一次。
        guards.pop();
        assert_eq!(store.reserved_amount(id), 9.0);
        let g = store.try_reserve_spend(id, 0.0, limit, 1.0);
        assert!(g.is_ok());
        drop(g);
        drop(guards);
        // 全部释放后归零并从 map 移除。
        assert_eq!(store.reserved_amount(id), 0.0);
        assert!(store.reservations.lock().is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn try_reserve_spend_counts_existing_spent() {
        let path = tmp_path("reserve_spent");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        // 已花 9.5,上限 10,预估 1.0:9.5+0+1.0=10.5>10 → 首个请求即拒(已接近上限)。
        assert!(store.try_reserve_spend(1, 9.5, 10.0, 1.0).is_err());
        // 已花 9.5,预估 0.4:9.5+0.4=9.9<=10 → 通过;再来一次 9.5+0.4+0.4=10.3>10 拒。
        let g = store.try_reserve_spend(1, 9.5, 10.0, 0.4).expect("应通过");
        assert!(store.try_reserve_spend(1, 9.5, 10.0, 0.4).is_err());
        drop(g);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn try_reserve_spend_rejects_non_finite_spent_or_limit() {
        // finding #7 兜底:NaN/Inf 的 spent 或 limit 必须保守拒绝,
        // 否则 `spent + reserved + est > limit` 里 NaN 参与的比较恒 false → 误放行。
        let path = tmp_path("reserve_nonfinite");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        assert!(store.try_reserve_spend(1, f64::NAN, 10.0, 1.0).is_err());
        assert!(
            store
                .try_reserve_spend(1, f64::INFINITY, 10.0, 1.0)
                .is_err()
        );
        assert!(store.try_reserve_spend(1, 0.0, f64::NAN, 1.0).is_err());
        assert!(store.try_reserve_spend(1, 0.0, f64::INFINITY, 1.0).is_err());
        // 拒绝路径不得写入任何预留残留。
        assert!(store.reservations.lock().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn concurrent_same_spent_admissions_do_not_over_admit() {
        // finding #7 核心:N 个并发请求读到**同一** under-limit 的 spent 快照后各自尝试预留,
        // 由于 reserved 在同一把锁内累加,总准入被界定在“至多一次在途请求”越界之内,
        // 不会“大家都据同一 spent 各自放行”造成无界 over-admission。
        let path = tmp_path("reserve_concurrent");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let id = 1u32;
        let spent = 9.0; // 所有请求都读到这个同一快照
        let limit = 10.0;
        let est = 1.0;

        // 模拟 8 个并发请求争抢:仅 spent+reserved+est<=limit 时准入。
        // spent=9,limit=10,est=1 → 只允许 1 个在途预留(9+0+1=10<=10),第 2 个 9+1+1=11>10 拒。
        let mut threads = Vec::new();
        let admitted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hold = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        for _ in 0..8 {
            let s = store.clone();
            let a = admitted.clone();
            let h = hold.clone();
            threads.push(std::thread::spawn(move || {
                if let Ok(g) = s.try_reserve_spend(id, spent, limit, est) {
                    a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    // 持有 guard(不释放)以让并发预留真正叠加。
                    h.lock().push(g);
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        // 至多一次在途请求越界:恰好 1 个被准入,其余全被在途预留挡住。
        assert_eq!(admitted.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(store.reserved_amount(id), 1.0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reservation_cleanup_removes_entry_after_float_residue() {
        // finding #14:反复 reserve/release 无法精确表示的 est 后,
        // 全部释放应把该项从 map 移除(不留近零僵尸项),即便有浮点残渣。
        let path = tmp_path("reserve_residue");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let id = 1u32;
        // est = 1.0/0.72 无法用 f64 精确表示,累加/相减会产生残渣。
        let est = 1.0 / 0.72;
        for _ in 0..64 {
            let mut guards = Vec::new();
            for _ in 0..5 {
                guards.push(store.try_reserve_spend(id, 0.0, 1_000_000.0, est).unwrap());
            }
            drop(guards); // 全部释放
        }
        // 每轮全释放后都应彻底清项:map 里不得残留该 id 的近零僵尸项。
        assert_eq!(store.reserved_amount(id), 0.0);
        assert!(
            store.reservations.lock().is_empty(),
            "浮点残渣不得导致 map 残留近零项"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn try_reserve_spend_non_positive_est_is_zero() {
        let path = tmp_path("reserve_zero");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        // 负/NaN 预估视作 0,不得把预留算成负数“扩容”上限。
        let g = store
            .try_reserve_spend(1, 10.0, 10.0, -5.0)
            .expect("est=0 且 spent==limit 应通过");
        assert_eq!(store.reserved_amount(1), 0.0);
        drop(g);
        assert!(store.try_reserve_spend(1, 10.0, 10.0, f64::NAN).is_ok());
        let _ = std::fs::remove_file(&path);
    }

    /// 每个测试独享的空目录(备份档要按 `.corrupt.*` 枚举,不能和别的用例共用目录)。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "kiro2api_apikey_{}_{}_{}",
            tag,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 目录下的损坏备份文件列表(`*.corrupt.*`)。
    fn corrupt_backups(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".corrupt."))
            .collect();
        v.sort();
        v
    }

    /// 回归:一条 key 写坏不得让整份 api_keys.json 被静默当成空 store。
    ///
    /// 修复前 `read_json(..).ok().flatten()` 把解析错误和"文件不存在"折叠成同一个 None:
    /// 全部客户当场 401、日志里一个字都没有,管理员随手新建一个 key 就会让 5s 刷盘把这份
    /// 仍可人工挽救的文件整份覆写掉。
    #[test]
    fn corrupt_entry_is_salvaged_backed_up_and_does_not_recycle_ids() {
        let dir = unique_temp_dir("salvage");
        let path = dir.join("api_keys.json");
        // 三条 key,中间那条的 created_at 写坏(untagged 枚举下会让**整份**文件解析失败)。
        let broken = r#"{
          "next_id": 4,
          "keys": [
            {"id":1,"key":"sk-aaa","name":"0001","enabled":true,"created_at":"2026-07-20T00:00:00Z"},
            {"id":2,"key":"sk-bbb","name":"0002","enabled":true,"created_at":"not-a-timestamp"},
            {"id":3,"key":"sk-ccc","name":"0003","enabled":true,"created_at":"2026-07-20T00:00:00Z"}
          ]
        }"#;
        std::fs::write(&path, broken).unwrap();

        let store = ApiKeyStore::load(&path);
        // 好的两条照常鉴权,坏的那条跳过 —— 不是全军覆没。
        assert_eq!(store.len(), 2, "坏条目不得拖垮其余 key");
        let now = ts("2026-07-21T00:00:00Z");
        assert!(matches!(
            store.validate("sk-aaa", now),
            ApiKeyAuthResult::Valid { id: 1, .. }
        ));
        assert!(matches!(
            store.validate("sk-ccc", now),
            ApiKeyAuthResult::Valid { id: 3, .. }
        ));

        // 原文件原地未动(供人工修复),且已原样备份一份覆不掉的副本。
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        let backups = corrupt_backups(&dir);
        assert_eq!(backups.len(), 1, "坏文件必须被备份:{backups:?}");
        assert_eq!(std::fs::read(&backups[0]).unwrap(), broken.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&backups[0]).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "备份含 key 明文,必须仅属主可读写,实际 {mode:o}"
            );
        }

        // next_id 不因坏条目被跳过而回退:新 key 不得复用 id 2/3(会继承前任用量与消费)。
        let k = store.create("new".into(), None, None, None, None, None, now);
        assert_eq!(k.id, 4, "id 不得回收复用");

        // 幂等:同一份坏内容再载入一次不再堆备份。
        let _ = ApiKeyStore::load(&path);
        assert_eq!(corrupt_backups(&dir).len(), 1, "同一份坏内容不应重复备份");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归:被截断/手工写坏语法的文件同样要备份 + 报错,而不是静默空载入。
    /// 此时逐条抢救无能为力(连 JSON 都不是),备份档就是唯一的挽救手段。
    #[test]
    fn truncated_file_is_backed_up_and_left_in_place() {
        let dir = unique_temp_dir("truncated");
        let path = dir.join("api_keys.json");
        let broken = r#"{"next_id":9,"keys":[{"id":1,"key":"sk-aaa","na"#;
        std::fs::write(&path, broken).unwrap();

        let store = ApiKeyStore::load(&path);
        assert!(store.is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            broken,
            "原文件必须留在原处供人工修复"
        );
        let backups = corrupt_backups(&dir);
        assert_eq!(backups.len(), 1, "坏文件必须被备份:{backups:?}");
        assert_eq!(std::fs::read(&backups[0]).unwrap(), broken.as_bytes());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归:旧版裸数组里有坏条目时,同样逐条抢救,且 next_id 下界要盖住**坏条目**的 id。
    #[test]
    fn corrupt_legacy_array_salvages_and_covers_bad_entry_id() {
        let dir = unique_temp_dir("salvage_legacy");
        let path = dir.join("api_keys.json");
        // 裸数组(无 next_id 字段),最大 id 的那条恰好是坏的。
        let broken = r#"[
          {"id":1,"key":"sk-aaa","name":"0001","enabled":true,"created_at":"2026-07-20T00:00:00Z"},
          {"id":9,"key":"sk-bbb","name":"0009","created_at":"2026-07-20T00:00:00Z","limit_unit":123}
        ]"#;
        std::fs::write(&path, broken).unwrap();

        let store = ApiKeyStore::load(&path);
        assert_eq!(store.len(), 1);
        let now = ts("2026-07-21T00:00:00Z");
        // 坏条目虽被跳过,它占的 id=9 仍计入下界 → 新 key 从 10 起,绝不复用 9。
        let k = store.create("new".into(), None, None, None, None, None, now);
        assert_eq!(k.id, 10, "坏条目的 id 也不得被复用");
        assert_eq!(corrupt_backups(&dir).len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归:文件是对象但没有 `keys` 数组(被别的内容顶掉/字段名写坏)也算坏文件,
    /// 必须备份 + 报错,而不是被 serde 的 default 悄悄解析成空 store。
    #[test]
    fn object_without_keys_array_is_treated_as_corrupt() {
        let dir = unique_temp_dir("nokeys");
        let path = dir.join("api_keys.json");
        std::fs::write(&path, r#"{"next_id":12}"#).unwrap();

        let store = ApiKeyStore::load(&path);
        assert!(store.is_empty());
        assert_eq!(corrupt_backups(&dir).len(), 1, "缺 keys 数组应判为坏文件");
        // 抢救出的 next_id 仍被采纳,新 key 不撞历史 id。
        let k = store.create(
            "new".into(),
            None,
            None,
            None,
            None,
            None,
            ts("2026-07-21T00:00:00Z"),
        );
        assert_eq!(k.id, 12);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 正常路径不得被"抢救"逻辑打扰:健康文件照常载入,且**不留任何备份档**。
    #[test]
    fn healthy_file_loads_without_backup() {
        let dir = unique_temp_dir("healthy");
        let path = dir.join("api_keys.json");
        let now = ts("2026-07-20T00:00:00Z");
        {
            let store = ApiKeyStore::load(&path);
            store.create("a".into(), None, None, None, None, None, now);
            store.create("b".into(), None, None, None, None, None, now);
            store.save_now().unwrap();
        }
        let reloaded = ApiKeyStore::load(&path);
        assert_eq!(reloaded.len(), 2);
        assert!(corrupt_backups(&dir).is_empty(), "健康文件不该产生备份");
        // 文件不存在也是正常路径:不报错、不备份。
        let missing = dir.join("nope.json");
        assert!(ApiKeyStore::load(&missing).is_empty());
        assert!(corrupt_backups(&dir).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 造一条带指定惰性时长的 key(直接构造,绕过 `create` 的收敛),
    /// 用来模拟"修复前已落盘 / 被手工编辑写进去"的坏数据。
    fn key_with_duration(days: f64, now: DateTime<Utc>) -> ApiKey {
        ApiKey {
            id: 1,
            key: "sk-poison".into(),
            name: "p".into(),
            enabled: true,
            created_at: now,
            expires_at: None,
            spending_limit: None,
            limit_unit: default_limit_unit(),
            duration_days: Some(days),
            activated_at: None,
            bound_credential_ids: None,
        }
    }

    /// 回归:离谱的 `durationDays` 不得让惰性激活 panic。
    ///
    /// 修复前 `now + Duration::milliseconds((days*86_400_000.0).round() as i64)` 在
    /// `|days| > ~9.5e7` 时溢出,chrono 的 Add 实现内部 expect 直接 panic;而该路径是
    /// `auth::require_api_key` 每个 key 首次使用都会走的鉴权热路径
    /// (→ `ApiKeyStore::activate_key` → `ApiKey::activate`),panic 即 500/断连。
    ///
    /// 这里直接构造 ApiKey(不走 `create`),是因为已经落盘的坏值也必须扛得住 ——
    /// 入库收敛只管住新写入,`activate` 自身不许有任何会 panic 的取值。
    #[test]
    fn absurd_duration_days_never_panic_on_activation() {
        let now = ts("2026-07-20T00:00:00Z");
        for days in [
            1e9_f64,
            -1e9,
            1e18,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            f64::MAX,
            f64::MIN,
        ] {
            let mut k = key_with_duration(days, now);
            assert!(k.activate(now), "首次激活应返回 true:{days}");
            let exp = k.expires_at.expect("激活后应有绝对过期时刻");
            // 过期判定同样不得 panic(仅比较,不再做算术)。
            let _ = k.is_expired(now);
            let _ = k.is_expired(ts("2099-01-01T00:00:00Z"));
            // 正时长 → 远期(实际上永不过期);负时长/NaN → 立即过期(失败方向取更严)。
            if days > 0.0 {
                assert!(exp > now, "正时长应算出未来时刻:{days}");
            } else {
                assert!(exp <= now, "负时长/NaN 应立即过期:{days}");
            }
        }

        // 端到端走一遍 store:1e9 天的 key 激活后到 2099 年仍可用,不是 panic 也不是当场失效。
        let path = tmp_path("absurd_duration");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let long = store.create("long".into(), None, None, None, Some(1e9), None, now);
        assert_eq!(store.activate_key(long.id, now), Some(true));
        assert!(matches!(
            store.validate(&long.key, ts("2099-01-01T00:00:00Z")),
            ApiKeyAuthResult::Valid { .. }
        ));
        let _ = std::fs::remove_file(&path);
    }

    /// 回归:越界 `durationDays` 入库即收敛(创建口与更新口都要),
    /// 免得坏值落到盘上、重启后每次读回来都得再防一遍。
    #[test]
    fn absurd_duration_days_are_clamped_on_write() {
        let path = tmp_path("clamp_duration");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let now = ts("2026-07-20T00:00:00Z");

        for days in [1e9_f64, -1e9, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            let k = store.create("x".into(), None, None, None, Some(days), None, now);
            let stored = k.duration_days.expect("应保留惰性时长");
            assert!(stored.is_finite(), "落盘时长必须有限:{days} → {stored}");
            assert!(
                stored.abs() <= MAX_DURATION_DAYS,
                "落盘时长必须在安全范围内:{days} → {stored}"
            );
        }
        // 更新口同样收敛,别让越界值绕过创建口从这里进来。
        let k = store.create("upd".into(), None, None, None, None, None, now);
        let upd = store
            .update(k.id, None, None, None, None, None, Some(Some(1e300)), None)
            .unwrap();
        assert_eq!(upd.duration_days, Some(MAX_DURATION_DAYS));
        // 正常值不受影响。
        let ok = store.update(k.id, None, None, None, None, None, Some(Some(30.0)), None);
        assert_eq!(ok.unwrap().duration_days, Some(30.0));

        let _ = std::fs::remove_file(&path);
    }

    /// 正常时长的换算不得被 clamp 改动(happy path 回归)。
    #[test]
    fn ordinary_duration_days_are_unchanged() {
        let now = ts("2026-07-20T00:00:00Z");
        assert_eq!(
            expiry_from_duration_days(now, 0.25),
            ts("2026-07-20T06:00:00Z")
        );
        assert_eq!(
            expiry_from_duration_days(now, 30.0),
            ts("2026-08-19T00:00:00Z")
        );
        assert_eq!(expiry_from_duration_days(now, 0.0), now);
        assert_eq!(sanitize_duration_days(365.0), 365.0);
    }

    #[test]
    fn api_keys_path_inference() {
        assert_eq!(
            api_keys_path_from("/data/credentials.json"),
            PathBuf::from("/data/api_keys.json")
        );
        assert_eq!(
            api_keys_path_from("credentials.json"),
            PathBuf::from("./api_keys.json")
        );
    }
}
