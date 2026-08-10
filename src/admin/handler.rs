//! Admin 只读/启停处理器:账号用量统计、配置只读展示(脱敏)、账号手动启停。
//!
//! 全部输出经过脱敏:`AccountStat` 本身不携带 token(见 `kiro::pool`),
//! `AdminConfigView` 只出 `api_key_set`/`admin_api_key_set` 布尔,绝不出 key 明文。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::kiro::pool::AccountStat;
use crate::protocol::anthropic::handler::MessagesState;
use crate::stats::model::{FailureEvent, ThrottleEvent, UsageRecord as StoredUsageRecord};
use crate::stats::usage::{DailyRollup, DaySummary, Page, RangeSummary, UsageBucket};

/// 当前 unix 秒;供各 handler 计算 `now` 供池的冷却/统计判定用。
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 内部 String id → 前端期望的数值 id。真机 credentials.json 的 id 是整数,
/// 非数值 id(理论罕见)回落 0,不影响启停(启停按 URL 里的原始字符串匹配)。
fn id_as_number(id: &str) -> i64 {
    id.parse::<i64>().unwrap_or(0)
}

/// unix 秒 → RFC3339 字符串(UTC、秒精度、Z 后缀);0 视为"无"返回 None。
fn unix_to_rfc3339(secs: u64) -> Option<String> {
    if secs == 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// 健康度分级(展示用):禁用 > 封禁 > 冷却(unhealthy)> 有失败 strike(warning)> 健康。
///
/// 封禁必须排在冷却之前、且不能落到 `healthy`:冷却是计时器,过期即回池,而封禁是上游给的
/// 结论,不随时间解除。此前只看计时器,冷却一过封禁号就报 `healthy`,于是面板一边挂着
/// 「封禁」标签、可用数一边把它算进去——同一个账号两个互相打架的说法。
fn health_status(a: &AccountStat) -> &'static str {
    if a.disabled {
        "disabled"
    } else if a.status_reason == "banned" || a.in_cooldown {
        "unhealthy"
    } else if a.strikes > 0 {
        "warning"
    } else {
        "healthy"
    }
}

/// 单个凭据状态项,camelCase 对齐前端 `CredentialStatusItem`。
/// 全部字段为非密元数据 + 计数;绝不携带 token/secret(源自 `AccountStat`,本身已脱敏)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatusItem {
    pub id: i64,
    pub priority: i64,
    pub weight: u32,
    pub disabled: bool,
    /// 累计失败次数(展示"失败"列;点开即该账号的失败日志)。
    ///
    /// **不是**连续失败连击数(`strikes`)。曾经装的是后者,于是一个刚失败过、但 strike 已被
    /// 冷却分支清零的账号显示为「失败 0」,面板上看不出它出过事。
    pub failure_count: u32,
    pub is_current: bool,
    pub expires_at: Option<String>,
    pub auth_method: Option<String>,
    pub has_profile_arn: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    pub success_count: u64,
    pub last_used_at: Option<String>,
    pub has_proxy: bool,
    pub health_status: &'static str,
    /// 最近一次失败的具体原因:`none` / `banned` / `quota` / `token_expired` / `throttled`。
    /// 与 `healthStatus` 正交 —— 前者答"能不能用",本字段答"为什么不能用"。
    pub status_reason: &'static str,
    /// 限流事件条数(展示"限流"列;点开即该账号的限流日志)。
    ///
    /// 取自限流事件日志本身,**不是**累计失败数。曾经装的是后者,于是被上游封禁的账号
    /// 在面板上显示成「限流 1」——把"账号被停用、需联系客服"错报成"歇一会儿就好"。
    pub throttle_count: u64,
}

/// 凭据状态汇总响应,camelCase 对齐前端 `CredentialsStatusResponse`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsStatusResponse {
    pub total: usize,
    pub available: usize,
    pub current_id: i64,
    pub credentials: Vec<CredentialStatusItem>,
}

/// `throttles` 是该账号在限流事件日志里的条数。
///
/// 此前两个计数的名字和取值是错位的:`failureCount` 装的是 `strikes`(连续失败连击数,
/// 成功一次即清零),`throttleCount` 装的是 `failures`(累计失败总数,与限流无关)。于是一个
/// 被上游**封禁**的账号在面板上显示为「限流 1」,而「失败」列是 0 —— 两个数都在说假话,
/// 且都指向错误的排查方向。现在各归其位:失败=累计失败数,限流=真的限流事件条数。
fn account_to_item(a: &AccountStat, throttles: u64) -> CredentialStatusItem {
    CredentialStatusItem {
        id: id_as_number(&a.id),
        priority: a.priority as i64,
        weight: a.weight,
        disabled: a.disabled,
        failure_count: a.failures as u32,
        is_current: a.is_current,
        expires_at: unix_to_rfc3339(a.expires_at_unix),
        auth_method: Some(a.auth_method.clone()),
        has_profile_arn: a.has_profile_arn,
        email: a.email.clone(),
        nickname: a.nickname.clone(),
        success_count: a.successes,
        last_used_at: unix_to_rfc3339(a.last_used_unix),
        has_proxy: a.has_proxy,
        health_status: health_status(a),
        status_reason: a.status_reason,
        throttle_count: throttles,
    }
}

/// `GET /api/admin/credentials`:凭据状态列表(camelCase),前端登录后首个拉取的端点,
/// 也是隐式"登录校验"面——鉴权层放行即视为 key 有效。
pub async fn credentials(State(state): State<MessagesState>) -> Json<CredentialsStatusResponse> {
    let now = now_unix();
    let pool = state.pool.lock().await;
    let accounts = pool.stats(now);
    let available = pool.active_count(now);
    drop(pool);

    // 一次遍历取全部账号的限流条数(逐账号查会把整份日志扫 N 遍)。
    let throttles = state.stats.throttle_log.counts_by_credential().await;
    let credentials: Vec<CredentialStatusItem> = accounts
        .iter()
        .map(|a| {
            let n = throttles.get(&id_as_u32(&a.id)).copied().unwrap_or(0);
            account_to_item(a, n)
        })
        .collect();
    Json(CredentialsStatusResponse {
        total: accounts.len(),
        available,
        current_id: -1,
        credentials,
    })
}

/// `POST /api/admin/credentials/{id}/disabled` 请求体。
#[derive(Debug, Deserialize)]
pub struct SetDisabledRequest {
    pub disabled: bool,
}

/// 统一 `SuccessResponse`(camelCase 对齐前端)。
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

/// `POST /api/admin/credentials/{id}/disabled`:按前端契约设置禁用状态,
/// body `{disabled}`,返回 `{success,message}`。
pub async fn set_disabled(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
    Json(req): Json<SetDisabledRequest>,
) -> Response {
    let mut pool = state.pool.lock().await;
    let found = pool.set_disabled(&id, req.disabled);
    drop(pool);
    // 手工启停同样要立刻落盘:此前只改活池,靠后续某次刷新顺带把它带下去,中间重启一次
    // 这个操作就没了——运维禁用的账号会自己回到池里。
    if found && let Err(e) = persist_pool_credentials(&state).await {
        tracing::warn!(error = %e, "启停后落盘失败");
    }
    if found {
        Json(SuccessResponse {
            success: true,
            message: if req.disabled {
                "credential disabled".into()
            } else {
                "credential enabled".into()
            },
        })
        .into_response()
    } else {
        not_found(&id)
    }
}

/// 模型条目,字段名与前端 `ModelItem` 保持一致(snake_case,非 camelCase)。
#[derive(Debug, Serialize)]
pub struct ModelItem {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
    pub display_name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub max_tokens: u32,
    /// 上下文窗口(能读多少)。与 `max_tokens`(能写多少)是两回事。
    /// 上游给了就用真值,没给才回落静态目录 —— 此前一律按 200K 报,1M 的模型被低报五倍。
    pub context_window: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_multiplier: Option<f64>,
}

/// 模型列表响应,对齐前端 `ModelsResponse`。
#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<ModelItem>,
}

/// 模型条目的 `created` 常量(固定值,前端仅展示不排序)。
const MODEL_CREATED: u64 = 1_700_000_000;

/// 由动态并集里的 [`crate::models_cache::ModelInfo`] 映射成前端 `ModelItem`。
/// `max_tokens` 上游缺失(0)时回落 200_000(前端表格数值列需要一个合理默认)。
fn model_item_from_info(m: &crate::models_cache::ModelInfo) -> ModelItem {
    ModelItem {
        id: m.id.clone(),
        object: "model".to_string(),
        created: MODEL_CREATED,
        owned_by: m.owned_by.clone(),
        display_name: m.display_name.clone(),
        kind: "chat".to_string(),
        max_tokens: if m.max_tokens == 0 {
            200_000
        } else {
            m.max_tokens
        },
        // 上游给了就用它的真值;没给才回落静态目录里该模型的窗口(再没有则 200K)。
        context_window: m.context_window.unwrap_or_else(|| {
            crate::models_catalog::CATALOG
                .iter()
                .find(|e| e.id == m.id)
                .map(|e| e.max_tokens)
                .unwrap_or(200_000)
        }),
        rate_multiplier: m.rate_multiplier,
    }
}

/// 静态 17 模型目录(动态并集为空时的回落)。抽成独立函数以便动态/回落两路共用形状。
/// 静态回落目录:上游模型并集缓存为空时用它作答。
///
/// 内容来自 [`crate::models_catalog::CATALOG`] —— 与三个协议侧 `/models` 同一份数据源,
/// 改一处即四个端点同步生效,不会再出现"管理面 17 条、协议侧各三条且互不相同"的分裂。
fn build_static_model_list() -> Vec<ModelItem> {
    crate::models_catalog::CATALOG
        .iter()
        .map(|e| ModelItem {
            id: e.id.to_string(),
            object: "model".to_string(),
            created: MODEL_CREATED,
            owned_by: "kiro2api".to_string(),
            display_name: e.display_name.to_string(),
            kind: "chat".to_string(),
            max_tokens: e.max_tokens,
            context_window: e.max_tokens,
            rate_multiplier: None,
        })
        .collect()
}

/// `GET /api/admin/models`:返回各账号上游 `ListAvailableModels` 并集(缓存命中即用),
/// 缓存为空(无账号刷新过或全过期)时回落静态 17 模型目录。
///
/// 惰性回填:并集为空且池里有账号时,后台先按 TTL 触发一次实拉(不阻塞本次响应),
/// 使下一次请求能拿到动态清单;本次仍以静态目录兜底,保证前端始终有数据。
/// 回填是**单飞 + 有界 + 带冷却**的(见 [`LazyRefreshGate`] / [`lazy_refresh_sweep`]):
/// 本端点被前端每次渲染仪表盘都会打一遍,不能让每个请求各起一轮全池扫描。
pub async fn models(State(state): State<MessagesState>) -> Json<ModelsResponse> {
    let now = now_unix();
    let union = state.models_cache.get_union(now).await;
    if !union.is_empty() {
        let data = union.iter().map(model_item_from_info).collect();
        return Json(ModelsResponse {
            object: "list".to_string(),
            data,
        });
    }
    // 并集为空:后台惰性回填(不阻塞),本次以静态目录兜底。
    spawn_lazy_refresh(state.clone(), now);
    Json(ModelsResponse {
        object: "list".to_string(),
        data: build_static_model_list(),
    })
}

/// 惰性回填的失败上限:本轮失败累计到此数即停。
///
/// 上游整体故障时(账号被封 / 区域抖动 / 额度耗尽)没有任何账号会成功,成功类上限
/// ([`DISCOVERY_SUCCESS_CAP`] / [`DISCOVERY_STALL_LIMIT`])一个都不会触发,少了这道闸
/// 一轮扫描就会把全池上千账号挨个打一遍。
const LAZY_REFRESH_FAILURE_CAP: usize = 8;

/// 惰性回填的冷却:一轮扫描结束后这段时间内不再起新的一轮。
///
/// 为什么必须有:模型缓存纯内存,回填失败时并集恒为空,于是**每次**请求都会走到回填。
/// 只有单飞没有冷却的话,上一轮刚结束下一次仪表盘渲染就能再起一轮 = 负缓存缺失下的
/// 无限重扫。取 60s:上游真恢复时最迟一分钟就能回填上,对上游又只是每分钟至多一轮。
const LAZY_REFRESH_COOLDOWN_SECS: u64 = 60;

/// 惰性回填闸门:进程级**单飞 + 冷却 + 轮转游标**。
///
/// 背景:`ModelsCache` 纯内存不落盘,进程重启后并集必为空,`GET /api/admin/models` 又是
/// 前端每次渲染仪表盘都打的端点。旧实现无任何互斥与上限——N 个并发请求就起 N 轮全池扫描,
/// 每轮对上千账号各发一次 `ListAvailableModels`(还可能各带一次令牌刷新),足以打爆上游并
/// 招来风控。闸门保证:同一时刻至多一轮扫描,且一轮结束后 [`LAZY_REFRESH_COOLDOWN_SECS`]
/// 内不再起新轮。
///
/// 用进程级 `static` 而非 state 字段:`MessagesState` 由 relay/admin 共用且本模块不改它;
/// 一个进程只服务一个池,进程级单飞与"每个池一份"等价。
struct LazyRefreshGate {
    /// 是否已有一轮扫描在跑(CAS 抢占,输的一方直接放弃、连 task 都不 spawn)。
    in_flight: AtomicBool,
    /// 下一次允许起扫描的 unix 秒(上一轮结束时刻 + 冷却);0 = 从未跑过,立即可起。
    next_allowed_unix: AtomicU64,
    /// 全池轮转游标:下一轮从上一轮停下的位置继续。
    ///
    /// 没有它的话,上游只对池首那几个账号持续失败时(封号/额度耗尽),失败上限会让**每一轮**
    /// 都反复重试同样那几个坏账号,后面的好账号永远轮不到、并集永久为空——那才是真正的回归。
    cursor: AtomicUsize,
}

impl LazyRefreshGate {
    const fn new() -> Self {
        Self {
            in_flight: AtomicBool::new(false),
            next_allowed_unix: AtomicU64::new(0),
            cursor: AtomicUsize::new(0),
        }
    }

    /// 抢扫描权:仍在冷却窗口内 / 已有一轮在跑 → `None`;抢到 → `Some(本轮起始游标)`。
    fn try_acquire(&self, now: u64) -> Option<usize> {
        if now < self.next_allowed_unix.load(Ordering::Acquire) {
            return None;
        }
        // CAS false→true:并发请求里只有一个能成功,其余立刻返回(这就是单飞)。
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        Some(self.cursor.load(Ordering::Relaxed))
    }

    /// 一轮扫描结束:推进轮转游标、起冷却窗口、最后才释放单飞标志。
    /// 顺序不可颠倒——先释放的话,并发请求可能在冷却窗口写入前抢到扫描权、绕过冷却。
    fn finish(&self, now: u64, scanned: usize) {
        self.cursor.fetch_add(scanned, Ordering::Relaxed);
        self.next_allowed_unix.store(
            now.saturating_add(LAZY_REFRESH_COOLDOWN_SECS),
            Ordering::Release,
        );
        self.in_flight.store(false, Ordering::Release);
    }
}

/// 进程内唯一的惰性回填闸门。
static LAZY_REFRESH_GATE: LazyRefreshGate = LazyRefreshGate::new();

/// 扫描许可(RAII):Drop 时结算闸门。用守卫而非收尾代码,是为了扫描中途 panic
/// 也能释放单飞标志——否则闸门被永久卡死,惰性回填此后再不会发生。
struct LazyRefreshPermit {
    gate: &'static LazyRefreshGate,
    /// 本轮在池内走过的位置数(扫描结束前回填;panic 时保持 0 = 游标不推进,下轮重来)。
    scanned: usize,
}

impl Drop for LazyRefreshPermit {
    fn drop(&mut self) {
        // 冷却从"扫描真正结束"起算,故这里取当下时刻而非请求进来时的 now。
        self.gate.finish(now_unix(), self.scanned);
    }
}

/// 一轮有界惰性回填的结果(供闸门推进游标;字段亦被测试用来断言上限确实生效)。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct LazySweepOutcome {
    /// 实际向上游发起实拉的账号数(受各上限约束)。
    attempts: usize,
    /// 本轮在池内走过的位置数(含被跳过的禁用/缓存仍新鲜账号),用于推进轮转游标。
    scanned: usize,
    successes: usize,
    failures: usize,
}

/// 后台惰性回填:从 `start` 起在池内**轮转**扫描,跳过禁用/缓存仍新鲜的账号,对其余账号
/// 逐个实拉一次 `ListAvailableModels`。失败仅记 warn(已在 `refresh_one` 内记),下轮再试。
///
/// 上限与 [`refresh_all_models`] 的"有界发现"同口径(别让隐式触发的回填比管理员显式点的
/// 批量刷新还猛):
/// - 成功累计到 [`DISCOVERY_SUCCESS_CAP`] 即停;
/// - 并集连续 [`DISCOVERY_STALL_LIMIT`] 次成功无增长即停(其余账号的档位很可能已被涵盖);
/// - 失败累计到 [`LAZY_REFRESH_FAILURE_CAP`] 即停(上游整体故障时不再往下打)。
async fn lazy_refresh_sweep(state: &MessagesState, now: u64, start: usize) -> LazySweepOutcome {
    let creds = {
        let pool = state.pool.lock().await;
        pool.snapshot_credentials()
    };
    let mut out = LazySweepOutcome::default();
    if creds.is_empty() {
        return out;
    }
    let len = creds.len();
    let start = start % len;
    let mut stall = 0usize;
    for off in 0..len {
        if out.successes >= DISCOVERY_SUCCESS_CAP
            || out.failures >= LAZY_REFRESH_FAILURE_CAP
            || stall >= DISCOVERY_STALL_LIMIT
        {
            break;
        }
        out.scanned = off + 1;
        let cred = &creds[(start + off) % len];
        if cred.disabled {
            continue;
        }
        if state.models_cache.is_fresh(&cred.id, now).await {
            continue;
        }
        out.attempts += 1;
        let before = state.models_cache.get_union(now).await.len();
        // 惰性回填 fire-and-forget:失败已在 refresh_one 记 WARN,此处只计数。
        match refresh_one(state, cred, now).await {
            Ok(_) => {
                out.successes += 1;
                let after = state.models_cache.get_union(now).await.len();
                if after > before {
                    stall = 0;
                } else {
                    stall += 1;
                }
            }
            Err(_) => out.failures += 1,
        }
    }
    out
}

/// 起一轮后台惰性回填(fire-and-forget,不阻塞请求)。
/// 抢不到闸门(已有一轮在跑 / 仍在冷却)时直接返回,连 task 都不 spawn。
fn spawn_lazy_refresh(state: MessagesState, now: u64) {
    let Some(start) = LAZY_REFRESH_GATE.try_acquire(now) else {
        return;
    };
    // 守卫在 spawn **之前**建好再 move 进任务:这样任务哪怕(运行时关停时)一次都没被 poll
    // 就被丢弃,守卫也会随之 Drop 并释放闸门,不会把单飞标志永久留在 true。
    let permit = LazyRefreshPermit {
        gate: &LAZY_REFRESH_GATE,
        scanned: 0,
    };
    tokio::spawn(async move {
        let mut permit = permit;
        let outcome = lazy_refresh_sweep(&state, now, start).await;
        // 结算信息交给守卫:正常结束推进游标,中途 panic 则按 scanned=0 只释放不推进。
        permit.scanned = outcome.scanned;
    });
}

/// 对单个凭据实拉一次上游模型清单并回填缓存;成功返回归约后的条目数,失败回传携带
/// 状态码+上游说明文字的 [`ModelsError`](供上层把真因透出到响应/日志)。
async fn refresh_one(
    state: &MessagesState,
    cred: &crate::kiro::credential::Credential,
    now: u64,
) -> Result<usize, crate::models_cache::ModelsError> {
    // 集中保鲜:先确保 access_token 新鲜(即将过期则刷新并写回活池),再拉模型清单。
    match crate::models_cache::fetch_available_models_fresh(
        &state.control_client,
        &state.cfg,
        &state.pool,
        &cred.id,
        now,
        Some(&state.refresh_ctx),
    )
    .await
    {
        Ok(resp) => {
            let infos = resp.to_model_infos();
            let n = infos.len();
            state.models_cache.put(cred.id.clone(), infos, now).await;
            Ok(n)
        }
        Err(e) => {
            // 数据面逐账号失败记 WARN(带账号 id + 状态码 + 上游短说明,绝不含令牌),
            // 让「查看日志」页在管理操作失败时看得到活动、看得清真因;
            // 批量刷新另在 refresh_all_models 汇总一条 summary。
            tracing::warn!("账号 {} 模型刷新失败: {e}", cred.id);
            Err(e)
        }
    }
}

/// `POST /api/admin/credentials/{id}/models/refresh`:按需实拉单个账号的模型清单并回填缓存。
/// 未知/禁用 id → 404;上游失败 → 502;成功 → `{success, count}`。
pub async fn refresh_credential_models(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
) -> Response {
    let now = now_unix();
    let creds = {
        let pool = state.pool.lock().await;
        pool.snapshot_credentials()
    };
    let Some(cred) = creds.into_iter().find(|c| c.id == id) else {
        return not_found(&id);
    };
    match refresh_one(&state, &cred, now).await {
        Ok(count) => Json(json!({ "success": true, "id": id, "count": count })).into_response(),
        // 把 ModelsError 的 Display(含状态码+上游说明,如 HTTP 403: Your User ID ... suspended)
        // 直接透到响应,让前端能显示真因而非泛化的 "failed to fetch models"。
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "success": false, "id": id, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 未知档位的有界发现:并集连续 K 次成功无增长即停(其它档位很可能已被涵盖)。
const DISCOVERY_STALL_LIMIT: usize = 3;
/// 未知档位的有界发现:总成功数上限(防冷启动全池串行刷 175 账号)。
const DISCOVERY_SUCCESS_CAP: usize = 12;

/// `POST /api/admin/credentials/models/refresh`:按订阅档位刷新模型清单并回填缓存。
///
/// 背景:`get_union` 会跨账号去重取并集,而**不同订阅档位服务不同模型**
/// (KIRO FREE ≈ 9 个,KIRO PRO+ ≈ 18 个)。旧实现"首个账号成功即停"只会缓存
/// 一个档位、漏掉另一档位的模型;而串行刷全部 ~175 账号又太慢(60s+)且猛打上游。
///
/// 本实现按档位精确刷新,保证**每个已知档位**各出一个代表账号、并集自然涵盖全档位模型:
/// 1. 快照池内凭据(跳过已禁用)。
/// 2. 用 `balance.tier_of(id)` 按档位分组:每个**已知**档位挑一个代表账号;
///    档位未缓存的账号(如冷启动余额缓存尚空)归入 `unknown` 列表。
/// 3. 刷新每个已知档位的那**一个**代表账号(`refresh_one` 会 PUT 进模型缓存,并集即含该档位)。
/// 4. 对 `unknown` 档位账号跑**有界发现**:每次成功后看并集大小,连续 K=3 次不增长、
///    或累计成功达 `DISCOVERY_SUCCESS_CAP` 上限、或全部 unknown 试完即停
///    (发现尚未被代表的档位,而不必刷全池)。
/// 5. 并集自动合并(模型缓存 `get_union`)。逐账号错误照旧收集进 `errors[]`。
///
/// 返回 `{ success:true, refreshed, failed, errors, tiers:[已涵盖的档位列表] }`。
/// 若无任何已知档位且发现也一无所获 → `refreshed` 可能为 0(优雅兜底,UI 据此提示)。
pub async fn refresh_all_models(State(state): State<MessagesState>) -> Response {
    let now = now_unix();
    let creds = {
        let pool = state.pool.lock().await;
        pool.snapshot_credentials()
    };

    // 按档位分组:每个已知档位保留一个代表 Credential;档位未缓存者归入 unknown。
    // 用 IndexMap 语义(Vec + 顺序探测)保证"档位覆盖列表"稳定、可测。
    let mut tier_reps: Vec<(String, crate::kiro::credential::Credential)> = Vec::new();
    let mut unknown: Vec<crate::kiro::credential::Credential> = Vec::new();
    for cred in creds {
        if cred.disabled {
            continue;
        }
        match state.balance.tier_of(&cred.id, now).await {
            Some(tier) => {
                if !tier_reps.iter().any(|(t, _)| t == &tier) {
                    tier_reps.push((tier, cred));
                }
                // 同档位其余账号无需再刷(并集已由代表账号涵盖)。
            }
            None => unknown.push(cred),
        }
    }

    let mut refreshed = 0usize;
    let mut errors: Vec<serde_json::Value> = Vec::new();
    // 实际成功刷到的档位集合(已知档位刷成功 + 发现阶段命中的账号档位),供 UI 展示。
    let mut tiers_covered: Vec<String> = Vec::new();

    // 步骤 3:刷每个已知档位的代表账号。
    for (tier, cred) in &tier_reps {
        match refresh_one(&state, cred, now).await {
            Ok(_) => {
                refreshed += 1;
                if !tiers_covered.contains(tier) {
                    tiers_covered.push(tier.clone());
                }
            }
            Err(e) => errors.push(json!({ "id": id_as_number(&cred.id), "error": e.to_string() })),
        }
    }

    // 步骤 4:未知档位的有界发现(并集停止增长 / 达上限 / 试完即停)。
    let mut discovery_successes = 0usize;
    let mut stall = 0usize;
    for cred in &unknown {
        if discovery_successes >= DISCOVERY_SUCCESS_CAP || stall >= DISCOVERY_STALL_LIMIT {
            break;
        }
        let before = state.models_cache.get_union(now).await.len();
        match refresh_one(&state, cred, now).await {
            Ok(_) => {
                refreshed += 1;
                discovery_successes += 1;
                let after = state.models_cache.get_union(now).await.len();
                if after > before {
                    stall = 0;
                } else {
                    stall += 1;
                }
                // 发现阶段:若该账号档位此刻已缓存(refresh 期间余额可能被旁路刷新),
                // 补入覆盖列表;拿不到档位则记一个占位符表示"已发现但档位未知"。
                match state.balance.tier_of(&cred.id, now).await {
                    Some(tier) if !tiers_covered.contains(&tier) => tiers_covered.push(tier),
                    Some(_) => {}
                    None => {
                        if !tiers_covered.iter().any(|t| t == "unknown") {
                            tiers_covered.push("unknown".to_string());
                        }
                    }
                }
            }
            Err(e) => errors.push(json!({ "id": id_as_number(&cred.id), "error": e.to_string() })),
        }
    }

    let failed = errors.len();
    // 汇总一条日志(逐账号失败已在 refresh_one 记 WARN),保留 summary 便于总体观测。
    if failed > 0 {
        tracing::warn!("模型刷新: {refreshed} 成功, {failed} 失败, 涵盖档位 {tiers_covered:?}");
    } else {
        tracing::info!("模型刷新: {refreshed} 成功, {failed} 失败, 涵盖档位 {tiers_covered:?}");
    }
    // 批量调用本身成功(success:true),但把 failed 计数 + errors[] + 涵盖档位一并暴露。
    Json(json!({
        "success": true,
        "refreshed": refreshed,
        "failed": failed,
        "errors": errors,
        "tiers": tiers_covered,
    }))
    .into_response()
}

/// `GET /admin/api/stats`:每账号用量快照 + 汇总(总数/存活/禁用/冷却中)。
pub async fn stats(State(state): State<MessagesState>) -> Json<serde_json::Value> {
    let now = now_unix();
    let pool = state.pool.lock().await;
    let accounts = pool.stats(now);
    let active = pool.active_count(now);
    drop(pool);

    let total = accounts.len();
    let disabled = accounts.iter().filter(|a| a.disabled).count();
    let in_cooldown = accounts.iter().filter(|a| a.in_cooldown).count();

    Json(json!({
        "accounts": accounts,
        "summary": {
            "total": total,
            "active": active,
            "disabled": disabled,
            "in_cooldown": in_cooldown,
        },
    }))
}

/// 配置只读展示视图;`*_set` 布尔代替真实 key 值,绝不泄露密钥。
#[derive(Debug, Serialize)]
pub struct AdminConfigView {
    pub host: String,
    pub port: u16,
    pub region: String,
    pub load_balancing_mode: String,
    pub max_rpm_per_credential: u32,
    pub kiro_version: String,
    pub system_version: String,
    pub node_version: String,
    pub credentials_path: String,
    pub api_key_set: bool,
    pub admin_api_key_set: bool,
}

/// `GET /admin/api/config`:配置只读展示,密钥仅出是否已设置的布尔。
pub async fn config_view(State(state): State<MessagesState>) -> Json<AdminConfigView> {
    let cfg = &state.cfg;
    Json(AdminConfigView {
        host: cfg.host.clone(),
        port: cfg.port,
        region: cfg.region.clone(),
        load_balancing_mode: cfg.load_balancing_mode.clone(),
        max_rpm_per_credential: cfg.max_rpm_per_credential,
        kiro_version: cfg.kiro_version.clone(),
        system_version: cfg.system_version.clone(),
        node_version: cfg.node_version.clone(),
        credentials_path: cfg.credentials_path.clone(),
        api_key_set: cfg.api_key.as_deref().is_some_and(|s| !s.is_empty()),
        admin_api_key_set: cfg.admin_api_key.as_deref().is_some_and(|s| !s.is_empty()),
    })
}

// ==========================================================================
// Phase 5:运行期可变配置端点(设置面板)。
// 全部读写 `state.runtime_cfg`(SharedRuntimeConfig);写入后按需即时作用到池
// (set_mode / set_max_rpm)并原子落盘 config.json。GET 侧对 key 做脱敏,绝不出明文。
// ==========================================================================

/// key 脱敏:保留前半段可见,其余以 `***` 收尾;空串原样返回空串。
/// 前端设置面板仅用于状态展示(“已设置某值”),不做复制;故半可见即可。
fn mask_key(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let n = key.chars().count();
    let visible = n / 2;
    let head: String = key.chars().take(visible).collect();
    format!("{head}***")
}

/// GET `/api/admin/config/load-balancing`:当前负载均衡模式。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadBalancingResponse {
    pub mode: String,
}

pub async fn get_load_balancing(State(state): State<MessagesState>) -> Json<LoadBalancingResponse> {
    let mode = state.runtime_cfg.read().load_balancing_mode.clone();
    Json(LoadBalancingResponse { mode })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLoadBalancingRequest {
    pub mode: String,
}

/// PUT `/api/admin/config/load-balancing`:切换负载均衡模式。
/// 校验 mode ∈ {priority, balanced} → 写运行期 → 即时作用到池(set_mode)→ 原子落盘。
/// 落盘失败:运行期已生效,记日志并返回 500(告知调用方持久化未成功)。
pub async fn set_load_balancing(
    State(state): State<MessagesState>,
    Json(payload): Json<SetLoadBalancingRequest>,
) -> Response {
    if payload.mode != "priority" && payload.mode != "balanced" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid mode", "valid": ["priority", "balanced"] })),
        )
            .into_response();
    }

    // 写运行期。
    {
        let mut rc = state.runtime_cfg.write();
        rc.load_balancing_mode = payload.mode.clone();
    }

    // 即时作用到池。
    let lb_mode = if payload.mode == "balanced" {
        crate::kiro::pool::LbMode::Balanced
    } else {
        crate::kiro::pool::LbMode::Priority
    };
    {
        let mut pool = state.pool.lock().await;
        pool.set_mode(lb_mode);
    }

    // 原子落盘。
    let snapshot = state.runtime_cfg.read().clone();
    if let Err(e) = snapshot.persist() {
        tracing::error!("持久化负载均衡模式失败: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "persist failed" })),
        )
            .into_response();
    }

    Json(LoadBalancingResponse { mode: payload.mode }).into_response()
}

