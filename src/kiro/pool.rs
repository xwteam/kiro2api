//! Kiro 多账号池:健康账号选择 + 失败分级冷却 + 负载均衡。
//! 冷却时长与错误分类均按可观测错误语义自行设计(非移植);时钟以 now_unix 注入。
use crate::kiro::credential::{AuthMethod, Credential};
use serde::Serialize;

/// 凭据可变字段的局部更新集合(Phase 3):字段为 `None` 即"不改动"。
/// 只承载可安全改写的元数据/凭据轮换字段;健康态计数(strikes/cooldown/用量)不在此。
#[derive(Debug, Default, Clone)]
pub struct CredentialUpdate {
    pub refresh_token: Option<String>,
    pub auth: Option<AuthMethod>,
    pub email: Option<String>,
    pub nickname: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub profile_arn: Option<String>,
    pub machine_id: Option<String>,
    pub weight: Option<u32>,
    pub region: Option<String>,
}

/// 负载均衡方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LbMode {
    /// 轮转优先(等权轮询)。
    Priority,
    /// 均衡(按权重轮询)。
    Balanced,
}

/// 失败类别(按可观测语义)。
///
/// `Auth` 拆成两档,以避免"一个裸 401/403 状态码就把账号永久禁用"这一过激处置:
/// - [`FailureKind::AuthInvalid`]:响应体带有真凭据失效信号(令牌无效/授权作废等),
///   才做**永久禁用**(仅 admin 手工可复活);
/// - [`FailureKind::AuthAmbiguous`]:401/403 但响应体**没有**明确失效信号(瞬时/歧义,
///   如上游偶发权限抖动、网关 403),按瞬时类**累计 strike 后冷却**,不永久禁用。
///
/// 数据面若无法拿到响应体,应保守落在 [`FailureKind::AuthAmbiguous`](冷却)而非永久禁用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Quota,
    /// 真凭据失效(响应体确认)→ 永久禁用。
    AuthInvalid,
    /// 401/403 但无明确失效信号 → 冷却(带 strike),不永久禁用。
    AuthAmbiguous,
    Transient,
    /// 确定性请求错误(如上游 `INVALID_MODEL_ID`:所请求的模型对当前账号档位不可用)。
    /// **非账号故障**:换任何同档账号都会同样失败,故不冷却/不禁用/不跨账号重试,
    /// 由上层直接以 400 把清晰的不可用说明回给客户端(见 handler `RelayError::InvalidModel`)。
    InvalidRequest,
}

/// 自设冷却时长(秒)。
const QUOTA_COOLDOWN: u64 = 30 * 60; // 配额类:30 分钟
const TRANSIENT_COOLDOWN: u64 = 90; // 瞬时类:90 秒
/// 歧义鉴权类(401/403 无明确失效信号)专用冷却:比瞬时类长,给上游权限抖动留恢复余地,
/// 又不至于像配额那样锁 30 分钟。
const AUTH_AMBIGUOUS_COOLDOWN: u64 = 5 * 60; // 5 分钟
/// 瞬时错误连续多少次才冷却(自设)。
const TRANSIENT_STRIKES: u32 = 3;
/// 歧义鉴权类连续多少次才冷却(自设,比瞬时类更敏感:2 次即冷却)。
const AUTH_AMBIGUOUS_STRIKES: u32 = 2;
/// RPM 滑动窗口宽度(秒,自设):按分钟粒度限流,窗口内选择次数超过
/// max_rpm 即视为该账号本分钟已耗尽,跳过等窗口滑出。
const RPM_WINDOW_SECS: u64 = 60;

/// 仅凭 HTTP 状态码的保守分类(拿不到响应体时用)。
///
/// 关键:401/403 落在 [`FailureKind::AuthAmbiguous`](冷却),**绝不**因裸状态码就永久禁用;
/// 402/429 落在 [`FailureKind::Quota`](长冷却)。要区分"真凭据失效永久禁用"需拿到响应体,
/// 见 [`classify_with_body`]。
pub fn classify(status: u16) -> FailureKind {
    classify_with_body(status, "")
}

/// 响应体感知的分类:在状态码基础上,用响应体的语义标记做升级判定。
///
/// - 402/429 → [`FailureKind::Quota`](配额/额度耗尽,长冷却);其中 402 常为
///   `MONTHLY_REQUEST_COUNT` 月度额度用尽。
/// - 401/403:
///     - **先**做防御性短路([`body_signals_recoverable`]):体里出现 expired /
///       ExpiredTokenException / suspend / throttl / too many requests / rate exceeded /
///       quota / security precaution 等**可自愈**信号 → 一律 [`FailureKind::AuthAmbiguous`]
///       (冷却,靠刷新自愈),**绝不**永久禁用;
///     - 否则,响应体含**真凭据失效信号**([`body_signals_auth_invalid`]) →
///       [`FailureKind::AuthInvalid`](永久禁用);
///     - 再否则 → [`FailureKind::AuthAmbiguous`](冷却,不永久禁用)。
/// - 其它 → 若响应体带配额标记则 [`FailureKind::Quota`],否则 [`FailureKind::Transient`]。
///
/// `body` 传空串等价于"无响应体信息":401/403 保守落在 `AuthAmbiguous`(冷却)。
///
/// 关键:`body` 应传**完整原始响应体**(而非只含 `message` 字段的有损摘要),否则活在
/// `__type` 里的机器稳定异常名(ExpiredTokenException/InvalidGrantException/…)会被漏看,
/// 分类退化(见 provider 的 `#6`)。本函数同时匹配 `__type` 与 `message` 措辞。
pub fn classify_with_body(status: u16, body: &str) -> FailureKind {
    match status {
        402 | 429 => FailureKind::Quota,
        401 | 403 => {
            // 防御性短路(defense-in-depth):任何"可自愈"信号(过期/暂停/限流/配额/风控)
            // 一律降级为 AuthAmbiguous(冷却),永久禁用只保留给真正作废的凭据。
            // 必须**先于** invalid 判定,因为 "...is expired." 之类措辞可能与 invalid 标记
            // 在同一体里共存(如 ExpiredTokenException 的 message),优先按可恢复处置。
            if body_signals_recoverable(body) {
                FailureKind::AuthAmbiguous
            } else if body_signals_auth_invalid(body) {
                FailureKind::AuthInvalid
            } else {
                FailureKind::AuthAmbiguous
            }
        }
        _ => {
            if body_signals_invalid_request(body) {
                // 确定性请求错误(如 400 INVALID_MODEL_ID):非账号故障,不重试、不冷却。
                FailureKind::InvalidRequest
            } else if body_signals_quota(body) {
                FailureKind::Quota
            } else {
                FailureKind::Transient
            }
        }
    }
}

