//! Kiro 多账号池:健康账号选择 + 失败分级冷却 + 负载均衡。
//! 冷却时长与错误分类均按可观测错误语义自行设计(非移植);时钟以 now_unix 注入。
use crate::kiro::credential::{AuthMethod, Credential};
use crate::server::auth::BoundCredentialIds;
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

/// 响应体是否为**确定性请求错误**(换账号重试也无用)。刻意只匹配机器稳定的 reason 码,
/// 不泛化到任意 400,避免把可重试的瞬时错误误判为永久。
///
/// 目前两种:
/// - `INVALID_MODEL_ID` —— 该模型对当前账号档位不可用(FREE 档请求 opus/GPT 即命中)。
/// - `CONTENT_LENGTH_EXCEEDS_THRESHOLD` —— 请求体超过上游长度上限。
/// - `TOOL_CONFIG_MISSING` —— 消息里有 `toolUse`/`toolResult` 块却没有工具定义。
///   正常情况下中枢不该造出这种请求(见 `convert::tool_specs_with_history_fallback`),
///   留这一条是兜底:万一仍然发生,也必须**当场以 400 告知**,而不是当成瞬时错误
///   跨账号重试一遍、把整池账号打伤。
///
/// 第二种此前没被认出来,落进 `Transient`(瞬时错误、值得重试),后果实测很重:
/// 一个超长请求会被跨账号重试一遍,**每换一个账号就给它记一次失败、攒一次 strike**,
/// 而换账号根本不可能成功(问题在请求本身)。线上一个下午就把 253 个健康账号打成
/// 149 个带伤、26 个进冷却,可用池缩水后开始回 503「no available upstream account」。
/// 客户端那头看到的则是 502 `upstream request failed` —— 完全看不出是上下文超长。
pub fn body_signals_invalid_request(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("invalid_model_id")
        || lower.contains("content_length_exceeds_threshold")
        || lower.contains("tool_config_missing")
}

/// 确定性请求错误回给客户端的说明。**必须按 reason 码分别给话**:同一档 `InvalidRequest`
/// 现在含义不止一种,笼统套一句"模型不可用"会把"上下文超长"讲成完全无关的另一回事,
/// 使用者照着去换模型,换多少次都没用。
pub fn invalid_request_message(body: &str, model: &str) -> String {
    let lower = body.to_ascii_lowercase();
    if lower.contains("tool_config_missing") {
        return "Tool configuration missing: the conversation contains tool calls but the \
                request declared no tools. Resend the tool definitions along with the history."
            .to_string();
    }
    if lower.contains("content_length_exceeds_threshold") {
        // 讲清"为什么"和"怎么办":这个错误不会自愈 —— 客户端每轮重发完整历史,
        // 下一轮只会更长,该会话从此必然失败,唯一出路是缩短上下文或新开会话。
        "Input is too long: the request exceeds the upstream context limit. \
         This will not recover on its own -- each turn resends the whole conversation, \
         so the next one is larger still. Start a new conversation or trim the context."
            .to_string()
    } else {
        format!(
            "Invalid model '{model}': not available for the current account. \
             Please select a different model to continue."
        )
    }
}

/// 账号最近一次失败的**具体原因**,用于把笼统的"异常"拆成运维真正关心的几档。
///
/// 分类只影响**展示与筛选**,不改变选号纪律:`suspend` 命中的账号照旧按可自愈处理(冷却后
/// 重新参选),因为被暂停的账号确实可能恢复;但运维需要一眼看出"这批是被上游封了",而不是
/// 和限流、令牌过期混在同一个"异常"里查不出所以然。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusReason {
    /// 无最近失败,或失败原因不属于下列任何一类。
    #[default]
    None,
    /// 上游响应体含 suspend/suspended:账号被上游暂停(封禁)。
    Banned,
    /// 配额/额度耗尽(429,或响应体含 quota)。
    QuotaExhausted,
    /// 令牌过期类(ExpiredTokenException 等)。刷新即可自愈,不是账号问题。
    TokenExpired,
    /// 限流 / 上游风控临时拦截。
    Throttled,
    /// **续期被上游拒绝**(如 `access_denied`):refreshToken 本身在,格式也对,是上游不给续。
    ///
    /// 与 [`StatusReason::TokenExpired`] 的区别很要紧:后者刷一下就好,前者刷多少次都没用,
    /// 通常意味着这批账号在上游被整体处置(权限收回 / 组织侧禁用 / 密钥轮换)。运维看到这一
    /// 档就该去找账号来源,而不是继续等它自愈。
    RefreshDenied,
}

impl Entry {
    /// 设置结论,并同步到落盘那份。
    ///
    /// 两者必须一起改,理由与 `disabled`/`cred.disabled` 完全相同:只改内存,重启即丢
    /// (封禁账号会悄悄回到可用池);只改落盘,本轮判定不生效。
    fn set_status_reason(&mut self, r: StatusReason) {
        self.status_reason = r;
        self.cred.status_reason = match r {
            StatusReason::None => None,
            other => Some(other.as_str().to_string()),
        };
    }
}

impl StatusReason {
    /// 从落盘的字符串还原;认不出的一律当 `None`。
    ///
    /// 与 [`as_str`](Self::as_str) 严格互逆:封禁结论要跨重启活下来,就必须能从盘上读回来。
    /// 认不出时回落 `None`(而非报错),是为了让旧版写下的、或将来新增而本版还不认识的取值
    /// 只丢一个标签,不至于让整份 credentials.json 加载失败、全池打不开。
    pub fn from_wire(s: &str) -> Self {
        match s {
            "banned" => StatusReason::Banned,
            "quota" => StatusReason::QuotaExhausted,
            "token_expired" => StatusReason::TokenExpired,
            "throttled" => StatusReason::Throttled,
            "refresh_denied" => StatusReason::RefreshDenied,
            _ => StatusReason::None,
        }
    }

    /// 出站线格式用的稳定短名(前端按它筛选)。
    pub fn as_str(self) -> &'static str {
        match self {
            StatusReason::None => "none",
            StatusReason::Banned => "banned",
            StatusReason::QuotaExhausted => "quota",
            StatusReason::TokenExpired => "token_expired",
            StatusReason::Throttled => "throttled",
            StatusReason::RefreshDenied => "refresh_denied",
        }
    }
}