/// GET `/api/admin/config/auth-keys`:脱敏后的 auth key(前端展示用)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthKeysResponse {
    pub api_key: String,
    pub admin_api_key: String,
}

pub async fn get_auth_keys(State(state): State<MessagesState>) -> Json<AuthKeysResponse> {
    let rc = state.runtime_cfg.read();
    Json(AuthKeysResponse {
        api_key: rc.api_key.as_deref().map(mask_key).unwrap_or_default(),
        admin_api_key: rc
            .admin_api_key
            .as_deref()
            .map(mask_key)
            .unwrap_or_default(),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAuthKeysRequest {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub admin_api_key: Option<String>,
}

/// PUT `/api/admin/config/auth-keys`:轮换 auth key。
/// 字段可选(省略=不改);提供但为空(trim 后)→ 400。校验通过后写运行期
/// (auth 闸下次请求即读到新值,旧 key 立即失效)→ 原子落盘。
pub async fn set_auth_keys(
    State(state): State<MessagesState>,
    Json(payload): Json<SetAuthKeysRequest>,
) -> Response {
    if payload.api_key.is_none() && payload.admin_api_key.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no field provided" })),
        )
            .into_response();
    }
    if let Some(k) = &payload.api_key
        && k.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "apiKey cannot be empty" })),
        )
            .into_response();
    }
    if let Some(k) = &payload.admin_api_key
        && k.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "adminApiKey cannot be empty" })),
        )
            .into_response();
    }

    // 写运行期(仅提供的字段)。trim 后存储,避免尾随空白污染 key。
    {
        let mut rc = state.runtime_cfg.write();
        if let Some(k) = payload.api_key {
            rc.api_key = Some(k.trim().to_string());
        }
        if let Some(k) = payload.admin_api_key {
            rc.admin_api_key = Some(k.trim().to_string());
        }
    }

    // 原子落盘。
    let snapshot = state.runtime_cfg.read().clone();
    if let Err(e) = snapshot.persist() {
        tracing::error!("持久化 auth key 失败: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "persist failed" })),
        )
            .into_response();
    }

    Json(json!({ "success": true, "message": "认证密钥已更新" })).into_response()
}

/// GET `/api/admin/server-info`:服务器版本 + 主 API key 状态。
/// 前端(api-keys 面板)据 `masterApiKey` 是否为 null 判断是否配置了主 key。
/// ⚠️ `masterApiKey` 是**完整明文**,不脱敏(前端 api-keys 面板自己 `maskKey()` 显示、复制时
/// 需要拿全值,故此处刻意不截断;`server_info_reports_full_master_key_when_set` 钉住该行为)。
/// 要脱敏形态请用 `/config/auth-keys`(那条走 [`mask_key`])。未配置则 `null`。
/// 本条注释此前写反了("已脱敏、绝不出完整明文"),与实现和其自身单测都矛盾,已更正。
/// `version` = 本服务(crate)版本;
/// `kiroVersion` = 伪装 UA 中的上游 Kiro 版本号(前端如需展示伪装版本用它)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfoResponse {
    pub master_api_key: Option<String>,
    pub version: String,
    pub kiro_version: String,
    /// 构建时捕获的 rustc 版本(build.rs 注入 KIRO_RUST_VERSION,保证有值)。
    /// 前端展示 'Rust 版本' 行,对齐 gemini2api 的 'Python 版本'。
    pub rust_version: String,
    // ---- 系统指标(camelCase)----
    /// 本地时间,格式 'YYYY/MM/DD HH:MM:SS'。
    pub server_time: String,
    /// unix 纪元秒(便于前端自行格式化/校准)。
    pub server_time_unix: i64,
    /// 内核串,如 'Linux 6.8.0-101-generic';缺失回落 std::env::consts::OS。
    pub os: String,
    /// 本进程 RSS 字节;读不到 → null。
    pub memory_used_bytes: Option<u64>,
    /// 系统 MemTotal 字节;读不到 → null。
    pub memory_total_bytes: Option<u64>,
    /// 系统级 CPU 忙碌率 %(读 /proc/stat 聚合 'cpu ' 行,约 100ms 采样,保留 1 位小数);
    /// 读不到 → null。用全机忙碌率而非本进程占用,避免空闲中转恒显 ~0% 的观感问题。
    pub cpu_percent: Option<f64>,
    /// 运行模式:'Docker' 或 'Bare'。
    pub run_mode: String,
    /// 本进程 PID。
    pub pid: u32,
    /// 服务已运行秒数(前端自行格式化为 天/时/分/秒)。
    pub uptime_secs: u64,
}

pub async fn server_info(State(state): State<MessagesState>) -> Json<ServerInfoResponse> {
    // 返回完整主 API Key:前端(api-keys-panel)以 maskKey() 自行脱敏显示,
    // 但「复制」按钮需要完整值。此端点已在 admin 鉴权闸之后,仅管理员可见。
    let master_api_key = {
        let rc = state.runtime_cfg.read();
        rc.api_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .map(str::to_string)
    };
    let (now_str, now_unix_secs) = local_now_formatted();
    Json(ServerInfoResponse {
        master_api_key,
        version: env!("CARGO_PKG_VERSION").to_string(),
        kiro_version: state.cfg.kiro_version.clone(),
        rust_version: env!("KIRO_RUST_VERSION").to_string(),
        server_time: now_str,
        server_time_unix: now_unix_secs,
        os: os_string(),
        memory_used_bytes: read_process_rss_bytes(),
        memory_total_bytes: read_mem_total_bytes(),
        cpu_percent: sample_process_cpu_percent().await,
        run_mode: detect_run_mode(),
        pid: std::process::id(),
        uptime_secs: crate::server::server_uptime_secs(),
    })
}

// ==========================================================================
// 版本检查 / 更新 / 重启(对齐 gemini2api 的 /check-update、/update、/restart）。
// 均在 admin 鉴权闸之后,仅管理员可达。
// ==========================================================================

/// 本服务的 GitHub 仓库(用于查 Release 与生成更新/仓库链接)。
const REPO_SLUG: &str = "xwteam/kiro2api";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckUpdateResponse {
    pub current: String,
    pub latest: String,
    pub has_update: bool,
    pub update_url: String,
    pub release_notes: String,
}

/// GET `/api/admin/check-update`:查 GitHub Releases 最新版并与当前 crate 版本比对。
///
/// 与 gemini2api 一致:拉 `releases/latest` 的 `tag_name`(去掉前导 `v`)与
/// [`env!("CARGO_PKG_VERSION")`] 比较。网络失败 / 仓库无 Release / 私有仓 404 一律**保守**
/// 返回 `has_update=false`、`latest=current`(不报错、不阻塞 UI)。出站走短请求控制面客户端。
pub async fn check_update(State(state): State<MessagesState>) -> Json<CheckUpdateResponse> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let repo_releases = format!("https://github.com/{REPO_SLUG}/releases");
    let api = format!("https://api.github.com/repos/{REPO_SLUG}/releases/latest");

    let fetched = async {
        let resp = state
            .control_client
            .get(&api)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::USER_AGENT, "kiro2api")
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        let tag = v
            .get("tag_name")?
            .as_str()?
            .trim_start_matches('v')
            .to_string();
        let url = v
            .get("html_url")
            .and_then(|x| x.as_str())
            .unwrap_or(&repo_releases)
            .to_string();
        let notes = v
            .get("body")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        Some((tag, url, notes))
    }
    .await;

    match fetched {
        Some((latest, url, notes)) if !latest.is_empty() => Json(CheckUpdateResponse {
            has_update: latest != current,
            current,
            latest,
            update_url: url,
            release_notes: notes,
        }),
        _ => Json(CheckUpdateResponse {
            latest: current.clone(),
            current,
            has_update: false,
            update_url: repo_releases,
            release_notes: String::new(),
        }),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateResponse {
    pub status: &'static str,
    pub message: String,
    pub command: String,
}

/// POST `/api/admin/update`:返回在服务器上手动更新的命令(**不自动执行**,与 gemini2api 一致)。
/// Docker Compose 部署:拉取新镜像并重建。前端展示命令 + 复制按钮。
pub async fn perform_update() -> Json<UpdateResponse> {
    Json(UpdateResponse {
        status: "ok",
        message: "请在服务器上执行以下命令完成更新:".to_string(),
        command: "docker compose pull && docker compose up -d".to_string(),
    })
}

#[derive(Debug, Deserialize)]
pub struct RestartQuery {
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartResponse {
    pub status: &'static str,
    pub message: String,
}

/// 进程退出前的收尾:把**全部**去抖存储立刻落盘。
///
/// `std::process::exit` 不跑析构、也不给后台刷盘循环最后一拍的机会,不在此显式刷盘就会丢掉
/// 最近一个去抖周期(约 5s)内的写。刷盘失败由各存储自行记 error 日志,这里不阻断退出。
///
/// 范围不能只有统计:API-KEY 的增删同样是「改内存 + 置脏 + 等下一拍」,而它丢一拍是**安全
/// 问题**——管理员发现某把 key 泄露、在面板上删掉,再顺手点「重启」,本 handler 只 sleep 500ms
/// 就 `exit(0)`,后台那一拍根本轮不到:进程带着旧 api_keys.json 重新拉起,刚吊销的 key 复活、
/// 照样鉴权通过(反过来,刚建出交给用户的 key 则凭空消失)。
///
/// 余额缓存与失败/限流事件日志同理,且各有自己的坑:前者丢的 `invalidate` 会让复用 id 的新
/// 账号顶着已删账号的余额/订阅档位(最长 5 分钟),后者丢的是运营者点「重启」前刚发生的
/// 401/403、429 现场。故 [`crate::server::PersistHandles`] 的四项一个都不能少。
///
/// 落盘复用 [`crate::server::PersistHandles::flush_before_exit`],与 SIGTERM 停机路径共用同一套
/// 规矩(含"磁盘上的 api_keys.json 解析不动且内存里一把 key 都没有时跳过写入"的自保阀),
/// 不在此另起一套平行实现。
async fn flush_persistent_state_before_exit(state: &MessagesState) {
    crate::server::PersistHandles {
        stats: state.stats.clone(),
        balance: state.balance.clone(),
        api_keys: state.api_keys.clone(),
        // 与 `build_router` 同一推断规则(数据目录 = credentials_path 的父目录),
        // 故这里算出的就是本进程 ApiKeyStore 实际读写的那份 api_keys.json。
        api_keys_path: crate::apikey::api_keys_path_from(&state.cfg.credentials_path),
    }
    .flush_before_exit()
    .await;
}

/// POST `/api/admin/restart?confirm=true`:二次确认后退出进程,由容器 `restart` 策略拉起。
///
/// 需 `confirm=true` 防误触(与 gemini2api 同,避免单击即中断可用性)。确认后先返回响应,再由
/// 后台任务延时 0.5s、把去抖存储(统计 + API-KEY,见 [`flush_persistent_state_before_exit`])
/// 全部刷盘后 `exit(0)`——容器以 `restart: unless-stopped` 运行,退出即被重新拉起。
/// 裸机运行(无守护)则等价于停止:此时应由 systemd/supervisor 保活。
pub async fn restart_server(
    State(state): State<MessagesState>,
    Query(q): Query<RestartQuery>,
) -> Response {
    if !q.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": { "message": "重启需二次确认,请带查询参数 ?confirm=true", "type": "confirmation_required" }
            })),
        )
            .into_response();
    }
    tracing::warn!(
        event = "admin_restart",
        "管理员触发重启:0.5s 后刷盘(统计 + API-KEY)并退出进程,交由容器/守护拉起"
    );
    // 整个 state 随任务搬走:退出前要刷的不只是统计,还有 API-KEY 存储(见
    // flush_persistent_state_before_exit)。MessagesState 全是 Arc/克隆廉价的句柄。
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        flush_persistent_state_before_exit(&state).await;
        std::process::exit(0);
    });
    (
        StatusCode::OK,
        Json(RestartResponse {
            status: "ok",
            message: "Server restarting...".to_string(),
        }),
    )
        .into_response()
}

// ==========================================================================
// 系统指标读取(纯 std + /proc,无新增依赖)。全部 best-effort:
// 缺 /proc / 非 Linux / 解析失败一律回落 null 或合理默认,绝不 panic。
// 绝不读取/返回任何 token、secret;仅内核/内存/CPU/PID 等非密系统信息。
// ==========================================================================

/// 本地时间格式化为 'YYYY/MM/DD HH:MM:SS',并返回 unix 秒。chrono 已是依赖,直接用本地时区。
fn local_now_formatted() -> (String, i64) {
    let now = chrono::Local::now();
    (now.format("%Y/%m/%d %H:%M:%S").to_string(), now.timestamp())
}

/// 内核串:读 /proc/sys/kernel/osrelease 加 'Linux ' 前缀;缺失回落 std::env::consts::OS。
fn os_string() -> String {
    match std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        Ok(rel) => {
            let rel = rel.trim();
            if rel.is_empty() {
                std::env::consts::OS.to_string()
            } else {
                format!("Linux {rel}")
            }
        }
        Err(_) => std::env::consts::OS.to_string(),
    }
}

/// 从 /proc/self/status 解析 VmRSS(kB)→ 字节。读不到/无该行 → None。
fn read_process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_kb_line(&status, "VmRSS:").map(|kb| kb * 1024)
}

/// 从 /proc/meminfo 解析 MemTotal(kB)→ 字节。读不到/无该行 → None。
fn read_mem_total_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_kb_line(&meminfo, "MemTotal:").map(|kb| kb * 1024)
}

/// 从 /proc 风格的 'Key: <num> kB' 文本里按前缀取那一行的数值(kB)。
/// 形如 `VmRSS:\t   94208 kB`;找不到前缀或解析失败 → None。抽出以便单测。
fn parse_kb_line(content: &str, prefix: &str) -> Option<u64> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            // rest 形如 "\t   94208 kB";取首个数字 token。
            return rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    None
}

/// /proc/stat 聚合 'cpu ' 行的一次快照:总时钟滴答与空闲时钟滴答。
/// total = user+nice+system+idle+iowait+irq+softirq+steal(忽略 guest,它已计入 user)。
/// idle  = idle+iowait(iowait 期间 CPU 也是空闲的,计入空闲更贴近"忙碌率")。
#[derive(Clone, Copy)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

/// 从 /proc/stat 文本解析聚合 'cpu ' 行(以 "cpu " 开头,不含带编号的 cpu0/cpu1…)。
/// 至少要有 idle(第 4 个数值)才算有效;字段不足/非数值 → None。抽出以便单测。
fn parse_proc_stat_cpu(content: &str) -> Option<CpuTimes> {
    for line in content.lines() {
        // 聚合行是 "cpu " 前缀(带尾随空格),区别于 "cpu0"/"cpu1"。
        if let Some(rest) = line.strip_prefix("cpu ") {
            let vals: Vec<u64> = rest
                .split_whitespace()
                .map(|t| t.parse::<u64>().unwrap_or(0))
                .collect();
            // user nice system idle iowait irq softirq steal ...
            // 至少要有到 idle(索引 3)才有意义。
            if vals.len() < 4 {
                return None;
            }
            let total: u64 = vals.iter().sum();
            let idle = vals[3] + vals.get(4).copied().unwrap_or(0); // idle + iowait
            return Some(CpuTimes { total, idle });
        }
    }
    None
}

fn read_proc_stat_cpu() -> Option<CpuTimes> {
    let content = std::fs::read_to_string("/proc/stat").ok()?;
    parse_proc_stat_cpu(&content)
}

/// 由两次 /proc/stat 快照计算系统级 CPU 忙碌率百分比:
/// busy% = (Δtotal − Δidle) / Δtotal * 100,保留 1 位小数,钳制 [0,100]。
/// Δtotal==0(采样窗口内无滴答变化)→ 视为 0%。抽出以便单测。
fn cpu_busy_percent(t0: CpuTimes, t1: CpuTimes) -> f64 {
    let d_total = t1.total.saturating_sub(t0.total) as f64;
    let d_idle = t1.idle.saturating_sub(t0.idle) as f64;
    if d_total <= 0.0 {
        return 0.0;
    }
    let busy = ((d_total - d_idle) / d_total) * 100.0;
    let clamped = busy.clamp(0.0, 100.0);
    (clamped * 10.0).round() / 10.0
}

/// 系统级 CPU%(对齐 gemini2api):采样 /proc/stat 聚合 'cpu ' 行,睡约 100ms,
/// 再采样,按 (Δtotal−Δidle)/Δtotal*100 计算全机忙碌率,保留 1 位小数、钳制 [0,100]。
/// 读不到 /proc/stat(非 Linux/无 /proc)→ None。用系统级而非本进程,避免"中转多数
/// 时间空闲 → 本进程 CPU 恒 ~0%"看起来像坏了的观感。
///
/// 采样窗口须让出 worker(`tokio::time::sleep`):同步 sleep 会占住整条 tokio 工作线程,
/// 使同线程上的其它请求陪等这 100ms。两次 /proc 读取是极短的内存文件读,不必外抛到阻塞线程池。
async fn sample_process_cpu_percent() -> Option<f64> {
    let t0 = read_proc_stat_cpu()?;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let t1 = read_proc_stat_cpu()?;
    Some(cpu_busy_percent(t0, t1))
}

/// 运行模式检测:存在 /.dockerenv 或 /proc/1/cgroup 含 'docker'/'containerd' → 'Docker',否则 'Bare'。
fn detect_run_mode() -> String {
    if std::path::Path::new("/.dockerenv").exists() {
        return "Docker".to_string();
    }
    if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup")
        && (cgroup.contains("docker") || cgroup.contains("containerd"))
    {
        return "Docker".to_string();
    }
    "Bare".to_string()
}

/// 账号找不到时的统一 404 响应体。
fn not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "account not found", "id": id })),
    )
        .into_response()
}

/// `POST /admin/api/accounts/{id}/enable`:恢复账号参与选择。
pub async fn enable(State(state): State<MessagesState>, Path(id): Path<String>) -> Response {
    let mut pool = state.pool.lock().await;
    let found = pool.set_disabled(&id, false);
    drop(pool);
    if found {
        (
            StatusCode::OK,
            Json(json!({ "ok": true, "id": id, "disabled": false })),
        )
            .into_response()
    } else {
        not_found(&id)
    }
}

/// `POST /admin/api/accounts/{id}/disable`:手动禁用账号(不参与选择)。
pub async fn disable(State(state): State<MessagesState>, Path(id): Path<String>) -> Response {
    let mut pool = state.pool.lock().await;
    let found = pool.set_disabled(&id, true);
    drop(pool);
    if found {
        (
            StatusCode::OK,
            Json(json!({ "ok": true, "id": id, "disabled": true })),
        )
            .into_response()
    } else {
        not_found(&id)
    }
}

// ============ Phase 1 统计只读端点 ============

/// 分页查询参数,`page`/`page_size` 均可选;缺省 page=1、page_size=20。
/// 越界/0 值由存储层 `paginate` 钳制(page 钳到 [1,total_pages],page_size 至少 1)。
#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}

impl PageQuery {
    fn page(&self) -> usize {
        self.page.unwrap_or(1)
    }
    fn page_size(&self) -> usize {
        self.page_size.unwrap_or(20)
    }
}

/// 单条用量记录,camelCase 对齐前端 `UsageRecord`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordView {
    pub model: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub estimated_cost: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits_used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits_saved: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<i32>,
    pub created_at: String,
    pub credential_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,
}

impl From<StoredUsageRecord> for UsageRecordView {
    fn from(r: StoredUsageRecord) -> Self {
        Self::from_with_labels(r, None)
    }
}

impl UsageRecordView {
    /// 由存储记录构造视图,可选传入 `credential_id → 标签` 映射解析 `credential_label`。
    /// `labels` 为 `None`(或该 id 缺失)时标签留空,序列化时按 `skip_serializing_if` 不落字段。
    fn from_with_labels(
        r: StoredUsageRecord,
        labels: Option<&std::collections::HashMap<u32, String>>,
    ) -> Self {
        let credential_label = labels.and_then(|m| m.get(&r.credential_id).cloned());
        UsageRecordView {
            model: r.model,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            estimated_cost: r.estimated_cost,
            credits_used: r.credits_used,
            // Phase 2 占位:Phase 1 存储不产出 creditsSaved,恒为 None(不落字段)。
            credits_saved: None,
            cache_read_input_tokens: r.cache_read_input_tokens,
            cache_creation_input_tokens: r.cache_creation_input_tokens,
            created_at: crate::stats::model::unix_to_rfc3339(r.created_at_unix),
            credential_id: r.credential_id,
            // 凭据标签由 pool 快照解析:昵称 → 邮箱 → "#{id}"(见 credential_label_map)。
            credential_label,
            client_ip: r.client_ip,
        }
    }
}

/// 用量记录分页响应,camelCase 对齐前端 `UsageRecordsResponse`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsResponse {
    pub records: Vec<UsageRecordView>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub total_pages: usize,
}

impl From<Page<StoredUsageRecord>> for UsageRecordsResponse {
    fn from(p: Page<StoredUsageRecord>) -> Self {
        Self::from_page_with_labels(p, None)
    }
}

impl UsageRecordsResponse {
    /// 由分页构造响应,可选传入 `credential_id → 标签` 映射为每条记录解析 `credentialLabel`。
    fn from_page_with_labels(
        p: Page<StoredUsageRecord>,
        labels: Option<&std::collections::HashMap<u32, String>>,
    ) -> Self {
        UsageRecordsResponse {
            records: p
                .items
                .into_iter()
                .map(|r| UsageRecordView::from_with_labels(r, labels))
                .collect(),
            total: p.total,
            page: p.page,
            page_size: p.page_size,
            total_pages: p.total_pages,
        }
    }
}

/// 由 pool 快照构建 `credential_id(u32) → 展示标签` 映射。
/// 标签优先级:昵称 → 邮箱 → `#{id}`(与账号列表展示口径一致)。
/// key 用 [`id_as_u32`] 归一,与用量记录里的数值 `credential_id` 同口径。
async fn credential_label_map(
    pool: &std::sync::Arc<tokio::sync::Mutex<crate::kiro::pool::Pool>>,
) -> std::collections::HashMap<u32, String> {
    let creds = {
        let guard = pool.lock().await;
        guard.snapshot_credentials()
    };
    creds
        .into_iter()
        .map(|c| {
            let numeric = id_as_u32(&c.id);
            let label = c
                .nickname
                .filter(|s| !s.is_empty())
                .or(c.email.filter(|s| !s.is_empty()))
                .unwrap_or_else(|| format!("#{numeric}"));
            (numeric, label)
        })
        .collect()
}

/// 单条失败/限流日志,camelCase 对齐前端 `FailureLogRecord`/`ThrottleLogRecord`(同形)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLogRecordView {
    pub credential_id: u32,
    pub request_type: String,
    pub status_code: u16,
    pub response_body: String,
    pub created_at: String,
}

impl From<FailureEvent> for EventLogRecordView {
    fn from(e: FailureEvent) -> Self {
        EventLogRecordView {
            credential_id: e.credential_id,
            request_type: e.request_type,
            status_code: e.status_code,
            response_body: e.response_body,
            created_at: crate::stats::model::unix_to_rfc3339(e.created_at_unix),
        }
    }
}

impl From<ThrottleEvent> for EventLogRecordView {
    fn from(e: ThrottleEvent) -> Self {
        EventLogRecordView {
            credential_id: e.credential_id,
            request_type: e.request_type,
            status_code: e.status_code,
            response_body: e.response_body,
            created_at: crate::stats::model::unix_to_rfc3339(e.created_at_unix),
        }
    }
}

/// 事件日志分页响应,camelCase 对齐前端 `FailureLogsResponse`/`ThrottleLogsResponse`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLogsResponse {
    pub records: Vec<EventLogRecordView>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub total_pages: usize,
}

fn event_page_to_response<E>(p: Page<E>) -> EventLogsResponse
where
    EventLogRecordView: From<E>,
{
    EventLogsResponse {
        records: p.items.into_iter().map(EventLogRecordView::from).collect(),
        total: p.total,
        page: p.page,
        page_size: p.page_size,
        total_pages: p.total_pages,
    }
}

/// 单凭据当日(CST)用量汇总,camelCase 对齐前端 `CredentialDaySummary`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialDaySummaryView {
    pub date: String,
    pub credential_id: u32,
    pub total_requests: u64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    pub total_credits: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_credits_saved: Option<f64>,
}

/// 全局每日用量汇总,camelCase 对齐前端 `DailySummary`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailySummaryView {
    pub date: String,
    pub total_requests: u64,
    pub total_cost: f64,
    pub total_credits: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_credits_saved: Option<f64>,
}