/// 响应体是否为**确定性请求错误**(换账号重试也无用):当前只认上游对不支持的模型返回的
/// `INVALID_MODEL_ID`(FREE 档账号请求 opus/GPT 等即命中)。刻意只匹配机器稳定的 reason 码,
/// 不泛化到任意 400,避免把可重试的瞬时错误误判为永久。
pub fn body_signals_invalid_request(body: &str) -> bool {
    body.to_ascii_lowercase().contains("invalid_model_id")
}

/// 响应体是否带有**可自愈**信号:过期/暂停/限流/配额/风控。命中 → 一律冷却(AuthAmbiguous),
/// **绝不**永久禁用。作为 [`classify_with_body`] 401/403 分支的防御性短路(先于 invalid 判定)。
///
/// - `expired` / `expiredtokenexception`:令牌过期是**可刷新**的,绝不能因此永久禁用
///   (AWS `ExpiredTokenException` = "The security token included in the request is expired.")。
/// - `suspend`:被暂停账号仍可能恢复,非凭据作废。
/// - `throttl` / `too many requests` / `rate exceeded`:限流,瞬时。
/// - `quota`:配额耗尽,靠时间/额度恢复。
/// - `security precaution`:上游风控临时拦截,非凭据失效。
///
/// 同时匹配 `__type`(机器稳定异常名)与 `message`(自然语措辞),故传**完整原始体**最稳。
pub fn body_signals_recoverable(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    const RECOVERABLE: [&str; 8] = [
        "expired", // 覆盖 expiredtokenexception / "...is expired." / token expired
        "expiredtokenexception",
        "suspend", // suspended / suspension
        "throttl", // throttled / throttlingexception
        "too many requests",
        "rate exceeded",
        "quota",
        "security precaution",
    ];
    RECOVERABLE.iter().any(|m| lower.contains(m))
}