/// 从上游原始响应体判定失败原因。
///
/// 顺序有讲究:`suspend` 优先于 `throttl`/`quota` —— 上游把账号停用时,响应体里常常同时
/// 带着限流措辞,先匹配限流会把封禁误报成一次普通限流,运维就永远等不到那批号"自己恢复"。
pub fn status_reason_from_body(body: &str) -> StatusReason {
    let lower = body.to_ascii_lowercase();
    if lower.contains("suspend") {
        StatusReason::Banned
    } else if lower.contains("quota") {
        StatusReason::QuotaExhausted
    } else if lower.contains("expired") {
        StatusReason::TokenExpired
    } else if lower.contains("throttl")
        || lower.contains("too many requests")
        || lower.contains("rate exceeded")
        || lower.contains("security precaution")
    {
        StatusReason::Throttled
    } else {
        StatusReason::None
    }
}

/// 从**令牌刷新**的失败结果判定原因。
///
/// 与 [`status_reason_from_body`](自 relay 数据面响应体判定)分开:刷新端点的措辞是 OAuth
/// 那一套(`access_denied` / `invalid_grant` / `expired_token`),和数据面的异常名不同源。
pub fn refresh_reason_from(status: u16, detail: &str) -> StatusReason {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("access_denied") || lower.contains("access denied") {
        StatusReason::RefreshDenied
    } else if lower.contains("invalid_grant") {
        // 授权已作废 —— 同样是"刷多少次都没用",归入同一档,运维的处置动作一致。
        StatusReason::RefreshDenied
    } else if lower.contains("expired") {
        StatusReason::TokenExpired
    } else if lower.contains("throttl") || lower.contains("too many requests") || status == 429 {
        StatusReason::Throttled
    } else if lower.contains("quota") {
        StatusReason::QuotaExhausted
    } else {
        StatusReason::None
    }
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

/// **刷新端点**(Kiro auth `/refreshToken`、AWS SSO-OIDC `/token`)失败的分类。
///
/// 不能直接复用 [`classify_with_body`]:OAuth/OIDC token 端点用 **400 + `error=invalid_grant`**
/// 表达"该授权已作废",而数据面的 400 是请求错误;若按数据面口径分类,refresh_token 被吊销的
/// 账号只会被记成一次瞬时失败,继续以健康态被选中、每次都白打一枪注定失败的刷新。
///
/// 判定顺序与 [`classify_with_body`] 一致地保守:
/// 1. 体带**可自愈**信号([`body_signals_recoverable`]:过期/暂停/限流/配额/风控)→ `AuthAmbiguous`;
/// 2. **仅在 400/401/403** 上,体带**真凭据失效**信号([`body_signals_auth_invalid`]:invalid_grant
///    等)→ `AuthInvalid`(永久禁用);
/// 3. 402/429 → `Quota`;401/403 但无确证 → `AuthAmbiguous`(冷却,不禁用);
/// 4. 其余(5xx/网关错误/无体)→ `Transient`(记 strike);其中若体里恰好带失效字样,
///    降级为 `AuthAmbiguous`(冷却,可自愈)而非禁用。
pub fn classify_refresh_failure(status: u16, body: &str) -> FailureKind {
    if body_signals_recoverable(body) {
        return FailureKind::AuthAmbiguous;
    }
    // 永久禁用的升级**必须**被状态码收口:只有上游真的在"报授权作废"的那几个码才算确证——
    // token 端点用 400 + error=invalid_grant(RFC 6749 §5.2)表达授权已作废,401/403 则是凭据被拒。
    // 其余状态码(5xx、网关/代理返回的 HTML 错误页、404、传输层无应答的 0)哪怕体里恰好出现
    // "invalid" 字样,也只是错误页文案的巧合,不足以证明凭据作废;不收口就会因上游一次抖动把
    // 完全健康的账号永久禁用。这与数据面 [`classify_with_body`] 把同一判定关在 401|403 分支内
    // 是同一条纪律。
    let signals_auth_invalid = body_signals_auth_invalid(body);
    if signals_auth_invalid && matches!(status, 400 | 401 | 403) {
        return FailureKind::AuthInvalid;
    }
    match status {
        402 | 429 => FailureKind::Quota,
        401 | 403 => FailureKind::AuthAmbiguous,
        // 命中失效标记但状态码不构成确证 → 落 AuthAmbiguous(自愈冷却),绝不永久禁用。
        _ if signals_auth_invalid => FailureKind::AuthAmbiguous,
        _ => FailureKind::Transient,
    }
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
    /// 运行时禁用态。与 `cred.disabled`(待落盘的那份)**始终同步**:两者任一被单独改写,
    /// 都会让"手工启停/永久禁用"在下一次落盘或下一次重启时静默丢失。
    disabled: bool,
    cooldown_until: u64,
    strikes: u32,
    /// 最近一次"换新令牌"的尝试因**传输层错误**(连接超时/DNS 失败,无 HTTP 应答)未成行。
    /// 此时账号从未真的用新令牌重试过,紧随其后的 [`FailureKind::AuthInvalid`] 判定缺乏确证,
    /// 按本模块"拿不到确证就保守冷却"的纪律降级为 [`FailureKind::AuthAmbiguous`]。
    /// 一次性:被降级消费、或换新令牌成功写回、或调用成功后清除。
    unconfirmed_refresh: bool,
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
    /// 最近一次失败的具体原因(仅用于展示/筛选,见 [`StatusReason`])。成功即清空。
    status_reason: StatusReason,
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
    /// 最近一次失败的具体原因(见 [`StatusReason::as_str`]);无则 `"none"`。
    pub status_reason: &'static str,
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
    /// 下一个待分配的数值 id(单调递增,删除账号**不回收**其编号)。
    next_id: u64,
}

impl Pool {
    pub fn new(creds: Vec<Credential>, mode: LbMode) -> Self {
        Self::with_next_id(creds, mode, 0)
    }

    /// 带 id 高水位的构造:`persisted_next_id` 为上次落盘记下的"下一个可用 id"
    /// (见 [`crate::kiro::credential::load_next_id`]);无该记录时传 0。
    ///
    /// 实际起点取 `max(persisted_next_id, max(现有数值 id) + 1)`——旧数据没有高水位记录时
    /// 平滑退化为原来的 `max+1` 语义,有记录时则连"末号账号已被删除"也不会再复用其编号。
    pub fn with_next_id(creds: Vec<Credential>, mode: LbMode, persisted_next_id: u64) -> Self {
        let max_existing = creds
            .iter()
            .filter_map(|c| c.id.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        let entries = creds
            .into_iter()
            .map(|c| Entry {
                // 配置自相矛盾的凭据(声明 api_key 却没给 key)入池即禁用:它取不到 bearer,
                // 又被判定为 API Key 凭据(故不刷新),留在池里只会在跨账号重试中反复空转。
                // 与其让运行时每次选中都失败一次,不如在这一刻就判死。
                disabled: c.disabled || c.is_invalid_api_key_config(),
                // 结论从盘上还原。strike 与冷却**故意不还原**:那两个是计时器,重启后从零
                // 开始无非让账号早重试一次;结论不还原则会让封禁账号在每次重启后悄悄回池。
                status_reason: StatusReason::from_wire(c.status_reason.as_deref().unwrap_or("")),
                cooldown_until: 0,
                strikes: 0,
                unconfirmed_refresh: false,
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
            next_id: persisted_next_id.max(max_existing.saturating_add(1)),
        }
    }

    /// 当前 id 高水位(下一个待分配的数值 id),供落盘时一并持久化
    /// (见 [`crate::kiro::credential::save_next_id`])。
    pub fn next_id_hint(&self) -> u64 {
        self.next_id
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

    /// 可用 = 未禁用 && 不在冷却 && 上一次的结论不是"被上游封禁"。
    ///
    /// 前两项是**计时器**,一到点账号就回池;而封禁是个**结论**——上游原话是"账号已锁定,
    /// 请联系客服验证身份",不会因为等了五分钟或半小时就解除。只看计时器的后果是:冷却一过
    /// 账号重新入选、再失败、再冷却,循环烧真实请求,而面板同时挂着"封禁"标签、可用数却把它
    /// 算在内——两个数字互相矛盾,运维无从判断哪个可信。
    ///
    /// 封禁账号因此不再被选中,也不计入可用数;它不会自己恢复,出口是面板的「重置」
    /// ([`reset_failures`](Self::reset_failures),会一并清掉该结论)。
    fn is_active(e: &Entry, now_unix: u64) -> bool {
        !e.disabled && e.cooldown_until <= now_unix && e.status_reason != StatusReason::Banned
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

    /// 选一个健康账号,但跳过 `exclude_ids` 中已试过的凭据 id(跨账号重试用),并在
    /// `allowed` 在场时只从它放行的凭据里选。除这两层过滤外,选择纪律与
    /// [`select`](Self::select) 完全一致(disabled/冷却/RPM 均跳过、沿用当前 LB 模式与
    /// cursor)。无满足条件的账号时返回 None(中转层据此回 503)。
    ///
    /// 供中转层跨账号重试:某账号数据面失败后,以已试过的 id 集合再选下一个不同的健康账号。
    ///
    /// `allowed` 为本次请求的 store-key 绑定白名单(鉴权闸从命中的那条 key 上解析,经请求
    /// 扩展下传;`None` = 不受限)。**这是绑定唯一的执行点**:成员判定一律委托
    /// [`BoundCredentialIds::allows`],不在此处另写一份 id 解析,以免两处口径漂移。
    pub fn select_with_exclude(
        &mut self,
        now_unix: u64,
        exclude_ids: &std::collections::HashSet<String>,
        allowed: Option<&BoundCredentialIds>,
    ) -> Option<Credential> {
        self.select_excluding(now_unix, |c| {
            exclude_ids.contains(&c.id) || allowed.is_some_and(|b| !b.allows(&c.id))
        })
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
    ///
    /// `disabled` 以运行时那份为准写出:落盘路径绝不能拿 `cred` 里可能陈旧的旧值,
    /// 把刚发生的手工停用/永久禁用覆写回去。
    pub fn snapshot_credentials(&self) -> Vec<Credential> {
        self.entries
            .iter()
            .map(|e| {
                let mut c = e.cred.clone();
                c.disabled = e.disabled;
                c
            })
            .collect()
    }

    /// 为新凭据分配数值 id:取单调递增计数器,并与"现有 id 最大值 +1"取较大者
    /// (兜住手工编辑 credentials.json 塞进更大 id 的情形)。分配后计数器前进。
    ///
    /// 单调:删除账号**不回收**其编号。若按 `max(现有 id)+1` 分配,删掉末号账号后新账号会复用
    /// 该编号,与"刷新完成后按 id 写回池""按 id 归属用量统计"叠加即张冠李戴。
    fn alloc_id(&mut self) -> String {
        let max_existing = self
            .entries
            .iter()
            .filter_map(|e| e.cred.id.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        let id = self.next_id.max(max_existing.saturating_add(1));
        self.next_id = id.saturating_add(1);
        id.to_string()
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
        let id = self.alloc_id();
        cred.id = id.clone();
        let email = cred.email.clone();
        self.entries.push(Entry {
            disabled: cred.disabled,
            cooldown_until: 0,
            strikes: 0,
            unconfirmed_refresh: false,
            req_times: Vec::new(),
            requests: 0,
            successes: 0,
            failures: 0,
            last_used_unix: 0,
            status_reason: StatusReason::None,
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
            e.unconfirmed_refresh = false;
            true
        } else {
            false
        }
    }

    /// 刷新写回(**带身份校验**):仅当目标 entry 的 `refresh_token` 仍等于发起刷新时持有的
    /// `expected_refresh_token` 才写回;不匹配返回 false 且**不改动**该 entry。
    ///
    /// 动机:刷新期间该 id 上的账号可能已被删除、并由新导入的账号占用同一编号。此时按 id 盲写
    /// 会把旧账号刷来的令牌盖到新账号头上并随后落盘,新账号的 refresh_token 就此静默丢失。
    /// `refresh_token` 是账号的稳定标识(导入侧已按它去重,见 [`Self::find_id_by_refresh_token`]),
    /// 故用它比对身份。
    pub fn update_credential_tokens_checked(
        &mut self,
        id: &str,
        expected_refresh_token: &str,
        access_token: String,
        refresh_token: String,
        expires_at_unix: u64,
    ) -> bool {
        let Some(e) = self.find(id) else {
            return false;
        };
        if e.cred.refresh_token != expected_refresh_token {
            return false;
        }
        e.cred.access_token = access_token;
        e.cred.refresh_token = refresh_token;
        e.cred.expires_at_unix = expires_at_unix;
        e.unconfirmed_refresh = false;
        true
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
            // 配置自相矛盾的凭据不得被"重置"救活:重置只清 strike/冷却/结论,
            // 改不了"声明了 api_key 却没有 key"这件事,复活后立刻重新走回同一条错误路径。
            // 出路是先把配置改对再重启,而不是反复点重置。(照观测,结论同上。)
            if e.cred.is_invalid_api_key_config() {
                return false;
            }
            e.strikes = 0;
            e.cooldown_until = 0;
            // 结论也要清。封禁账号被 `is_active` 挡在池外、永远轮不到成功来清标签,
            // 「重置」是它唯一的出口;只清计时器不清结论,这个按钮对封禁号就是个空操作。
            e.set_status_reason(StatusReason::None);
            true
        } else {
            false
        }
    }

    /// 手动启停:按 id 找账号设置 disabled;不动 cooldown/strikes。
    /// 返回是否找到该账号。
    ///
    /// 运行时禁用态与待落盘凭据里的那份**一并**改写:只改其中一处,状态要么落不了盘
    /// (重启即复活),要么被下一次落盘用旧值覆写。
    pub fn set_disabled(&mut self, id: &str, disabled: bool) -> bool {
        if let Some(e) = self.find(id) {
            e.disabled = disabled;
            e.cred.disabled = disabled;
            true
        } else {
            false
        }
    }

    /// 记录一次因**传输层错误**(连接超时/DNS 失败,无 HTTP 应答)而未成行的换新令牌尝试。
    ///
    /// 该标记会让**紧随其后**的一次 [`FailureKind::AuthInvalid`] 反馈降级为
    /// [`FailureKind::AuthAmbiguous`](冷却):账号从未真的用新令牌重试过,那次失效判定
    /// 没有确证。返回是否命中该账号。
    pub fn note_refresh_transport_failure(&mut self, id: &str) -> bool {
        if let Some(e) = self.find(id) {
            e.unconfirmed_refresh = true;
            true
        } else {
            false
        }
    }

    /// 成功:清零连续失败计数,累加成功用量统计。
    pub fn report_success(&mut self, id: &str) {
        if let Some(e) = self.find(id) {
            e.strikes = 0;
            e.unconfirmed_refresh = false;
            e.successes += 1;
            // 一次成功即说明上次那个原因(封禁/限流/过期…)已经过去,标签必须跟着清,
            // 否则面板会把一个已经恢复的账号一直挂在"封禁"那一档。
            e.set_status_reason(StatusReason::None);
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
        self.report_failure_with_reason(id, kind, StatusReason::None, now_unix);
    }

    /// 同 [`report_failure`](Self::report_failure),但额外记下**这次失败的具体原因**
    /// (由上游响应体判定,见 [`status_reason_from_body`]),供管理面把"异常"拆成
    /// 封禁 / 额度耗尽 / 令牌过期 / 限流几档展示。
    ///
    /// 原因只进展示层:选号纪律、冷却时长、禁用判定一律不受它影响 —— 分类错了最多让面板
    /// 上的标签不准,绝不能让一个健康账号因为措辞匹配被判死。
    pub fn report_failure_with_reason(
        &mut self,
        id: &str,
        kind: FailureKind,
        reason: StatusReason,
        now_unix: u64,
    ) {
        if let Some(e) = self.find(id) {
            e.failures += 1;
            if reason != StatusReason::None {
                e.set_status_reason(reason);
            }
            // 上一次换新令牌的尝试刚因传输层错误未成行 → 这次 AuthInvalid 背后并没有"用新令牌
            // 重试过"的确证(403 的失效措辞对被服务端轮换掉的 access_token 同样成立,那类账号
            // 强制刷新即自愈)。按保守纪律降级为冷却,标记一次性消费。
            let kind = if kind == FailureKind::AuthInvalid && e.unconfirmed_refresh {
                e.unconfirmed_refresh = false;
                FailureKind::AuthAmbiguous
            } else {
                kind
            };
            match kind {
                FailureKind::AuthInvalid => {
                    e.disabled = true;
                    e.cred.disabled = true;
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
                status_reason: e.status_reason.as_str(),
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
        AuthMethod::ApiKey => "apikey",
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
            weight,
            label: None,
            disabled: false,
            status_reason: None,
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

    /// 封禁必须能从"异常"里被单独识别出来 —— 否则运维在 1000 个账号里分不清哪些是被上游
    /// 停用、哪些只是限流,而两者的处置完全不同(前者要换号,后者等一会儿就好)。
    #[test]
    fn status_reason_separates_ban_from_the_other_recoverable_failures() {
        // 优先级:上游停用账号时响应体常常同时带限流措辞,先匹配限流会把封禁误报成限流。
        assert_eq!(
            status_reason_from_body(
                r#"{"__type":"AccessDeniedException","message":"Your User ID is suspended. Too many requests"}"#
            ),
            StatusReason::Banned
        );
        assert_eq!(
            status_reason_from_body(r#"{"message":"Monthly quota exceeded"}"#),
            StatusReason::QuotaExhausted
        );
        assert_eq!(
            status_reason_from_body(r#"{"__type":"ExpiredTokenException"}"#),
            StatusReason::TokenExpired
        );
        assert_eq!(
            status_reason_from_body(r#"{"__type":"ThrottlingException"}"#),
            StatusReason::Throttled
        );
        assert_eq!(status_reason_from_body("{}"), StatusReason::None);
    }

    /// 刷新失败的原因必须能和"过期了刷一下就好"区分开。
    ///
    /// 今天线上就栽在这:上游对整批账号回 `access_denied`,而失败原因既没落日志也没进状态,
    /// 面板上只看到"账号全过期了" —— 运维无从判断是账号被上游处置了、还是中转自己坏了,
    /// 只能手工拿凭据去 curl 上游才问得出来。
    #[test]
    fn refresh_denial_is_distinguishable_from_a_merely_expired_token() {
        // 上游明确拒绝续期:刷多少次都没用,必须单列。
        assert_eq!(
            refresh_reason_from(
                400,
                r#"{"error":"access_denied","error_description":"Access denied"}"#
            ),
            StatusReason::RefreshDenied
        );
        // 授权已作废,处置动作与上者一致(换号),归同一档。
        assert_eq!(
            refresh_reason_from(400, r#"{"error":"invalid_grant"}"#),
            StatusReason::RefreshDenied
        );
        // 令牌过期是可自愈的,绝不能和上面混为一档。
        assert_eq!(
            refresh_reason_from(400, r#"{"error":"expired_token"}"#),
            StatusReason::TokenExpired
        );
        assert_eq!(
            refresh_reason_from(429, "slow down"),
            StatusReason::Throttled
        );
        assert_eq!(refresh_reason_from(500, "internal"), StatusReason::None);
        assert_eq!(StatusReason::RefreshDenied.as_str(), "refresh_denied");
    }

    /// 原因进得去、出得来,而且**一次成功就清空** —— 否则恢复了的账号会一直挂在"封禁"档。
    #[test]
    fn status_reason_is_exposed_and_cleared_on_success() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        assert_eq!(p.stats(0)[0].status_reason, "none");

        p.report_failure_with_reason("a", FailureKind::AuthAmbiguous, StatusReason::Banned, 0);
        assert_eq!(p.stats(0)[0].status_reason, "banned");

        // 分类只影响展示:AuthAmbiguous 单次不禁用,账号仍可选。
        assert!(!p.stats(0)[0].disabled);

        p.report_success("a");
        assert_eq!(
            p.stats(0)[0].status_reason,
            "none",
            "成功一次就说明那个原因过去了,标签必须跟着清"
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
            assert_eq!(p.select_with_exclude(0, &ex, None).unwrap().id, "b");
        }
        // 两个都排除 → None(即便都健康)。
        ex.insert("b".to_string());
        assert!(p.select_with_exclude(0, &ex, None).is_none());
        // 空排除集 → 行为等同 select(可选到任一健康账号)。
        let none: HashSet<String> = HashSet::new();
        assert!(p.select_with_exclude(0, &none, None).is_some());
    }

    /// 绑定必须在**选号层**生效,而不是"值传到了就算数":上一轮的修复只把白名单塞进请求
    /// 扩展、没人读,回归测试却只断言扩展在场,于是绑定形同虚设。这里直接断言选出来的账号。
    #[test]
    fn binding_restricts_selection_to_whitelisted_accounts() {
        use std::collections::HashSet;
        let none: HashSet<String> = HashSet::new();
        let mut p = Pool::new(
            vec![cred("3", 1), cred("7", 1), cred("9", 1)],
            LbMode::Priority,
        );
        // 只绑 7 号:轮询转多少圈都只能选到 7。
        let only7 = BoundCredentialIds(vec![7]);
        for _ in 0..10 {
            assert_eq!(
                p.select_with_exclude(0, &none, Some(&only7)).unwrap().id,
                "7"
            );
        }
        // 绑定 + 排除集叠加:绑 3/7 而 7 已试过 → 只剩 3。
        let three_seven = BoundCredentialIds(vec![3, 7]);
        let mut ex = HashSet::new();
        ex.insert("7".to_string());
        for _ in 0..10 {
            assert_eq!(
                p.select_with_exclude(0, &ex, Some(&three_seven))
                    .unwrap()
                    .id,
                "3"
            );
        }
        // 白名单指向池里不存在的账号 → 选不出(fail-closed,绝不回落到未授权账号)。
        let ghost = BoundCredentialIds(vec![4242]);
        assert!(p.select_with_exclude(0, &none, Some(&ghost)).is_none());
        // 白名单里唯一的账号进冷却 → 同样选不出,不会漏给别的健康账号。
        p.report_failure("7", FailureKind::Quota, 100);
        assert!(p.select_with_exclude(200, &none, Some(&only7)).is_none());
        assert!(p.select_with_exclude(200, &none, None).is_some()); // 不受限时仍有健康账号
    }

    /// 非数值 id 在绑定在场时一律不放行(fail-closed);不受限时照选。
    #[test]
    fn binding_rejects_non_numeric_credential_ids() {
        use std::collections::HashSet;
        let none: HashSet<String> = HashSet::new();
        let mut p = Pool::new(vec![cred("uuid-abc", 1)], LbMode::Priority);
        assert!(
            p.select_with_exclude(0, &none, Some(&BoundCredentialIds(vec![1])))
                .is_none()
        );
        assert!(p.select_with_exclude(0, &none, None).is_some());
    }

    #[test]
    fn select_with_exclude_still_skips_disabled_and_cooldown() {
        use std::collections::HashSet;
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        // b 进入配额冷却;排除集为空但 b 仍不可选 → 只剩 a。
        p.report_failure("b", FailureKind::Quota, 100);
        let none: HashSet<String> = HashSet::new();
        for _ in 0..5 {
            assert_eq!(p.select_with_exclude(200, &none, None).unwrap().id, "a");
        }
        // 再把 a 也排除 → 无可选(b 冷却中 + a 被排除)。
        let mut ex = HashSet::new();
        ex.insert("a".to_string());
        assert!(p.select_with_exclude(200, &ex, None).is_none());
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
    fn classify_refresh_failure_uses_token_endpoint_semantics() {
        // OAuth/OIDC token 端点用 400 + invalid_grant 表达"授权已作废" → AuthInvalid(永久禁用)。
        assert_eq!(
            classify_refresh_failure(400, r#"{"error":"invalid_grant"}"#),
            FailureKind::AuthInvalid
        );
        assert_eq!(
            classify_refresh_failure(400, r#"{"__type":"InvalidGrantException"}"#),
            FailureKind::AuthInvalid
        );
        // 5xx / 网关错误 → Transient(记 strike,不禁用)。
        assert_eq!(
            classify_refresh_failure(503, "upstream unavailable"),
            FailureKind::Transient
        );
        assert_eq!(classify_refresh_failure(0, ""), FailureKind::Transient);
        // 可自愈信号优先短路 → 冷却,绝不永久禁用。
        assert_eq!(
            classify_refresh_failure(400, r#"{"__type":"ThrottlingException"}"#),
            FailureKind::AuthAmbiguous
        );
        // 401/403 但无确证(上游只说 "Bad credentials")→ 冷却,不禁用。
        assert_eq!(
            classify_refresh_failure(401, r#"{"message":"Bad credentials"}"#),
            FailureKind::AuthAmbiguous
        );
        assert_eq!(classify_refresh_failure(429, ""), FailureKind::Quota);
    }

    /// 永久禁用只允许发生在上游"真的在报授权作废"的状态码(400/401/403)上;其余状态码
    /// 哪怕体里恰好带 invalid 字样(网关 HTML 错误页/代理文案),也只能冷却。
    #[test]
    fn classify_refresh_failure_confines_permanent_disable_to_auth_statuses() {
        // (a) 5xx + 看着像 invalid_grant 的体 → 冷却,**不**永久禁用。
        for status in [500u16, 502, 503, 504] {
            assert_eq!(
                classify_refresh_failure(status, r#"{"error":"invalid_grant"}"#),
                FailureKind::AuthAmbiguous,
                "status {status} 不得因体里带 invalid_grant 就永久禁用"
            );
        }
        // 网关/代理返回的 HTML 错误页里出现 "...is invalid." 之类措辞,同样只冷却。
        assert_eq!(
            classify_refresh_failure(
                502,
                "<html><body>Bad Gateway: the upstream response is invalid.</body></html>"
            ),
            FailureKind::AuthAmbiguous
        );
        assert_eq!(
            classify_refresh_failure(404, r#"{"__type":"InvalidGrantException"}"#),
            FailureKind::AuthAmbiguous
        );
        // 传输层无 HTTP 应答(status=0,ensure_fresh 用 0 传短标签)→ 也不得禁用。
        assert_eq!(
            classify_refresh_failure(0, "connect error: invalid_token"),
            FailureKind::AuthAmbiguous
        );

        // (b) 真正的 invalid_grant 状态码(token 端点 400,以及 401/403)→ 仍然永久禁用。
        for status in [400u16, 401, 403] {
            assert_eq!(
                classify_refresh_failure(status, r#"{"error":"invalid_grant"}"#),
                FailureKind::AuthInvalid,
                "status {status} + invalid_grant 必须仍判永久失效"
            );
        }
        assert_eq!(
            classify_refresh_failure(401, r#"{"__type":"UnauthorizedException"}"#),
            FailureKind::AuthInvalid
        );

        // (c) 瞬时类判定不受影响:无失效标记的 5xx / 无体 / 传输层错误仍是 Transient。
        assert_eq!(
            classify_refresh_failure(500, "internal error"),
            FailureKind::Transient
        );
        assert_eq!(classify_refresh_failure(502, ""), FailureKind::Transient);
        assert_eq!(
            classify_refresh_failure(0, "connection timed out"),
            FailureKind::Transient
        );
        // 可自愈信号仍最先短路(即便状态码是 400/401/403)。
        assert_eq!(
            classify_refresh_failure(400, r#"{"__type":"ExpiredTokenException"}"#),
            FailureKind::AuthAmbiguous
        );
        assert_eq!(
            classify_refresh_failure(503, r#"{"__type":"ThrottlingException"}"#),
            FailureKind::AuthAmbiguous
        );
    }

    #[test]
    fn disabled_state_is_mirrored_into_persisted_snapshot() {
        // 手工停用与 AuthInvalid 永久禁用都必须同时进入待落盘的凭据,否则重启/下次落盘即丢失。
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        assert!(p.set_disabled("a", true));
        let snap = p.snapshot_credentials();
        assert!(snap.iter().find(|c| c.id == "a").unwrap().disabled);
        assert!(!snap.iter().find(|c| c.id == "b").unwrap().disabled);
        // 复活也要落到凭据上。
        assert!(p.set_disabled("a", false));
        assert!(!p.snapshot_credentials()[0].disabled);
        // AuthInvalid 触发的永久禁用同样同步。
        p.report_failure("b", FailureKind::AuthInvalid, 100);
        let snap = p.snapshot_credentials();
        assert!(snap.iter().find(|c| c.id == "b").unwrap().disabled);
        // 用快照重建池(模拟落盘 → 重启加载):禁用态仍在。
        let p2 = Pool::new(snap, LbMode::Priority);
        assert_eq!(p2.active_count(1_000_000), 1);
    }

    #[test]
    fn unconfirmed_refresh_downgrades_auth_invalid_to_cooldown() {
        // 换新令牌因传输层错误未成行 → 紧随其后的 AuthInvalid 缺乏确证,降级为冷却而非永久禁用。
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        assert!(p.note_refresh_transport_failure("a"));
        p.report_failure("a", FailureKind::AuthInvalid, 100);
        let st = p.stats(100);
        assert!(!st[0].disabled, "传输层刷新失败后不得永久禁用");
        assert!(p.select(100).is_some(), "首次降级只记 1 strike,账号仍可用");
        // 标记是一次性的:再来一次 AuthInvalid(此时已无未成行的刷新)照常永久禁用。
        p.report_failure("a", FailureKind::AuthInvalid, 100);
        assert!(p.stats(100)[0].disabled);
        // 未知 id → false。
        assert!(!p.note_refresh_transport_failure("nope"));
    }

    #[test]
    fn successful_refresh_writeback_clears_unconfirmed_refresh() {
        // 换新令牌成功写回 → 标记清除,后续真失效判定不再被误降级。
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        p.note_refresh_transport_failure("a");
        assert!(p.update_credential_tokens("a", "new-at".into(), "rt".into(), 9999));
        p.report_failure("a", FailureKind::AuthInvalid, 100);
        assert!(p.stats(100)[0].disabled, "标记已清除,应照常永久禁用");
    }

    #[test]
    fn next_id_is_monotonic_and_never_reuses_removed_numbers() {
        let mut p = Pool::new(vec![cred("1", 1), cred("2", 1)], LbMode::Priority);
        let (id3, _) = p.add_credential(cred("ignored", 1));
        assert_eq!(id3, "3");
        // 删掉末号账号后再新增:**不得**复用 3(旧的 max+1 会复用)。
        assert!(p.remove_credential("3"));
        let (id4, _) = p.add_credential(cred("ignored", 1));
        assert_eq!(id4, "4", "已删除的编号不得被复用");
        // 全删空后仍继续递增。
        assert!(p.remove_credential("1"));
        assert!(p.remove_credential("2"));
        assert!(p.remove_credential("4"));
        let (id5, _) = p.add_credential(cred("ignored", 1));
        assert_eq!(id5, "5");
        assert_eq!(p.next_id_hint(), 6);
    }

    #[test]
    fn with_next_id_takes_persisted_high_water_mark() {
        // 持久化的高水位大于现有 id → 以高水位为准(重启后不复用已删除编号)。
        let mut p = Pool::with_next_id(vec![cred("2", 1)], LbMode::Priority, 9);
        assert_eq!(p.next_id_hint(), 9);
        assert_eq!(p.add_credential(cred("x", 1)).0, "9");
        // 无高水位记录(旧文件)→ 平滑退化为 max(现有 id)+1。
        let p2 = Pool::with_next_id(vec![cred("7", 1)], LbMode::Priority, 0);
        assert_eq!(p2.next_id_hint(), 8);
        // 高水位落后于现有 id(手工编辑过 credentials.json)→ 取现有 id 最大值 +1。
        let p3 = Pool::with_next_id(vec![cred("20", 1)], LbMode::Priority, 3);
        assert_eq!(p3.next_id_hint(), 21);
    }

    #[test]
    fn update_credential_tokens_checked_rejects_identity_mismatch() {
        let mut p = Pool::new(vec![cred("1", 1)], LbMode::Priority);
        // 身份一致 → 正常写回。
        assert!(p.update_credential_tokens_checked(
            "1",
            "rt",
            "new-at".into(),
            "new-rt".into(),
            9999
        ));
        assert_eq!(p.snapshot_credentials()[0].access_token, "new-at");
        // 该 id 上的账号已换人(refresh_token 不再是发起刷新时那个)→ 拒绝写回、不改动。
        assert!(!p.update_credential_tokens_checked(
            "1",
            "rt",
            "stale-at".into(),
            "stale-rt".into(),
            1
        ));
        let snap = p.snapshot_credentials();
        assert_eq!(snap[0].access_token, "new-at");
        assert_eq!(snap[0].refresh_token, "new-rt");
        assert_eq!(snap[0].expires_at_unix, 9999);
        // 未知 id → false。
        assert!(!p.update_credential_tokens_checked("nope", "rt", "x".into(), "y".into(), 1));
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

    /// 封禁不是计时器能解除的:上游原话是「账号已锁定,请联系客服验证身份」。
    /// 此前 `is_active` 只看 disabled+冷却,冷却一过封禁号就回到可用池——面板挂着
    /// 「封禁」标签、`available` 却把它算在内,两个数字互相矛盾。
    #[test]
    fn banned_account_is_neither_active_nor_counted_nor_selected() {
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        assert_eq!(p.active_count(0), 2);

        p.report_failure_with_reason("a", FailureKind::AuthAmbiguous, StatusReason::Banned, 0);

        // 时间推到任何冷却都早已过期之后
        let long_after = 10_000_000;
        assert_eq!(
            p.active_count(long_after),
            1,
            "封禁号不得计入可用数,哪怕冷却已过"
        );
        for _ in 0..10 {
            let picked = p.select(long_after).expect("另一个健康号仍可选");
            assert_eq!(picked.id, "b", "封禁号不得被选中");
        }
    }

    /// 封禁号被挡在池外后永远轮不到一次成功来清标签,「重置」是它唯一的出口。
    /// 只清计时器不清结论的话,这个按钮对封禁号就是空操作。
    #[test]
    fn reset_clears_the_banned_verdict_so_the_account_can_come_back() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        p.report_failure_with_reason("a", FailureKind::AuthAmbiguous, StatusReason::Banned, 0);
        assert_eq!(p.active_count(10_000_000), 0);

        assert!(p.reset_failures("a"));

        assert_eq!(p.stats(0)[0].status_reason, "none", "重置须清掉封禁结论");
        assert_eq!(p.active_count(0), 1, "重置后账号回到可用池");
        assert!(p.select(0).is_some());
    }

    /// 封禁结论必须跨重启活下来。只活在内存里的话,每次重启/发版都会把它抹掉,
    /// 账号悄悄回到可用池,直到再失败一次才重新被挡——「253 个账号 1 个封禁、
    /// 可用数却是 253」这件事就会在每次重启后重现。
    #[test]
    fn banned_verdict_survives_a_restart() {
        let mut p = Pool::new(vec![cred("a", 1), cred("b", 1)], LbMode::Priority);
        p.report_failure_with_reason("a", FailureKind::AuthAmbiguous, StatusReason::Banned, 0);

        // 模拟落盘 → 重启 → 从盘上重新加载
        let persisted = p.snapshot_credentials();
        let reloaded = Pool::new(persisted, LbMode::Priority);

        assert_eq!(
            reloaded.stats(0)[0].status_reason,
            "banned",
            "结论没落盘,重启后封禁账号会悄悄回池"
        );
        assert_eq!(
            reloaded.active_count(10_000_000),
            1,
            "重启后封禁号仍不计入可用"
        );
    }

    /// 反过来:重置清掉的结论也要落盘,否则重启后封禁标签又冒出来。
    #[test]
    fn reset_is_also_persisted() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        p.report_failure_with_reason("a", FailureKind::AuthAmbiguous, StatusReason::Banned, 0);
        p.reset_failures("a");

        let reloaded = Pool::new(p.snapshot_credentials(), LbMode::Priority);
        assert_eq!(reloaded.stats(0)[0].status_reason, "none");
        assert_eq!(reloaded.active_count(0), 1);
    }

    /// 上游对超长请求回 `400 CONTENT_LENGTH_EXCEEDS_THRESHOLD`。这是**请求本身**的问题,
    /// 换任何账号都同样失败,故必须判成 `InvalidRequest`(不重试/不冷却/不累计 strike)。
    ///
    /// 此前只认 `INVALID_MODEL_ID`,这个码落进了 `Transient`,于是一个超长请求被跨账号重试
    /// 一遍、**每换一个账号就记一次失败**。线上实测:一个下午把 253 个健康账号打成 149 个
    /// 带伤、26 个进冷却,可用池缩水后开始回 503「no available upstream account」。
    #[test]
    fn oversized_request_is_a_request_error_not_an_account_failure() {
        let body =
            r#"{"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#;
        assert_eq!(classify_with_body(400, body), FailureKind::InvalidRequest);
        assert!(body_signals_invalid_request(body));
    }

    /// `InvalidRequest` 落到池里必须什么都不做:不禁用、不冷却、不攒 strike。
    /// 这是"一个坏请求不会打伤整池账号"的最后一道保障。
    #[test]
    fn invalid_request_leaves_the_account_untouched() {
        let mut p = Pool::new(vec![cred("a", 1)], LbMode::Priority);
        for _ in 0..10 {
            p.report_failure_with_reason("a", FailureKind::InvalidRequest, StatusReason::None, 0);
        }
        let st = &p.stats(0)[0];
        assert_eq!(st.strikes, 0, "确定性请求错误不得累计 strike");
        assert!(!st.disabled, "不得禁用");
        assert!(!st.in_cooldown, "不得进冷却");
        assert_eq!(p.active_count(0), 1, "账号必须仍然可用");
    }

    /// 两种 `InvalidRequest` 的文案必须分开:笼统套一句"模型不可用"会把"上下文超长"
    /// 讲成完全无关的另一回事,使用者照着去换模型,换多少次都没用。
    #[test]
    fn message_names_the_actual_reason() {
        let too_long =
            r#"{"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#;
        let m = invalid_request_message(too_long, "claude-sonnet-4.5");
        assert!(m.contains("too long"), "须点明是长度问题: {m}");
        assert!(!m.contains("Invalid model"), "不得误报成模型不可用: {m}");

        let bad_model = r#"{"reason":"INVALID_MODEL_ID"}"#;
        let m2 = invalid_request_message(bad_model, "claude-opus-4.6");
        assert!(m2.contains("claude-opus-4.6"), "须点名是哪个模型: {m2}");
        assert!(!m2.contains("too long"));
    }

    /// `TOOL_CONFIG_MISSING` 是兜底档:正常情况下中枢不该造出这种请求(转换层会从历史补齐
    /// 工具规格),但万一仍然发生,必须当场以 400 告知,而**不是**当成瞬时错误跨账号重试
    /// 一遍——线上实测那样会在半小时里波及 25 个账号。
    #[test]
    fn missing_tool_config_is_a_request_error_not_an_account_failure() {
        let body = r#"{"message":"Bedrock error message: The toolConfig field must be defined when using toolUse and toolResult content blocks.","reason":"TOOL_CONFIG_MISSING"}"#;
        assert_eq!(classify_with_body(400, body), FailureKind::InvalidRequest);
        assert!(body_signals_invalid_request(body));

        let m = invalid_request_message(body, "claude-sonnet-4.5");
        assert!(
            m.contains("Tool configuration missing"),
            "须点明是工具定义缺失: {m}"
        );
        assert!(!m.contains("too long"), "不得串成上下文超长: {m}");
        assert!(!m.contains("Invalid model"), "不得串成模型不可用: {m}");
    }

    /// 声明了 `authMethod=api_key` 却没给 `kiroApiKey` 的凭据,**入池即禁用**。
    ///
    /// 它取不到 bearer,又因为 `is_api_key()` 取并集而被判定为 API Key 凭据(故不刷新),
    /// 留在池里只会在跨账号重试里反复空转 —— 每次选中都在同一处失败。
    /// (照观测补齐。)
    #[test]
    fn a_credential_claiming_api_key_without_one_is_disabled_on_load() {
        let mut c = cred("bad", 1);
        c.auth = crate::kiro::credential::AuthMethod::ApiKey;
        c.kiro_api_key = None;
        let p = Pool::new(vec![c], LbMode::Priority);
        assert_eq!(p.active_count(0), 0, "配置自相矛盾的凭据不得进入可用池");
        assert!(p.stats(0)[0].disabled);
    }

    /// 「重置」不得救活配置无效的凭据:重置只清 strike/冷却/结论,改不了"声明了
    /// api_key 却没有 key"这件事,复活后立刻重新走回同一条错误路径。
    #[test]
    fn reset_refuses_to_revive_an_invalid_api_key_config() {
        let mut c = cred("bad", 1);
        c.auth = crate::kiro::credential::AuthMethod::ApiKey;
        c.kiro_api_key = None;
        let mut p = Pool::new(vec![c], LbMode::Priority);

        assert!(!p.reset_failures("bad"), "重置须拒绝配置无效的凭据");
        assert_eq!(p.active_count(0), 0, "拒绝之后它仍不得回到可用池");
    }

    /// 对照:配置完整的 API Key 凭据照常入池可用。
    #[test]
    fn a_well_formed_api_key_credential_is_usable() {
        let mut c = cred("good", 1);
        c.auth = crate::kiro::credential::AuthMethod::ApiKey;
        c.kiro_api_key = Some("ksk_abc".into());
        let p = Pool::new(vec![c], LbMode::Priority);
        assert_eq!(p.active_count(0), 1);
    }
}