impl From<DailyRollup> for DailySummaryView {
    fn from(r: DailyRollup) -> Self {
        DailySummaryView {
            date: r.date,
            total_requests: r.total_requests,
            total_cost: r.total_cost,
            total_credits: r.total_credits,
            total_credits_saved: None,
        }
    }
}

/// `GET /api/admin/credentials/{id}/usage/records?page=&page_size=`
/// 某凭据的用量记录分页(降序,最新在前)。未知 id/空存储 → 空页(total=0),不 500。
pub async fn credential_usage_records(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Json<UsageRecordsResponse> {
    let cred = id_as_u32(&id);
    let page = state
        .stats
        .usage
        .records_for_credential(cred, q.page(), q.page_size())
        .await;
    let labels = credential_label_map(&state.pool).await;
    Json(UsageRecordsResponse::from_page_with_labels(
        page,
        Some(&labels),
    ))
}

/// `GET /api/admin/credentials/{id}/usage/today`
/// 某凭据当日(CST/UTC+8)用量汇总。未知 id → 全零汇总(不 500)。
pub async fn credential_today_summary(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
) -> Json<CredentialDaySummaryView> {
    let cred = id_as_u32(&id);
    let now = now_unix() as i64;
    let s: DaySummary = state.stats.usage.today_summary(cred, now).await;
    Json(CredentialDaySummaryView {
        date: crate::stats::model::cst_daykey(now),
        credential_id: cred,
        total_requests: s.total_requests,
        total_input_tokens: s.total_input_tokens,
        total_output_tokens: s.total_output_tokens,
        total_cost: s.total_cost,
        total_credits: s.total_credits,
        total_credits_saved: None,
    })
}

/// `GET /api/admin/credentials/{id}/failure-logs?page=&page_size=`
/// 某凭据 401/403 失败日志分页(降序)。未知 id/空 → 空页(不 500)。
pub async fn credential_failure_logs(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Json<EventLogsResponse> {
    let cred = id_as_u32(&id);
    let page = state
        .stats
        .failure_log
        .records_for_credential(cred, q.page(), q.page_size())
        .await;
    Json(event_page_to_response(page))
}

/// `GET /api/admin/credentials/{id}/throttle-logs?page=&page_size=`
/// 某凭据 429 限流日志分页(降序)。未知 id/空 → 空页(不 500)。
pub async fn credential_throttle_logs(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Json<EventLogsResponse> {
    let cred = id_as_u32(&id);
    let page = state
        .stats
        .throttle_log
        .records_for_credential(cred, q.page(), q.page_size())
        .await;
    Json(event_page_to_response(page))
}

/// `GET /api/admin/usage/daily`
/// 跨全部凭据的每日(CST)用量汇总,按日期降序(最新在前)。空存储 → 空数组(不 500)。
pub async fn daily_usage_summary(
    State(state): State<MessagesState>,
) -> Json<Vec<DailySummaryView>> {
    let rollup = state.stats.usage.daily_rollup().await;
    Json(rollup.into_iter().map(DailySummaryView::from).collect())
}

/// `GET /api/admin/usage/daily/{date}/records?page=&page_size=`
/// 单个 CST 日期的用量记录分页(降序,最多 2000 条)。未知日期/空 → 空页(不 500)。
pub async fn daily_records(
    State(state): State<MessagesState>,
    Path(date): Path<String>,
    Query(q): Query<PageQuery>,
) -> Json<UsageRecordsResponse> {
    let page = state
        .stats
        .usage
        .records_for_day(&date, q.page(), q.page_size())
        .await;
    let labels = credential_label_map(&state.pool).await;
    Json(UsageRecordsResponse::from_page_with_labels(
        page,
        Some(&labels),
    ))
}

// ============ 用量窗口汇总(range summary)============

/// `GET /api/admin/usage/summary` 查询参数。二选一:
/// - `range`:枚举 `6h|24h|3d|7d|30d`(优先)。
/// - `hours`:任意正整数小时数(range 缺省时用它)。
/// 都缺省 → 默认 24h。非法 range → 400。
#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    pub range: Option<String>,
    pub hours: Option<u32>,
}

/// range 枚举 → 窗口秒数;非法值 → None(handler 回 400)。
fn range_to_secs(range: &str) -> Option<i64> {
    match range {
        "6h" => Some(6 * 3600),
        "24h" => Some(24 * 3600),
        "3d" => Some(3 * 86400),
        "7d" => Some(7 * 86400),
        "30d" => Some(30 * 86400),
        _ => None,
    }
}

/// 窗口秒数 → 图表分桶宽度(秒)。短窗(≤ 24h)按小时,长窗按天,便于前端画趋势。
fn bucket_secs_for_window(window_secs: i64) -> i64 {
    if window_secs <= 24 * 3600 {
        3600 // 每小时
    } else {
        86400 // 每天
    }
}

/// 单个图表分桶,camelCase 对齐前端。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucketView {
    /// 桶起始 unix 秒(桶宽见响应 `bucketSecs`)。
    pub bucket_start_unix: i64,
    pub total_requests: u64,
    pub total_cost: f64,
    pub total_credits: f64,
}

impl From<UsageBucket> for UsageBucketView {
    fn from(b: UsageBucket) -> Self {
        UsageBucketView {
            bucket_start_unix: b.bucket_start_unix,
            total_requests: b.total_requests,
            total_cost: b.total_cost,
            total_credits: b.total_credits,
        }
    }
}

/// 窗口用量汇总响应,camelCase 对齐前端。所有数值以 f64/i64 原始精度返回,前端自行格式化。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummaryResponse {
    /// 生效窗口标签(回显规整后的 range,如 "24h";用 hours 时为 "<N>h")。
    pub range: String,
    /// 窗口秒数。
    pub window_secs: i64,
    /// 窗口起始/结束 unix 秒(闭区间;结束 = 当前时刻)。
    pub since_unix: i64,
    pub until_unix: i64,
    /// 图表分桶宽度(秒):短窗 3600(每小时),长窗 86400(每天)。
    pub bucket_secs: i64,
    pub total_requests: u64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    /// 估算成本合计(USD),f64 原始精度未预舍入。
    pub total_cost: f64,
    /// 积分消耗合计,f64 原始精度未预舍入。
    pub total_credits: f64,
    /// 长窗口下是否用每日汇总兜底补齐了原始记录被淘汰的部分(见下)。
    pub daily_fallback_applied: bool,
    /// 时间分桶序列(按桶起始升序),供图表;空窗口 → 空数组。
    pub series: Vec<UsageBucketView>,

    // ===== 运行健康指标(#6)。口径见 usage_summary 文档;皆为"窗口内、尽力而为"的最佳可得值。 =====
    /// 窗口内成功中转的请求数(= 窗口内用量记录条数;每条成功中转落一条)。
    pub successful_requests: u64,
    /// 窗口内失败的请求数(= 失败日志 401/403 + 限流日志 429 在窗口内的条数)。
    /// 注:事件日志按凭据有 LRU 上限,极高频失败下最旧事件可能已淘汰 → 此值为下界(errorRate 偏保守)。
    pub failed_requests: u64,
    /// 错误率 = failedRequests / (successfulRequests + failedRequests)。
    /// 分母为 0(窗口内无任何活动)→ 0.0。范围 [0,1],未预舍入。
    pub error_rate: f64,
    /// 窗口内成功请求的平均端到端延迟(毫秒)= Σlatency_ms / 有延迟样本数。
    /// 仅统计带 latency 的记录(旧记录无 latency 不计入);无样本 → 0.0。
    pub avg_latency_ms: f64,
    /// 轮换成功率 ≈ successfulRequests / (successfulRequests + failedRequests)
    /// = 最终成功送达上游的请求占全部尝试的比例(含跨账号重试后成功的)。
    /// 这是"最终成功 / 总尝试"的近似:跨账号重试链路本身不单独埋点计数,而是以"是否最终落一条成功
    /// 用量记录"为成功信号、以"落一条失败/限流事件"为失败信号来近似。分母 0 → 1.0(无活动视为无异常)。
    pub rotation_success_rate: f64,
}

/// `GET /api/admin/usage/summary?range=<6h|24h|3d|7d|30d>` 或 `?hours=N`
///
/// 跨全部凭据、在时间窗口内聚合原始用量记录,返回**未预舍入**的 f64 精度合计
/// (requests / input+output tokens / estimatedCost / creditsUsed)+ 时间分桶序列。
///
/// **长窗口兜底**:原始记录按凭据有 `USAGE_CAP_PER_CREDENTIAL` 上限,7d/30d 等长窗
/// 下高流量凭据的最旧记录可能已被淘汰,直接对原始记录求和会偏低。为此对**长窗口**
/// (> 1 天)按 CST 日与 `daily_rollup`(每日汇总,不受单条淘汰影响)交叉核对:
/// 对每个落在窗口内的完整 CST 日,取 `max(原始记录该日聚合, 每日汇总该日值)`,
/// 用差额补齐 requests/cost/credits 合计(tokens 无每日汇总,只能给原始值,故长窗
/// tokens 可能偏低——响应 `dailyFallbackApplied=true` 时前端应据此提示)。短窗口
/// (≤ 1 天,原始记录一般未淘汰)不做兜底,直接返回原始精确聚合。
///
/// **运行健康指标(#6)**:在同一窗口内额外计算三项"尽力而为"的指标:
/// - `errorRate` = 失败数 /(成功数 + 失败数)。成功数取窗口内用量记录条数(每条成功中转落一条);
///   失败数取窗口内失败日志(401/403)+ 限流日志(429)条数。事件日志按凭据有 LRU 上限,极高频
///   失败下最旧事件可能已淘汰,故失败数为下界、errorRate 偏保守(不会虚高)。
/// - `avgLatencyMs` = 窗口内成功记录 `latency_ms` 之和 / 有延迟样本数(旧记录无 latency 不计)。
/// - `rotationSuccessRate` ≈ 成功数 /(成功数 + 失败数):以"最终是否落一条成功用量记录"为成功信号,
///   近似"跨账号轮换后最终送达上游的请求占比"。跨账号重试链路本身不单独埋点,故此为近似而非精确
///   计数(诚实标注:精确的"需跨账号重试且仍成功 / 总"需在重试包装处埋点,当前未做)。
///
/// 空存储/空窗口 → 全零 + 空 series(200,不 500)。非法 range → 400。
pub async fn usage_summary(
    State(state): State<MessagesState>,
    Query(q): Query<RangeQuery>,
) -> Response {
    // 解析窗口:range 优先,其次 hours,再缺省 24h。
    let (label, window_secs) = if let Some(r) = q.range.as_deref() {
        match range_to_secs(r) {
            Some(secs) => (r.to_string(), secs),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "invalid range",
                        "allowed": ["6h", "24h", "3d", "7d", "30d"],
                        "hint": "use ?range=<enum> or ?hours=<positive int>"
                    })),
                )
                    .into_response();
            }
        }
    } else if let Some(h) = q.hours {
        if h == 0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "hours must be a positive integer" })),
            )
                .into_response();
        }
        (format!("{h}h"), h as i64 * 3600)
    } else {
        ("24h".to_string(), 24 * 3600)
    };

    let until_unix = now_unix() as i64;
    let since_unix = until_unix - window_secs;
    let bucket_secs = bucket_secs_for_window(window_secs);

    // 原始记录精确聚合 + 分桶。
    let (mut summary, series) = state
        .stats
        .usage
        .range_summary(since_unix, until_unix, bucket_secs)
        .await;

    // 运行健康指标(#6):窗口内失败/限流事件计数(失败率与轮换成功率的分子/分母之一)。
    // 与用量记录同窗口口径;各自尽力而为(事件日志 LRU 上限下为下界,见响应字段文档)。
    let failure_count = state
        .stats
        .failure_log
        .count_in_window(since_unix, until_unix)
        .await;
    let throttle_count = state
        .stats
        .throttle_log
        .count_in_window(since_unix, until_unix)
        .await;

    // 长窗口(> 1 天)用每日汇总兜底补齐被淘汰的旧记录。
    let mut daily_fallback_applied = false;
    if window_secs > 86400 {
        daily_fallback_applied =
            apply_daily_fallback(&state, &mut summary, since_unix, until_unix).await;
    }

    // 平均延迟:仅对带 latency 的成功记录求均值(旧记录无 latency 不计;无样本 → 0)。
    let avg_latency_ms = if summary.latency_sample_count > 0 {
        summary.latency_ms_sum as f64 / summary.latency_sample_count as f64
    } else {
        0.0
    };

    // 成功 = 窗口内用量记录条数(含长窗兜底补齐后的 total_requests);失败 = 401/403 + 429。
    let successful_requests = summary.total_requests;
    let failed_requests = failure_count + throttle_count;
    let attempted = successful_requests + failed_requests;
    // 错误率:无活动 → 0;轮换成功率:无活动 → 1(无异常)。二者互补(在此近似口径下之和为 1)。
    let (error_rate, rotation_success_rate) = if attempted == 0 {
        (0.0, 1.0)
    } else {
        let er = failed_requests as f64 / attempted as f64;
        (er, 1.0 - er)
    };

    Json(UsageSummaryResponse {
        range: label,
        window_secs,
        since_unix,
        until_unix,
        bucket_secs,
        total_requests: summary.total_requests,
        total_input_tokens: summary.total_input_tokens,
        total_output_tokens: summary.total_output_tokens,
        total_cost: summary.total_cost,
        total_credits: summary.total_credits,
        daily_fallback_applied,
        series: series.into_iter().map(UsageBucketView::from).collect(),
        successful_requests,
        failed_requests,
        error_rate,
        avg_latency_ms,
        rotation_success_rate,
    })
    .into_response()
}

/// 长窗口兜底:逐 CST 完整日,把 `daily_rollup` 与原始记录同日聚合比对,按
/// `max(原始, 每日)` 的差额补齐 requests/cost/credits。返回是否实际补齐过。
///
/// 只对**完全落在窗口内的完整 CST 日**兜底(该日 [00:00,24:00) CST 全部 ⊆ 窗口),
/// 避免边界日(窗口只覆盖其一部分)被整日汇总高估。tokens 无每日汇总,不补。
async fn apply_daily_fallback(
    state: &MessagesState,
    summary: &mut RangeSummary,
    since_unix: i64,
    until_unix: i64,
) -> bool {
    let rollup = state.stats.usage.daily_rollup().await;
    if rollup.is_empty() {
        return false;
    }
    // 原始记录同窗口按 CST 日聚合,用于与每日汇总取 max。
    let raw_by_day = state
        .stats
        .usage
        .raw_daily_agg_in_window(since_unix, until_unix)
        .await;
    let mut applied = false;
    for d in &rollup {
        // 该 CST 日的 [00:00, 24:00) CST 对应的 unix 秒区间。
        let Some(day_start_unix) = cst_day_start_unix(&d.date) else {
            continue;
        };
        let day_end_unix = day_start_unix + 86400; // 独占上界
        // 完整日必须 [day_start, day_end) ⊆ [since, until]。
        if day_start_unix < since_unix || day_end_unix - 1 > until_unix {
            continue;
        }
        let (raw_req, raw_cost, raw_credits) =
            raw_by_day.get(&d.date).copied().unwrap_or((0, 0.0, 0.0));
        // 每日汇总 ≥ 原始 → 用差额补齐(原始已计入 summary)。
        if d.total_requests > raw_req {
            summary.total_requests += d.total_requests - raw_req;
            applied = true;
        }
        if d.total_cost > raw_cost {
            summary.total_cost += d.total_cost - raw_cost;
            applied = true;
        }
        if d.total_credits > raw_credits {
            summary.total_credits += d.total_credits - raw_credits;
            applied = true;
        }
    }
    applied
}

/// CST 日键 "YYYY-MM-DD" → 该日 00:00:00 CST 的 unix 秒。解析失败 → None。
fn cst_day_start_unix(date: &str) -> Option<i64> {
    use crate::stats::model::CST_OFFSET_SECS;
    // 把 "YYYY-MM-DD" 当 CST 墙钟 00:00:00 解释:先按 UTC 解析该日 00:00,再减去 CST 偏移。
    let naive = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let midnight_utc = naive.and_hms_opt(0, 0, 0)?.and_utc().timestamp();
    Some(midnight_utc - CST_OFFSET_SECS)
}

/// 内部 String id → 统计层数值 credential_id(u32)。非数值/负值回落 0,
/// 对应空结果(未知凭据优雅返回空页,不 500)。
fn id_as_u32(id: &str) -> u32 {
    id.parse::<u32>().unwrap_or(0)
}

// ============ Phase 4 余额端点 ============

/// 余额响应,camelCase 对齐前端 `BalanceResponse`。
/// `usageLimit`/`remaining`/`currentUsage` 单位为积分(credits)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceView {
    pub id: i64,
    pub subscription_title: Option<String>,
    pub current_usage: f64,
    pub usage_limit: f64,
    pub remaining: f64,
    pub usage_percentage: f64,
    pub next_reset_at: Option<i64>,
}

impl BalanceView {
    fn from_snapshot(id: &str, s: &crate::balance::BalanceSnapshot) -> Self {
        BalanceView {
            id: id_as_number(id),
            subscription_title: s.subscription_title.clone(),
            current_usage: s.current_usage,
            usage_limit: s.usage_limit,
            remaining: s.remaining,
            usage_percentage: s.usage_percentage,
            next_reset_at: s.next_reset_at,
        }
    }
}

/// `GET /api/admin/credentials/{id}/balance`
/// 单个凭据的上游剩余额度(getUsageLimits)。命中 5 分钟 TTL 缓存直接返回;
/// miss/过期则实拉上游、归约成快照回填缓存再返回。
///
/// 存在性判定走**内存池**(令牌由 `ensure_fresh` 从活池取,本模块不外泄也不改池)。
/// 未知 id → 404;上游失败 → 502(不落缓存,前端"全局积分"侧静默跳过该项)。
pub async fn credential_balance(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
) -> Response {
    let now = now_unix();
    // 先查缓存
    if let Some(snap) = state.balance.get_fresh(&id, now).await {
        return Json(BalanceView::from_snapshot(&id, &snap)).into_response();
    }
    // miss/过期:先确认该 id 存在,保持"未知 id → 404"的既有语义。
    //
    // 这里**不读盘**:`credential::load` 是同步 `std::fs::read_to_string` + 全量 serde 解析,
    // 直接跑在 Tokio worker 上阻塞调度;而仪表盘"全局积分"会给每个账号并发打一次本端点,
    // 冷缓存时等于把 MB 级 credentials.json 读+解析上千遍。改判内存池:池是凭据的权威副本
    // (admin 增删改一律先落池再落盘),`rpm_of` 对未知 id 返回 None、对已禁用账号照样返回
    // Some,与旧的磁盘全量判定等价,且零克隆零 I/O。顺带修掉一个真 bug:旧写法
    // `load(..).unwrap_or_default()` 会把"文件读失败/解析失败"吞成空列表,于是所有账号的
    // 余额查询统统误报 404。
    let exists = {
        let pool = state.pool.lock().await;
        pool.rpm_of(&id, now).is_some()
    };
    if !exists {
        return not_found(&id);
    }
    // 集中保鲜实拉:ensure_fresh 从活池取凭据、即将过期则刷新并写回池(令牌与 relay/models
    // 共享,避免各自反复轮换令牌导致的级联 403),再用刷新后的凭据调 getUsageLimits。
    match crate::balance::fetch_usage_limits_fresh(
        &state.control_client,
        &state.cfg,
        &state.pool,
        &id,
        now,
        Some(&state.refresh_ctx),
    )
    .await
    {
        Ok(resp) => {
            let snap = crate::balance::BalanceSnapshot::from_response(&resp, now);
            // 顺手把上游给的**真实**配额恢复时刻记进池并落盘:比按月估的准。
            // 额度已用尽时才记 —— 还有额度的账号不该被打上"等到某时才可用"的标记。
            if snap.remaining <= 0.0
                && let Some(reset) = snap.next_reset_at
                && reset > now as i64
            {
                {
                    let mut pool = state.pool.lock().await;
                    pool.set_quota_reset(&id, reset as u64);
                }
                if let Err(e) = persist_pool_credentials(&state).await {
                    tracing::warn!(error = %e, "配额恢复时刻落盘失败");
                }
            }
            state.balance.put(id.clone(), snap.clone()).await;
            Json(BalanceView::from_snapshot(&id, &snap)).into_response()
        }
        Err(e) => {
            // 数据面余额拉取失败记 WARN(带账号 id + 状态码 + 上游短说明,绝不含令牌),
            // 让「查看日志」页看得到活动;detail 里透出 BalanceError 的 Display(含真因)。
            tracing::warn!("账号 {id} 余额刷新失败: {e}");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": "failed to fetch balance", "id": id, "detail": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// `GET /api/admin/credits/global` 响应,camelCase 对齐前端 `GlobalCreditsResponse`。
///
/// 纯读共享 `BalanceCache`:对池内每个凭据取仍新鲜(5 分钟 TTL)的缓存快照,
/// 累加 `remaining` 得全局剩余积分。**绝不触发上游**——缓存 miss/过期的账号
/// 直接跳过(由账号页自动查询/仪表盘手动刷新去回填缓存)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalCreditsResponse {
    /// 命中新鲜缓存的各账号 `remaining` 之和。
    pub global_credits: f64,
    /// 命中新鲜缓存的账号数。
    pub cached_count: u32,
    /// 池内账号总数。
    pub total_count: u32,
    /// 参与求和的缓存条目里最旧的抓取时刻(Unix 秒),供"更新于 X 前"展示;
    /// 无任何命中时为 None。
    pub oldest_cache_unix: Option<u64>,
}

/// `GET /api/admin/credits/global`
/// 全局剩余积分聚合(**只读缓存,零上游**):遍历池内凭据 id,仅读各自仍新鲜的
/// `BalanceCache` 条目,累加 `remaining`;缓存 miss/过期的账号跳过(不实拉)。
/// 返回 `{globalCredits, cachedCount, totalCount, oldestCacheUnix}`。
pub async fn global_credits(State(state): State<MessagesState>) -> Json<GlobalCreditsResponse> {
    let ids: Vec<String> = {
        let pool = state.pool.lock().await;
        pool.snapshot_credentials()
            .into_iter()
            .map(|c| c.id)
            .collect()
    };
    let total_count = ids.len() as u32;

    // 展示取**全部**缓存条目,不按 TTL 过滤:TTL 决定"要不要重查上游",不该决定
    // "要不要显示"。此前按新鲜度过滤,导致超过 5 分钟没打开账号页首页就一片空白,
    // 用户只能点刷新 —— 那次刷新正是这份缓存本该避免的上游调用。
    // 年龄由 `oldestCacheUnix` 带给前端,由界面说清数据有多旧。
    let cached = state.balance.get_any_for_ids(&ids).await;
    let cached_count = cached.len() as u32;
    let global_credits: f64 = cached.iter().map(|(_, s)| s.remaining).sum();
    let oldest_cache_unix = cached.iter().map(|(_, s)| s.fetched_at_unix).min();

    Json(GlobalCreditsResponse {
        global_credits,
        cached_count,
        total_count,
        oldest_cache_unix,
    })
}

// ============ Phase 2 API-KEY 管理端点 ============

use crate::apikey::ApiKey;
use crate::stats::usage::{ApiKeyUsageSummary, ModelUsageAgg};

/// RFC3339(UTC、秒精度、Z 后缀)字符串化。
fn dt_to_rfc3339(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 单个 API-KEY 视图,camelCase 对齐前端 `ApiKeyItem`。
///
/// 契约要求:`enabled/createdAt/expiresAt/spendingLimit/limitUnit/durationDays/activatedAt`
/// 均为**非可选**(nullable 者以 `null` 显式出现),故这些字段不做 `skip_serializing_if`。
/// `boundCredentialIds` 前端为可选(`number[]?`),None 时省略。
/// `key` 出完整明文——前端列表卡片自行 `maskKey` 脱敏、复制按钮取完整值,契约如此。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyView {
    pub id: u32,
    pub key: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub spending_limit: Option<f64>,
    pub limit_unit: String,
    pub duration_days: Option<f64>,
    pub activated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_credential_ids: Option<Vec<u64>>,
}

impl From<ApiKey> for ApiKeyView {
    fn from(k: ApiKey) -> Self {
        ApiKeyView {
            id: k.id,
            key: k.key,
            name: k.name,
            enabled: k.enabled,
            created_at: dt_to_rfc3339(k.created_at),
            expires_at: k.expires_at.map(dt_to_rfc3339),
            spending_limit: k.spending_limit,
            limit_unit: k.limit_unit,
            duration_days: k.duration_days,
            activated_at: k.activated_at.map(dt_to_rfc3339),
            bound_credential_ids: k.bound_credential_ids,
        }
    }
}

/// 单模型用量明细,camelCase 对齐前端 `ModelUsage`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageView {
    pub model: String,
    pub requests: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
}

impl From<ModelUsageAgg> for ModelUsageView {
    fn from(m: ModelUsageAgg) -> Self {
        ModelUsageView {
            model: m.model,
            requests: m.requests,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            cost: m.cost,
        }
    }
}

/// 单 API-KEY 用量汇总视图,camelCase 对齐前端 `UsageSummary`。
/// `totalCredits` 按 `totalCost / 0.72` 换算(与前端 detail 页一致);`totalCreditsSaved`
/// 当前无数据源,恒省略。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyUsageView {
    pub api_key_id: u32,
    pub total_requests: u64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    pub total_credits: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_credits_saved: Option<f64>,
    pub by_model: Vec<ModelUsageView>,
}

impl From<ApiKeyUsageSummary> for ApiKeyUsageView {
    fn from(s: ApiKeyUsageSummary) -> Self {
        ApiKeyUsageView {
            api_key_id: s.api_key_id,
            total_requests: s.total_requests,
            total_input_tokens: s.total_input_tokens,
            total_output_tokens: s.total_output_tokens,
            total_cost: s.total_cost,
            // 用上游回报的真实 credits,不再由 cost 反算。
            // 反算出来的是另一个量纲的数字,与账单无关:实测同一把 key,反算得 0.0037,
            // 真实消耗约 1.37 —— 面板据此画成 0.00/2.00,而额度其实已用掉七成。
            total_credits: s.total_credits,
            total_credits_saved: None,
            by_model: s.by_model.into_iter().map(ModelUsageView::from).collect(),
        }
    }
}

/// `POST /api/admin/api-keys` 请求体,对齐前端 `CreateApiKeyRequest`。
/// 可空字段用 `Option<Option<_>>` 以区分"缺省"与"显式 null":前端可能传 `null`
/// (如 `boundCredentialIds: null`),内层 None 表示不设值。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyRequest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub spending_limit: Option<f64>,
    #[serde(default)]
    pub limit_unit: Option<String>,
    #[serde(default)]
    pub duration_days: Option<f64>,
    #[serde(default)]
    pub bound_credential_ids: Option<Vec<u64>>,
}

/// `PUT /api/admin/api-keys/{id}` 请求体,对齐前端 `UpdateApiKeyRequest`。
/// 用 `#[serde(default, deserialize_with)]` 三态:字段缺省=不动;出现即"要改"
/// (含显式 null=清空)。以 `Option<Option<_>>` 承载。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateApiKeyRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "double_option", skip_serializing)]
    pub expires_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing)]
    pub spending_limit: Option<Option<f64>>,
    #[serde(default)]
    pub limit_unit: Option<String>,
    #[serde(default, deserialize_with = "double_option", skip_serializing)]
    pub duration_days: Option<Option<f64>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing)]
    pub bound_credential_ids: Option<Option<Vec<u64>>>,
}

/// serde 三态辅助:字段缺省 → 外层 None(不动);字段出现(含 null)→ 外层 Some,
/// 内层为实际值或 None(清空)。
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// RFC3339 字符串 → `DateTime<Utc>`;解析失败回落 None(不 500,视为"未设")。
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// 当前 UTC 时刻(创建/激活时间基准)。
fn now_utc() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

/// API-KEY 未找到时的统一 404。
fn apikey_not_found(id: u32) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "api key not found", "id": id })),
    )
        .into_response()
}

/// `GET /api/admin/api-keys`:全部 API-KEY 列表(camelCase 数组;含完整 key 明文,
/// 前端自行脱敏)。空 → `[]`。
pub async fn list_api_keys(State(state): State<MessagesState>) -> Json<Vec<ApiKeyView>> {
    let keys = state.api_keys.list();
    Json(keys.into_iter().map(ApiKeyView::from).collect())
}