/// 响应体是否带有**真凭据失效**信号(才允许永久禁用)。
///
/// 命中即认为该账号的 refresh/凭据已作废,单纯冷却无意义,应永久禁用等 admin 处置。
/// 标记刻意**精确**:只认无歧义的"凭据/授权已作废"信号,不认 expired(可刷新,已在
/// [`body_signals_recoverable`] 短路)、不认单纯 forbidden/unauthorized/AccessDenied
/// (可能只是上游抖动/临时权限/被暂停,归 `AuthAmbiguous` 冷却)。
///
/// 同时匹配 `__type`(机器稳定异常名,如 `InvalidGrantException`/`UnauthorizedException`)
/// 与 `message`(如 "...is invalid.");故调用方应传**完整原始响应体**(见 provider `#6`)。
pub fn body_signals_auth_invalid(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    const MARKERS: [&str; 6] = [
        // OAuth/OIDC 明确的授权作废(RFC 6749)。
        "invalid_grant",
        // AWS 机器稳定异常名(__type),明确指向不可自愈的凭据/授权失效。
        "invalidgrantexception",
        "unauthorizedexception",
        // AWS/上游常见精确措辞(完整):"The security token included in the request is invalid."
        // 注意刻意保留完整短语,避免误吞 "...is expired." 变体(那属可刷新,已被短路)。
        "security token included in the request is invalid",
        // OIDC/OAuth token 端点常见:"invalid_token"(machine-stable 参数值)。
        "invalid_token",
        // 明确的作废措辞(带尾句号):"...is invalid." —— 与 expired 变体互斥。
        "is invalid.",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

/// 响应体是否带有**配额/额度耗尽**标记(用于非 402/429 状态码上仍能识别配额)。
pub fn body_signals_quota(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("monthly_request_count")
        || lower.contains("quota")
        || lower.contains("request limit")
        || lower.contains("reached the limit")
}

struct Entry {
    cred: Credential,
    disabled: bool,
    cooldown_until: u64,
    strikes: u32,
    /// 近 RPM_WINDOW_SECS 秒内被 select 选中的时刻集合。
    req_times: Vec<u64>,
    /// 累计被选中次数(用量统计)。
    requests: u64,
    /// 累计成功次数(用量统计)。
    successes: u64,
    /// 累计失败次数(用量统计)。
    failures: u64,
    /// 最近一次被选中的时刻(用量统计)。
    last_used_unix: u64,
}

/// 单账号用量快照(供后续 admin 只读展示用,内部观测,无需改名)。
/// 仅含非密元数据 + 计数;绝不携带 access_token/refresh_token/client_secret/client_id。
#[derive(Debug, Clone, Serialize)]
pub struct AccountStat {
    pub id: String,
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub last_used_unix: u64,
    pub disabled: bool,
    pub cooldown_until: u64,
    pub in_cooldown: bool,
    /// 鉴权方式("social"/"idc"),由 `Credential.auth` 转小写串。
    pub auth_method: String,
    /// 数据面 region。
    pub region: String,
    /// 账号邮箱(非密,可能缺失)。
    pub email: Option<String>,
    /// 账号昵称(非密,可能缺失)。
    pub nickname: Option<String>,
    /// 负载均衡权重(下限 1 生效前的原始 weight;展示用)。
    pub weight: u32,
    /// 令牌过期时刻(unix 秒;非密元数据,展示"expiresAt"用)。
    pub expires_at_unix: u64,
    /// 是否携带 profile_arn(仅出布尔,绝不泄露 arn 明文)。
    pub has_profile_arn: bool,
    /// 连续失败计数(瞬时类 strike 累计;展示"failureCount"用)。
    pub strikes: u32,
    /// 当前实时 RPM:近 RPM_WINDOW_SECS 秒内被 select 选中的次数(展示"RPM"用)。
    pub rpm: u32,
}

/// 账号池。
pub struct Pool {
    entries: Vec<Entry>,
    mode: LbMode,
    cursor: usize,
    /// 每账号每分钟最大请求数;0 = 无限(默认,兼容既有行为)。
    max_rpm: u32,
}

impl Pool {
    pub fn new(creds: Vec<Credential>, mode: LbMode) -> Self {
        let entries = creds
            .into_iter()
            .map(|c| Entry {
                disabled: c.disabled,
                cooldown_until: 0,
                strikes: 0,
                req_times: Vec::new(),
                requests: 0,
                successes: 0,
                failures: 0,
                last_used_unix: 0,
                cred: c,
            })
            .collect();
        Self {
            entries,
            mode,
            cursor: 0,
            max_rpm: 0,
        }
    }

    /// 无损 setter:设置每账号 RPM 上限(0 = 无限)。不改变 `new` 签名,
    /// 现有调用点(server/handler/live 测试)默认继承无限行为。
    pub fn set_max_rpm(&mut self, max_rpm: u32) {
        self.max_rpm = max_rpm;
    }

    /// 无损 setter:切换负载均衡模式。
    pub fn set_mode(&mut self, mode: LbMode) {
        self.mode = mode;
    }

    fn is_active(e: &Entry, now_unix: u64) -> bool {
        !e.disabled && e.cooldown_until <= now_unix
    }

    /// 窗口内计数:近 RPM_WINDOW_SECS 秒(即 now_unix 起回溯的滑动窗口)内
    /// req_times 中仍未滑出的时刻数量。RPM 判定与只读展示共用此口径。
    fn window_count(e: &Entry, now_unix: u64) -> u32 {
        let window_start = now_unix.saturating_sub(RPM_WINDOW_SECS);
        e.req_times.iter().filter(|&&t| t > window_start).count() as u32
    }

    /// RPM 判定:max_rpm==0 视为无限;否则统计窗口内选择次数是否低于上限。
    fn rpm_ok(e: &Entry, now_unix: u64, max_rpm: u32) -> bool {
        if max_rpm == 0 {
            return true;
        }
        Self::window_count(e, now_unix) < max_rpm
    }

    /// 单账号当前实时 RPM:近 RPM_WINDOW_SECS 秒内被 select 选中的次数;
    /// 未知 id 返回 None。now_unix 注入(测试可控)。只读,不改动 req_times。
    pub fn rpm_of(&self, id: &str, now_unix: u64) -> Option<u32> {
        self.entries
            .iter()
            .find(|e| e.cred.id == id)
            .map(|e| Self::window_count(e, now_unix))
    }

    /// 全部账号当前实时 RPM 映射:id → 窗口内选择次数(只读展示用)。
    pub fn rpm_all(&self, now_unix: u64) -> Vec<(String, u32)> {
        self.entries
            .iter()
            .map(|e| (e.cred.id.clone(), Self::window_count(e, now_unix)))
            .collect()
    }

    /// 记录一次选择:push 当前时刻并剪掉滑出窗口的旧时刻;同步用量统计。
    fn record_request(&mut self, idx: usize, now_unix: u64) {
        let window_start = now_unix.saturating_sub(RPM_WINDOW_SECS);
        let e = &mut self.entries[idx];
        e.req_times.retain(|&t| t > window_start);
        e.req_times.push(now_unix);
        e.requests += 1;
        e.last_used_unix = now_unix;
    }

    /// 池内凭据总数(含禁用/冷却;跨账号重试用来计算自适应上限)。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 池是否为空(clippy `len_without_is_empty` 配套)。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 当前可用账号数。
    pub fn active_count(&self, now_unix: u64) -> usize {
        self.entries
            .iter()
            .filter(|e| Self::is_active(e, now_unix))
            .count()
    }

    /// 选一个健康账号(跳过禁用/冷却/RPM 超限),按 LB 轮转;无则 None。
    pub fn select(&mut self, now_unix: u64) -> Option<Credential> {
        self.select_excluding(now_unix, |_| false)
    }

    /// 选一个健康账号,但跳过 `exclude_ids` 中已试过的凭据 id(跨账号重试用)。
    /// 除排除集外,选择纪律与 [`select`](Self::select) 完全一致(disabled/冷却/RPM 均跳过、
    /// 沿用当前 LB 模式与 cursor)。无满足条件的账号时返回 None。
    ///
    /// 供中转层跨账号重试:某账号数据面失败后,以已试过的 id 集合再选下一个不同的健康账号。
    pub fn select_with_exclude(
        &mut self,
        now_unix: u64,
        exclude_ids: &std::collections::HashSet<String>,
    ) -> Option<Credential> {
        self.select_excluding(now_unix, |c| exclude_ids.contains(&c.id))
    }

    /// [`select`](Self::select) 与 [`select_with_exclude`](Self::select_with_exclude) 的共享内核:
    /// `excluded(&cred)` 返回 true 的凭据被视作不可选(等同临时不 active),其余选择纪律不变。
    fn select_excluding(
        &mut self,
        now_unix: u64,
        excluded: impl Fn(&Credential) -> bool,
    ) -> Option<Credential> {
        let n = self.entries.len();
        if n == 0 {
            return None;
        }
        let max_rpm = self.max_rpm;
        let eligible = |e: &Entry| {
            Self::is_active(e, now_unix) && Self::rpm_ok(e, now_unix, max_rpm) && !excluded(&e.cred)
        };
        // 收集可用下标:disabled/cooldown/RPM 过滤之外再叠加排除集过滤
        let active: Vec<usize> = (0..n).filter(|&i| eligible(&self.entries[i])).collect();
        if active.is_empty() {
            return None;
        }
        let pick = match self.mode {
            LbMode::Priority => {
                // 等权轮询:从 cursor 起找下一个可用(disabled/cooldown/RPM/排除集均需跳过)
                self.cursor = (self.cursor + 1) % n;
                let mut idx = self.cursor;
                let mut found = None;
                for _ in 0..n {
                    if eligible(&self.entries[idx]) {
                        found = Some(idx);
                        break;
                    }
                    idx = (idx + 1) % n;
                }
                self.cursor = idx;
                found?
            }
            LbMode::Balanced => {
                // 按权重轮询(仅在可用 active 集合上展开):cursor 在"权重展开"序上前进
                let total: u32 = active
                    .iter()
                    .map(|&i| self.entries[i].cred.effective_weight())
                    .sum();
                let step = (self.cursor as u32 + 1) % total.max(1);
                self.cursor = step as usize;
                let mut acc = 0u32;
                let mut chosen = active[0];
                for &i in &active {
                    acc += self.entries[i].cred.effective_weight();
                    if step < acc {
                        chosen = i;
                        break;
                    }
                }
                chosen
            }
        };
        self.record_request(pick, now_unix);
        Some(self.entries[pick].cred.clone())
    }

    fn find(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.cred.id == id)
    }

    // ======================================================================
    // Phase 3:凭据增删改(活池就地变更)。
    // 这些方法只改内存池;调用方拿到返回的凭据快照后自行原子落盘
    // (credential::save),使"活池变更"与"持久化"解耦、锁不跨 await。
    // ======================================================================

    /// 当前全部凭据快照(顺序即池内顺序),供调用方落盘 credentials.json。
    pub fn snapshot_credentials(&self) -> Vec<Credential> {
        self.entries.iter().map(|e| e.cred.clone()).collect()
    }

    /// 为新凭据分配数值 id:已有 id 里能解析为整数的取最大值 +1;空池或全非数值 → "1"。
    fn next_id(&self) -> String {
        let max = self
            .entries
            .iter()
            .filter_map(|e| e.cred.id.parse::<i64>().ok())
            .max()
            .unwrap_or(0);
        (max + 1).to_string()
    }

    /// 池中是否已存在相同 `refresh_token` 的凭据(导入去重用);命中返回其 id。
    /// refresh_token 是账号的唯一稳定标识——同一 token 重复导入会产生两条抢同一轮换令牌的
    /// 凭据(刷新时互相作废、浪费配额、增加上游风控),故导入前据此跳过已存在账号。
    pub fn find_id_by_refresh_token(&self, refresh_token: &str) -> Option<String> {
        if refresh_token.is_empty() {
            return None;
        }
        self.entries
            .iter()
            .find(|e| e.cred.refresh_token == refresh_token)
            .map(|e| e.cred.id.clone())
    }

    /// 新增一个凭据到活池:分配新数值 id(忽略入参 `cred.id`)、以默认健康态入池。
    /// 返回 (新 id, email);不落盘(调用方用 `snapshot_credentials` 落盘)。
    pub fn add_credential(&mut self, mut cred: Credential) -> (String, Option<String>) {
        let id = self.next_id();
        cred.id = id.clone();
        let email = cred.email.clone();
        self.entries.push(Entry {
            disabled: cred.disabled,
            cooldown_until: 0,
            strikes: 0,
            req_times: Vec::new(),
            requests: 0,
            successes: 0,
            failures: 0,
            last_used_unix: 0,
            cred,
        });
        (id, email)
    }

    /// 局部更新一个凭据的可变字段(仅 `Some` 的字段生效;绝不动 access_token/
    /// refresh_token 之外的健康态计数)。`refresh_token` 若提供则更新(轮换凭据)。
    /// 返回是否命中该 id。
    pub fn update_credential(&mut self, id: &str, upd: CredentialUpdate) -> bool {
        let Some(e) = self.find(id) else {
            return false;
        };
        let c = &mut e.cred;
        if let Some(v) = upd.refresh_token {
            c.refresh_token = v;
        }
        if let Some(v) = upd.auth {
            c.auth = v;
        }
        if let Some(v) = upd.email {
            c.email = Some(v);
        }
        if let Some(v) = upd.nickname {
            c.nickname = Some(v);
        }
        if let Some(v) = upd.client_id {
            c.client_id = Some(v);
        }
        if let Some(v) = upd.client_secret {
            c.client_secret = Some(v);
        }
        if let Some(v) = upd.profile_arn {
            c.profile_arn = Some(v);
        }
        if let Some(v) = upd.machine_id {
            c.machine_id = Some(v);
        }
        if let Some(v) = upd.weight {
            c.weight = v;
        }
        if let Some(v) = upd.region {
            c.region = v;
        }
        true
    }

    /// 刷新后写回:就地更新一个已存凭据的 access_token / refresh_token / 过期时刻
    /// (令牌轮换)。仅改这三项,不动健康态计数或其它元数据。返回是否命中该 id。
    ///
    /// 供集中刷新助手(`kiro::ensure_fresh`)在网络刷新完成后把新令牌回灌活池,
    /// 使 relay/balance/models 共享同一份最新令牌、避免各自反复轮换互相作废。
    pub fn update_credential_tokens(
        &mut self,
        id: &str,
        access_token: String,
        refresh_token: String,
        expires_at_unix: u64,
    ) -> bool {
        if let Some(e) = self.find(id) {
            e.cred.access_token = access_token;
            e.cred.refresh_token = refresh_token;
            e.cred.expires_at_unix = expires_at_unix;
            true
        } else {
            false
        }
    }

    /// 从活池移除一个凭据。返回是否命中并移除。
    pub fn remove_credential(&mut self, id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.cred.id != id);
        self.entries.len() != before
    }

    /// 设置凭据优先级(映射到负载均衡 weight;下限 1)。返回是否命中。
    pub fn set_priority(&mut self, id: &str, priority: i64) -> bool {
        if let Some(e) = self.find(id) {
            e.cred.weight = if priority < 1 { 1 } else { priority as u32 };
            true
        } else {
            false
        }
    }

    /// 清零连续失败计数(strikes)并解除冷却,使凭据即时可重新参与选择。
    /// 纯瞬态,不落盘。返回是否命中。
    pub fn reset_failures(&mut self, id: &str) -> bool {
        if let Some(e) = self.find(id) {
            e.strikes = 0;
            e.cooldown_until = 0;
            true
        } else {
            false
        }
    }

    /// 手动启停:按 id 找账号设置 disabled;不动 cooldown/strikes。
    /// 返回是否找到该账号。
    pub fn set_disabled(&mut self, id: &str, disabled: bool) -> bool {
        if let Some(e) = self.find(id) {
            e.disabled = disabled;
            true
        } else {
            false
        }
    }

    /// 成功:清零连续失败计数,累加成功用量统计。
    pub fn report_success(&mut self, id: &str) {
        if let Some(e) = self.find(id) {
            e.strikes = 0;
            e.successes += 1;
        }
    }

    /// 失败:按类别分级处理,累加失败用量统计。
    ///
    /// 处置分级:
    /// - [`FailureKind::AuthInvalid`]:真凭据失效 → **永久禁用**(仅 admin 手工复活)。
    /// - [`FailureKind::Quota`]:配额/额度耗尽 → 长冷却([`QUOTA_COOLDOWN`]),清零 strike。
    /// - [`FailureKind::AuthAmbiguous`]:401/403 无失效信号 → 累计 strike,达
    ///   [`AUTH_AMBIGUOUS_STRIKES`] 次后进入 [`AUTH_AMBIGUOUS_COOLDOWN`] 冷却,**不永久禁用**。
    /// - [`FailureKind::Transient`]:瞬时错误 → 累计 strike,达 [`TRANSIENT_STRIKES`] 次后
    ///   进入 [`TRANSIENT_COOLDOWN`] 冷却。
    ///
    /// 注意:调用方应尽量用 [`classify_with_body`] 依响应体分类,让"永久禁用"只在真凭据失效时发生;
    /// 拿不到响应体时用 [`classify`],401/403 会保守落在 `AuthAmbiguous`(冷却)而非永久禁用。
    pub fn report_failure(&mut self, id: &str, kind: FailureKind, now_unix: u64) {
        if let Some(e) = self.find(id) {
            e.failures += 1;
            match kind {
                FailureKind::AuthInvalid => {
                    e.disabled = true;
                }
                FailureKind::Quota => {
                    e.cooldown_until = now_unix.saturating_add(QUOTA_COOLDOWN);
                    e.strikes = 0;
                }
                FailureKind::AuthAmbiguous => {
                    e.strikes = e.strikes.saturating_add(1);
                    if e.strikes >= AUTH_AMBIGUOUS_STRIKES {
                        e.cooldown_until = now_unix.saturating_add(AUTH_AMBIGUOUS_COOLDOWN);
                        e.strikes = 0;
                    }
                }
                FailureKind::Transient => {
                    e.strikes = e.strikes.saturating_add(1);
                    if e.strikes >= TRANSIENT_STRIKES {
                        e.cooldown_until = now_unix.saturating_add(TRANSIENT_COOLDOWN);
                        e.strikes = 0;
                    }
                }
                // 确定性请求错误(如 INVALID_MODEL_ID):非账号故障,不冷却、不禁用、不累计 strike。
                // (上层其实会在反馈池前就短路,不会带此类走到这里;此 arm 仅为防御性穷尽。)
                FailureKind::InvalidRequest => {}
            }
        }
    }

    /// 每账号用量统计快照(供后续 admin 只读展示)。
    pub fn stats(&self, now_unix: u64) -> Vec<AccountStat> {
        self.entries
            .iter()
            .map(|e| AccountStat {
                id: e.cred.id.clone(),
                requests: e.requests,
                successes: e.successes,
                failures: e.failures,
                last_used_unix: e.last_used_unix,
                disabled: e.disabled,
                cooldown_until: e.cooldown_until,
                in_cooldown: e.cooldown_until > now_unix,
                auth_method: auth_method_str(e.cred.auth).to_string(),
                region: e.cred.region.clone(),
                email: e.cred.email.clone(),
                nickname: e.cred.nickname.clone(),
                weight: e.cred.weight,
                expires_at_unix: e.cred.expires_at_unix,
                has_profile_arn: e.cred.profile_arn.is_some(),
                strikes: e.strikes,
                rpm: Self::window_count(e, now_unix),
            })
            .collect()
    }
}

