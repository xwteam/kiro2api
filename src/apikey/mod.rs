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

use chrono::{DateTime, Duration, Utc};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

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
            // 以毫秒精度换算,兼容 0.25 天(6 小时)这类小数时长。
            let ms = (days * 86_400_000.0).round() as i64;
            self.expires_at = Some(now + Duration::milliseconds(ms));
        }
        true
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

/// 线程安全的 API-KEY 存储。持锁在内存改动,落盘经脏标记 + 后台刷盘去抖。
pub struct ApiKeyStore {
    keys: RwLock<Vec<ApiKey>>,
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
    pub fn load<P: AsRef<Path>>(path: P) -> Arc<Self> {
        let path = path.as_ref().to_path_buf();
        let initial: Vec<ApiKey> = read_json(&path).ok().flatten().unwrap_or_default();
        let (dirty, handle) = dirty_channel();
        let store = Arc::new(Self {
            keys: RwLock::new(initial),
            dirty,
            file_path: path.clone(),
            reservations: Mutex::new(HashMap::new()),
        });
        // 后台刷盘依赖 tokio 运行时(spawn)。有运行时才起后台任务;无运行时
        // (如纯同步单测)跳过——此时写不自动落盘,须显式 `save_now`。
        if tokio::runtime::Handle::try_current().is_ok() {
            let snap = store.clone();
            spawn_flush_loop(path, handle, FLUSH_INTERVAL_SECS, move || {
                let guard = snap.keys.read();
                serde_json::to_vec_pretty(&*guard).unwrap_or_else(|_| b"[]".to_vec())
            });
        }
        store
    }

    /// 同步原子落盘(测试/关键路径即时持久化用;常规写走脏标记后台刷盘)。
    pub fn save_now(&self) -> std::io::Result<()> {
        let guard = self.keys.read();
        atomic_write_json(&self.file_path, &*guard)
    }

    /// 校验明文 key:命中后按 启用 + 过期(注入 now)裁决。不检查消费上限
    /// (上限求和依赖 stats 层,由 relay/handler 另行处理)。
    pub fn validate(&self, key_str: &str, now: DateTime<Utc>) -> ApiKeyAuthResult {
        let guard = self.keys.read();
        match guard.iter().find(|k| k.key == key_str) {
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

    /// 新建一个 key。id 取当前最大值 +1(从 1 起)。生成 "sk-" 前缀随机明文。
    /// `created_at` 由注入的 `now` 决定。返回新建的 ApiKey 克隆。
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
            let next_id = guard.iter().map(|k| k.id).max().unwrap_or(0) + 1;
            let key = ApiKey {
                id: next_id,
                key: generate_api_key(),
                name,
                enabled: true,
                created_at: now,
                expires_at,
                spending_limit,
                limit_unit: limit_unit.unwrap_or_else(default_limit_unit),
                duration_days,
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
                k.duration_days = v;
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