/// `POST /api/admin/api-keys`:创建一个新 key,返回新建项(含完整明文 key,供前端一次性复制)。
pub async fn create_api_key(
    State(state): State<MessagesState>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Json<ApiKeyView> {
    let expires_at = req.expires_at.as_deref().and_then(parse_rfc3339);
    let created = state.api_keys.create(
        req.name,
        expires_at,
        req.spending_limit,
        req.limit_unit,
        req.duration_days,
        req.bound_credential_ids,
        now_utc(),
    );
    Json(ApiKeyView::from(created))
}

/// `PUT /api/admin/api-keys/{id}`:局部更新;未知 id → 404(不 500)。返回更新后的项。
pub async fn update_api_key(
    State(state): State<MessagesState>,
    Path(id): Path<u32>,
    Json(req): Json<UpdateApiKeyRequest>,
) -> Response {
    // 三态映射:外层 Some 表示"要改"、内层字符串再解析成时刻。
    let expires_at = req
        .expires_at
        .map(|inner| inner.and_then(|s| parse_rfc3339(&s)));
    let updated = state.api_keys.update(
        id,
        req.name,
        req.enabled,
        expires_at,
        req.spending_limit,
        req.limit_unit,
        req.duration_days,
        req.bound_credential_ids,
    );
    match updated {
        Some(k) => Json(ApiKeyView::from(k)).into_response(),
        None => apikey_not_found(id),
    }
}

/// `DELETE /api/admin/api-keys/{id}`:删除;未知 id → 404。返回 `{success,message}`。
pub async fn delete_api_key(State(state): State<MessagesState>, Path(id): Path<u32>) -> Response {
    if state.api_keys.delete(id) {
        Json(SuccessResponse {
            success: true,
            message: "api key deleted".into(),
        })
        .into_response()
    } else {
        apikey_not_found(id)
    }
}

/// `GET /api/admin/api-keys/usage`:全部出现过的 API-KEY 用量汇总数组。空 → `[]`(不 500)。
pub async fn all_api_key_usage(State(state): State<MessagesState>) -> Json<Vec<ApiKeyUsageView>> {
    let summaries = state.stats.get_summaries_by_api_key().await;
    Json(summaries.into_iter().map(ApiKeyUsageView::from).collect())
}

/// `GET /api/admin/api-keys/{id}/usage`:单 key 用量汇总。未知/无记录 id → 全零汇总(不 500)。
pub async fn api_key_usage(
    State(state): State<MessagesState>,
    Path(id): Path<u32>,
) -> Json<ApiKeyUsageView> {
    let summary = state.stats.get_summary_by_api_key(id).await;
    Json(ApiKeyUsageView::from(summary))
}

/// `DELETE /api/admin/api-keys/{id}/usage`:清空单 key 用量记录。返回 `{success,message}`。
/// 未知 id / 无记录 → 仍 200 success(幂等清空,不 500/404)。
pub async fn reset_api_key_usage(
    State(state): State<MessagesState>,
    Path(id): Path<u32>,
) -> Json<SuccessResponse> {
    let removed = state.stats.reset_by_api_key(id).await;
    Json(SuccessResponse {
        success: true,
        message: format!("cleared {removed} usage record(s)"),
    })
}

/// `GET /api/admin/api-keys/{id}/usage/records?page=&page_size=`:单 key 用量记录分页(降序)。
/// 未知 id / 空 → 空页(total=0),不 500。复用既有 `UsageRecordsResponse`(camelCase)。
pub async fn api_key_usage_records(
    State(state): State<MessagesState>,
    Path(id): Path<u32>,
    Query(q): Query<PageQuery>,
) -> Json<UsageRecordsResponse> {
    let page = state
        .stats
        .get_records_by_api_key(id, q.page(), q.page_size())
        .await;
    let labels = credential_label_map(&state.pool).await;
    Json(UsageRecordsResponse::from_page_with_labels(
        page,
        Some(&labels),
    ))
}

/// 实时 RPM 快照,camelCase 对齐前端 `RpmSnapshot`。
/// `global` = 全池当前最大单账号 RPM;`by_credential` = 账号 id(字符串)→ 窗口内 RPM;
/// `by_api_key` = API-KEY id(字符串)→ 窗口内 RPM。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpmSnapshotView {
    pub global: u32,
    pub by_credential: std::collections::HashMap<String, u32>,
    pub by_api_key: std::collections::HashMap<String, u32>,
}

/// `GET /api/admin/rpm`:每账号 + 每 API-KEY 的实时 RPM(近 60s)只读快照。
///
/// - `by_credential` 取池的 `rpm_all(now)`(与 relay 数据面共享同一 `Arc<Mutex<Pool>>`),
///   即近 60s 内该账号被 select 选中的次数;`global` 取其最大值。
/// - `by_api_key` 取统计层的 `rpm_by_api_key(now)`,即近 60s 内归属该 key 的用量记录条数。
///   池只认账号、不认 key,故 key 维度只能从账本的时刻走——两者同为 60s 滑动窗口,数值可对照,
///   仅计数时点不同(池在选中账号时计,账本在请求跑完落库时计)。
///   无归属记录(api_key_id=0,即用全局 key 直连的流量)不计入,窗口内没跑过的 key 不出现。
///
/// 这一维度不能再留空:API-KEY 页每张卡片的 "RPM x" 直接读本字段(前端
/// `sec-apikeys.js` 的 `byApiKey`),留空就等于所有 key 永远显示 RPM 0。
pub async fn rpm_snapshot(State(state): State<MessagesState>) -> Json<RpmSnapshotView> {
    let now = now_unix();
    let pool = state.pool.lock().await;
    let per = pool.rpm_all(now);
    drop(pool);

    let global = per.iter().map(|(_, r)| *r).max().unwrap_or(0);
    let by_credential: std::collections::HashMap<String, u32> = per.into_iter().collect();
    // 前端按 String(key.id) 取值,故这里也以字符串 id 为键(与 by_credential 一致)。
    let by_api_key: std::collections::HashMap<String, u32> = state
        .stats
        .rpm_by_api_key(now as i64)
        .await
        .into_iter()
        .map(|(id, rpm)| (id.to_string(), rpm))
        .collect();

    Json(RpmSnapshotView {
        global,
        by_credential,
        by_api_key,
    })
}

// ============ Phase 3 凭据 CRUD + 活池变更 + 持久化 ============

use crate::kiro::credential::{AuthMethod, Credential};
use crate::kiro::pool::CredentialUpdate;

/// `POST /api/admin/credentials` 请求体,对齐前端 `AddCredentialRequest`(camelCase)。
/// proxy* 字段自 v0.10.1 起**真正落库并生效**(此前是"收到即丢",`hasProxy` 恒 false),
/// 故接收但不持久化——保持契约兼容、不新增存储面。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialRequest {
    #[serde(default)]
    pub refresh_token: String,
    /// Kiro API Key(`ksk_…`)。给了它就不需要 `refreshToken`:这类凭据不换令牌,
    /// key 本身即数据面 bearer。
    #[serde(default, alias = "ksk")]
    pub kiro_api_key: Option<String>,
    #[serde(default)]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub profile_arn: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub weight: Option<u32>,
    #[serde(default)]
    pub auth_region: Option<String>,
    #[serde(default)]
    pub api_region: Option<String>,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub proxy_username: Option<String>,
    #[serde(default)]
    pub proxy_password: Option<String>,
}

/// `POST /api/admin/credentials` 响应体,对齐前端 `AddCredentialResponse`(camelCase)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialResponse {
    pub success: bool,
    pub message: String,
    pub credential_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// 该 refreshToken 已存在池中 → 未新增(去重跳过);`credential_id` 为既有账号 id。
    #[serde(default)]
    pub duplicate: bool,
}

/// `PUT /api/admin/credentials/{id}` 请求体,对齐前端 `UpdateCredentialRequest`。
/// 字段缺省=不改;proxy* 同 Add 说明(现已落库生效)。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCredentialRequest {
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub profile_arn: Option<String>,
    #[serde(default)]
    pub weight: Option<u32>,
    #[serde(default)]
    pub auth_region: Option<String>,
    #[serde(default)]
    pub api_region: Option<String>,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub proxy_username: Option<String>,
    #[serde(default)]
    pub proxy_password: Option<String>,
}

/// `POST /api/admin/credentials/{id}/priority` 请求体,对齐前端 `SetPriorityRequest`。
#[derive(Debug, Deserialize)]
pub struct SetPriorityRequest {
    pub priority: i64,
}

/// 400 统一响应体。
fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "success": false, "error": msg })),
    )
        .into_response()
}

/// 持久化失败统一 500(活池已变更但落盘失败,提示调用方重试)。
fn persist_failed(e: &anyhow::Error) -> Response {
    tracing::error!("持久化 credentials.json 失败: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "success": false, "error": "persist failed" })),
    )
        .into_response()
}

/// `authMethod` 串 → 枚举;缺省/未知回落 Social(与前端可选默认一致)。
/// `"idc"`(大小写不敏感)→ Idc,其余 → Social。
fn parse_auth_method(s: Option<&str>) -> AuthMethod {
    match s {
        Some(v) if v.eq_ignore_ascii_case("idc") => AuthMethod::Idc,
        _ => AuthMethod::Social,
    }
}

/// **可复用核心**:把一个已构造好的 `Credential` 加入活池并原子落盘 credentials.json。
/// 登录流(下一阶段)完成换取 token 后也调用此函数落库,保持"活池 + 持久化"单一入口。
///
/// 入参 `cred.id` 会被池忽略并重新分配数值 id。返回 `(新数值 id, email)` 供响应;
/// 落盘失败以 `Err` 返回(调用方转 500)。
///
/// #13(lost update)修复:先在 pool 锁临界区内改池(add),随后经
/// [`persist_pool_credentials`] 在共享 `persist_lock` 下重新快照 + 原子落盘。所有凭据
/// 落盘(admin CRUD + 刷新写回)共用同一把锁,序列化写盘、且各自落最新池状态,消除
/// 「慢的旧快照覆盖新状态」竞态。
/// 入活池 + 落盘,**自带去重**:池锁内先查同 `refresh_token` 是否已存在(原子,防 TOCTOU),
/// 已存在则不重复添加、不落盘,返回既有 id + `is_duplicate=true`;新增则返回新 id + `false`。
/// 返回元组第三位为「是否重复」。
pub async fn add_credential_to_pool_and_persist(
    state: &MessagesState,
    cred: Credential,
) -> anyhow::Result<(String, Option<String>, bool)> {
    let (id, email, is_duplicate) = {
        let mut pool = state.pool.lock().await;
        match pool.find_id_by_refresh_token(&cred.refresh_token) {
            Some(existing_id) => (existing_id, cred.email.clone(), true),
            None => {
                let (id, email) = pool.add_credential(cred);
                (id, email, false)
            }
        }
    };
    // 仅新增才落盘;重复导入不改动池,无需写盘。
    if !is_duplicate {
        persist_pool_credentials(state).await?;
    }
    Ok((id, email, is_duplicate))
}

/// 经共享 `persist_lock` 序列化落盘活池凭据。全部 admin 凭据写盘的单一入口,与刷新路径
/// (`ensure_fresh::persist_pool_credentials`)共用 `state.refresh_ctx.persist_lock`,故
/// admin 之间、admin 与刷新之间的落盘互斥、不交错、各落最新快照(见 #13 修复说明)。
async fn persist_pool_credentials(state: &MessagesState) -> anyhow::Result<()> {
    crate::kiro::credential::persist_pool_credentials_serialized(
        &state.pool,
        &state.refresh_ctx.persist_lock,
        &state.refresh_ctx.credentials_path,
    )
    .await
}

/// `POST /api/admin/credentials`:新增单个凭据 → 入活池 → 落盘。
/// 校验:`refreshToken` 非空;`authMethod=idc` 时 `clientId`+`clientSecret` 必填。
pub async fn add_credential(
    State(state): State<MessagesState>,
    Json(req): Json<AddCredentialRequest>,
) -> Response {
    // 两种导入方式二选一:refreshToken(OAuth)或 kiroApiKey(ksk)。
    // ksk 凭据不换令牌,强求 refreshToken 会把这类账号挡在门外。
    let ksk = req.kiro_api_key.filter(|s| !s.trim().is_empty());
    if req.refresh_token.trim().is_empty() && ksk.is_none() {
        return bad_request("refreshToken or kiroApiKey is required");
    }
    // 给了 ksk 即按 API Key 凭据处理,不看 authMethod 写了什么:显式声明 idc 却带着 ksk
    // 会走进"必填 clientId/clientSecret"的校验,把一个本来完整的凭据判成缺字段。
    let auth = if ksk.is_some() {
        AuthMethod::ApiKey
    } else {
        parse_auth_method(req.auth_method.as_deref())
    };
    if auth == AuthMethod::Idc
        && (req.client_id.as_deref().unwrap_or("").trim().is_empty()
            || req.client_secret.as_deref().unwrap_or("").trim().is_empty())
    {
        return bad_request("clientId and clientSecret are required for idc auth");
    }

    // region:优先 apiRegion(数据面),回落 authRegion,再回落 us-east-1。
    let region = req
        .api_region
        .clone()
        .or_else(|| req.auth_region.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "us-east-1".to_string());

    // weight:优先显式 weight,回落 priority(下限 1),再回落 1。
    let weight = req
        .weight
        .or_else(|| req.priority.map(|p| if p < 1 { 1 } else { p as u32 }))
        .unwrap_or(1);

    let cred = Credential {
        quota_reset_unix: None,
        id: String::new(), // 池分配
        access_token: String::new(),
        refresh_token: req.refresh_token,
        kiro_api_key: ksk,
        expires_at_unix: 0, // 首次刷新时补齐(ApiKey 无此概念,恒不判过期)
        region,
        auth,
        client_id: req.client_id.filter(|s| !s.is_empty()),
        client_secret: req.client_secret.filter(|s| !s.is_empty()),
        profile_arn: req.profile_arn.filter(|s| !s.is_empty()),
        machine_id: req.machine_id.filter(|s| !s.is_empty()),
        email: req.email.filter(|s| !s.is_empty()),
        nickname: req.nickname.filter(|s| !s.is_empty()),
        weight,
        // 选号优先级:**导入一律 999(最低)**,显式给了就用给的。
        // 默认不猜由谁先顶上——那是运营决策,交给运营者显式设定(面板可改)。
        priority: req
            .priority
            .map(|p| p.clamp(0, u32::MAX as i64) as u32)
            .unwrap_or(crate::kiro::credential::DEFAULT_PRIORITY),
        label: None,
        disabled: false,
        status_reason: None,
        // 代理三件套**真正落库**。此前这里是"收下即丢",而状态里 `hasProxy` 还硬编码
        // `false` —— 用户在面板上填了代理、界面显示保存成功,实际全程直连。
        proxy_url: req.proxy_url.filter(|s| !s.is_empty()),
        proxy_username: req.proxy_username.filter(|s| !s.is_empty()),
        proxy_password: req.proxy_password.filter(|s| !s.is_empty()),
    };

    match add_credential_to_pool_and_persist(&state, cred).await {
        Ok((id, email, is_duplicate)) => Json(AddCredentialResponse {
            success: true,
            message: if is_duplicate {
                "credential already exists".into()
            } else {
                "credential added".into()
            },
            credential_id: id_as_number(&id),
            email,
            duplicate: is_duplicate,
        })
        .into_response(),
        Err(e) => persist_failed(&e),
    }
}

/// `PUT /api/admin/credentials/{id}`:局部更新可变字段 → 落盘。未知 id → 404。
pub async fn update_credential(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateCredentialRequest>,
) -> Response {
    // authMethod 若提供则解析(仅 idc/social);其余非空字符串字段进 CredentialUpdate。
    let auth = req
        .auth_method
        .as_deref()
        .map(|s| parse_auth_method(Some(s)));
    // region:提供 apiRegion 或 authRegion 时更新(apiRegion 优先)。
    let region = req
        .api_region
        .clone()
        .or_else(|| req.auth_region.clone())
        .filter(|s| !s.trim().is_empty());

    let upd = CredentialUpdate {
        refresh_token: req.refresh_token.filter(|s| !s.is_empty()),
        auth,
        email: req.email.filter(|s| !s.is_empty()),
        nickname: req.nickname.filter(|s| !s.is_empty()),
        client_id: req.client_id.filter(|s| !s.is_empty()),
        client_secret: req.client_secret.filter(|s| !s.is_empty()),
        profile_arn: req.profile_arn.filter(|s| !s.is_empty()),
        machine_id: req.machine_id.filter(|s| !s.is_empty()),
        weight: req.weight,
        region,
    };

    let found = {
        let mut pool = state.pool.lock().await;
        pool.update_credential(&id, upd)
    };
    if !found {
        return not_found(&id);
    }
    // #13:经共享 persist_lock 重新快照 + 原子落盘(与刷新/其它 admin 写盘序列化)。
    if let Err(e) = persist_pool_credentials(&state).await {
        return persist_failed(&e);
    }
    Json(SuccessResponse {
        success: true,
        message: "credential updated".into(),
    })
    .into_response()
}

/// `DELETE /api/admin/credentials/{id}`:从活池移除 → 落盘 → 清掉该 id 的全部按 id 键控的
/// 残留状态(余额缓存、模型缓存、用量记录、失败/限流事件)。未知 id → 404。
///
/// 这些状态一律以凭据 id 为键,而 id 会被后续新增凭据复用:账号编号高水位存在旁挂文件里,
/// 它写失败(best-effort)、缺失(从旧版本升级、从备份还原 credentials.json、drop-in 一份
/// 原生 credentials.json、只搬 JSON 不搬旁挂文件)时,重启后编号退回 `max(现有 id)+1`,
/// 于是刚删掉的最大号会被下一个新账号原样领走。不清就等于新账号一上线就继承前任的余额、
/// 模型清单、用量与消费历史、失败/限流明细——统计与计费跨账号串味,弹窗里凭空多出别人的
/// 上游报错。缓存/记录都是本地状态,清理不触上游。
///
/// 另有一层与 id 复用无关、无条件成立的理由:不清的话已删账号的用量记录永远留在账本里,
/// 继续占着每凭据记录上限、继续出现在聚合统计里,而面板上已经没有这个账号了。
pub async fn delete_credential(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
) -> Response {
    let found = {
        let mut pool = state.pool.lock().await;
        pool.remove_credential(&id)
    };
    if !found {
        return not_found(&id);
    }
    // #13:经共享 persist_lock 重新快照 + 原子落盘(与刷新/其它 admin 写盘序列化)。
    if let Err(e) = persist_pool_credentials(&state).await {
        return persist_failed(&e);
    }
    state.balance.invalidate(&id).await;
    state.models_cache.invalidate(&id).await;
    // 统计层按数值 id 键控:`id_as_u32` 对非数值 id 回落 0,而 0 是"无归属"这一整桶
    // (relay 拿不到数值账号 id 时就落 0),绝不能拿它去清——那会误删别的账号的记录。
    let numeric_id = id_as_u32(&id);
    if numeric_id != 0 {
        let purged = state.stats.purge_credential(numeric_id).await;
        if purged > 0 {
            tracing::info!(
                credential_id = numeric_id,
                purged_usage_records = purged,
                "已随账号删除清理其用量记录与失败/限流事件(防编号复用后新账号继承前任历史)"
            );
        }
    }
    Json(SuccessResponse {
        success: true,
        message: "credential deleted".into(),
    })
    .into_response()
}

/// `POST /api/admin/credentials/{id}/priority`:设置优先级(映射 weight,下限 1)→ 落盘。
/// 未知 id → 404。
pub async fn set_credential_priority(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
    Json(req): Json<SetPriorityRequest>,
) -> Response {
    let found = {
        let mut pool = state.pool.lock().await;
        pool.set_priority(&id, req.priority)
    };
    if !found {
        return not_found(&id);
    }
    // #13:经共享 persist_lock 重新快照 + 原子落盘(与刷新/其它 admin 写盘序列化)。
    if let Err(e) = persist_pool_credentials(&state).await {
        return persist_failed(&e);
    }
    Json(SuccessResponse {
        success: true,
        message: "priority updated".into(),
    })
    .into_response()
}

/// `POST /api/admin/credentials/{id}/reset`:清零失败计数并解冷却(纯瞬态,不落盘)。
/// 未知 id → 404。
pub async fn reset_credential_failure(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
) -> Response {
    let found = {
        let mut pool = state.pool.lock().await;
        pool.reset_failures(&id)
    };
    if !found {
        return not_found(&id);
    }
    // 必须立刻落盘。重置会清掉「封禁」这个**持久**结论,而封禁账号被挡在池外、永远等不到
    // 一次成功来清它——重置是它唯一的出口。只改内存的话,下次重启会从盘上把封禁读回来,
    // 账号又被挡住,而运维明明已经点过重置了。
    if let Err(e) = persist_pool_credentials(&state).await {
        tracing::warn!(error = %e, "重置后落盘失败");
    }
    Json(SuccessResponse {
        success: true,
        message: "failure count reset".into(),
    })
    .into_response()
}