/// `AuthMethod` → 小写串("social"/"idc"),供 `AccountStat` 展示用。
fn auth_method_str(auth: crate::kiro::credential::AuthMethod) -> &'static str {
    use crate::kiro::credential::AuthMethod;
    match auth {
        AuthMethod::Social => "social",
        AuthMethod::Idc => "idc",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};

    fn cred(id: &str, weight: u32) -> Credential {
        Credential {
            id: id.into(),
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at_unix: u64::MAX,
            region: "us-east-1".into(),
            auth: AuthMethod::Social,
            client_id: None,
            client_secret: None,
            profile_arn: None,
            machine_id: None,
            email: None,
            nickname: None,
            weight,
            label: None,
            disabled: false,
        }
    }

    #[test]
    fn classify_by_status() {
        // 无响应体:401/403 保守落在 AuthAmbiguous(冷却),绝不因裸状态码永久禁用。
        assert_eq!(classify(429), FailureKind::Quota);
        assert_eq!(classify(402), FailureKind::Quota); // 402 = 额度耗尽
        assert_eq!(classify(401), FailureKind::AuthAmbiguous);
        assert_eq!(classify(403), FailureKind::AuthAmbiguous);
        assert_eq!(classify(503), FailureKind::Transient);
    }

    #[test]
    fn classify_with_body_flags_invalid_model_id_as_invalid_request() {
        // 上游对不支持的模型返回的 400 INVALID_MODEL_ID → InvalidRequest(不重试/不冷却)。
        let body = r#"{"message":"Invalid model. Please select a different model to continue.","reason":"INVALID_MODEL_ID"}"#;
        assert_eq!(classify_with_body(400, body), FailureKind::InvalidRequest);
        // 大小写无关。
        assert_eq!(
            classify_with_body(400, r#"{"reason":"invalid_model_id"}"#),
            FailureKind::InvalidRequest
        );
        // 其它 400(无该 reason 码)仍保守落 Transient(可重试),不误伤。
        assert_eq!(
            classify_with_body(400, "Bad Request"),
            FailureKind::Transient
        );
        assert_eq!(classify_with_body(400, ""), FailureKind::Transient);
        // report_failure 收到 InvalidRequest 不冷却/不禁用/不累计 strike(防御性穷尽 arm)。
        let mut pool = Pool::new(vec![cred("c1", 1)], LbMode::Priority);
        pool.report_failure("c1", FailureKind::InvalidRequest, 1000);
        let e = pool.find("c1").unwrap();
        assert!(!e.disabled, "InvalidRequest 不该禁用账号");
        assert_eq!(e.cooldown_until, 0, "InvalidRequest 不该冷却账号");
        assert_eq!(e.strikes, 0, "InvalidRequest 不该累计 strike");
    }

    #[test]
    fn classify_with_body_distinguishes_auth_invalid_from_ambiguous() {
        // 401/403 带真凭据失效信号 → AuthInvalid(永久禁用)。
        assert_eq!(
            classify_with_body(401, r#"{"error":"invalid_grant"}"#),
            FailureKind::AuthInvalid
        );
        assert_eq!(
            classify_with_body(
                403,
                "The security token included in the request is invalid."
            ),
            FailureKind::AuthInvalid
        );
        // 精确尾句号措辞 "...is invalid." 命中(与 "...is expired." 互斥)。
        assert_eq!(
            classify_with_body(401, "The refresh token is invalid."),
            FailureKind::AuthInvalid
        );
        // 401/403 无失效信号(纯 forbidden / 空体)→ AuthAmbiguous(冷却)。
        assert_eq!(
            classify_with_body(403, "Forbidden"),
            FailureKind::AuthAmbiguous
        );
        assert_eq!(classify_with_body(401, ""), FailureKind::AuthAmbiguous);
    }

    #[test]
    fn body_signals_auth_invalid_is_precise_not_overbroad() {
        // #5 精确性:只有真正不可自愈的凭据/授权失效才算 AuthInvalid。
        // 命中集(真失效):
        assert!(body_signals_auth_invalid(
            "The security token included in the request is invalid."
        ));
        assert!(body_signals_auth_invalid(r#"{"error":"invalid_grant"}"#));
        assert!(body_signals_auth_invalid(r#"{"error":"invalid_token"}"#));
        assert!(body_signals_auth_invalid(
            r#"{"__type":"InvalidGrantException"}"#
        ));
        assert!(body_signals_auth_invalid("The refresh token is invalid."));
        // 非命中集(可恢复/非凭据失效)——绝不能永久禁用:
        // AccessDeniedException 单独出现≠失效(被暂停账号也用它);throttl/quota/suspended 同理。
        assert!(!body_signals_auth_invalid(
            r#"{"__type":"AccessDeniedException","message":"forbidden"}"#
        ));
        assert!(!body_signals_auth_invalid("account suspended"));
        assert!(!body_signals_auth_invalid(
            "ThrottlingException: rate exceeded"
        ));
        assert!(!body_signals_auth_invalid(
            "MONTHLY_REQUEST_COUNT quota reached"
        ));
        assert!(!body_signals_auth_invalid("Forbidden"));
        assert!(!body_signals_auth_invalid("Bad Credentials")); // 太含糊,不再算失效
        assert!(!body_signals_auth_invalid(""));
        // #1 关键:ExpiredToken-类过期措辞**不得**被当作失效(可刷新)。
        assert!(!body_signals_auth_invalid(
            "The security token included in the request is expired."
        ));
        assert!(!body_signals_auth_invalid("token has expired"));
    }

    #[test]
    fn classify_with_body_does_not_permanent_disable_suspended_or_throttle() {
        // #5:403 + AccessDeniedException/suspended/throttle 落 AuthAmbiguous(冷却),不永久禁用。
        assert_eq!(
            classify_with_body(403, r#"{"__type":"AccessDeniedException"}"#),
            FailureKind::AuthAmbiguous
        );
        assert_eq!(
            classify_with_body(403, "account suspended"),
            FailureKind::AuthAmbiguous
        );
        assert_eq!(
            classify_with_body(401, "ThrottlingException"),
            FailureKind::AuthAmbiguous
        );
    }

    #[test]
    fn expired_token_exception_is_ambiguous_not_invalid() {
        // #1 回归:AWS ExpiredTokenException 的真实响应体(__type + message 都在)。
        // "The security token included in the request is expired." —— 过期是**可刷新**的,
        // 绝不能因它永久禁用一个 refreshable 令牌。必须落 AuthAmbiguous(冷却,自愈)。
        let raw = r#"{"__type":"ExpiredTokenException","message":"The security token included in the request is expired."}"#;
        assert!(body_signals_recoverable(raw));
        assert!(!body_signals_auth_invalid(raw));
        assert_eq!(classify_with_body(403, raw), FailureKind::AuthAmbiguous);
        assert_eq!(classify_with_body(401, raw), FailureKind::AuthAmbiguous);
    }

    #[test]
    fn suspended_access_denied_is_ambiguous_not_invalid() {
        // #2 回归:被暂停账号常见 403 + AccessDeniedException/suspended 措辞。
        // 这类**可恢复**,绝不能永久禁用 → AuthAmbiguous。
        let raw = r#"{"__type":"AccessDeniedException","message":"Your account has been suspended as a security precaution."}"#;
        assert!(body_signals_recoverable(raw)); // "suspend" + "security precaution"
        assert!(!body_signals_auth_invalid(raw));
        assert_eq!(classify_with_body(403, raw), FailureKind::AuthAmbiguous);
    }

    #[test]
    fn throttling_403_is_ambiguous_not_invalid() {
        // #2 回归:ThrottlingException / 429-语义出现在 403 体里 → AuthAmbiguous(冷却),不永久禁用。
        let raw = r#"{"__type":"ThrottlingException","message":"Rate exceeded"}"#;
        assert!(body_signals_recoverable(raw));
        assert_eq!(classify_with_body(403, raw), FailureKind::AuthAmbiguous);
        // 429 状态码本身仍归 Quota(与体无关)。
        assert_eq!(classify_with_body(429, raw), FailureKind::Quota);
    }

    #[test]
    fn genuine_invalid_grant_is_auth_invalid() {
        // 真作废:InvalidGrantException(__type)/ invalid_grant / "...is invalid." → AuthInvalid(永久)。
        assert_eq!(
            classify_with_body(
                400,
                r#"{"error":"invalid_grant","error_description":"refresh token revoked"}"#
            ),
            // 400 不是 401/403,invalid_grant 不在 quota 标记里 → Transient(状态码保守)。
            FailureKind::Transient
        );
        // 但在 401/403 上,真作废信号 → AuthInvalid。
        assert_eq!(
            classify_with_body(
                401,
                r#"{"__type":"InvalidGrantException","message":"The provided token grant is invalid."}"#
            ),
            FailureKind::AuthInvalid
        );
        assert_eq!(
            classify_with_body(
                403,
                "The security token included in the request is invalid."
            ),
            FailureKind::AuthInvalid
        );
        assert_eq!(
            classify_with_body(401, r#"{"error":"invalid_grant"}"#),
            FailureKind::AuthInvalid
        );
    }

    #[test]
    fn classify_with_body_maps_quota_markers() {
        // 402 一律配额。
        assert_eq!(
            classify_with_body(402, r#"{"reason":"MONTHLY_REQUEST_COUNT"}"#),
            FailureKind::Quota
        );
        // 非 402/429 状态码但体里带配额标记 → 仍归 Quota。
        assert_eq!(
            classify_with_body(400, r#"{"reason":"MONTHLY_REQUEST_COUNT"}"#),
            FailureKind::Quota
        );
        // 非 402/429 且无配额标记 → Transient。
        assert_eq!(
            classify_with_body(500, "internal error"),
            FailureKind::Transient
        );
    }

    #[test]
    fn round_robin_rotates_across_accounts() {
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        let first = p.select(0).unwrap().id;
        let second = p.select(0).unwrap().id;
        assert_ne!(first, second); // 两次挑到不同账号
    }

    #[test]
    fn select_with_exclude_skips_tried_ids() {
        use std::collections::HashSet;
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        // 排除 "a" → 只能选到 "b"(多次都一样)。
        let mut ex = HashSet::new();
        ex.insert("a".to_string());
        for _ in 0..5 {
            assert_eq!(p.select_with_exclude(0, &ex).unwrap().id, "b");
        }
        // 两个都排除 → None(即便都健康)。
        ex.insert("b".to_string());
        assert!(p.select_with_exclude(0, &ex).is_none());
        // 空排除集 → 行为等同 select(可选到任一健康账号)。
        let none: HashSet<String> = HashSet::new();
        assert!(p.select_with_exclude(0, &none).is_some());
    }

    #[test]
    fn select_with_exclude_still_skips_disabled_and_cooldown() {
        use std::collections::HashSet;
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        // b 进入配额冷却;排除集为空但 b 仍不可选 → 只剩 a。
        p.report_failure("b", FailureKind::Quota, 100);
        let none: HashSet<String> = HashSet::new();
        for _ in 0..5 {
            assert_eq!(p.select_with_exclude(200, &none).unwrap().id, "a");
        }
        // 再把 a 也排除 → 无可选(b 冷却中 + a 被排除)。
        let mut ex = HashSet::new();
        ex.insert("a".to_string());
        assert!(p.select_with_exclude(200, &ex).is_none());
    }

    #[test]
    fn auth_invalid_disables_permanently() {
        // 只有真凭据失效(响应体确认)才永久禁用。经 classify_with_body 得到 AuthInvalid。
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        let kind = classify_with_body(401, r#"{"error":"invalid_grant"}"#);
        assert_eq!(kind, FailureKind::AuthInvalid);
        p.report_failure("a", kind, 100);
        assert_eq!(p.active_count(100), 0); // 已禁用
        assert_eq!(p.active_count(1_000_000), 0); // 永不恢复
        assert!(p.select(1_000_000).is_none());
    }

    #[test]
    fn ambiguous_auth_cools_with_strikes_not_permanent_disable() {
        // 裸 403(无失效信号)不永久禁用:累计 AUTH_AMBIGUOUS_STRIKES 次后才冷却,冷却后恢复。
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        let kind = classify(403); // 无响应体 → AuthAmbiguous
        assert_eq!(kind, FailureKind::AuthAmbiguous);
        p.report_failure("a", kind, 100);
        assert!(p.select(100).is_some()); // 第 1 次还不冷却
        p.report_failure("a", kind, 100);
        assert!(p.select(100).is_none()); // 第 2 次(达阈值)进入冷却
        // 5 分钟冷却后恢复,且从未 disabled(永久)。
        assert!(p.select(100 + AUTH_AMBIGUOUS_COOLDOWN + 1).is_some());
        let st = p.stats(100 + AUTH_AMBIGUOUS_COOLDOWN + 1);
        assert!(!st[0].disabled);
    }

    #[test]
    fn quota_failure_cools_then_recovers() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        p.report_failure("a", FailureKind::Quota, 100);
        assert!(p.select(200).is_none()); // 冷却中
        assert!(p.select(100 + 30 * 60 + 1).is_some()); // 30 分钟后恢复
    }

    #[test]
    fn transient_cools_only_after_threshold() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        p.report_failure("a", FailureKind::Transient, 100);
        p.report_failure("a", FailureKind::Transient, 100);
        assert!(p.select(100).is_some()); // 2 次还不冷却
        p.report_failure("a", FailureKind::Transient, 100);
        assert!(p.select(100).is_none()); // 第 3 次冷却
        assert!(p.select(100 + 91).is_some()); // 90 秒后恢复
        // 成功清零
        p.report_failure("a", FailureKind::Transient, 300);
        p.report_success("a");
        p.report_failure("a", FailureKind::Transient, 300);
        p.report_failure("a", FailureKind::Transient, 300);
        assert!(p.select(300).is_some()); // 清零后又要凑够 3 次
    }

    #[test]
    fn balanced_favors_higher_weight() {
        let mut p = Pool::new(vec![cred("a", 3), cred("b", 1)], LbMode::Balanced);
        let mut count_a = 0;
        let mut count_b = 0;
        for _ in 0..100 {
            match p.select(0).unwrap().id.as_str() {
                "a" => count_a += 1,
                "b" => count_b += 1,
                other => panic!("unexpected id: {other}"),
            }
        }
        assert_eq!(count_a + count_b, 100);
        assert!(count_a > count_b);
        assert!(count_a >= 2 * count_b);
    }

    /// set_mode 运行期切换即时改变选择行为:priority 下等权轮转两账号各半;
    /// 切到 balanced 后高权(a:3)明显多于低权(b:1)。证明 admin PUT load-balancing
    /// 调 pool.set_mode 后无需重建池即生效。
    #[test]
    fn set_mode_switches_selection_behavior_live() {
        let mut p = Pool::new(vec![cred("a", 3), cred("b", 1)], LbMode::Priority);
        // priority:等权轮转 → 两账号计数接近(各约一半)。
        let mut a0 = 0;
        for _ in 0..100 {
            if p.select(0).unwrap().id == "a" {
                a0 += 1;
            }
        }
        assert!(
            (40..=60).contains(&a0),
            "priority should be roughly balanced, got a={a0}"
        );

        // 运行期切到 balanced → 高权 a 明显多于低权 b。
        p.set_mode(LbMode::Balanced);
        let mut a1 = 0;
        for _ in 0..100 {
            if p.select(0).unwrap().id == "a" {
                a1 += 1;
            }
        }
        assert!(a1 >= 70, "balanced should favor higher weight, got a={a1}");
    }

    #[test]
    fn disabled_credential_is_excluded_from_selection() {
        let mut disabled_cred = cred("bad", 1);
        disabled_cred.disabled = true;
        let mut p = Pool::new(vec![disabled_cred, cred("ok", 1)], LbMode::Priority);
        assert_eq!(p.active_count(0), 1);
        for _ in 0..5 {
            assert_eq!(p.select(0).unwrap().id, "ok");
        }
    }

    #[test]
    fn rpm_limit_blocks_after_threshold_then_recovers_outside_window() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        p.set_max_rpm(2);
        assert_eq!(p.select(1000).unwrap().id, "a");
        assert_eq!(p.select(1000).unwrap().id, "a");
        assert!(p.select(1000).is_none()); // 同窗口内第 3 次超限
        assert_eq!(p.select(1061).unwrap().id, "a"); // 超 60s 窗口,旧时刻滑出
    }

    #[test]
    fn rpm_readout_counts_window_and_slides_out() {
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        // 未知 id → None
        assert_eq!(p.rpm_of("nope", 1000), None);
        // 初始 0
        assert_eq!(p.rpm_of("a", 1000), Some(0));
        // 选 3 次(a、b、a):a 计 2、b 计 1(Priority 轮转)
        p.select(1000);
        p.select(1000);
        p.select(1000);
        let a = p.rpm_of("a", 1000).unwrap();
        let b = p.rpm_of("b", 1000).unwrap();
        assert_eq!(a + b, 3);
        // rpm_all 与逐个一致,且总和守恒
        let all: std::collections::HashMap<_, _> = p.rpm_all(1000).into_iter().collect();
        assert_eq!(all["a"], a);
        assert_eq!(all["b"], b);
        // 只读:重复读不改动计数
        assert_eq!(p.rpm_of("a", 1000), Some(a));
        // 越过 60s 窗口后旧时刻滑出 → 0
        assert_eq!(p.rpm_of("a", 1061), Some(0));
        assert_eq!(p.rpm_of("b", 1061), Some(0));
        // AccountStat.rpm 与 rpm_of 同口径
        let st = p.stats(1000);
        let sa = st.iter().find(|s| s.id == "a").unwrap();
        assert_eq!(sa.rpm, a);
    }

    #[test]
    fn zero_max_rpm_is_unlimited_regression() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        // 不 set_max_rpm,默认 0 = 无限
        for _ in 0..5 {
            assert!(p.select(1000).is_some());
        }
    }

    #[test]
    fn rpm_limit_spreads_across_accounts_then_exhausts() {
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        p.set_max_rpm(1);
        let first = p.select(1000).unwrap().id;
        let second = p.select(1000).unwrap().id;
        assert_ne!(first, second); // a 满后跳到 b
        assert!(p.select(1000).is_none()); // 两个都超限
    }

    #[test]
    fn stats_tracks_requests_and_last_used() {
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        let picked = p.select(500).unwrap().id;
        let stats = p.stats(500);
        assert_eq!(stats.len(), 2);
        let picked_stat = stats.iter().find(|s| s.id == picked).unwrap();
        assert!(picked_stat.requests > 0);
        assert_eq!(picked_stat.last_used_unix, 500);
        let other_stat = stats.iter().find(|s| s.id != picked).unwrap();
        assert_eq!(other_stat.requests, 0);
        assert_eq!(other_stat.last_used_unix, 0);
    }

    #[test]
    fn stats_reflects_success_count() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        p.report_success("a");
        let stats = p.stats(0);
        assert_eq!(stats[0].successes, 1);
    }

    #[test]
    fn stats_reflects_auth_failure() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        p.report_failure("a", FailureKind::AuthInvalid, 100);
        let stats = p.stats(100);
        assert_eq!(stats[0].failures, 1);
        assert!(stats[0].disabled);
    }

    #[test]
    fn stats_reflects_quota_cooldown() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        p.report_failure("a", FailureKind::Quota, 100);
        let stats = p.stats(100);
        assert_eq!(stats[0].failures, 1);
        assert!(stats[0].in_cooldown);
        assert!(stats[0].cooldown_until > 100);
    }

    #[test]
    fn account_stat_serializes_to_json() {
        let p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        let stats = p.stats(0);
        let value = serde_json::to_value(&stats[0]).unwrap();
        assert_eq!(value["id"], "a");
        assert_eq!(value["requests"], 0);
        assert_eq!(value["successes"], 0);
        assert_eq!(value["failures"], 0);
        assert_eq!(value["last_used_unix"], 0);
        assert_eq!(value["disabled"], false);
        assert_eq!(value["cooldown_until"], 0);
        assert_eq!(value["in_cooldown"], false);
        assert_eq!(value["auth_method"], "social");
        assert_eq!(value["region"], "us-east-1");
        assert_eq!(value["email"], serde_json::Value::Null);
    }

    #[test]
    fn set_disabled_toggles_and_excludes_from_selection() {
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        assert!(p.set_disabled("a", true));
        let stats = p.stats(0);
        let a_stat = stats.iter().find(|s| s.id == "a").unwrap();
        assert!(a_stat.disabled);
        // select 只应挑到 b(a 被禁用)
        for _ in 0..5 {
            assert_eq!(p.select(0).unwrap().id, "b");
        }
        // 恢复
        assert!(p.set_disabled("a", false));
        let stats = p.stats(0);
        let a_stat = stats.iter().find(|s| s.id == "a").unwrap();
        assert!(!a_stat.disabled);
        // 不存在的账号返回 false
        assert!(!p.set_disabled("nope", true));
    }

    #[test]
    fn account_stat_metadata_matches_credential() {
        let mut idc_cred = cred("idc-acct", 1);
        idc_cred.auth = AuthMethod::Idc;
        idc_cred.region = "eu-west-1".into();
        idc_cred.email = Some("user@example.com".into());
        let p = Pool::new(vec![idc_cred], LbMode::Priority);
        let stats = p.stats(0);
        assert_eq!(stats[0].auth_method, "idc");
        assert_eq!(stats[0].region, "eu-west-1");
        assert_eq!(stats[0].email.as_deref(), Some("user@example.com"));
    }

    #[test]
    fn add_credential_assigns_sequential_numeric_ids() {
        let mut p = Pool::new(vec![cred("5", 1), cred("9", 1)], LbMode::Priority);
        // 空入参 id 会被忽略,分配 max(5,9)+1 = 10
        let mut new_cred = cred("ignored", 2);
        new_cred.email = Some("new@example.com".into());
        let (id, email) = p.add_credential(new_cred);
        assert_eq!(id, "10");
        assert_eq!(email.as_deref(), Some("new@example.com"));
        // 立即可被选择(健康态)
        assert_eq!(p.active_count(0), 3);
        // 快照包含新凭据且 id 已重写
        let snap = p.snapshot_credentials();
        assert!(snap.iter().any(|c| c.id == "10" && c.weight == 2));
    }

    #[test]
    fn add_credential_to_empty_pool_starts_at_one() {
        let mut p = Pool::new(vec![], LbMode::Priority);
        let (id, _) = p.add_credential(cred("x", 1));
        assert_eq!(id, "1");
        assert_eq!(p.snapshot_credentials().len(), 1);
    }

    #[test]
    fn update_credential_changes_only_provided_fields() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        let upd = CredentialUpdate {
            email: Some("changed@example.com".into()),
            weight: Some(7),
            refresh_token: Some("new-rt".into()),
            ..Default::default()
        };
        assert!(p.update_credential("a", upd));
        let snap = p.snapshot_credentials();
        let c = &snap[0];
        assert_eq!(c.email.as_deref(), Some("changed@example.com"));
        assert_eq!(c.weight, 7);
        assert_eq!(c.refresh_token, "new-rt");
        // 未提供的字段保持不变
        assert_eq!(c.access_token, "at");
        assert_eq!(c.region, "us-east-1");
        // 未知 id → false
        assert!(!p.update_credential("nope", CredentialUpdate::default()));
    }

    #[test]
    fn update_credential_tokens_rotates_in_place() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        assert!(p.update_credential_tokens("a", "new-at".into(), "new-rt".into(), 9999));
        let snap = p.snapshot_credentials();
        let c = &snap[0];
        assert_eq!(c.access_token, "new-at");
        assert_eq!(c.refresh_token, "new-rt");
        assert_eq!(c.expires_at_unix, 9999);
        // 其它字段不受影响
        assert_eq!(c.region, "us-east-1");
        assert_eq!(c.weight, 1);
        // 未知 id → false
        assert!(!p.update_credential_tokens("nope", "x".into(), "y".into(), 1));
    }

    #[test]
    fn remove_credential_drops_entry() {
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        assert!(p.remove_credential("a"));
        let snap = p.snapshot_credentials();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "b");
        // 再删同一个 → false
        assert!(!p.remove_credential("a"));
        // 池只剩 b 可选
        assert_eq!(p.select(0).unwrap().id, "b");
    }

    #[test]
    fn set_priority_maps_to_weight_with_floor_one() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        assert!(p.set_priority("a", 5));
        assert_eq!(p.snapshot_credentials()[0].weight, 5);
        // 低于 1 的优先级钳到 1
        assert!(p.set_priority("a", 0));
        assert_eq!(p.snapshot_credentials()[0].weight, 1);
        assert!(p.set_priority("a", -3));
        assert_eq!(p.snapshot_credentials()[0].weight, 1);
        // 未知 id → false
        assert!(!p.set_priority("nope", 2));
    }

    #[test]
    fn reset_failures_clears_strikes_and_cooldown() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        // 打到配额冷却
        p.report_failure("a", FailureKind::Quota, 100);
        assert!(p.select(200).is_none()); // 冷却中
        assert!(p.reset_failures("a"));
        assert!(p.select(200).is_some()); // 重置后立即可选
        // strikes 也清零
        p.report_failure("a", FailureKind::Transient, 300);
        p.report_failure("a", FailureKind::Transient, 300);
        assert!(p.reset_failures("a"));
        let st = p.stats(300);
        assert_eq!(st[0].strikes, 0);
        // 未知 id → false
        assert!(!p.reset_failures("nope"));
    }

    #[test]
    fn account_stat_never_leaks_secret_tokens() {
        let mut secret_cred = cred("secret-acct", 1);
        secret_cred.access_token = "SEKRET-AT".into();
        secret_cred.refresh_token = "SEKRET-RT".into();
        secret_cred.client_secret = Some("SEKRET-CS".into());
        let p = Pool::new(vec![secret_cred], LbMode::Priority);
        let stats = p.stats(0);
        let serialized = serde_json::to_string(&stats[0]).unwrap();
        assert!(!serialized.contains("SEKRET-AT"));
        assert!(!serialized.contains("SEKRET-RT"));
        assert!(!serialized.contains("SEKRET-CS"));
        assert!(!serialized.contains("accessToken"));
        assert!(!serialized.contains("refreshToken"));
    }
}