/// `POST /api/admin/credentials/{id}/refresh` —— 立刻强制换发一枚新令牌。
///
/// 用途:回答"这个账号的 refreshToken 还活着吗"。此前只能等它自然过期,或发一次真实请求
/// 去撞 —— 而后者会把一个可能已经失效的账号推到数据面上。`force_refresh` 早就写好了,
/// 只是从来没接过路由。
///
/// 出口按该账号自己的代理取,与它的数据面一致。
pub async fn refresh_credential_token(
    State(state): State<MessagesState>,
    Path(id): Path<String>,
) -> Response {
    let snapshot = {
        let pool = state.pool.lock().await;
        pool.snapshot_credentials().into_iter().find(|c| c.id == id)
    };
    let Some(cred) = snapshot else {
        return not_found(&id);
    };
    // API Key 凭据没有可换的东西(ksk 本身就是数据面 bearer),明确说清而不是去打一个
    // 注定失败的端点。
    if cred.is_api_key() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(SuccessResponse {
                success: false,
                message: "API Key 凭据不刷新:ksk 本身就是数据面令牌".into(),
            }),
        )
            .into_response();
    }
    let now = now_unix();
    // 传当前 access_token 作双检基线:池内若已被别人换过,直接复用而不再轮换一次
    //(轮换会作废他人正在用的令牌)。
    let r = crate::kiro::ensure_fresh::force_refresh(
        &state.pool,
        &id,
        &crate::http::unary_for(&cred),
        now,
        &cred.access_token,
        Some(&state.refresh_ctx),
    )
    .await;
    match r {
        Ok(c) => Json(serde_json::json!({
            "success": true,
            "message": "token refreshed",
            "expiresAt": unix_to_rfc3339(c.expires_at_unix),
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(SuccessResponse {
                success: false,
                message: format!("刷新失败: {e:?}"),
            }),
        )
            .into_response(),
    }
}

// ============ 批量 / KAM 导入(server-side,与前端 batch/KAM 弹窗契约同形) ============
//
// 前端 batch-import / KAM-import 弹窗当前在浏览器逐条解析并调 `POST /credentials`,
// 且 local-cache / web-cookie 弹窗同样走单条 `POST /credentials`——四者均已被既有 CRUD
// add 端点覆盖,无需专门后端。此处新增的批量端点是**额外的服务端入口**:接受与前端弹窗
// 完全相同的 JSON 形态(数组 / KAM `{version,accounts}` / 单对象 / 顶层扁平 refreshToken),
// 服务端逐条规整+校验+入池落盘,带**逐项韧性**(单条失败不阻断其余),返回逐项结果与计数。
// 供脚本/自动化/未来前端一次性提交使用;命名 `import_*`,与登录流 `login_*` 不撞。

/// 批量导入的单条规整结果(内部用):从任意受支持形态抽出的凭据字段。
struct NormalizedImportItem {
    refresh_token: String,
    email: Option<String>,
    nickname: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    auth_region: Option<String>,
    api_region: Option<String>,
    machine_id: Option<String>,
    priority: Option<i64>,
}

/// 从一个 JSON 对象规整出 `NormalizedImportItem`。
/// 支持两种嵌套:(a) 顶层扁平(`refreshToken`/`clientId`... 直接在对象上);
/// (b) KAM 结构(`credentials: { refreshToken, clientId, clientSecret, region, ... }`,
/// `email`/`nickname`/`machineId` 在外层)。`credentials` 存在时其内字段优先。
/// 无有效 `refreshToken`(去空白后非空)→ 返回 `None`(调用方计为跳过/失败)。
fn normalize_import_object(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<NormalizedImportItem> {
    let s = |m: &serde_json::Map<String, serde_json::Value>, k: &str| -> Option<String> {
        m.get(k)
            .and_then(|v| v.as_str())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    // KAM 嵌套优先,回落顶层扁平。
    let creds = obj.get("credentials").and_then(|v| v.as_object());
    let pick = |k: &str| -> Option<String> { creds.and_then(|c| s(c, k)).or_else(|| s(obj, k)) };

    let refresh_token = pick("refreshToken")?;

    // region:KAM 里是 `credentials.region`;扁平里可能是 region/authRegion/apiRegion。
    let region = pick("region");
    let auth_region = s(obj, "authRegion").or_else(|| region.clone());
    let api_region = s(obj, "apiRegion");

    Some(NormalizedImportItem {
        refresh_token,
        email: s(obj, "email").or_else(|| s(obj, "nickname")),
        nickname: s(obj, "nickname"),
        client_id: pick("clientId"),
        client_secret: pick("clientSecret"),
        auth_region,
        api_region,
        machine_id: s(obj, "machineId").or_else(|| creds.and_then(|c| s(c, "machineId"))),
        priority: obj.get("priority").and_then(|v| v.as_i64()),
    })
}

/// 把顶层 JSON 拆成"待规整对象"列表,支持前端 batch/KAM 全部形态:
/// - 数组:`[ {...}, {...} ]`
/// - KAM 标准:`{ "version":..., "accounts": [ {...} ] }`
/// - 单对象:`{ ... }`(含 `credentials` 嵌套或顶层扁平 `refreshToken`)
/// 无法识别 → `Err(消息)`。
fn split_import_payload(v: &serde_json::Value) -> Result<Vec<serde_json::Value>, String> {
    if let Some(arr) = v.as_array() {
        return Ok(arr.clone());
    }
    if let Some(obj) = v.as_object() {
        if let Some(accounts) = obj.get("accounts").and_then(|a| a.as_array()) {
            return Ok(accounts.clone());
        }
        return Ok(vec![v.clone()]);
    }
    Err("payload must be an array, an object, or a KAM export ({accounts:[...]})".into())
}

/// `POST /api/admin/credentials/batch-import` 请求体。
/// `data` 承载任意受支持形态(数组 / KAM `{accounts}` / 单对象);用 untyped `Value`
/// 以对齐前端"粘贴任意 JSON"的宽松契约,规整/校验在服务端逐条完成。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchImportRequest {
    pub data: serde_json::Value,
}

/// 批量导入逐项结果(camelCase 对齐前端命名习惯)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchImportItemResult {
    /// 1 基序号(输入顺序)。
    pub index: usize,
    /// `added` | `failed`。
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /api/admin/credentials/batch-import` 响应体。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchImportResponse {
    pub success: bool,
    pub message: String,
    pub total: usize,
    pub added: usize,
    /// 因 refreshToken 已存在池中而被去重跳过的条数(未新增)。
    pub duplicate: usize,
    pub failed: usize,
    pub results: Vec<BatchImportItemResult>,
}

/// `POST /api/admin/credentials/batch-import`:一次性提交一批凭据(数组 / KAM 导出 /
/// 单对象),服务端逐条规整+校验+入池落盘,**逐项韧性**(某条失败不影响其余),
/// 返回逐项结果与 total/added/failed 计数。
///
/// 校验(每条):`refreshToken` 非空;若解析出 `clientId`/`clientSecret` 二者之一缺失
/// 而另一存在 → 判为不合法 idc(失败)。落盘沿用单条 add 的原子写(每条成功后落盘)。
/// 顶层 payload 无法识别 → 400;可识别但空 → 200 且 total=0。
pub async fn import_credentials_batch(
    State(state): State<MessagesState>,
    Json(req): Json<BatchImportRequest>,
) -> Response {
    let items = match split_import_payload(&req.data) {
        Ok(items) => items,
        Err(e) => return bad_request(&e),
    };

    let total = items.len();
    let mut results: Vec<BatchImportItemResult> = Vec::with_capacity(total);
    let mut added = 0usize;
    let mut duplicate = 0usize;
    let mut failed = 0usize;

    for (i, raw) in items.iter().enumerate() {
        let index = i + 1;

        // 1) 必须是对象。
        let obj = match raw.as_object() {
            Some(o) => o,
            None => {
                failed += 1;
                results.push(BatchImportItemResult {
                    index,
                    status: "failed".into(),
                    credential_id: None,
                    email: None,
                    error: Some("item is not a JSON object".into()),
                });
                continue;
            }
        };

        // 2) 规整字段 + refreshToken 校验。
        let item = match normalize_import_object(obj) {
            Some(it) => it,
            None => {
                failed += 1;
                results.push(BatchImportItemResult {
                    index,
                    status: "failed".into(),
                    credential_id: None,
                    email: None,
                    error: Some("missing valid refreshToken".into()),
                });
                continue;
            }
        };

        // 3) auth 推断 + idc 完整性校验(与单条 add 语义一致:两者需成对)。
        let has_id = item.client_id.is_some();
        let has_secret = item.client_secret.is_some();
        if has_id != has_secret {
            failed += 1;
            results.push(BatchImportItemResult {
                index,
                status: "failed".into(),
                credential_id: None,
                email: item.email.clone(),
                error: Some(
                    "clientId and clientSecret must be provided together for idc auth".into(),
                ),
            });
            continue;
        }
        let auth = if has_id && has_secret {
            AuthMethod::Idc
        } else {
            AuthMethod::Social
        };

        // region:apiRegion 优先,回落 authRegion,再回落 us-east-1(与单条 add 一致)。
        let region = item
            .api_region
            .clone()
            .or_else(|| item.auth_region.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "us-east-1".to_string());

        // weight:回落 priority(下限 1),再回落 1。
        let weight = item
            .priority
            .map(|p| if p < 1 { 1 } else { p as u32 })
            .unwrap_or(1);

        let cred = Credential {
            quota_reset_unix: None,
            priority: crate::kiro::credential::DEFAULT_PRIORITY,
            id: String::new(),
            access_token: String::new(),
            refresh_token: item.refresh_token,
            expires_at_unix: 0,
            region,
            auth,
            client_id: item.client_id,
            client_secret: item.client_secret,
            profile_arn: None,
            machine_id: item.machine_id,
            email: item.email.clone(),
            nickname: item.nickname,
            weight,
            label: None,
            disabled: false,
            status_reason: None,
            kiro_api_key: None,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
        };

        // 4) 入池落盘(逐项韧性:某条落盘失败仅该条判失败,继续下条)。
        match add_credential_to_pool_and_persist(&state, cred).await {
            Ok((id, email, is_dup)) => {
                if is_dup {
                    duplicate += 1;
                    results.push(BatchImportItemResult {
                        index,
                        status: "duplicate".into(),
                        credential_id: Some(id_as_number(&id)),
                        email,
                        error: None,
                    });
                } else {
                    added += 1;
                    results.push(BatchImportItemResult {
                        index,
                        status: "added".into(),
                        credential_id: Some(id_as_number(&id)),
                        email,
                        error: None,
                    });
                }
            }
            Err(e) => {
                failed += 1;
                results.push(BatchImportItemResult {
                    index,
                    status: "failed".into(),
                    credential_id: None,
                    email: item.email,
                    error: Some(format!("persist failed: {e}")),
                });
            }
        }
    }

    Json(BatchImportResponse {
        success: failed == 0,
        message: format!(
            "imported {added} of {total} credential(s), {duplicate} duplicate, {failed} failed"
        ),
        total,
        added,
        duplicate,
        failed,
        results,
    })
    .into_response()
}

// ============ Phase 3 交互式登录流(Builder-ID / IAM SSO / SSO Token)============
//
// 登录逻辑复用既有 `crate::kiro::login::{builderid,iam_sso,sso_token}`(PKCE/设备码/portal 批准),
// 本节只做 HTTP 暴露 + 会话中转 + 落库(经 §Phase 3 的 `add_credential_to_pool_and_persist`)。
// 会话中转态存 `MessagesState.{builderid_sessions,iam_sso_sessions}`(注入时钟、~600s TTL)。

use crate::admin::login_session::{BuilderIdSession, IamSsoSession};
use crate::kiro::login::{LoginError, MintedCredential, oidc_base};

/// SSO Token 批准走的 portal 基址(标准 AWS SSO portal 主机,按 region 组装)。
fn portal_base(region: &str) -> String {
    format!("https://portal.sso.{region}.amazonaws.com")
}

/// region 归一:非空 trim 后用之,否则回落 `us-east-1`。
fn region_or_default(region: Option<&str>) -> String {
    region
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("us-east-1")
        .to_string()
}

/// `MintedCredential` → 待落库 `Credential`。id 交由池分配(置空);`expires_at_unix`
/// 由 `expires_in_secs` 叠加当前 `now` 得出(0 视为未知,首次刷新补齐)。auth=Idc:
/// Builder-ID/SSO-token/IAM SSO 换出的均经 AWS SSO-OIDC `/client/register` + `/token`,
/// 首刷须走 SSO-OIDC refresh_token 授权端点并重放 clientId+clientSecret,故 auth 定为 Idc
/// 且把 `MintedCredential` 带出的 `client_id`/`client_secret` 落库(丢掉则首刷打错端点而失败)。
fn minted_to_credential(m: MintedCredential, now: u64) -> Credential {
    let expires_at_unix = if m.expires_in_secs == 0 {
        0
    } else {
        now.saturating_add(m.expires_in_secs)
    };
    Credential {
        quota_reset_unix: None,
        priority: crate::kiro::credential::DEFAULT_PRIORITY,
        id: String::new(),
        access_token: m.access_token,
        refresh_token: m.refresh_token,
        kiro_api_key: None,
        expires_at_unix,
        region: m.region,
        auth: AuthMethod::Idc,
        client_id: Some(m.client_id),
        client_secret: Some(m.client_secret),
        profile_arn: None,
        machine_id: None,
        email: None,
        nickname: None,
        weight: 1,
        label: None,
        disabled: false,
        status_reason: None,
        proxy_url: None,
        proxy_username: None,
        proxy_password: None,
    }
}

/// `POST /api/admin/login/builderid/start` 请求体,对齐前端 `BuilderIdStartRequest`。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BuilderIdStartRequest {
    #[serde(default)]
    pub region: Option<String>,
}

/// `POST /api/admin/login/builderid/start` 响应体,对齐前端 `BuilderIdStartResponse`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuilderIdStartResponse {
    pub session_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
}

/// `POST /api/admin/login/builderid/start`:注册客户端 + 发起设备授权,存会话回 sessionId。
/// 上游失败 → 502(不泄露内部细节)。region 会话内记住,供 /poll 换 token 复用。
pub async fn login_builderid_start(
    State(state): State<MessagesState>,
    Json(req): Json<BuilderIdStartRequest>,
) -> Response {
    let region = region_or_default(req.region.as_deref());
    let base = oidc_base(&region);
    match crate::kiro::login::builderid::start(&state.client, &base).await {
        Ok(pending) => {
            let user_code = pending.user_code.clone();
            let verification_uri = pending.verification_uri.clone();
            let interval = pending.interval_secs;
            // region 随会话保留(Pending 本身不含 region,poll 需要它重建 oidc base)。
            let entry = BuilderIdSession { pending, region };
            let session_id = state.builderid_sessions.put(entry, now_unix());
            Json(BuilderIdStartResponse {
                session_id,
                user_code,
                verification_uri,
                interval,
            })
            .into_response()
        }
        Err(e) => login_upstream_error(&e),
    }
}

/// `POST /api/admin/login/builderid/poll` 请求体,对齐前端 `BuilderIdPollRequest`。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuilderIdPollRequest {
    pub session_id: String,
}

/// `POST /api/admin/login/builderid/poll` 响应体,对齐前端 `BuilderIdPollResponse`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuilderIdPollResponse {
    pub success: bool,
    pub completed: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// 设备码轮询错误是否为**确证的终态**——只有终态才允许销毁登录会话。
///
/// 终态 = 用户拒绝(access_denied)、设备码过期(expired_token)、上游明确回的其它 OAuth
/// 错误码(invalid_client / invalid_grant…),以及请求本身非法(BadCallback,设备码流不产生)。
/// 其余一律按瞬态处理:传输层抖动、应答体无法解析、429 与 5xx 都可能发生在用户已经在浏览器
/// 点了授权之后,销毁会话会让他从头再来一遍;会话另有 ~600s TTL 兜底,多留一会儿代价有限。
/// 新增的错误变体默认落入瞬态一侧,方向上偏保守(宁可多轮询,不可误销毁)。
fn is_terminal_poll_error(e: &LoginError) -> bool {
    matches!(
        e,
        LoginError::Denied
            | LoginError::Expired
            | LoginError::Upstream(_)
            | LoginError::BadCallback
    )
}

/// 瞬态轮询失败是否该让前端拉长间隔:429/5xx 说明上游正被打疼,退避再问。
fn should_back_off(e: &LoginError) -> bool {
    matches!(e, LoginError::UpstreamHttp { status, .. } if *status == 429 || *status >= 500)
}

/// `POST /api/admin/login/builderid/poll`:非消费读取会话,轮询一次设备码换 token。
/// pending → 继续等;slow_down → 回退间隔;成功 → 落库并清会话回 completed;
/// denied/expired → 清会话并以 400/410 语义化(前端按 status 文案提示)。
/// 瞬态失败(网络抖动/不可解析应答/429/5xx)→ 保留会话并回 200 非终态,让前端继续轮询。
pub async fn login_builderid_poll(
    State(state): State<MessagesState>,
    Json(req): Json<BuilderIdPollRequest>,
) -> Response {
    let now = now_unix();
    let Some(entry) = state.builderid_sessions.get(&req.session_id, now) else {
        return login_session_not_found();
    };
    let base = oidc_base(&entry.region);
    match crate::kiro::login::builderid::poll(&state.client, &base, &entry.region, &entry.pending)
        .await
    {
        Ok(minted) => {
            let cred = minted_to_credential(minted, now);
            match add_credential_to_pool_and_persist(&state, cred).await {
                Ok((id, email, _is_dup)) => {
                    state.builderid_sessions.remove(&req.session_id);
                    Json(BuilderIdPollResponse {
                        success: true,
                        completed: true,
                        status: "completed".into(),
                        interval: None,
                        credential_id: Some(id_as_number(&id)),
                        email,
                    })
                    .into_response()
                }
                Err(e) => persist_failed(&e),
            }
        }
        Err(LoginError::Pending) => Json(BuilderIdPollResponse {
            success: true,
            completed: false,
            status: "pending".into(),
            interval: Some(entry.pending.interval_secs),
            credential_id: None,
            email: None,
        })
        .into_response(),
        Err(LoginError::SlowDown) => Json(BuilderIdPollResponse {
            success: true,
            completed: false,
            status: "slow_down".into(),
            interval: Some(entry.pending.interval_secs.saturating_add(5)),
            credential_id: None,
            email: None,
        })
        .into_response(),
        Err(e) if !is_terminal_poll_error(&e) => {
            // 瞬态:会话原样保留(用户可能已在浏览器点了授权),回一个非终态让前端接着轮询。
            let back_off = should_back_off(&e);
            tracing::warn!(
                event = "builderid_poll_transient",
                error = %login_error_str(&e),
                back_off,
                "设备码轮询瞬态失败,保留会话继续轮询"
            );
            let (status, interval) = if back_off {
                ("slow_down", entry.pending.interval_secs.saturating_add(5))
            } else {
                ("pending", entry.pending.interval_secs)
            };
            Json(BuilderIdPollResponse {
                success: true,
                completed: false,
                status: status.into(),
                interval: Some(interval),
                credential_id: None,
                email: None,
            })
            .into_response()
        }
        Err(e) => {
            // 确证终态:清会话并语义化错误。
            state.builderid_sessions.remove(&req.session_id);
            login_upstream_error(&e)
        }
    }
}

/// `POST /api/admin/login/iam-sso/start` 请求体,对齐前端 `IamSsoStartRequest`。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IamSsoStartRequest {
    pub start_url: String,
    #[serde(default)]
    pub region: Option<String>,
}

/// `POST /api/admin/login/iam-sso/start` 响应体,对齐前端 `IamSsoStartResponse`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IamSsoStartResponse {
    pub session_id: String,
    pub authorize_url: String,
}

/// `POST /api/admin/login/iam-sso/start`:注册客户端 + 构造 authorize URL(含 PKCE/state),
/// 存会话回 sessionId + authorizeUrl。startUrl 空 → 400;上游失败 → 502。
pub async fn login_iam_sso_start(
    State(state): State<MessagesState>,
    Json(req): Json<IamSsoStartRequest>,
) -> Response {
    if req.start_url.trim().is_empty() {
        return bad_request("startUrl is required");
    }
    let region = region_or_default(req.region.as_deref());
    let base = oidc_base(&region);
    match crate::kiro::login::iam_sso::start(&state.client, &base, req.start_url.trim()).await {
        Ok(auth_start) => {
            let authorize_url = auth_start.authorize_url.clone();
            let entry = IamSsoSession {
                auth: auth_start,
                region,
            };
            let session_id = state.iam_sso_sessions.put(entry, now_unix());
            Json(IamSsoStartResponse {
                session_id,
                authorize_url,
            })
            .into_response()
        }
        Err(e) => login_upstream_error(&e),
    }
}

/// `POST /api/admin/login/iam-sso/complete` 请求体,对齐前端 `IamSsoCompleteRequest`。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IamSsoCompleteRequest {
    pub session_id: String,
    pub callback_url: String,
}

/// `POST /api/admin/login/iam-sso/complete`:非消费读取会话,解析回调 URL(校验 state 防 CSRF)、
/// 授权码 + verifier 换 token、落库。返回 `AddCredentialResponse`(camelCase)。
/// 未知/过期会话 → 404;回调非法/state 不符 → 400;上游失败 → 502。
///
/// 会话只在换 token 成功后才消费:粘错回调 URL(400)或换 token 撞上瞬态失败(502)时会话
/// 仍在,用户改一下就能重试,不必从 `/start` 重走一遍。授权码本身一次性,成功后立刻
/// `remove` 使会话失效,同一 sessionId 无法被重放去换第二份凭据。
pub async fn login_iam_sso_complete(
    State(state): State<MessagesState>,
    Json(req): Json<IamSsoCompleteRequest>,
) -> Response {
    let now = now_unix();
    let Some(entry) = state.iam_sso_sessions.get(&req.session_id, now) else {
        return login_session_not_found();
    };
    // 解析回调:error 优先 → state CSRF → code。
    let code =
        match crate::kiro::login::iam_sso::parse_callback(&req.callback_url, &entry.auth.state) {
            Ok(c) => c,
            Err(LoginError::BadCallback) => {
                return bad_request("invalid callback url or state mismatch");
            }
            Err(e) => return login_upstream_error(&e),
        };
    let base = oidc_base(&entry.region);
    match crate::kiro::login::iam_sso::complete(
        &state.client,
        &base,
        &entry.region,
        &entry.auth,
        &code,
    )
    .await
    {
        Ok(minted) => {
            // 授权码已兑换:此刻消费会话(防重放),再落库。
            state.iam_sso_sessions.remove(&req.session_id);
            let cred = minted_to_credential(minted, now);
            match add_credential_to_pool_and_persist(&state, cred).await {
                Ok((id, email, is_dup)) => Json(AddCredentialResponse {
                    success: true,
                    message: if is_dup {
                        "credential already exists".into()
                    } else {
                        "credential added".into()
                    },
                    credential_id: id_as_number(&id),
                    email,
                    duplicate: is_dup,
                })
                .into_response(),
                Err(e) => persist_failed(&e),
            }
        }
        // 换 token 失败(含瞬态):会话保留,用户可原样重试或换一个回调 URL 再试。
        Err(e) => login_upstream_error(&e),
    }
}

/// `POST /api/admin/login/sso-token` 请求体,对齐前端 `SsoTokenImportRequest`。
/// `bearerToken` 为整段换行分隔文本,后端按行拆分逐行兑换。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoTokenImportRequest {
    pub bearer_token: String,
    #[serde(default)]
    pub region: Option<String>,
}

/// 单行失败项,对齐前端 `SsoTokenFailure`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoTokenFailure {
    pub line_index: usize,
    pub error: String,
}

/// `POST /api/admin/login/sso-token` 响应体,对齐前端 `SsoTokenImportResponse`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoTokenImportResponse {
    pub added: usize,
    pub failed: Vec<SsoTokenFailure>,
}

/// `POST /api/admin/login/sso-token`:批量导入。按行拆分(去空行/空白),逐行走设备码
/// + portal 自动批准换 token 并落库;每行独立成败(上限 ~200,超出截断)。返回 `{added,failed[]}`。
pub async fn login_sso_token(
    State(state): State<MessagesState>,
    Json(req): Json<SsoTokenImportRequest>,
) -> Response {
    let region = region_or_default(req.region.as_deref());
    let oidc = oidc_base(&region);
    let portal = portal_base(&region);

    // 拆行:保留原始行号(line_index)以便前端定位;跳过纯空白行。
    let lines: Vec<(usize, String)> = req
        .bearer_token
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let t = l.trim();
            if t.is_empty() {
                None
            } else {
                Some((i, t.to_string()))
            }
        })
        .collect();

    let mut added = 0usize;
    let mut failed: Vec<SsoTokenFailure> = Vec::new();

    // 上限截断(与 sso_token::redeem_bulk 的 MAX_BULK 一致语义);超出的行标记为 failed。
    const MAX_BULK: usize = 200;
    for (line_index, bearer) in lines.into_iter() {
        if added + failed.len() >= MAX_BULK {
            failed.push(SsoTokenFailure {
                line_index,
                error: "exceeded bulk import cap (200)".into(),
            });
            continue;
        }
        let now = now_unix();
        match crate::kiro::login::sso_token::redeem(&state.client, &oidc, &portal, &region, &bearer)
            .await
        {
            Ok(minted) => {
                let cred = minted_to_credential(minted, now);
                match add_credential_to_pool_and_persist(&state, cred).await {
                    Ok(_) => added += 1,
                    Err(_) => failed.push(SsoTokenFailure {
                        line_index,
                        error: "persist failed".into(),
                    }),
                }
            }
            Err(e) => failed.push(SsoTokenFailure {
                line_index,
                error: login_error_str(&e).to_string(),
            }),
        }
    }

    Json(SsoTokenImportResponse { added, failed }).into_response()
}

/// 登录会话未找到/过期的统一 404。
fn login_session_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "success": false, "error": "login session not found or expired" })),
    )
        .into_response()
}

/// `LoginError` → 对外错误串。对 `UpstreamHttp` 展开状态码 + 上游信息,便于定位真因
/// (信息已在提取时截断/单行化,不含令牌)。
fn login_error_str(e: &LoginError) -> String {
    match e {
        LoginError::Http => "无法连接上游(网络错误/超时)".to_string(),
        LoginError::Upstream(m) => format!("上游拒绝请求: {m}"),
        LoginError::UpstreamHttp { status, body } => {
            if body.is_empty() {
                format!("上游返回 HTTP {status}")
            } else {
                format!("上游返回 HTTP {status}: {body}")
            }
        }
        LoginError::Transient { status, detail } => {
            format!("上游暂时不可用(HTTP {status}),可稍后重试: {detail}")
        }
        LoginError::Pending => "授权待批准".to_string(),
        LoginError::SlowDown => "轮询过快,请放慢".to_string(),
        LoginError::Expired => "设备码已过期".to_string(),
        LoginError::Denied => "授权被拒绝".to_string(),
        LoginError::BadCallback => "回调 URL 无效或 state 不匹配".to_string(),
    }
}

/// 登录上游错误 → HTTP 响应,携带信息化文案(而非笼统 "network error"):
/// - `UpstreamHttp`:4xx 上游 → 400(请求侧问题),5xx 上游 → 502(上游侧问题),
///   文案形如 "IAM SSO 注册失败: HTTP 400 …";
/// - denied → 403、expired → 410、bad callback → 400、其余 → 502。
fn login_upstream_error(e: &LoginError) -> Response {
    let (status, message) = match e {
        LoginError::UpstreamHttp { status: up, body } => {
            let detail = if body.is_empty() {
                format!("HTTP {up}")
            } else {
                format!("HTTP {up} {body}")
            };
            let http = if *up >= 500 {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::BAD_REQUEST
            };
            (http, format!("IAM SSO 注册失败: {detail}"))
        }
        LoginError::Denied => (StatusCode::FORBIDDEN, login_error_str(e)),
        LoginError::Expired => (StatusCode::GONE, login_error_str(e)),
        LoginError::BadCallback => (StatusCode::BAD_REQUEST, login_error_str(e)),
        LoginError::Http => (StatusCode::BAD_GATEWAY, login_error_str(e)),
        _ => (StatusCode::BAD_GATEWAY, login_error_str(e)),
    };
    (status, Json(json!({ "success": false, "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::admin_api_router;
    use crate::config::Config;
    use crate::kiro::credential::{AuthMethod, Credential};
    use crate::kiro::pool::{LbMode, Pool};
    use crate::stats::StatsManager;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode as HttpStatusCode};
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    fn cred(id: &str) -> Credential {
        Credential {
            quota_reset_unix: None,
            priority: 999,
            id: id.into(),
            access_token: "SEKRET-AT".into(),
            refresh_token: "SEKRET-RT".into(),
            kiro_api_key: None,
            expires_at_unix: u64::MAX,
            region: "us-east-1".into(),
            auth: AuthMethod::Social,
            client_id: None,
            client_secret: None,
            profile_arn: None,
            machine_id: None,
            email: None,
            nickname: None,
            weight: 1,
            label: None,
            disabled: false,
            status_reason: None,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
        }
    }

    fn state_with(creds: Vec<Credential>, cfg: Config) -> MessagesState {
        state_with_stats(creds, cfg, empty_stats("state"))
    }

    fn state_with_stats(
        creds: Vec<Credential>,
        cfg: Config,
        stats: Arc<StatsManager>,
    ) -> MessagesState {
        // 生产同款不变量:refresh_ctx 的落盘路径 == cfg.credentials_path,admin CRUD 与
        // 刷新写回落到同一文件、共用同一把 persist_lock(#13 lost-update 修复所依赖)。
        let cfg_credentials_path = cfg.credentials_path.clone();
        MessagesState {
            pool: Arc::new(Mutex::new(Pool::new(creds, LbMode::Priority))),
            client: reqwest::Client::new(),
            control_client: reqwest::Client::new(),
            cfg: Arc::new(cfg),
            runtime_cfg: crate::config::shared_runtime_config(&crate::config::Config::default()),
            endpoint_override: None,
            stats,
            api_keys: crate::apikey::ApiKeyStore::load(std::env::temp_dir().join(format!(
                    "kiro2api_admin_apikeys_{}_{}.json",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                ))),
            balance: crate::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
            models_cache: crate::models_cache::ModelsCache::new(),
            builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            log_capture: None,
            refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(cfg_credentials_path),
        }
    }

    /// 建一个指向唯一临时目录的空 StatsManager(每测试隔离,避免共享落盘文件串扰)。
    fn empty_stats(tag: &str) -> Arc<StatsManager> {
        let dir = crate::test_tmp::dir(&format!("admin_{tag}"));
        StatsManager::load_from_dir(&dir)
    }

    /// 唯一临时 credentials.json 路径的 Config(每测试隔离持久化落盘)。
    fn cfg_with_temp_creds() -> Config {
        let path = crate::test_tmp::file("admin_creds", "credentials.json");
        Config {
            credentials_path: path.to_string_lossy().into_owned(),
            ..Config::default()
        }
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn stats_returns_accounts_and_summary_without_leaking_tokens() {
        let state = state_with(vec![cred("a")], Config::default());
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["accounts"].as_array().unwrap().len() == 1);
        assert_eq!(v["summary"]["total"], 1);
        assert!(!text.contains("accessToken"));
        assert!(!text.contains("refreshToken"));
        assert!(!text.contains("SEKRET-AT"));
        assert!(!text.contains("SEKRET-RT"));
    }

    /// refresh_all_models 必须暴露 failed 计数 + errors[] 明细,而不是被 success:true 掩盖。
    /// 用不可解析 region 让上游拉取在传输层确定性失败,断言响应形状含 errors[]。
    #[tokio::test]
    async fn refresh_all_models_exposes_failures_in_errors_array() {
        let mut c = cred("7");
        // 不可解析的 host → fetch_available_models 传输层失败 → 计入 errors[](确定性,不打真 AWS)。
        c.region = "invalid-region-does-not-resolve".into();
        let state = state_with(vec![c], cfg_with_temp_creds());
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/credentials/models/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["success"], true,
            "batch call itself stays success:true: {text}"
        );
        assert_eq!(v["refreshed"], 0);
        assert_eq!(v["failed"], 1);
        let errors = v["errors"].as_array().expect("errors[] present");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["id"], 7);
        assert!(errors[0]["error"].as_str().unwrap().contains("models"));
        // 绝不泄露令牌。
        assert!(!text.contains("SEKRET-AT"));
        assert!(!text.contains("SEKRET-RT"));
    }

    /// 同一已缓存档位的多账号只刷一个代表:两个账号同为 "KIRO FREE",
    /// 上游不可达故两者都会"失败若被尝试",但按档位分组后只有代表账号被尝试
    /// → errors[] 恰好 1 条(证明未对同档位第二个账号发起刷新)。
    #[tokio::test]
    async fn refresh_all_models_one_rep_per_cached_tier() {
        let mut a = cred("7");
        let mut b = cred("8");
        a.region = "invalid-region-does-not-resolve".into();
        b.region = "invalid-region-does-not-resolve".into();
        let state = state_with(vec![a, b], cfg_with_temp_creds());
        let now = super::now_unix();
        // 两账号预置同一新鲜档位 KIRO FREE。
        for id in ["7", "8"] {
            state
                .balance
                .put(
                    id,
                    crate::balance::BalanceSnapshot {
                        subscription_title: Some("KIRO FREE".into()),
                        current_usage: 1.0,
                        usage_limit: 100.0,
                        remaining: 99.0,
                        usage_percentage: 1.0,
                        next_reset_at: None,
                        fetched_at_unix: now,
                    },
                )
                .await;
        }
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/credentials/models/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["refreshed"], 0);
        // 只刷了一个代表账号 → 恰好 1 条错误(而非 2 条)。
        assert_eq!(v["failed"], 1, "只对每个已缓存档位刷一个代表账号: {v}");
        assert!(v["tiers"].is_array());
    }

    /// 不同已缓存档位各出一个代表:FREE + PRO+ 两档,各刷一个账号 → 2 次尝试。
    #[tokio::test]
    async fn refresh_all_models_distinct_tiers_each_get_a_rep() {
        let mut a = cred("7");
        let mut b = cred("8");
        a.region = "invalid-region-does-not-resolve".into();
        b.region = "invalid-region-does-not-resolve".into();
        let state = state_with(vec![a, b], cfg_with_temp_creds());
        let now = super::now_unix();
        let mk = |title: &str| crate::balance::BalanceSnapshot {
            subscription_title: Some(title.into()),
            current_usage: 1.0,
            usage_limit: 100.0,
            remaining: 99.0,
            usage_percentage: 1.0,
            next_reset_at: None,
            fetched_at_unix: now,
        };
        state.balance.put("7", mk("KIRO FREE")).await;
        state.balance.put("8", mk("KIRO PRO+")).await;
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/credentials/models/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        // 两个不同档位各刷一个代表 → 2 次尝试(此处均因不可达 host 失败)。
        assert_eq!(v["failed"], 2, "每个不同档位各出一个代表: {v}");
    }

    #[tokio::test]
    async fn config_view_exposes_only_booleans_never_key_values() {
        let cfg = Config {
            api_key: Some("secret".into()),
            admin_api_key: Some("adm-secret".into()),
            region: "eu-west-1".into(),
            ..Config::default()
        };
        let state = state_with(vec![], cfg);
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["api_key_set"], true);
        assert_eq!(v["admin_api_key_set"], true);
        assert_eq!(v["region"], "eu-west-1");
        assert!(!text.contains("secret"));
        assert!(!text.contains("adm-secret"));
    }

    #[tokio::test]
    async fn config_view_reports_false_when_keys_unset() {
        let state = state_with(vec![], Config::default());
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["api_key_set"], false);
        assert_eq!(v["admin_api_key_set"], false);
    }

    #[tokio::test]
    async fn credentials_list_camelcase_shape_no_token_leak() {
        let state = state_with(vec![cred("12345")], Config::default());
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(v["available"], 1);
        assert!(v["currentId"].is_i64());
        let c = &v["credentials"][0];
        assert_eq!(c["id"], 12345); // 数值 id
        assert_eq!(c["disabled"], false);
        assert_eq!(c["authMethod"], "social");
        assert_eq!(c["healthStatus"], "healthy");
        assert!(c["successCount"].is_u64());
        assert!(c["failureCount"].is_u64());
        assert!(c.get("hasProfileArn").is_some());
        // 脱敏
        assert!(!text.contains("SEKRET-AT"));
        assert!(!text.contains("SEKRET-RT"));
        assert!(!text.contains("accessToken"));
        assert!(!text.contains("refreshToken"));
    }

    #[tokio::test]
    async fn balance_endpoint_serves_cached_snapshot_camelcase() {
        // 预置一条新鲜缓存;端点应命中缓存(不触网)并按前端契约 camelCase 出形。
        let state = state_with(vec![cred("7")], Config::default());
        let now = super::now_unix();
        state
            .balance
            .put(
                "7",
                crate::balance::BalanceSnapshot {
                    subscription_title: Some("KIRO PRO+".into()),
                    current_usage: 10.0,
                    usage_limit: 100.0,
                    remaining: 90.0,
                    usage_percentage: 10.0,
                    next_reset_at: Some(1_784_462_400),
                    fetched_at_unix: now,
                },
            )
            .await;
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/credentials/7/balance")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["subscriptionTitle"], "KIRO PRO+");
        assert_eq!(v["currentUsage"], 10.0);
        assert_eq!(v["usageLimit"], 100.0);
        assert_eq!(v["remaining"], 90.0);
        assert_eq!(v["usagePercentage"], 10.0);
        assert_eq!(v["nextResetAt"], 1_784_462_400i64);
    }

    #[tokio::test]
    async fn balance_endpoint_unknown_id_is_404() {
        // 缓存 miss + 磁盘无该凭据(默认配置 credentials.json 不存在)→ 404,不 500。
        let state = state_with(vec![], Config::default());
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/credentials/999/balance")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_disabled_endpoint_toggles_and_returns_success() {
        // 落盘路径必须落在临时目录:启停现在会立刻写 credentials.json,用 `Config::default()`
        // 的相对路径会把带假 token 的凭据文件写进仓库根目录,`Config::default()` 从此加载到
        // 非空池,别处「空池应回 503」的测试全部变 502(实际踩过)。
        let state = state_with(vec![cred("7")], cfg_with_temp_creds());
        let app = admin_api_router(state);

        // disable via body {disabled:true}
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/credentials/7/disabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"disabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(v["success"], true);

        // reflected in list
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(v["credentials"][0]["disabled"], true);
        assert_eq!(v["credentials"][0]["healthStatus"], "disabled");

        // re-enable
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/credentials/7/disabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"disabled":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);

        // unknown id 404
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/credentials/999/disabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"disabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn models_endpoint_matches_frontend_shape() {
        let state = state_with(vec![], Config::default());
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(v["object"], "list");
        let data = v["data"].as_array().unwrap();
        assert!(!data.is_empty());
        let m = &data[0];
        assert!(m["id"].is_string());
        assert_eq!(m["object"], "model");
        assert!(m["display_name"].is_string()); // snake_case per frontend ModelItem
        assert!(m["owned_by"].is_string());
        assert!(m["max_tokens"].is_u64());
    }

    #[tokio::test]
    async fn models_endpoint_returns_dynamic_union_when_cache_populated() {
        let state = state_with(vec![], Config::default());
        // 预置两个账号的动态缓存(并集去重后应为 auto/claude-sonnet-5/gpt-5.6-sol)
        let now = now_unix();
        state
            .models_cache
            .put(
                "a",
                vec![
                    crate::models_cache::ModelInfo {
                        context_window: None,
                        id: "auto".into(),
                        display_name: "Auto".into(),
                        owned_by: "kiro".into(),
                        max_tokens: 0,
                        rate_multiplier: None,
                    },
                    crate::models_cache::ModelInfo {
                        context_window: None,
                        id: "claude-sonnet-5".into(),
                        display_name: "Claude Sonnet 5".into(),
                        owned_by: "anthropic".into(),
                        max_tokens: 64_000,
                        rate_multiplier: Some(1.3),
                    },
                ],
                now,
            )
            .await;
        state
            .models_cache
            .put(
                "b",
                vec![crate::models_cache::ModelInfo {
                    context_window: None,
                    id: "gpt-5.6-sol".into(),
                    display_name: "GPT-5.6 Sol".into(),
                    owned_by: "openai".into(),
                    max_tokens: 128_000,
                    rate_multiplier: None,
                }],
                now,
            )
            .await;
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let data = v["data"].as_array().unwrap();
        // 动态并集共 3 个(去重),排序后 auto 在首
        assert_eq!(data.len(), 3);
        assert_eq!(data[0]["id"], "auto");
        // 上游 max_tokens=0 → 回落 200000
        assert_eq!(data[0]["max_tokens"], 200_000);
        assert_eq!(data[1]["id"], "claude-sonnet-5");
        assert_eq!(data[1]["owned_by"], "anthropic");
        assert_eq!(data[1]["rate_multiplier"], 1.3);
        assert_eq!(data[2]["id"], "gpt-5.6-sol");
    }

    /// 造 n 个上游确定性不可达的账号(不可解析 host → 传输层立刻失败,绝不打真 AWS)。
    fn unreachable_creds(n: usize) -> Vec<Credential> {
        (0..n)
            .map(|i| {
                let mut c = cred(&i.to_string());
                c.region = "invalid-region-does-not-resolve".into();
                c
            })
            .collect()
    }

    /// 惰性回填必须有界:上游整体故障(封号/区域抖动/额度耗尽)时没有任何账号会成功,
    /// 成功类上限一个都不会触发。旧实现没有失败上限 → 一轮回填把全池挨个打一遍
    /// (生产 ~1000 账号)。断言:在失败上限处停手,而不是走完整池。
    #[tokio::test]
    async fn lazy_refresh_sweep_stops_at_failure_cap_instead_of_walking_whole_pool() {
        let pool_size = super::LAZY_REFRESH_FAILURE_CAP * 3;
        let state = state_with(unreachable_creds(pool_size), cfg_with_temp_creds());
        let out = super::lazy_refresh_sweep(&state, super::now_unix(), 0).await;
        assert_eq!(
            out.attempts,
            super::LAZY_REFRESH_FAILURE_CAP,
            "上游整体失败时必须在失败上限处停手: {out:?}"
        );
        assert_eq!(out.failures, super::LAZY_REFRESH_FAILURE_CAP);
        assert_eq!(out.successes, 0);
        assert!(
            out.scanned < pool_size,
            "不得走完整池: scanned={} pool={pool_size}",
            out.scanned
        );
    }

    /// 惰性回填必须从给定游标起**轮转**扫描,否则坏账号前缀会把后面的好账号永久挡住
    /// (失败上限在同一批坏账号处反复停手,并集永远为空)。
    /// 用"缓存已新鲜的账号被跳过"制造可观测差异:同一个池,起点不同 → 走过的位置数不同。
    #[tokio::test]
    async fn lazy_refresh_sweep_honors_rotating_start_offset() {
        let skipped = 4usize;
        let pool_size = super::LAZY_REFRESH_FAILURE_CAP + skipped;
        let state = state_with(unreachable_creds(pool_size), cfg_with_temp_creds());
        let now = super::now_unix();
        // 前 4 个账号缓存仍新鲜 → 扫描时被跳过(不发上游请求,但占位置)。
        for i in 0..skipped {
            state
                .models_cache
                .put(
                    i.to_string(),
                    vec![crate::models_cache::ModelInfo {
                        context_window: None,
                        id: format!("m-{i}"),
                        display_name: "m".into(),
                        owned_by: "kiro".into(),
                        max_tokens: 0,
                        rate_multiplier: None,
                    }],
                    now,
                )
                .await;
        }
        // 起点 0:先跳过 4 个,再打 8 个(失败上限)→ 走过 12 个位置。
        let from_head = super::lazy_refresh_sweep(&state, now, 0).await;
        assert_eq!(from_head.attempts, super::LAZY_REFRESH_FAILURE_CAP);
        assert_eq!(from_head.scanned, pool_size, "起点 0 应走过全部位置");
        // 起点 4(上一轮停下处):直接打 8 个 → 只走过 8 个位置,证明起点确实生效。
        let from_offset = super::lazy_refresh_sweep(&state, now, skipped).await;
        assert_eq!(from_offset.attempts, super::LAZY_REFRESH_FAILURE_CAP);
        assert_eq!(
            from_offset.scanned,
            super::LAZY_REFRESH_FAILURE_CAP,
            "起点应从游标处开始,而不是恒从池首重来: {from_offset:?}"
        );
    }

    /// 闸门三件事:并发单飞(N 个并发请求只起 1 轮扫描)、一轮结束后有冷却窗口
    /// (并集恒为空时不至于每次仪表盘渲染都再起一轮)、游标随轮次前移。
    #[test]
    fn lazy_refresh_gate_is_single_flight_with_cooldown_and_rotating_cursor() {
        let gate = super::LazyRefreshGate::new();
        assert_eq!(gate.try_acquire(1_000), Some(0), "首个请求应拿到扫描权");
        assert!(
            gate.try_acquire(1_000).is_none(),
            "并发的第二个请求不得再起一轮全池扫描"
        );
        // 一轮结束(走过 5 个位置)
        gate.finish(1_000, 5);
        assert!(
            gate.try_acquire(1_000 + super::LAZY_REFRESH_COOLDOWN_SECS - 1)
                .is_none(),
            "冷却窗口内不得再起新一轮"
        );
        assert_eq!(
            gate.try_acquire(1_000 + super::LAZY_REFRESH_COOLDOWN_SECS),
            Some(5),
            "冷却结束后可再起,且从上一轮停下的位置继续"
        );
    }

    /// 余额端点的"未知 id → 404"判定必须查内存池,不许读盘:
    /// 旧实现 `credential::load(..).unwrap_or_default()` 会把"文件不存在/读失败/解析失败"
    /// 吞成空列表,于是池内真实存在的账号也被误报 404(且每次 miss 都同步阻塞读一遍 MB 级文件)。
    #[tokio::test]
    async fn balance_endpoint_existence_check_uses_pool_not_disk() {
        // 测试用 BalanceCache 共用 temp_dir 且会落盘,固定 id 会命中别的测试/上一轮跑留下的
        // 新鲜快照(直接 200 返回、走不到存在性判定),故用每次唯一的 id。
        let id = format!(
            "9{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                % 1_000_000_000
        );
        let mut c = cred(&id);
        c.region = "invalid-region-does-not-resolve".into();
        let cfg = cfg_with_temp_creds();
        // 该路径下并无落盘文件(等价于读盘拿不到内容的情形)。
        assert!(!std::path::Path::new(&cfg.credentials_path).exists());
        let state = state_with(vec![c], cfg);
        assert!(
            state
                .balance
                .get_fresh(&id, super::now_unix())
                .await
                .is_none(),
            "前置条件:余额缓存里不得有该 id 的新鲜快照,否则测不到存在性判定"
        );
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/admin/credentials/{id}/balance"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            HttpStatusCode::NOT_FOUND,
            "池内存在的账号不得因读不到 credentials.json 被误报 404"
        );
        // 账号存在 → 走到上游实拉,上游不可达 → 502(而非 404)。
        assert_eq!(resp.status(), HttpStatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn api_admin_config_served_at_new_prefix() {
        let cfg = Config {
            region: "ap-southeast-1".into(),
            ..Config::default()
        };
        let state = state_with(vec![], cfg);
        let app = admin_api_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(v["region"], "ap-southeast-1");
    }

    #[tokio::test]
    async fn disable_then_enable_roundtrip_and_missing_account_404s() {
        let state = state_with(vec![cred("a"), cred("b")], Config::default());
        let app = admin_api_router(state);

        // disable "a"
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/admin/api/accounts/a/disable")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["disabled"], true);
        assert_eq!(v["id"], "a");

        // stats now show "a" disabled
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let accounts = v["accounts"].as_array().unwrap();
        let a = accounts.iter().find(|x| x["id"] == "a").unwrap();
        assert_eq!(a["disabled"], true);
        assert_eq!(v["summary"]["disabled"], 1);

        // enable "a" restores it
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/admin/api/accounts/a/enable")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["disabled"], false);

        // disabling a nonexistent account 404s
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/admin/api/accounts/nope/disable")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::NOT_FOUND);
    }

    // ---- Phase 1 统计端点测试 ----

    async fn get(app: &Router, uri: &str) -> (HttpStatusCode, serde_json::Value) {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        (status, v)
    }

    #[tokio::test]
    async fn rpm_snapshot_shape_is_camel_case_contract() {
        let app = admin_api_router(state_with(vec![cred("1"), cred("2")], Config::default()));
        let (st, v) = get(&app, "/api/admin/rpm").await;
        assert_eq!(st, HttpStatusCode::OK);
        // 契约字段齐备且 camelCase(RpmSnapshot: global/byCredential/byApiKey)
        assert!(v["global"].is_number());
        assert!(v["byCredential"].is_object());
        assert!(v["byApiKey"].is_object());
        // 每账号 id 均在 byCredential 里,初始 RPM 为 0
        assert_eq!(v["byCredential"]["1"], 0);
        assert_eq!(v["byCredential"]["2"], 0);
        assert_eq!(v["global"], 0);
        // 账本为空 → key 维度自然没有条目(注意:这是"没跑过流量",不是恒空占位,
        // 有流量时必须出数,见 rpm_snapshot_reports_per_api_key_traffic)
        assert_eq!(v["byApiKey"].as_object().unwrap().len(), 0);
    }

    /// 关键回归:API-KEY 页每张卡片那行 "RPM x" 直接读 `/api/admin/rpm` 的 `byApiKey`
    /// (前端 sec-apikeys.js 取 `byApiKey[String(key.id)] || 0`)。后端此前无条件返回空对象,
    /// 于是不管这把 key 正在跑多少流量,面板永远显示 RPM 0 —— 数据源根本没接。
    #[tokio::test]
    async fn rpm_snapshot_reports_per_api_key_traffic() {
        let stats = empty_stats("rpm_bykey");
        let now = super::now_unix() as i64;
        async fn rec(stats: &Arc<StatsManager>, api_key_id: u32, at: i64) {
            stats
                .usage
                .record_usage_with_api_key(
                    1,
                    api_key_id,
                    "claude-sonnet-4".into(),
                    10,
                    20,
                    0.5,
                    None,
                    None,
                    None,
                    at,
                )
                .await;
        }
        // key 1 在窗口内跑了 3 次,key 2 跑了 1 次。
        rec(&stats, 1, now).await;
        rec(&stats, 1, now - 1).await;
        rec(&stats, 1, now - 2).await;
        rec(&stats, 2, now - 3).await;
        // key 1 还有一次是 90s 前的:已滑出 60s 窗口,不该计入。
        rec(&stats, 1, now - 90).await;
        // 无归属流量(用全局 key 直连,api_key_id=0)不属于任何 key,不得出现在 byApiKey 里。
        rec(&stats, 0, now).await;

        let app = admin_api_router(state_with_stats(
            vec![cred("1")],
            Config::default(),
            stats.clone(),
        ));
        let (st, v) = get(&app, "/api/admin/rpm").await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(
            v["byApiKey"]["1"], 3,
            "近 60s 内 key 1 跑了 3 次,面板不能再显示 RPM 0:{v}"
        );
        assert_eq!(v["byApiKey"]["2"], 1, "{v}");
        assert!(v["byApiKey"]["0"].is_null(), "无归属流量不属于任何 key:{v}");
    }

    #[tokio::test]
    async fn usage_records_empty_and_unknown_id_return_empty_page_not_500() {
        let stats = empty_stats("usage_empty");
        let app = admin_api_router(state_with_stats(vec![cred("1")], Config::default(), stats));

        // 空存储 → 空页
        let (st, v) = get(&app, "/api/admin/credentials/1/usage/records").await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["total"], 0);
        assert_eq!(v["page"], 1);
        assert_eq!(v["pageSize"], 20);
        assert_eq!(v["totalPages"], 0);
        assert!(v["records"].as_array().unwrap().is_empty());

        // 未知 id → 同样空页,不 500
        let (st2, v2) = get(&app, "/api/admin/credentials/999999/usage/records").await;
        assert_eq!(st2, HttpStatusCode::OK);
        assert_eq!(v2["total"], 0);
    }

    #[tokio::test]
    async fn usage_records_shape_and_pagination_camelcase() {
        let stats = empty_stats("usage_shape");
        // 播种 5 条 cred=3 的记录(created_at 递增)
        for i in 1..=5 {
            stats
                .usage
                .record_usage(
                    3,
                    "claude-sonnet-4.5".into(),
                    100,
                    200,
                    0.01 * i as f64,
                    Some("9.9.9.9".into()),
                    Some(10),
                    Some(20),
                    1000 + i,
                )
                .await;
        }
        let app = admin_api_router(state_with_stats(vec![cred("3")], Config::default(), stats));

        // page_size=2 → 3 页;降序,最新 created_at=1005 在首
        let (st, v) = get(
            &app,
            "/api/admin/credentials/3/usage/records?page=1&page_size=2",
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["total"], 5);
        assert_eq!(v["page"], 1);
        assert_eq!(v["pageSize"], 2);
        assert_eq!(v["totalPages"], 3);
        let recs = v["records"].as_array().unwrap();
        assert_eq!(recs.len(), 2);
        let r0 = &recs[0];
        // camelCase 字段齐全
        assert_eq!(r0["model"], "claude-sonnet-4.5");
        assert_eq!(r0["inputTokens"], 100);
        assert_eq!(r0["outputTokens"], 200);
        assert!(r0["estimatedCost"].is_number());
        assert_eq!(r0["cacheReadInputTokens"], 10);
        assert_eq!(r0["cacheCreationInputTokens"], 20);
        assert_eq!(r0["credentialId"], 3);
        assert_eq!(r0["clientIp"], "9.9.9.9");
        // RFC3339 Z 后缀
        assert!(r0["createdAt"].as_str().unwrap().ends_with('Z'));

        // 越界页钳到最后一页
        let (_, vp) = get(
            &app,
            "/api/admin/credentials/3/usage/records?page=99&page_size=2",
        )
        .await;
        assert_eq!(vp["page"], 3);
        assert_eq!(vp["records"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn today_summary_cst_and_unknown_id_zeroed() {
        let stats = empty_stats("today");
        let now = now_unix() as i64;
        // 两条“今天”(用 now 作为落库时刻,必落当日 CST 桶)
        stats
            .usage
            .record_usage(4, "m".into(), 100, 200, 0.5, None, None, None, now)
            .await;
        stats
            .usage
            .record_usage(4, "m".into(), 100, 300, 0.3, None, None, None, now)
            .await;
        let app = admin_api_router(state_with_stats(vec![cred("4")], Config::default(), stats));

        let (st, v) = get(&app, "/api/admin/credentials/4/usage/today").await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["credentialId"], 4);
        assert_eq!(v["totalRequests"], 2);
        assert_eq!(v["totalInputTokens"], 200);
        assert_eq!(v["totalOutputTokens"], 500);
        assert!(v["date"].is_string());
        assert!(v["totalCost"].is_number());

        // 未知 id → 全零,不 500
        let (st2, v2) = get(&app, "/api/admin/credentials/12345/usage/today").await;
        assert_eq!(st2, HttpStatusCode::OK);
        assert_eq!(v2["totalRequests"], 0);
        assert_eq!(v2["credentialId"], 12345);
    }

    #[tokio::test]
    async fn failure_and_throttle_logs_shape_empty_and_populated() {
        let stats = empty_stats("logs");
        stats
            .record_failure(6, "api", 403, "forbidden-body", 2000)
            .await;
        stats
            .record_throttle(6, "mcp", "too-many-requests", 2001)
            .await;
        let app = admin_api_router(state_with_stats(vec![cred("6")], Config::default(), stats));

        // failure logs
        let (st, v) = get(
            &app,
            "/api/admin/credentials/6/failure-logs?page=1&page_size=10",
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["total"], 1);
        assert_eq!(v["page"], 1);
        assert_eq!(v["pageSize"], 10);
        assert_eq!(v["totalPages"], 1);
        let f = &v["records"][0];
        assert_eq!(f["credentialId"], 6);
        assert_eq!(f["requestType"], "api");
        assert_eq!(f["statusCode"], 403);
        assert_eq!(f["responseBody"], "forbidden-body");
        assert!(f["createdAt"].as_str().unwrap().ends_with('Z'));

        // throttle logs
        let (st2, v2) = get(&app, "/api/admin/credentials/6/throttle-logs").await;
        assert_eq!(st2, HttpStatusCode::OK);
        assert_eq!(v2["total"], 1);
        let t = &v2["records"][0];
        assert_eq!(t["statusCode"], 429);
        assert_eq!(t["requestType"], "mcp");

        // 未知 id → 空页
        let (st3, v3) = get(&app, "/api/admin/credentials/999/failure-logs").await;
        assert_eq!(st3, HttpStatusCode::OK);
        assert_eq!(v3["total"], 0);
    }

    #[tokio::test]
    async fn daily_summary_and_daily_records() {
        let stats = empty_stats("daily");
        // 26 CST 与 27 CST 各若干
        let d26 = chrono::DateTime::parse_from_rfc3339("2026-06-26T02:00:00Z")
            .unwrap()
            .timestamp();
        let d27 = chrono::DateTime::parse_from_rfc3339("2026-06-27T02:00:00Z")
            .unwrap()
            .timestamp();
        stats
            .usage
            .record_usage(1, "m".into(), 1, 1, 1.0, None, None, None, d26)
            .await;
        stats
            .usage
            .record_usage(2, "m".into(), 1, 1, 2.0, None, None, None, d27)
            .await;
        stats
            .usage
            .record_usage(1, "m".into(), 1, 1, 3.0, None, None, None, d27)
            .await;
        let app = admin_api_router(state_with_stats(vec![], Config::default(), stats));

        // daily summary:降序,27 在前,跨凭据聚合
        let (st, v) = get(&app, "/api/admin/usage/daily").await;
        assert_eq!(st, HttpStatusCode::OK);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["date"], "2026-06-27");
        assert_eq!(arr[0]["totalRequests"], 2);
        assert!((arr[0]["totalCost"].as_f64().unwrap() - 5.0).abs() < 1e-9);
        assert_eq!(arr[1]["date"], "2026-06-26");

        // daily records for a date
        let (st2, v2) = get(
            &app,
            "/api/admin/usage/daily/2026-06-27/records?page=1&page_size=10",
        )
        .await;
        assert_eq!(st2, HttpStatusCode::OK);
        assert_eq!(v2["total"], 2);
        assert_eq!(v2["records"].as_array().unwrap().len(), 2);

        // 无记录的日期 → 空页
        let (st3, v3) = get(&app, "/api/admin/usage/daily/1999-01-01/records").await;
        assert_eq!(st3, HttpStatusCode::OK);
        assert_eq!(v3["total"], 0);
    }

    #[tokio::test]
    async fn usage_summary_range_and_hours_and_invalid() {
        let stats = empty_stats("usage_summary");
        let now = now_unix() as i64;
        // 三条:近窗内 2 条(59 分钟前、2 小时前),第三条 now-40h(超出 24h 窗口)。
        //
        // 第一条**故意不放在整 1 小时的边界上**。查询侧会自己重新取一次 now,只要这个测试
        // 从建数据到发请求之间跨过了 1 秒,放在 `now-3600` 的那条就会掉出 `hours=1` 的窗口
        // —— 于是测试在 CI 上间歇性变红,而被测代码完全正常。留 60 秒余量。
        // (边界闭区间本身该在能注入时钟的那一层单测,不该靠这条走 HTTP 的用例赌时序。)
        stats
            .usage
            .record_usage_full(
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
                now - 3540,
            )
            .await;
        stats
            .usage
            .record_usage_full(
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
                now - 7200,
            )
            .await;
        stats
            .usage
            .record_usage_full(
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
                now - 40 * 3600,
            )
            .await;
        let app = admin_api_router(state_with_stats(vec![], Config::default(), stats));

        // range=6h:含前两条(1h、2h),不含 10h。
        let (st, v) = get(&app, "/api/admin/usage/summary?range=6h").await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["range"], "6h");
        assert_eq!(v["windowSecs"], 6 * 3600);
        assert_eq!(v["bucketSecs"], 3600);
        assert_eq!(v["totalRequests"], 2);
        assert_eq!(v["totalInputTokens"], 15);
        assert_eq!(v["totalOutputTokens"], 25);
        assert!((v["totalCost"].as_f64().unwrap() - 1.5).abs() < 1e-9);
        assert!((v["totalCredits"].as_f64().unwrap() - 3.0).abs() < 1e-9);
        assert_eq!(v["dailyFallbackApplied"], false);
        assert!(v["series"].is_array());
        // 新增指标(#6):窗口内两条成功、无失败/限流 → errorRate=0、rotationSuccessRate=1。
        // 两条 latency=100+300 → avgLatencyMs=200。
        assert!((v["errorRate"].as_f64().unwrap() - 0.0).abs() < 1e-9);
        assert!((v["rotationSuccessRate"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        assert!((v["avgLatencyMs"].as_f64().unwrap() - 200.0).abs() < 1e-9);
        assert_eq!(v["successfulRequests"], 2);
        assert_eq!(v["failedRequests"], 0);

        // hours=1:只含 59 分钟前那一条。
        let (st2, v2) = get(&app, "/api/admin/usage/summary?hours=1").await;
        assert_eq!(st2, HttpStatusCode::OK);
        assert_eq!(v2["range"], "1h");
        assert_eq!(v2["totalRequests"], 1);

        // 缺省 → 24h,含前两条。
        let (st3, v3) = get(&app, "/api/admin/usage/summary").await;
        assert_eq!(st3, HttpStatusCode::OK);
        assert_eq!(v3["range"], "24h");
        assert_eq!(v3["totalRequests"], 2);

        // 非法 range → 400。
        let (st4, _v4) = get(&app, "/api/admin/usage/summary?range=99z").await;
        assert_eq!(st4, HttpStatusCode::BAD_REQUEST);

        // hours=0 → 400。
        let (st5, _v5) = get(&app, "/api/admin/usage/summary?hours=0").await;
        assert_eq!(st5, HttpStatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn usage_summary_empty_is_zero_not_500() {
        let stats = empty_stats("usage_summary_empty");
        let app = admin_api_router(state_with_stats(vec![], Config::default(), stats));
        let (st, v) = get(&app, "/api/admin/usage/summary?range=30d").await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["totalRequests"], 0);
        assert!((v["totalCost"].as_f64().unwrap() - 0.0).abs() < 1e-9);
        assert!(v["series"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn daily_summary_empty_is_empty_array_not_500() {
        let stats = empty_stats("daily_empty");
        let app = admin_api_router(state_with_stats(vec![], Config::default(), stats));
        let (st, v) = get(&app, "/api/admin/usage/daily").await;
        assert_eq!(st, HttpStatusCode::OK);
        assert!(v.as_array().unwrap().is_empty());
    }

    // ---- Phase 2 API-KEY 管理端点测试 ----

    /// 用给定的 api_keys store 与 stats 构造 MessagesState(其余字段占位)。
    fn state_with_apikeys(
        api_keys: std::sync::Arc<crate::apikey::ApiKeyStore>,
        stats: Arc<StatsManager>,
    ) -> MessagesState {
        MessagesState {
            pool: Arc::new(Mutex::new(Pool::new(vec![], LbMode::Priority))),
            client: reqwest::Client::new(),
            control_client: reqwest::Client::new(),
            cfg: Arc::new(Config::default()),
            runtime_cfg: crate::config::shared_runtime_config(&crate::config::Config::default()),
            endpoint_override: None,
            stats,
            api_keys,
            balance: crate::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
            models_cache: crate::models_cache::ModelsCache::new(),
            builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            log_capture: None,
            refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(
                std::env::temp_dir()
                    .join(format!(
                        "kiro2api_refreshctx_src_admin_handler_rs_{}.json",
                        std::process::id()
                    ))
                    .to_string_lossy()
                    .to_string(),
            ),
        }
    }

    fn empty_apikey_store(tag: &str) -> std::sync::Arc<crate::apikey::ApiKeyStore> {
        let path = std::env::temp_dir().join(format!(
            "kiro2api_admin_apikeys_test_{}_{}_{}.json",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        crate::apikey::ApiKeyStore::load(path)
    }

    async fn send(
        app: &Router,
        method: Method,
        uri: &str,
        body: Option<&str>,
    ) -> (HttpStatusCode, serde_json::Value) {
        let mut b = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(json_body) => {
                b = b.header("content-type", "application/json");
                b.body(Body::from(json_body.to_string())).unwrap()
            }
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    #[tokio::test]
    async fn api_keys_empty_list_is_empty_array() {
        let app = admin_api_router(state_with_apikeys(
            empty_apikey_store("empty_list"),
            empty_stats("ak_empty_list"),
        ));
        let (st, v) = get(&app, "/api/admin/api-keys").await;
        assert_eq!(st, HttpStatusCode::OK);
        assert!(v.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_returns_full_key_and_camelcase_shape() {
        let app = admin_api_router(state_with_apikeys(
            empty_apikey_store("create"),
            empty_stats("ak_create"),
        ));
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/api-keys",
            Some(r#"{"name":"0001","spendingLimit":100,"limitUnit":"usd","boundCredentialIds":[3,7]}"#),
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["id"], 1);
        assert_eq!(v["name"], "0001");
        assert_eq!(v["enabled"], true);
        // 完整明文 key 出现(前端 maskKey/复制依赖),sk- 前缀
        assert!(v["key"].as_str().unwrap().starts_with("sk-"));
        // camelCase 字段齐全,nullable 者以显式值/ null 出现
        assert!(v["createdAt"].as_str().unwrap().ends_with('Z'));
        assert_eq!(v["spendingLimit"], 100.0);
        assert_eq!(v["limitUnit"], "usd");
        assert_eq!(v["expiresAt"], serde_json::Value::Null);
        assert_eq!(v["durationDays"], serde_json::Value::Null);
        assert_eq!(v["activatedAt"], serde_json::Value::Null);
        assert_eq!(v["boundCredentialIds"], serde_json::json!([3, 7]));

        // list 现在含这条,且含完整 key
        let (_, list) = get(&app, "/api/admin/api-keys").await;
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert!(list[0]["key"].as_str().unwrap().starts_with("sk-"));
    }

    #[tokio::test]
    async fn create_with_duration_days_and_no_spending_limit() {
        let app = admin_api_router(state_with_apikeys(
            empty_apikey_store("create_dur"),
            empty_stats("ak_create_dur"),
        ));
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/api-keys",
            Some(r#"{"name":"lazy","durationDays":0.25}"#),
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["durationDays"], 0.25);
        assert_eq!(v["spendingLimit"], serde_json::Value::Null);
        assert_eq!(v["activatedAt"], serde_json::Value::Null);
        assert_eq!(v["expiresAt"], serde_json::Value::Null);
        assert_eq!(v["limitUnit"], "usd"); // 默认单位
    }

    #[tokio::test]
    async fn update_toggles_enabled_and_clears_fields() {
        let store = empty_apikey_store("update");
        let created = store.create(
            "n".into(),
            None,
            Some(50.0),
            Some("credits".into()),
            None,
            None,
            now_utc(),
        );
        let app = admin_api_router(state_with_apikeys(store, empty_stats("ak_update")));

        // 停用 + 清空 spendingLimit(显式 null)
        let (st, v) = send(
            &app,
            Method::PUT,
            &format!("/api/admin/api-keys/{}", created.id),
            Some(r#"{"enabled":false,"spendingLimit":null,"name":"renamed"}"#),
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["enabled"], false);
        assert_eq!(v["name"], "renamed");
        assert_eq!(v["spendingLimit"], serde_json::Value::Null);
        // 未提及的 limitUnit 不变
        assert_eq!(v["limitUnit"], "credits");

        // 未知 id → 404,不 500
        let (st2, _) = send(
            &app,
            Method::PUT,
            "/api/admin/api-keys/999999",
            Some(r#"{"name":"x"}"#),
        )
        .await;
        assert_eq!(st2, HttpStatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_expires_at_roundtrip() {
        let store = empty_apikey_store("update_exp");
        let created = store.create("n".into(), None, None, None, None, None, now_utc());
        let app = admin_api_router(state_with_apikeys(store, empty_stats("ak_update_exp")));

        let (st, v) = send(
            &app,
            Method::PUT,
            &format!("/api/admin/api-keys/{}", created.id),
            Some(r#"{"expiresAt":"2027-01-01T00:00:00Z"}"#),
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["expiresAt"], "2027-01-01T00:00:00Z");
    }

    #[tokio::test]
    async fn delete_known_and_unknown() {
        let store = empty_apikey_store("delete");
        let created = store.create("n".into(), None, None, None, None, None, now_utc());
        let app = admin_api_router(state_with_apikeys(store, empty_stats("ak_delete")));

        let (st, v) = send(
            &app,
            Method::DELETE,
            &format!("/api/admin/api-keys/{}", created.id),
            None,
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["success"], true);

        // 再删 → 404
        let (st2, _) = send(
            &app,
            Method::DELETE,
            &format!("/api/admin/api-keys/{}", created.id),
            None,
        )
        .await;
        assert_eq!(st2, HttpStatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn all_usage_and_per_key_usage_shape_and_credits() {
        let stats = empty_stats("ak_usage");
        // key_id=5 播种两条(cred=1),两种模型
        stats
            .usage
            .record_usage_with_api_key(
                1,
                5,
                "claude-opus-4.6".into(),
                100,
                200,
                0.72,
                None,
                None,
                None,
                1000,
            )
            .await;
        stats
            .usage
            .record_usage_with_api_key(
                1,
                5,
                "claude-sonnet-4.5".into(),
                10,
                20,
                0.36,
                None,
                None,
                None,
                1001,
            )
            .await;
        let app = admin_api_router(state_with_apikeys(empty_apikey_store("ak_usage"), stats));

        // 全部汇总
        let (st, v) = get(&app, "/api/admin/api-keys/usage").await;
        assert_eq!(st, HttpStatusCode::OK);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let s = &arr[0];
        assert_eq!(s["apiKeyId"], 5);
        assert_eq!(s["totalRequests"], 2);
        assert_eq!(s["totalInputTokens"], 110);
        assert_eq!(s["totalOutputTokens"], 220);
        assert!((s["totalCost"].as_f64().unwrap() - 1.08).abs() < 1e-9);
        // totalCredits 取上游回报的真实值之和,**不由 cost 反算**。
        // 反算(1.08/0.72=1.5)是另一个量纲的数字,与账单无关。
        assert!((s["totalCredits"].as_f64().unwrap() - 0.0).abs() < 1e-9);
        // byModel camelCase,按模型名升序
        let by = s["byModel"].as_array().unwrap();
        assert_eq!(by.len(), 2);
        assert_eq!(by[0]["model"], "claude-opus-4.6");
        assert_eq!(by[0]["inputTokens"], 100);
        assert_eq!(by[0]["outputTokens"], 200);
        assert!(by[0]["cost"].is_number());
        // totalCreditsSaved 省略
        assert!(s.get("totalCreditsSaved").is_none());

        // 单 key 汇总(静态段 usage 未被 {id} 吞掉)
        let (st2, s2) = get(&app, "/api/admin/api-keys/5/usage").await;
        assert_eq!(st2, HttpStatusCode::OK);
        assert_eq!(s2["apiKeyId"], 5);
        assert_eq!(s2["totalRequests"], 2);

        // 未知 key → 全零,不 500
        let (st3, s3) = get(&app, "/api/admin/api-keys/999/usage").await;
        assert_eq!(st3, HttpStatusCode::OK);
        assert_eq!(s3["apiKeyId"], 999);
        assert_eq!(s3["totalRequests"], 0);
        assert_eq!(s3["totalCredits"], 0.0);
        assert!(s3["byModel"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn usage_records_paginated_and_reset() {
        let stats = empty_stats("ak_records");
        for i in 1..=5 {
            stats
                .usage
                .record_usage_with_api_key(
                    2,
                    9,
                    "claude-sonnet-4.5".into(),
                    100,
                    200,
                    0.01 * i as f64,
                    Some("9.9.9.9".into()),
                    Some(10),
                    Some(20),
                    1000 + i,
                )
                .await;
        }
        let app = admin_api_router(state_with_apikeys(empty_apikey_store("ak_records"), stats));

        // 分页,降序 camelCase
        let (st, v) = get(
            &app,
            "/api/admin/api-keys/9/usage/records?page=1&page_size=2",
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["total"], 5);
        assert_eq!(v["page"], 1);
        assert_eq!(v["pageSize"], 2);
        assert_eq!(v["totalPages"], 3);
        let recs = v["records"].as_array().unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0]["model"], "claude-sonnet-4.5");
        assert_eq!(recs[0]["inputTokens"], 100);
        assert_eq!(recs[0]["clientIp"], "9.9.9.9");
        assert!(recs[0]["createdAt"].as_str().unwrap().ends_with('Z'));

        // 未知 id → 空页,不 500
        let (st2, v2) = get(&app, "/api/admin/api-keys/123456/usage/records").await;
        assert_eq!(st2, HttpStatusCode::OK);
        assert_eq!(v2["total"], 0);

        // 重置清空
        let (st3, v3) = send(&app, Method::DELETE, "/api/admin/api-keys/9/usage", None).await;
        assert_eq!(st3, HttpStatusCode::OK);
        assert_eq!(v3["success"], true);
        let (_, v4) = get(&app, "/api/admin/api-keys/9/usage/records").await;
        assert_eq!(v4["total"], 0);

        // 重置未知 id 仍 200(幂等),不 404/500
        let (st5, v5) = send(&app, Method::DELETE, "/api/admin/api-keys/777/usage", None).await;
        assert_eq!(st5, HttpStatusCode::OK);
        assert_eq!(v5["success"], true);
    }

    // ---- Phase 5 运行期可变配置端点测试 --------------------------------

    /// 唯一临时 config.json 路径(每测试隔离)。
    fn tmp_config_path(tag: &str) -> std::path::PathBuf {
        crate::test_tmp::file(&format!("admin_p5cfg_{tag}"), "config.json")
    }

    /// 构造带指定 runtime_cfg 的 admin router:runtime_cfg 的 config_path 指向 `cfg_path`,
    /// api_key/admin_api_key 取自传入 Config。用于设置端点的读/写/落盘验证。
    fn settings_app(
        cfg: Config,
        cfg_path: &std::path::Path,
    ) -> (Router, crate::config::SharedRuntimeConfig) {
        let mut state = state_with(vec![cred("1"), cred("2")], cfg.clone());
        let mut rc = crate::config::RuntimeConfig::from_config(&cfg);
        rc.config_path = cfg_path.to_string_lossy().into_owned();
        let shared = std::sync::Arc::new(parking_lot::RwLock::new(rc));
        state.runtime_cfg = shared.clone();
        (admin_api_router(state.clone()), shared)
    }

    #[tokio::test]
    async fn get_load_balancing_returns_current_mode() {
        let cfg = Config {
            load_balancing_mode: "balanced".into(),
            ..Config::default()
        };
        let path = tmp_config_path("lb_get");
        let (app, _rc) = settings_app(cfg, &path);
        let (st, v) = get(&app, "/api/admin/config/load-balancing").await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["mode"], "balanced");
    }

    #[tokio::test]
    async fn put_load_balancing_updates_runtime_pool_and_persists() {
        let path = tmp_config_path("lb_put");
        std::fs::write(&path, "{}").unwrap();
        let (app, rc) = settings_app(Config::default(), &path);
        // 初始 priority。
        let (st, v) = put(
            &app,
            "/api/admin/config/load-balancing",
            r#"{"mode":"balanced"}"#,
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["mode"], "balanced");
        // 运行期已更新。
        assert_eq!(rc.read().load_balancing_mode, "balanced");
        // 落盘为 camelCase。
        let disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(disk["loadBalancingMode"], "balanced");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn put_load_balancing_rejects_invalid_mode_400() {
        let path = tmp_config_path("lb_bad");
        std::fs::write(&path, "{}").unwrap();
        let (app, _rc) = settings_app(Config::default(), &path);
        let (st, _v) = put(
            &app,
            "/api/admin/config/load-balancing",
            r#"{"mode":"weird"}"#,
        )
        .await;
        assert_eq!(st, HttpStatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn get_auth_keys_masks_values_and_never_leaks_plaintext() {
        let cfg = Config {
            api_key: Some("sk-abcdef123456".into()),
            admin_api_key: Some("adm-secret9999".into()),
            ..Config::default()
        };
        let path = tmp_config_path("ak_get");
        let (app, _rc) = settings_app(cfg, &path);
        let (st, v) = get(&app, "/api/admin/config/auth-keys").await;
        assert_eq!(st, HttpStatusCode::OK);
        let api = v["apiKey"].as_str().unwrap();
        let adm = v["adminApiKey"].as_str().unwrap();
        // 脱敏:以 *** 收尾,绝不含完整明文。
        assert!(api.ends_with("***"));
        assert!(adm.ends_with("***"));
        assert!(!api.contains("sk-abcdef123456"));
        assert!(!adm.contains("adm-secret9999"));
    }

    #[tokio::test]
    async fn put_auth_keys_rotates_and_persists_only_provided_field() {
        let cfg = Config {
            api_key: Some("sk-old".into()),
            admin_api_key: Some("adm-old".into()),
            ..Config::default()
        };
        let path = tmp_config_path("ak_put");
        std::fs::write(&path, "{}").unwrap();
        let (app, rc) = settings_app(cfg, &path);
        // 仅改主 key。
        let (st, v) = put(
            &app,
            "/api/admin/config/auth-keys",
            r#"{"apiKey":"sk-new"}"#,
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["success"], true);
        assert_eq!(rc.read().api_key.as_deref(), Some("sk-new"));
        // admin key 未动。
        assert_eq!(rc.read().admin_api_key.as_deref(), Some("adm-old"));
        // 落盘 camelCase。
        let disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(disk["apiKey"], "sk-new");
        assert_eq!(disk["adminApiKey"], "adm-old");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn put_auth_keys_empty_value_rejected_400() {
        let path = tmp_config_path("ak_empty");
        std::fs::write(&path, "{}").unwrap();
        let (app, _rc) = settings_app(Config::default(), &path);
        let (st, _v) = put(&app, "/api/admin/config/auth-keys", r#"{"apiKey":"   "}"#).await;
        assert_eq!(st, HttpStatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn server_info_shape_and_master_key_null_when_unset() {
        let path = tmp_config_path("si_unset");
        let (app, _rc) = settings_app(Config::default(), &path);
        let (st, v) = get(&app, "/api/admin/server-info").await;
        assert_eq!(st, HttpStatusCode::OK);
        assert!(v["masterApiKey"].is_null());
        // version = 本服务 crate 版本;伪装 Kiro 版本另放 kiroVersion(默认 0.11.107)。
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["kiroVersion"], "0.11.107");
        // 系统指标字段(camelCase)存在且形状正确。
        assert!(v["serverTime"].is_string());
        assert!(v["serverTimeUnix"].is_i64());
        assert!(v["os"].is_string());
        assert!(v["runMode"].is_string());
        assert!(v["pid"].is_u64());
        assert!(v["uptimeSecs"].is_u64());
        // memory/cpu 在 Linux 为数值、非 Linux 为 null;两种都合法(不 panic 即达标)。
        assert!(v["memoryUsedBytes"].is_u64() || v["memoryUsedBytes"].is_null());
        assert!(v["memoryTotalBytes"].is_u64() || v["memoryTotalBytes"].is_null());
        assert!(v["cpuPercent"].is_f64() || v["cpuPercent"].is_null());
        // serverTime 形如 'YYYY/MM/DD HH:MM:SS'(19 字符)。
        let st_str = v["serverTime"].as_str().unwrap();
        assert_eq!(st_str.len(), 19, "serverTime = {st_str}");
    }

    #[tokio::test]
    async fn server_info_reports_full_master_key_when_set() {
        let cfg = Config {
            api_key: Some("sk-master".into()),
            ..Config::default()
        };
        let path = tmp_config_path("si_set");
        let (app, _rc) = settings_app(cfg, &path);
        let (st, v) = get(&app, "/api/admin/server-info").await;
        assert_eq!(st, HttpStatusCode::OK);
        // 返回完整主 Key:前端 api-keys-panel 自行以 maskKey() 脱敏显示,
        // 「复制」按钮需要完整值。此端点已在 admin 鉴权闸之后,仅管理员可见。
        assert_eq!(v["masterApiKey"], "sk-master");
    }

    /// PUT helper(复用 send)。
    async fn put(app: &Router, uri: &str, body: &str) -> (HttpStatusCode, serde_json::Value) {
        send(app, Method::PUT, uri, Some(body)).await
    }

    // ============ Phase 3 凭据 CRUD 端点测试 ============

    #[tokio::test]
    async fn add_credential_persists_and_assigns_numeric_id() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let state = state_with(vec![cred("5")], cfg);
        let app = admin_api_router(state);
        let (status, v) = send(
            &app,
            Method::POST,
            "/api/admin/credentials",
            Some(
                r#"{"refreshToken":"rt-new","authMethod":"social","email":"a@b.com","priority":3}"#,
            ),
        )
        .await;
        assert_eq!(status, HttpStatusCode::OK);
        assert_eq!(v["success"], true);
        assert_eq!(v["credentialId"], 6); // max(5)+1
        assert_eq!(v["email"], "a@b.com");
        // 落盘校验:文件里含新凭据(id=6, weight=3 来自 priority)
        let saved = crate::kiro::credential::load(&path).unwrap();
        assert!(
            saved
                .iter()
                .any(|c| c.id == "6" && c.weight == 3 && c.refresh_token == "rt-new")
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn add_credential_rejects_empty_refresh_token() {
        let state = state_with(vec![], cfg_with_temp_creds());
        let app = admin_api_router(state);
        let (status, v) = send(
            &app,
            Method::POST,
            "/api/admin/credentials",
            Some(r#"{"refreshToken":"   "}"#),
        )
        .await;
        assert_eq!(status, HttpStatusCode::BAD_REQUEST);
        assert_eq!(v["success"], false);
    }

    #[tokio::test]
    async fn add_credential_idc_requires_client_id_and_secret() {
        let state = state_with(vec![], cfg_with_temp_creds());
        let app = admin_api_router(state);
        let (status, _) = send(
            &app,
            Method::POST,
            "/api/admin/credentials",
            Some(r#"{"refreshToken":"rt","authMethod":"idc","clientId":"cid"}"#),
        )
        .await;
        assert_eq!(status, HttpStatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_credential_changes_fields_and_persists() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let state = state_with(vec![cred("7")], cfg);
        let app = admin_api_router(state);
        let (status, v) = put(
            &app,
            "/api/admin/credentials/7",
            r#"{"email":"upd@x.com","weight":9,"apiRegion":"eu-west-1"}"#,
        )
        .await;
        assert_eq!(status, HttpStatusCode::OK);
        assert_eq!(v["success"], true);
        let saved = crate::kiro::credential::load(&path).unwrap();
        let c = saved.iter().find(|c| c.id == "7").unwrap();
        assert_eq!(c.email.as_deref(), Some("upd@x.com"));
        assert_eq!(c.weight, 9);
        assert_eq!(c.region, "eu-west-1");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn update_credential_unknown_id_is_404() {
        let state = state_with(vec![cred("1")], cfg_with_temp_creds());
        let app = admin_api_router(state);
        let (status, _) = put(&app, "/api/admin/credentials/999", r#"{"email":"x@y.com"}"#).await;
        assert_eq!(status, HttpStatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_credential_removes_and_persists() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let state = state_with(vec![cred("3"), cred("4")], cfg);
        let app = admin_api_router(state);
        let (status, v) = send(&app, Method::DELETE, "/api/admin/credentials/3", None).await;
        assert_eq!(status, HttpStatusCode::OK);
        assert_eq!(v["success"], true);
        let saved = crate::kiro::credential::load(&path).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].id, "4");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn delete_credential_unknown_id_is_404() {
        let state = state_with(vec![cred("1")], cfg_with_temp_creds());
        let app = admin_api_router(state);
        let (status, _) = send(&app, Method::DELETE, "/api/admin/credentials/999", None).await;
        assert_eq!(status, HttpStatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_priority_persists_the_priority_field() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let state = state_with(vec![cred("2")], cfg);
        let app = admin_api_router(state);
        let (status, v) = send(
            &app,
            Method::POST,
            "/api/admin/credentials/2/priority",
            Some(r#"{"priority":8}"#),
        )
        .await;
        assert_eq!(status, HttpStatusCode::OK);
        assert_eq!(v["success"], true);
        let saved = crate::kiro::credential::load(&path).unwrap();
        assert_eq!(saved.iter().find(|c| c.id == "2").unwrap().priority, 8);
        let _ = std::fs::remove_file(&path);
        // 未知 id → 404
        let (status2, _) = send(
            &app,
            Method::POST,
            "/api/admin/credentials/999/priority",
            Some(r#"{"priority":1}"#),
        )
        .await;
        assert_eq!(status2, HttpStatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn reset_failure_clears_and_is_transient() {
        let state = state_with(vec![cred("1")], cfg_with_temp_creds());
        let app = admin_api_router(state);
        let (status, v) = send(&app, Method::POST, "/api/admin/credentials/1/reset", None).await;
        assert_eq!(status, HttpStatusCode::OK);
        assert_eq!(v["success"], true);
        // 未知 id → 404
        let (status2, _) = send(&app, Method::POST, "/api/admin/credentials/999/reset", None).await;
        assert_eq!(status2, HttpStatusCode::NOT_FOUND);
    }

    /// 重置必须**立刻落盘**。封禁是持久结论,账号被挡在池外后永远等不到一次成功来清它,
    /// 重置是唯一出口;只改内存的话,下次重启会从盘上把封禁读回来,账号又被挡住,
    /// 而运维明明已经点过重置了。
    /// 两个计数曾经错位:`failureCount` 装 `strikes`(成功一次即清零),`throttleCount` 装
    /// `failures`(累计失败数,与限流无关)。于是被上游封禁的账号在面板上显示成「限流 1、
    /// 失败 0」——两个数都在说假话,还把"账号被停用"错报成"歇一会儿就好"。
    #[tokio::test]
    async fn failure_and_throttle_counts_are_not_swapped() {
        let dir = tmp_dir("counter-mapping");
        let state = state_with_data_dir_creds(&dir, vec![cred("1")]);
        // 两次失败、零次限流
        {
            let mut pool = state.pool.lock().await;
            for _ in 0..2 {
                pool.report_failure_with_reason(
                    "1",
                    crate::kiro::pool::FailureKind::Transient,
                    crate::kiro::pool::StatusReason::None,
                    0,
                );
            }
        }
        let app = admin_api_router(state);
        let (status, v) = send(&app, Method::GET, "/api/admin/credentials", None).await;
        assert_eq!(status, HttpStatusCode::OK);
        let c = &v["credentials"][0];
        assert_eq!(c["failureCount"], 2, "失败列须是累计失败数");
        assert_eq!(c["throttleCount"], 0, "没发生过限流,限流列不得借用失败数");
    }

    #[tokio::test]
    async fn reset_persists_so_a_restart_does_not_resurrect_the_ban() {
        let dir = tmp_dir("reset-persist");
        let state = state_with_data_dir_creds(&dir, vec![cred("1")]);
        let creds_path = state.cfg.credentials_path.clone();
        state.pool.lock().await.report_failure_with_reason(
            "1",
            crate::kiro::pool::FailureKind::AuthAmbiguous,
            crate::kiro::pool::StatusReason::Banned,
            0,
        );
        let app = admin_api_router(state);

        let (status, _) = send(&app, Method::POST, "/api/admin/credentials/1/reset", None).await;
        assert_eq!(status, HttpStatusCode::OK);

        // 从盘上读回来——模拟重启
        let on_disk = crate::kiro::credential::load(&creds_path).expect("凭据文件应可读");
        assert_eq!(
            on_disk[0].status_reason, None,
            "重置没落盘,重启后封禁结论会复活"
        );
    }

    #[tokio::test]
    async fn add_then_list_reflects_new_credential() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let state = state_with(vec![], cfg);
        let app = admin_api_router(state);
        let (s1, _) = send(
            &app,
            Method::POST,
            "/api/admin/credentials",
            Some(r#"{"refreshToken":"rt1"}"#),
        )
        .await;
        assert_eq!(s1, HttpStatusCode::OK);
        // 列表端点应反映活池新增(共享同一 pool)
        let (s2, v) = send(&app, Method::GET, "/api/admin/credentials", None).await;
        assert_eq!(s2, HttpStatusCode::OK);
        assert_eq!(v["total"], 1);
        assert_eq!(v["credentials"][0]["id"], 1);
        let _ = std::fs::remove_file(&path);
    }

    // ---- Phase 3 批量/KAM 导入端点测试 ----

    #[tokio::test]
    async fn batch_import_array_of_flat_credentials() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let app = admin_api_router(state_with(vec![], cfg));
        let body = r#"{"data":[{"refreshToken":"rt-a","email":"a@x.io"},{"refreshToken":"rt-b"}]}"#;
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/credentials/batch-import",
            Some(body),
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["success"], true);
        assert_eq!(v["total"], 2);
        assert_eq!(v["added"], 2);
        assert_eq!(v["failed"], 0);
        assert_eq!(v["results"][0]["status"], "added");
        assert_eq!(v["results"][0]["email"], "a@x.io");
        assert_eq!(v["results"][1]["status"], "added");
        // 落库反映到列表
        let (_, list) = send(&app, Method::GET, "/api/admin/credentials", None).await;
        assert_eq!(list["total"], 2);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn batch_import_kam_export_format_nested_credentials() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let app = admin_api_router(state_with(vec![], cfg));
        // KAM 标准:{version, accounts:[{email, machineId, credentials:{refreshToken, clientId, clientSecret, region}}]}
        let body = r#"{"data":{"version":1,"accounts":[
            {"email":"kam@x.io","machineId":"m1","credentials":{"refreshToken":"rt-kam","clientId":"cid","clientSecret":"csec","region":"us-west-2"}}
        ]}}"#;
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/credentials/batch-import",
            Some(body),
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["total"], 1);
        assert_eq!(v["added"], 1);
        assert_eq!(v["results"][0]["email"], "kam@x.io");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn batch_import_per_item_resilience_missing_token_and_bad_idc() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let app = admin_api_router(state_with(vec![], cfg));
        // item1 ok; item2 无 refreshToken; item3 idc 只给 clientId(缺 clientSecret); item4 非对象
        let body = r#"{"data":[
            {"refreshToken":"rt-ok"},
            {"email":"no-token@x.io"},
            {"refreshToken":"rt-idc","clientId":"cid"},
            "not-an-object"
        ]}"#;
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/credentials/batch-import",
            Some(body),
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["success"], false);
        assert_eq!(v["total"], 4);
        assert_eq!(v["added"], 1);
        assert_eq!(v["failed"], 3);
        assert_eq!(v["results"][0]["status"], "added");
        assert_eq!(v["results"][1]["status"], "failed");
        assert_eq!(v["results"][2]["status"], "failed");
        assert_eq!(v["results"][3]["status"], "failed");
        // 只有合法那条落库
        let (_, list) = send(&app, Method::GET, "/api/admin/credentials", None).await;
        assert_eq!(list["total"], 1);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn batch_import_single_object_and_empty_accounts() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let app = admin_api_router(state_with(vec![], cfg));
        // 单对象(顶层扁平)
        let (st1, v1) = send(
            &app,
            Method::POST,
            "/api/admin/credentials/batch-import",
            Some(r#"{"data":{"refreshToken":"rt-solo"}}"#),
        )
        .await;
        assert_eq!(st1, HttpStatusCode::OK);
        assert_eq!(v1["total"], 1);
        assert_eq!(v1["added"], 1);
        // 空 accounts → total 0、success true
        let (st2, v2) = send(
            &app,
            Method::POST,
            "/api/admin/credentials/batch-import",
            Some(r#"{"data":{"version":1,"accounts":[]}}"#),
        )
        .await;
        assert_eq!(st2, HttpStatusCode::OK);
        assert_eq!(v2["total"], 0);
        assert_eq!(v2["added"], 0);
        assert_eq!(v2["success"], true);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn batch_import_unrecognized_payload_is_400() {
        let app = admin_api_router(state_with(vec![], cfg_with_temp_creds()));
        // data 是标量,既非数组也非对象 → 400
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/credentials/batch-import",
            Some(r#"{"data":42}"#),
        )
        .await;
        assert_eq!(st, HttpStatusCode::BAD_REQUEST);
        assert_eq!(v["success"], false);
    }

    // ============ Phase 3 交互式登录流端点 ============

    #[tokio::test]
    async fn builderid_poll_unknown_session_is_404() {
        let app = admin_api_router(state_with(vec![], cfg_with_temp_creds()));
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/login/builderid/poll",
            Some(r#"{"sessionId":"nope"}"#),
        )
        .await;
        assert_eq!(st, HttpStatusCode::NOT_FOUND);
        assert_eq!(v["success"], false);
    }

    #[tokio::test]
    async fn iam_sso_complete_unknown_session_is_404() {
        let app = admin_api_router(state_with(vec![], cfg_with_temp_creds()));
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/login/iam-sso/complete",
            Some(r#"{"sessionId":"nope","callbackUrl":"http://127.0.0.1/oauth/callback?code=c&state=s"}"#),
        )
        .await;
        assert_eq!(st, HttpStatusCode::NOT_FOUND);
        assert_eq!(v["success"], false);
    }

    #[tokio::test]
    async fn iam_sso_start_empty_start_url_is_400() {
        let app = admin_api_router(state_with(vec![], cfg_with_temp_creds()));
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/login/iam-sso/start",
            Some(r#"{"startUrl":"   "}"#),
        )
        .await;
        assert_eq!(st, HttpStatusCode::BAD_REQUEST);
        assert_eq!(v["success"], false);
    }

    #[tokio::test]
    async fn sso_token_empty_input_adds_nothing() {
        // 全空白输入 → 拆行后无有效 bearer,added=0/failed=[],不触网,不 500。
        let app = admin_api_router(state_with(vec![], cfg_with_temp_creds()));
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/login/sso-token",
            Some(r#"{"bearerToken":"\n   \n"}"#),
        )
        .await;
        assert_eq!(st, HttpStatusCode::OK);
        assert_eq!(v["added"], 0);
        assert!(v["failed"].as_array().unwrap().is_empty());
    }

    /// 瞬态失败(上游不可达)必须保留会话:用户可能已经在浏览器点了授权,
    /// 一次网络抖动不能把他打回 /start 重来。poll 命中会话(非 404),回非终态让前端接着轮询。
    /// 完整成功链见 kiro::login::builderid 的 wiremock 单测。
    #[tokio::test]
    async fn builderid_poll_transient_upstream_failure_keeps_session() {
        let state = state_with_unreachable_upstream(cfg_with_temp_creds());
        let pending = crate::kiro::login::builderid::Pending {
            user_code: "ABCD-1234".into(),
            verification_uri: "https://view.awsapps.com/start/#/device".into(),
            device_code: "dev".into(),
            interval_secs: 5,
            client_id: "cid".into(),
            client_secret: "csec".into(),
        };
        let sessions = state.builderid_sessions.clone();
        let session_id = sessions.put(
            BuilderIdSession {
                pending,
                region: "us-east-1".into(),
            },
            super::now_unix(),
        );
        let app = admin_api_router(state);
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/login/builderid/poll",
            Some(&format!(r#"{{"sessionId":"{session_id}"}}"#)),
        )
        .await;
        assert_ne!(st, HttpStatusCode::NOT_FOUND);
        // 非终态:前端据此按 interval 继续轮询(非 2xx 会让前端停在错误上)。
        assert_eq!(st, HttpStatusCode::OK, "{v}");
        assert_eq!(v["completed"], false);
        assert_eq!(v["status"], "pending");
        assert_eq!(v["interval"], 5);
        // 会话仍在,下一拍还能接着轮询。
        assert!(
            sessions.get(&session_id, super::now_unix()).is_some(),
            "瞬态失败不得销毁登录会话"
        );
    }

    // ============ 系统指标解析器单测 ============

    #[test]
    fn parse_kb_line_extracts_vmrss() {
        let status = "Name:\tkiro2api\nVmPeak:\t  123456 kB\nVmRSS:\t   94208 kB\nThreads:\t8\n";
        assert_eq!(super::parse_kb_line(status, "VmRSS:"), Some(94208));
    }

    #[test]
    fn parse_kb_line_extracts_memtotal() {
        let meminfo = "MemTotal:        2013840 kB\nMemFree:          123456 kB\n";
        assert_eq!(super::parse_kb_line(meminfo, "MemTotal:"), Some(2013840));
    }

    #[test]
    fn parse_kb_line_missing_prefix_is_none() {
        let meminfo = "MemFree:          123456 kB\n";
        assert_eq!(super::parse_kb_line(meminfo, "MemTotal:"), None);
    }

    #[test]
    fn parse_kb_line_non_numeric_is_none() {
        let bad = "VmRSS:\t  notanumber kB\n";
        assert_eq!(super::parse_kb_line(bad, "VmRSS:"), None);
    }

    #[test]
    fn parse_proc_stat_cpu_sums_total_and_idle() {
        // 典型 /proc/stat:聚合 'cpu ' 行 + 带编号行(应忽略后者)。
        // user=100 nice=20 system=30 idle=800 iowait=40 irq=5 softirq=5 steal=0
        let content = "cpu  100 20 30 800 40 5 5 0 0 0\n\
                       cpu0 50 10 15 400 20 2 2 0 0 0\n\
                       intr 12345\n";
        let t = super::parse_proc_stat_cpu(content).unwrap();
        // total = 100+20+30+800+40+5+5+0+0+0 = 1000
        assert_eq!(t.total, 1000);
        // idle = idle(800) + iowait(40) = 840
        assert_eq!(t.idle, 840);
    }

    #[test]
    fn parse_proc_stat_cpu_missing_line_is_none() {
        // 只有带编号行、没有聚合 'cpu ' 行 → None。
        let content = "cpu0 50 10 15 400 20 2 2 0\nintr 1\n";
        assert!(super::parse_proc_stat_cpu(content).is_none());
    }

    #[test]
    fn parse_proc_stat_cpu_too_few_fields_is_none() {
        // 不足 4 个数值(缺 idle)→ None。
        let content = "cpu  100 20 30\n";
        assert!(super::parse_proc_stat_cpu(content).is_none());
    }

    #[test]
    fn cpu_busy_percent_computes_and_rounds() {
        // Δtotal=1000,Δidle=800 → busy=(1000-800)/1000*100 = 20.0%
        let t0 = super::CpuTimes { total: 0, idle: 0 };
        let t1 = super::CpuTimes {
            total: 1000,
            idle: 800,
        };
        assert!((super::cpu_busy_percent(t0, t1) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn cpu_busy_percent_zero_delta_is_zero() {
        // 采样窗口内 total 无变化 → 0.0(不除零、不 NaN)。
        let t = super::CpuTimes {
            total: 500,
            idle: 300,
        };
        assert_eq!(super::cpu_busy_percent(t, t), 0.0);
    }

    #[test]
    fn cpu_busy_percent_clamps_to_100() {
        // 极端错序(idle 减少多于 total)不应超过 100。
        let t0 = super::CpuTimes {
            total: 0,
            idle: 500,
        };
        let t1 = super::CpuTimes {
            total: 1000,
            idle: 0,
        };
        // Δtotal=1000,Δidle=0(saturating)→ busy=100.0
        assert_eq!(super::cpu_busy_percent(t0, t1), 100.0);
    }

    /// 采样窗口必须让出 worker:单线程运行时上两次并发采样应当重叠完成(约 100ms),
    /// 同步 sleep 则会串成约 200ms —— 那正是"一个请求卡住同线程其它请求"的现场。
    #[tokio::test]
    async fn cpu_sampling_window_does_not_block_the_worker() {
        let began = std::time::Instant::now();
        let _ = tokio::join!(
            super::sample_process_cpu_percent(),
            super::sample_process_cpu_percent()
        );
        let elapsed = began.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(190),
            "两次并发采样耗时 {elapsed:?},采样窗口仍在阻塞线程"
        );
    }

    // ============ 重启前刷盘 / 登录会话韧性 / 删除后缓存失效 ============

    /// 建一个"上游必然不可达"的 state:出站全部指向本地无人监听的代理端口,使登录流的
    /// 网络调用确定性地以传输层错误告终 —— 既不打真实 AWS,也不看 CI 有没有网。
    fn state_with_unreachable_upstream(cfg: Config) -> MessagesState {
        let mut state = state_with(vec![], cfg);
        state.client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1").expect("proxy url"))
            .build()
            .expect("client");
        state
    }

    /// 唯一临时目录(每测试隔离)。
    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        crate::test_tmp::dir(&format!("admin_{tag}"))
    }

    /// 生产同款接线的 state:统计、API-KEY 存储、余额缓存、credentials.json 全落在同一个
    /// 数据目录下,使 `api_keys_path_from(cfg.credentials_path)` 指向 store 自己读写的那份文件
    /// (`state_with` 默认把 store 放在无关的临时路径上,验不了退出前刷盘的路径规约)。
    fn state_with_data_dir(dir: &std::path::Path) -> MessagesState {
        state_with_data_dir_creds(dir, vec![])
    }

    /// 同 [`state_with_data_dir`],但可带一池账号(验删账号 → 重启这条链路要真删得动)。
    fn state_with_data_dir_creds(dir: &std::path::Path, creds: Vec<Credential>) -> MessagesState {
        let creds_path = dir.join("credentials.json").to_string_lossy().into_owned();
        let cfg = Config {
            credentials_path: creds_path.clone(),
            ..Config::default()
        };
        let mut state = state_with_stats(creds, cfg, StatsManager::load_from_dir(dir));
        state.api_keys =
            crate::apikey::ApiKeyStore::load(crate::apikey::api_keys_path_from(&creds_path));
        // 余额缓存也必须落在本测试目录:生产由同一数据目录推断,默认助手把它放在共享的
        // 系统临时目录上(测试之间会串味,也验不了"重启后从盘上读回来的是什么")。
        state.balance = crate::balance::BalanceCache::load_from_dir(dir);
        state
    }

    /// 等各存储的后台刷盘循环走完 `tokio::time::interval` 的**立即首拍**。
    ///
    /// 为什么非等不可:首拍是零延迟触发的,若测试在它之前就置了脏,首拍会当场把文件写出去,
    /// 于是"退出前显式刷盘"的回归测试即便在刷盘被摘掉的情况下也照样变绿(靠的是后台那一拍,
    /// 不是被测代码)。先空跑掉首拍(此刻还没有脏数据,它什么都不写),之后的 5s 内再置脏
    /// 只会走"收到通知不落盘"的去抖分支,断言因而是确定性的。
    async fn settle_flush_loops() {
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    }

    /// 退出前必须把去抖窗口内的记录落盘:后台刷盘每约 5s 才一拍,restart 直接 exit(0)
    /// 不会给它最后一拍的机会,不显式刷盘就静默丢掉最近这批用量/计费。
    #[tokio::test]
    async fn flush_before_exit_persists_debounced_usage_records() {
        let dir = tmp_dir("flush_exit");
        let state = state_with_data_dir(&dir);
        state
            .stats
            .usage
            .record_usage(
                7,
                "claude-sonnet-4".into(),
                10,
                20,
                0.5,
                None,
                None,
                None,
                1_700_000_000,
            )
            .await;
        let path = dir.join("usage_records.json");
        // 去抖窗口内:后台循环还没到下一拍,盘上什么都没有。
        assert!(!path.exists(), "去抖窗口内不该已落盘");
        super::flush_persistent_state_before_exit(&state).await;
        let disk = std::fs::read_to_string(&path).expect("退出前刷盘应已写出 usage_records.json");
        assert!(disk.contains("claude-sonnet-4"), "落盘内容缺记录: {disk}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关键回归(安全):管理面「重启」前的刷盘范围必须**包含 API-KEY 存储**。
    ///
    /// 现场:管理员发现某把 key 泄露 → 点删除(只置脏,要等约 5s 的下一拍才落盘)→ 立刻点重启
    /// → handler 只 sleep 500ms 就 `exit(0)`,后台那一拍永远轮不到 → 进程读回旧 api_keys.json,
    /// 被吊销的 key 复活、继续鉴权通过。只刷统计的旧实现在这里会失败。
    #[tokio::test]
    async fn flush_before_exit_persists_api_key_revocation() {
        let dir = tmp_dir("restart_keyflush");
        let state = state_with_data_dir(&dir);
        let keys_path = dir.join("api_keys.json");

        let key = state.api_keys.create(
            "leaked".into(),
            None,
            None,
            None,
            None,
            None,
            chrono::Utc::now(),
        );
        // 先落一份"吊销前"的盘,模拟这把 key 已经在磁盘上活着。
        state.api_keys.save_now().unwrap();
        assert!(
            std::fs::read_to_string(&keys_path)
                .unwrap()
                .contains(&key.key),
            "前提:磁盘上确实有这把 key"
        );

        assert!(state.api_keys.delete(key.id), "删除应命中");
        // 去抖窗口内盘上仍是旧内容 —— 这一拍真的丢得掉(此处到断言之间无 await)。
        assert!(
            std::fs::read_to_string(&keys_path)
                .unwrap()
                .contains(&key.key),
            "前提:删除只置脏,尚未落盘"
        );

        super::flush_persistent_state_before_exit(&state).await;

        let after = std::fs::read_to_string(&keys_path).unwrap();
        assert!(
            !after.contains(&key.key),
            "重启前刷盘必须落下吊销,否则重启后这把已删除的 key 复活并继续鉴权通过"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关键回归:管理面「重启」前的刷盘范围必须**包含余额缓存**。
    ///
    /// 现场:管理员删掉账号 7(handler 会 `invalidate` 掉它的余额缓存,但那只是置脏)→ 立刻点
    /// 重启 → `exit(0)` 不跑析构、后台那一拍轮不到 → 进程读回旧 kiro_balance_cache.json,
    /// 已删账号的条目原样复活;账号编号高水位一旦缺失(旧版本升级/从备份还原 credentials.json/
    /// drop-in 一份原生文件),新加的账号会重新领到 id 7,于是它在余额 TTL 内(最长 5 分钟)
    /// 顶着上一位主人的剩余额度与订阅档位——正是 `BalanceCache::invalidate` 文档点名不许发生的事。
    #[tokio::test]
    async fn flush_before_exit_persists_balance_cache_invalidation() {
        let dir = tmp_dir("restart_balflush");
        let now = super::now_unix();
        // 先在盘上放一份"账号 7 的余额已缓存"的现场(等价于上次运行留下的文件)。
        let seeded: std::collections::HashMap<String, crate::balance::BalanceSnapshot> = [(
            "7".to_string(),
            crate::balance::BalanceSnapshot {
                subscription_title: Some("KIRO PRO+".into()),
                current_usage: 1.0,
                usage_limit: 100.0,
                remaining: 99.0,
                usage_percentage: 1.0,
                next_reset_at: None,
                fetched_at_unix: now,
            },
        )]
        .into_iter()
        .collect();
        let cache_path = dir.join(crate::balance::BALANCE_CACHE_FILE);
        std::fs::write(&cache_path, serde_json::to_vec(&seeded).unwrap()).unwrap();

        let state = state_with_data_dir_creds(&dir, vec![cred("7")]);
        settle_flush_loops().await;
        assert!(
            state.balance.get_fresh("7", now).await.is_some(),
            "前提:进程启动时从盘上读回了账号 7 的余额缓存"
        );

        let app = admin_api_router(state.clone());
        let (st, _v) = send(&app, Method::DELETE, "/api/admin/credentials/7", None).await;
        assert_eq!(st, HttpStatusCode::OK);
        // 删除只把缓存条目从内存里摘掉并置脏,盘上那份还是旧的(此处到断言之间无 await)。
        assert!(
            std::fs::read_to_string(&cache_path)
                .unwrap()
                .contains("\"7\""),
            "前提:invalidate 只置脏,尚未落盘"
        );

        super::flush_persistent_state_before_exit(&state).await;

        // 重启:新进程从同一目录把缓存读回来。
        let reloaded = crate::balance::BalanceCache::load_from_dir(&dir);
        assert!(
            reloaded.get_fresh("7", now).await.is_none(),
            "重启前刷盘必须落下 invalidate,否则复用 id 7 的新账号会顶着已删账号的余额/订阅档位(最长 5 分钟)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关键回归:管理面「重启」前的刷盘范围必须**包含失败/限流事件日志**。
    ///
    /// 现场:上游刚回了 401/403 与 429(events 只进内存 + 置脏)→ 运营者看到告警、顺手点重启
    /// 排障 → `exit(0)` 让那一拍永远轮不到 → 重启后点账号行上的「失败」「限流」计数,弹窗里
    /// 空空如也,usage-summary 的 errorRate 窗口计数也跟着少算——最想看的现场恰恰被重启抹掉。
    #[tokio::test]
    async fn flush_before_exit_persists_failure_and_throttle_events() {
        let dir = tmp_dir("restart_evflush");
        let state = state_with_data_dir(&dir);
        settle_flush_loops().await;
        state
            .stats
            .record_failure(7, "api", 403, "forbidden-body", 1_700_000_000)
            .await;
        state
            .stats
            .record_throttle(7, "api", "too-many-requests", 1_700_000_001)
            .await;
        // 去抖窗口内:后台循环还没到下一拍,盘上什么都没有。
        assert!(
            !dir.join("failure_log.json").exists() && !dir.join("throttle_log.json").exists(),
            "前提:事件只置脏,尚未落盘"
        );

        super::flush_persistent_state_before_exit(&state).await;

        // 重启:新进程从同一目录把事件读回来,弹窗里必须还查得到刚才那两条。
        let reloaded = StatsManager::load_from_dir(&dir);
        let f = reloaded.failure_log.records_for_credential(7, 1, 10).await;
        assert_eq!(f.total, 1, "重启后失败日志弹窗必须还查得到刚出的 403");
        assert_eq!(f.items[0].response_body, "forbidden-body");
        let t = reloaded.throttle_log.records_for_credential(7, 1, 10).await;
        assert_eq!(t.total, 1, "重启后限流日志弹窗必须还查得到刚出的 429");
        assert_eq!(t.items[0].response_body, "too-many-requests");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn restart_without_confirm_is_400_and_does_not_exit() {
        let app = admin_api_router(state_with(vec![], cfg_with_temp_creds()));
        let (st, v) = send(&app, Method::POST, "/api/admin/restart", None).await;
        assert_eq!(st, HttpStatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["type"], "confirmation_required");
    }

    /// 只有确证终态才允许销毁设备码会话;瞬态一侧的判定要覆盖网络抖动、不可解析应答与 429/5xx。
    #[test]
    fn only_definitive_states_are_terminal_for_device_poll() {
        assert!(super::is_terminal_poll_error(&LoginError::Denied));
        assert!(super::is_terminal_poll_error(&LoginError::Expired));
        assert!(super::is_terminal_poll_error(&LoginError::Upstream(
            "invalid_client".into()
        )));
        assert!(super::is_terminal_poll_error(&LoginError::BadCallback));
        // 传输层抖动 / 应答体不可解析 → 瞬态。
        assert!(!super::is_terminal_poll_error(&LoginError::Http));
        // 429 与 5xx → 瞬态,且需退避。
        for status in [429u16, 500, 503] {
            let e = LoginError::UpstreamHttp {
                status,
                body: String::new(),
            };
            assert!(!super::is_terminal_poll_error(&e), "status={status}");
            assert!(super::should_back_off(&e), "status={status}");
        }
        assert!(!super::should_back_off(&LoginError::Http));
    }

    /// 粘错回调 URL(state 不符)→ 400,但会话保留,用户改一下就能重试。
    #[tokio::test]
    async fn iam_sso_complete_bad_callback_keeps_session_for_retry() {
        let state = state_with(vec![], cfg_with_temp_creds());
        let sessions = state.iam_sso_sessions.clone();
        let session_id = sessions.put(
            IamSsoSession {
                auth: crate::kiro::login::iam_sso::AuthStart {
                    authorize_url: "https://oidc.us-east-1.amazonaws.com/authorize".into(),
                    verifier: "verifier".into(),
                    state: "expected-state".into(),
                    client_id: "cid".into(),
                    client_secret: "csec".into(),
                },
                region: "us-east-1".into(),
            },
            super::now_unix(),
        );
        let app = admin_api_router(state);
        let (st, v) = send(
            &app,
            Method::POST,
            "/api/admin/login/iam-sso/complete",
            Some(&format!(
                r#"{{"sessionId":"{session_id}","callbackUrl":"http://127.0.0.1/oauth/callback?code=c&state=WRONG"}}"#
            )),
        )
        .await;
        assert_eq!(st, HttpStatusCode::BAD_REQUEST);
        assert_eq!(v["success"], false);
        assert!(
            sessions.get(&session_id, super::now_unix()).is_some(),
            "校验回调失败不得消费会话"
        );
    }

    /// 回调合法但换 token 撞上瞬态失败(上游不可达)→ 502,会话仍保留供重试。
    #[tokio::test]
    async fn iam_sso_complete_transient_token_exchange_keeps_session() {
        let state = state_with_unreachable_upstream(cfg_with_temp_creds());
        let sessions = state.iam_sso_sessions.clone();
        let session_id = sessions.put(
            IamSsoSession {
                auth: crate::kiro::login::iam_sso::AuthStart {
                    authorize_url: "https://oidc.us-east-1.amazonaws.com/authorize".into(),
                    verifier: "verifier".into(),
                    state: "expected-state".into(),
                    client_id: "cid".into(),
                    client_secret: "csec".into(),
                },
                region: "us-east-1".into(),
            },
            super::now_unix(),
        );
        let app = admin_api_router(state);
        let (st, _v) = send(
            &app,
            Method::POST,
            "/api/admin/login/iam-sso/complete",
            Some(&format!(
                r#"{{"sessionId":"{session_id}","callbackUrl":"http://127.0.0.1/oauth/callback?code=c&state=expected-state"}}"#
            )),
        )
        .await;
        assert_eq!(st, HttpStatusCode::BAD_GATEWAY);
        assert!(
            sessions.get(&session_id, super::now_unix()).is_some(),
            "换 token 瞬态失败不得消费会话"
        );
    }

    /// 删除凭据后,按 id 键控的余额/模型缓存必须同步失效 —— id 会被新凭据复用,
    /// 残留条目会让新账号显示上一位主人的余额与模型清单。
    #[tokio::test]
    async fn delete_credential_invalidates_balance_and_models_cache() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let state = state_with(vec![cred("77991")], cfg);
        let now = super::now_unix();
        let balance = state.balance.clone();
        let models = state.models_cache.clone();
        balance
            .put(
                "77991",
                crate::balance::BalanceSnapshot {
                    subscription_title: Some("KIRO PRO+".into()),
                    current_usage: 1.0,
                    usage_limit: 100.0,
                    remaining: 99.0,
                    usage_percentage: 1.0,
                    next_reset_at: None,
                    fetched_at_unix: now,
                },
            )
            .await;
        models
            .put(
                "77991",
                vec![crate::models_cache::ModelInfo {
                    context_window: None,
                    id: "claude-sonnet-4".into(),
                    display_name: "Claude Sonnet 4".into(),
                    owned_by: "anthropic".into(),
                    max_tokens: 8192,
                    rate_multiplier: None,
                }],
                now,
            )
            .await;
        let app = admin_api_router(state);
        let (st, _v) = send(&app, Method::DELETE, "/api/admin/credentials/77991", None).await;
        assert_eq!(st, HttpStatusCode::OK);
        assert!(
            balance.get_fresh("77991", now).await.is_none(),
            "删除凭据后余额缓存应已失效"
        );
        assert!(
            models.get_fresh("77991", now).await.is_none(),
            "删除凭据后模型缓存应已失效"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 关键回归:删账号必须连它在统计层的用量记录与失败/限流事件一并清掉。
    ///
    /// 两层危害:
    /// 1. 无条件成立的账本泄漏——账号已经从面板上消失,它的记录却永远留在账本里,继续占着
    ///    每凭据记录上限、继续出现在聚合统计里;
    /// 2. 编号复用后的串味——账号编号高水位是旁挂文件里的 best-effort 值,写失败或文件缺失
    ///    (旧版本升级、从备份还原 credentials.json、drop-in 一份原生文件、只搬 JSON 不搬旁挂
    ///    文件)时,重启后编号退回 `max(现有 id)+1`,刚删掉的最大号会被下一个新账号原样领走,
    ///    于是新账号一上线就顶着前任的用量、消费与报错明细。
    #[tokio::test]
    async fn delete_credential_purges_usage_records_and_event_logs() {
        let cfg = cfg_with_temp_creds();
        let path = cfg.credentials_path.clone();
        let stats = empty_stats("del_purge");
        // 账号 5 跑过 2 次、报过一次 403、限过一次流;账号 6 各一条,用来验"只清被删的那个"。
        for (cred_id, at) in [
            (5u32, 1_700_000_000i64),
            (5, 1_700_000_001),
            (6, 1_700_000_002),
        ] {
            stats
                .usage
                .record_usage(
                    cred_id,
                    "claude-sonnet-4".into(),
                    10,
                    20,
                    0.5,
                    None,
                    None,
                    None,
                    at,
                )
                .await;
        }
        stats
            .record_failure(5, "api", 403, "forbidden-body", 1_700_000_000)
            .await;
        stats
            .record_throttle(5, "api", "too-many-requests", 1_700_000_000)
            .await;
        stats
            .record_failure(6, "api", 401, "unauthorized", 1_700_000_000)
            .await;

        let app = admin_api_router(state_with_stats(
            vec![cred("5"), cred("6")],
            cfg,
            stats.clone(),
        ));
        // 前提:删之前面板确实查得到账号 5 的这些明细。
        let (_st, v) = get(&app, "/api/admin/credentials/5/usage/records").await;
        assert_eq!(v["total"], 2, "前提:账号 5 有 2 条用量记录");

        let (st, _v) = send(&app, Method::DELETE, "/api/admin/credentials/5", None).await;
        assert_eq!(st, HttpStatusCode::OK);

        let (_st, v) = get(&app, "/api/admin/credentials/5/usage/records").await;
        assert_eq!(
            v["total"], 0,
            "已删账号的用量记录必须清干净,否则复用 id 5 的新账号一上线就继承前任的用量与消费:{v}"
        );
        let (_st, v) = get(&app, "/api/admin/credentials/5/failure-logs").await;
        assert_eq!(v["total"], 0, "已删账号的失败日志必须清干净:{v}");
        let (_st, v) = get(&app, "/api/admin/credentials/5/throttle-logs").await;
        assert_eq!(v["total"], 0, "已删账号的限流日志必须清干净:{v}");

        // 只清被删的那个:账号 6 的明细一条不少。
        let (_st, v) = get(&app, "/api/admin/credentials/6/usage/records").await;
        assert_eq!(v["total"], 1, "不得误伤其它账号的用量记录:{v}");
        let (_st, v) = get(&app, "/api/admin/credentials/6/failure-logs").await;
        assert_eq!(v["total"], 1, "不得误伤其它账号的失败日志:{v}");
        let _ = std::fs::remove_file(&path);
    }
}
