//! `POST /v1/messages` 处理器:Anthropic 请求 → Kiro 数据面 → Anthropic 响应(非流式文本 MVP)。
//!
//! 流程:解析 [`MessagesRequest`] → [`anthropic_to_kiro`] → 从池选账号 → 即将过期则内存刷新
//! (不写盘)→ 打 Kiro(生产走端点回退 `call_with_fallback`;测试/代理可用 `endpoint_override`
//! 指定单一 base)→ 读 body → 事件流解码 → [`kiro_events_to_anthropic`] → `Json` 返回。
//!
//! 时钟以 `now_unix` 注入 [`relay_core`],axum handler 计算真实 now 再委托,保持全仓一致的注入纪律。

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use tokio::sync::Mutex;

use crate::apikey::ApiKeyStore;
use crate::config::Config;
use crate::kiro::convert::{
    ConvertError, StreamException, Truncation, anthropic_to_kiro, exception_status,
    extract_exception, kiro_events_to_anthropic,
};
use crate::kiro::endpoint::Endpoint;
use crate::kiro::eventstream::decoder::StreamDecoder;
use crate::kiro::login::LoginError;
use crate::kiro::machine_id;
use crate::kiro::pool::FailureKind;
use crate::kiro::pool::{Pool, classify_with_body};
use crate::kiro::provider::{self, Impersonation};
use crate::protocol::anthropic::stream;
use crate::protocol::anthropic::types::{
    AnthropicModel, AnthropicModelList, CountTokensResponse, MessagesRequest, MessagesResponse,
};
use crate::server::auth::{ApiKeyId, BoundCredentialIds};
use crate::stats::StatsManager;

/// 选账号后,离过期不足此秒数即先内存刷新(仅内存,不写盘)。
const REFRESH_MARGIN_SECS: u64 = 300;

/// 从请求头或连接对端地址提取客户端真实 IP(CDN 无关)。
///
/// 优先级:`CF-Connecting-IP` / `True-Client-IP`(Cloudflare/Akamai 边缘写入,不可伪造)
/// → `X-Forwarded-For` 首跳(通用反代 / EdgeOne)→ `X-Real-IP`(nginx 等)
/// → socket 对端地址(`peer.ip()`,直连公网时即真实客户端)。
/// 经 Docker `-p` 端口映射且无反代时,本机回环请求的对端会被改写为 docker 网关(172.x),
/// 属预期(改用 host 网络或经 CDN/反代注入上述头即可拿到真实客户端 IP)。IP 非机密,可安全落库/展示。
/// 对端是否可信到能采信其转发头:回环 / 私网(RFC1918、CGNAT、link-local、唯一本地 IPv6)。
/// 无对端信息(测试或非 TCP 场景)按可信处理,保持既有单测与内部调用的语义不变。
fn peer_is_trusted(peer: Option<std::net::SocketAddr>) -> bool {
    use std::net::IpAddr;
    let Some(p) = peer else { return true };
    match p.ip() {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                // 100.64.0.0/10:运营商级 NAT,容器/编排网络也常用
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                // fc00::/7 唯一本地地址 + fe80::/10 链路本地
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // ::ffff:a.b.c.d 形式的 IPv4 映射地址按其 v4 语义判定
                || v6.to_ipv4_mapped().is_some_and(|v4| {
                    v4.is_loopback() || v4.is_private() || v4.is_link_local()
                })
        }
    }
}

/// 把一段文本收敛成**合法 IP 字面量**;解析不出就丢弃。
///
/// 转发头的值完全由外部写入,之前只做了 `trim()` 就原样落库,于是任何字符串(超长文本、
/// 控制字符、伪造成 SQL/日志格式的片段)都能进用量记录、失败日志和管理面展示。审计字段的
/// 第一要求是"可信且可比对",解析不出 IP 的值没有任何审计价值,宁可留空。
///
/// 顺带把 `[::1]:443` / `1.2.3.4:5678` 这类带端口的形态剥成纯 IP:部分反代会连端口一起写。
fn sanitize_ip(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(ip) = s.parse::<std::net::IpAddr>() {
        return Some(ip.to_string());
    }
    if let Ok(sock) = s.parse::<std::net::SocketAddr>() {
        return Some(sock.ip().to_string());
    }
    // `[::1]` 这种只加了方括号、没带端口的写法。
    let unbracketed = s.strip_prefix('[').and_then(|t| t.strip_suffix(']'))?;
    unbracketed
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

pub(crate) fn extract_client_ip(
    headers: &axum::http::HeaderMap,
    peer: Option<std::net::SocketAddr>,
    trusted_proxy_hops: u8,
) -> Option<String> {
    // 转发头一律**只在对端是私网/回环时**才采信:这些头是普通请求头,任何能直连本服务的
    // 客户端都能自己写一个。反代/CDN 回源必然来自私网或回环,故先按对端网段判别。
    //
    // ⚠️ 局限(照实说明):经 Docker `-p` 端口映射时,**直连公网端口的请求**在容器内看到的
    // 对端同样是 docker 网关(172.x,私网),与经反代回源无法区分。要真正堵死伪造,需把
    // 容器端口收到 `127.0.0.1:<port>` 只让反代进,或改用 host 网络。
    if !peer_is_trusted(peer) {
        return peer.map(|p| p.ip().to_string());
    }
    // hops = 0 表示前面根本没有反代:任何转发头都只可能是调用方自己写的,一律不采信。
    if trusted_proxy_hops == 0 {
        return peer.map(|addr| addr.ip().to_string());
    }
    // **从右往左数第 hops 项**,而不是最左那项。
    //
    // 这是本函数唯一真正防伪造的地方:XFF 的最左项是"最原始客户端"没错,但它同样是**调用方
    // 自己就能写死的那一项** —— 客户端发 `X-Forwarded-For: 1.2.3.4`,反代只会在其右侧追加
    // 自己看到的对端,于是取最左恒等于采信伪造值。每一跳可信反代追加的是它**亲眼看到的**
    // 地址,所以倒数第 hops 项 = 最外层可信反代观测到的调用方,伪造不了。
    //
    // 例:客户端伪造 "1.2.3.4" → 到达本服务时 XFF = "1.2.3.4, <真实IP>";hops=1 取倒数第 1
    // 项 = 真实 IP。CDN → 自己的反代 → 本服务则设 hops=2。
    if let Some(val) = headers.get("x-forwarded-for")
        && let Ok(s) = val.to_str()
    {
        let hops: Vec<&str> = s.split(',').collect();
        if let Some(entry) = hops.len().checked_sub(trusted_proxy_hops as usize)
            && let Some(ip) = hops.get(entry).and_then(|h| sanitize_ip(h))
        {
            return Some(ip);
        }
    }
    // CDN 边缘写入的权威客户端头。放在 XFF **之后**:Caddy/nginx 这类反代会把客户端自带的
    // 同名头原样透传(它们只管 X-Forwarded-*),所以在有 XFF 可用时,反代亲眼观测到的那一项
    // 比这些"声称由边缘写入"的头更可信;只有在压根没有 XFF 时才回退到它们。
    for h in ["cf-connecting-ip", "true-client-ip"] {
        if let Some(ip) = headers
            .get(h)
            .and_then(|v| v.to_str().ok())
            .and_then(sanitize_ip)
        {
            return Some(ip);
        }
    }
    // X-Real-IP(nginx / 部分反代注入的直连客户端)。
    if let Some(ip) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(sanitize_ip)
    {
        return Some(ip);
    }
    // 无任何反代头 → socket 对端(直连公网时即真实客户端 IP)。
    peer.map(|addr| addr.ip().to_string())
}

/// `/v1/messages` 处理器共享状态。
///
/// 池以 `Arc<tokio::sync::Mutex<Pool>>` 持有(非 parking_lot):handler 在 `call_with_fallback`
/// 内的网络 `.await` 期间持有 `&mut Pool`,若用同步锁跨 `.await` 持锁是 bug。用异步锁串行化池访问
/// 是本 MVP 的可接受取舍(单请求整条链路串行占锁,后续可细化为更短临界区)。
#[derive(Clone)]
pub struct MessagesState {
    pub pool: Arc<Mutex<Pool>>,
    pub client: reqwest::Client,
    /// 控制面(一问一答)出站客户端:令牌刷新 / 余额(getUsageLimits)/ 模型清单等短小请求走此
    /// 客户端。由 `crate::http::unary()` 构造,带 connect_timeout + 整请求 timeout(硬顶),使任何
    /// 环节卡死都在上限内失败。数据面(relay)不消费此字段——它需容忍长流,不能加整请求超时。
    pub control_client: reqwest::Client,
    pub cfg: Arc<Config>,
    /// 运行期可变配置(auth key / RPM / 负载均衡模式)。admin 设置端点写入并落盘,
    /// auth 闸读当前 key 使轮换即时生效。与 `cfg`(不可变启动值)分离,改动面最小。
    pub runtime_cfg: crate::config::SharedRuntimeConfig,
    /// 数据面基址覆盖(合法用途:自建代理/网关或测试 mock)。
    /// `Some(url)` → 直接 POST 到该 URL 并自行反馈池;`None` → 走生产端点回退。
    pub endpoint_override: Option<String>,
    /// 统计持久化层(Phase 1)。relay 在数据面记录用量/失败/限流;admin 只读查询。
    /// 记录经异步/批量存储,不在热路径做 I/O、不跨池锁调用。
    pub stats: Arc<StatsManager>,
    /// API-KEY 存储(P2)。auth 闸校验 store key、relay 归属用量、admin/user handler
    /// 增删改查与用量查询共用同一 `Arc<ApiKeyStore>`。本阶段仅接线到 state,
    /// 消费方(auth/relay 归属/admin/user)由后续阶段接入。
    pub api_keys: Arc<ApiKeyStore>,
    /// 余额缓存(Phase 4)。admin `GET /api/admin/credentials/{id}/balance` 查询上游剩余额度
    /// (getUsageLimits)时读缓存(5 分钟 TTL);miss/过期则实拉上游并回填。relay 热路径不消费。
    pub balance: Arc<crate::balance::BalanceCache>,
    /// 动态模型清单缓存(FIX 1)。admin `GET /api/admin/models` 汇总各账号上游
    /// `ListAvailableModels` 的并集(TTL 内命中缓存);空时回落静态 17 模型目录。
    /// 刷新端点按需实拉并回填。relay 热路径不消费。
    pub models_cache: Arc<crate::models_cache::ModelsCache>,
    /// Builder-ID 设备码登录会话中转态(Phase 3)。`/login/builderid/start` 放入待批准态并回
    /// sessionId,`/login/builderid/poll` 反复非消费读取直到终态。~600s TTL、注入时钟。
    /// admin 独占消费;relay 热路径不接触。
    pub builderid_sessions:
        crate::admin::login_session::LoginSessions<crate::admin::login_session::BuilderIdSession>,
    /// IAM SSO 授权码登录会话中转态(Phase 3)。`/login/iam-sso/start` 放入 AuthStart(含 PKCE
    /// verifier + state)并回 sessionId,`/login/iam-sso/complete` 消费取出换 token。~600s TTL。
    pub iam_sso_sessions:
        crate::admin::login_session::LoginSessions<crate::admin::login_session::IamSsoSession>,
    /// 实时日志捕获器(Phase 6)。`Some` 时 admin 日志端点(stream/snapshot/download)
    /// 从中读取历史环形缓冲并订阅广播;`None`(log_capacity=0 或未接线)时端点返回 503。
    /// relay 热路径不消费;捕获经全局 tracing 层完成,与业务解耦。
    pub log_capture: Option<Arc<crate::logcap::LogCapture>>,
    /// 令牌刷新上下文(单飞协调器 + credentials.json 路径 + 落盘锁)。
    /// relay/balance/models 三方经 [`crate::kiro::ensure_fresh`] 刷新时共用同一份:
    /// per-credential 单飞锁避免并发刷新同一凭据的级联 401(Bug A);刷新成功后原子落盘
    /// credentials.json,防止重启加载已轮换作废的旧 refresh_token(Bug B)。
    pub refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx,
}

/// 中转失败原因,携带对外可见的 HTTP 语义。
#[derive(Debug)]
pub enum RelayError {
    /// 请求无法转换为 Kiro 请求(含未映射模型)→ 400。
    Convert(ConvertError),
    /// 池中无可用账号 → 503。
    NoAccount,
    /// 刷新或上游调用失败 → 502。
    Upstream(String),
    /// 确定性请求错误:上游明确拒绝该请求(如 `INVALID_MODEL_ID` —— 所请求的模型对当前
    /// 账号档位不可用)→ 400,携带对客户端可见的说明(换账号重试无用,故不重试)。
    /// 上游**瞬态**失败(网络抖动、5xx、限流):换账号前值得退避一下,免得把上游的
    /// 抖动放大成尖峰。与 [`Upstream`](Self::Upstream) 对外表现一致(同为 502),
    /// 只在重试策略上区分开。
    UpstreamTransient(String),
    /// 确定性请求错误(换账号重试无用):模型对当前档位不可用、或请求体超上游长度上限。
    /// 曾名 `InvalidModel` —— 在它开始承载"上下文超长"之后,那个名字本身就是个假陈述。
    InvalidRequest(String),
}

impl RelayError {
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            RelayError::Convert(_) => StatusCode::BAD_REQUEST,
            RelayError::NoAccount => StatusCode::SERVICE_UNAVAILABLE,
            RelayError::Upstream(_) | RelayError::UpstreamTransient(_) => StatusCode::BAD_GATEWAY,
            RelayError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
        }
    }
    /// 对外错误类型串(不泄露内部细节/令牌)。
    fn err_type(&self) -> &'static str {
        match self {
            RelayError::Convert(_) => "invalid_request_error",
            RelayError::NoAccount => "overloaded_error",
            RelayError::Upstream(_) | RelayError::UpstreamTransient(_) => "api_error",
            RelayError::InvalidRequest(_) => "invalid_request_error",
        }
    }
    /// 对外错误消息(粗粒度,不含令牌/内部堆栈)。
    fn message(&self) -> String {
        match self {
            RelayError::Convert(e) => e.to_string(),
            RelayError::NoAccount => "no available upstream account".to_string(),
            RelayError::Upstream(_) | RelayError::UpstreamTransient(_) => {
                "upstream request failed".to_string()
            }
            // 清晰的不可用说明(确定性、可安全外露):把"该模型不可用、请换一个"透传给客户端。
            RelayError::InvalidRequest(msg) => msg.clone(),
        }
    }
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "type": "error",
            "error": { "type": self.err_type(), "message": self.message() },
        });
        (self.status(), Json(body)).into_response()
    }
}

/// HTTP 状态码 → Anthropic 错误类型串(照公开错误规范的对照表)。
/// 上游 exception 经 [`exception_status`] 只会落到 400/403/429/502 这几档,
/// 其余状态码一律按 `api_error` 处理。
fn anthropic_error_type(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        _ => "api_error",
    }
}

/// 组装标准 Anthropic 错误响应:`{"type":"error","error":{"type":…,"message":…}}`。
/// 状态码非法(理论上不会)时回落 502,避免 `from_u16` 直接 panic。
fn anthropic_error_response(status: u16, message: &str) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": anthropic_error_type(status), "message": message },
    });
    (code, Json(body)).into_response()
}

/// 流式 `error` 事件(照 Anthropic 公开 Messages streaming 规范:`event: error` +
/// 与非流式同形的错误体)。构造放在 handler 而非 `stream` 事件构造器里,
/// 因为错误类型要按上游 exception 映射出的状态码决定。
fn stream_error_event(status: u16, message: &str) -> stream::SseEvent {
    stream::SseEvent {
        event: "error",
        data: serde_json::json!({
            "type": "error",
            "error": { "type": anthropic_error_type(status), "message": message },
        })
        .to_string(),
    }
}

/// 把上游 exception 拼成对客户端可见的说明:有人类可读消息就 `<类型>: <消息>`,
/// 否则只给类型(消息为空是上游没带,不是内部细节被抹掉)。
fn exception_detail(e: &StreamException) -> String {
    if e.message.is_empty() {
        e.kind.clone()
    } else {
        format!("{}: {}", e.kind, e.message)
    }
}

/// 由选中的凭据构造伪装身份(machine_id 优先取显式、否则由 refresh_token 派生)。
fn impersonation_for(
    cfg: &Config,
    refresh_token: &str,
    explicit_machine_id: Option<&str>,
) -> Impersonation {
    Impersonation {
        machine_id: machine_id::resolve_with_config(
            explicit_machine_id,
            cfg.machine_id.as_deref(),
            refresh_token,
        ),
        kiro_version: cfg.kiro_version.clone(),
        agent_mode: "vibe".to_string(),
        system_version: cfg.system_version.clone(),
        node_version: cfg.node_version.clone(),
    }
}

/// 选-调成功的产物:上游响应 + 选中凭据的数值 id(供统计层记录用量,已在池锁外)。
pub(crate) struct CallOutcome {
    pub resp: reqwest::Response,
    /// 选中凭据的数值 id(`Credential.id` 解析为 u32;非数值/缺失回落 0)。
    pub credential_id: u32,
}

/// 把 `Credential.id`(内部 String,磁盘上多为整数)解析为统计层用的 u32;非数值回落 0。
fn credential_id_num(id: &str) -> u32 {
    id.parse::<u32>().unwrap_or(0)
}

/// 把一次分类失败落进统计层(池锁必须已释放)。热路径 fire-and-forget:
/// Auth(401/403)→ 失败日志;Quota(429)→ 限流日志;Transient 不落(非账号级凭据事件)。
/// `now_unix` 为 u64 秒,转成统计层的 i64;`request_type` 恒 "api"(Phase 1 无 MCP 区分)。
async fn record_classified_failure(
    stats: &StatsManager,
    credential_id: u32,
    kind: FailureKind,
    status: u16,
    response_body: &str,
    now_unix: u64,
) {
    // 生命周期日志(#7):上游/账号级失败,带分类与 HTTP 状态码(不含响应体/令牌)。
    // WARN 级别:比每请求 INFO 更醒目,便于在实时日志页快速定位账号问题。
    tracing::warn!(
        event = "upstream_failure",
        account_id = credential_id,
        kind = ?kind,
        status = status,
        "上游请求失败"
    );
    let now_i64 = now_unix as i64;
    match kind {
        // 两类鉴权失败(永久失效 / 歧义冷却)恒来自 401/403,均落失败日志。
        FailureKind::AuthInvalid | FailureKind::AuthAmbiguous => {
            // Auth 分类恒来自 401/403;status 为 0(无从解析)时回落 401 以满足模型语义。
            let code = if status == 401 || status == 403 {
                status
            } else {
                401
            };
            stats
                .record_failure(credential_id, "api", code, response_body, now_i64)
                .await;
        }
        FailureKind::Quota => {
            stats
                .record_throttle(credential_id, "api", response_body, now_i64)
                .await;
        }
        FailureKind::Transient => { /* 瞬时错误不落账号级事件日志 */ }
        // 确定性请求错误(INVALID_MODEL_ID 等):非账号故障,不落账号级失败日志。
        // (上层会在反馈池前短路,一般不会带此类走到这里;此 arm 仅为防御性穷尽。)
        FailureKind::InvalidRequest => {}
    }
}

/// 跨账号重试的上限(自适应,见 [`select_and_call_with_retry`])。
///
/// 采用 spec 的 `N = min(pool_size, 3)`(最多试 3 个不同账号即放弃,随池大小自然缩放),
/// 再叠加一个防御性硬顶,避免超大池上重试放飞。取值理由:3 是常见默认,足以覆盖"选中的
/// 账号偶发失败、换一个就好"的绝大多数场景,又不至于把一次请求拖成对全池的串行扫描。
const DEFAULT_MAX_CROSS_ACCOUNT_ATTEMPTS: usize = 3;
/// 跨账号重试的硬顶(防御性):即便池很大,单请求最多也只试这么多个账号。
const MAX_CROSS_ACCOUNT_ATTEMPTS_HARD_CAP: usize = 5;

/// 瞬态失败后的退避:指数增长 + 抖动,上限 2 秒。
///
/// 只用于**瞬态**错误(网络抖动、上游 5xx/限流)。账号级失败(令牌失效、额度耗尽)不等 ——
/// 那类失败换个账号立刻就能成,等待只是白白拖慢用户。此分工照观测:真实客户端在 408/429/5xx
/// 与发送失败上退避,在 401/403 换账号时不退避,而它打同一个上游长期稳定。
///
/// 抖动是为了避免上游抖动时多个并发请求同拍重试、把故障放大成尖峰。
fn transient_backoff(attempt: usize) -> std::time::Duration {
    const BASE_MS: u64 = 200;
    const MAX_MS: u64 = 2_000;
    let exp = BASE_MS.saturating_mul(2u64.saturating_pow(attempt.min(6) as u32));
    let backoff = exp.min(MAX_MS);
    let mut b = [0u8; 2];
    // 取不到随机数就不抖动:抖动是加分项,不该让请求失败。
    let jitter = if getrandom::getrandom(&mut b).is_ok() {
        (u16::from_le_bytes(b) as u64) % (backoff / 4).max(1)
    } else {
        0
    };
    std::time::Duration::from_millis(backoff.saturating_add(jitter))
}

/// 由池大小计算本请求的跨账号重试上限:`min(pool_size, 3)`,再叠加防御性硬顶。
/// 空池按 1 处理(仍走一次选择以产出 `NoAccount` 的既有语义)。
/// 有效上界取默认值与硬顶的较小者,`clamp` 的下界为 1(至少试一次)。
fn cross_account_attempts(pool_size: usize) -> usize {
    let ceiling = DEFAULT_MAX_CROSS_ACCOUNT_ATTEMPTS.min(MAX_CROSS_ACCOUNT_ATTEMPTS_HARD_CAP);
    pool_size.clamp(1, ceiling)
}

/// 跨账号重试包装:在账号级数据面失败时换下一个未试过的健康账号重试,直到成功或用尽
/// `min(pool_size, 3)` 次尝试;请求级致命错误(`Convert`/400)不重试、立即返回。
///
/// 与既有的每账号内重试正交、且在其外层组合:
/// - 每账号内:端点回退(`provider::call_with_fallback`)、403 强制刷新令牌重试一次、ensure_fresh——
///   这些都发生在 [`select_and_call_once`] 之内,本层只看其"最终成败"。
/// - 冷却/禁用跳过:失败账号照常经 `report_failure` 反馈池,下一次选择自动跳过它;
///   已试过的 id 还会显式排除,确保每次重试都换一个不同账号。
///
/// 关键:整个重试循环在**任何响应体被消费/流式转发之前**完成——`relay_core` 与 `relay_stream`
/// 都先拿到本函数返回的成功 `CallOutcome` 才开始读 body / 建 SSE,故失败账号的响应永不会开始下发。
///
/// 池锁纪律:选择/反馈池均为短临界区,绝不跨网络 `.await` 持锁(见 [`select_and_call_once`])。
pub(crate) async fn select_and_call_with_retry(
    state: &MessagesState,
    req: &MessagesRequest,
    now_unix: u64,
    bound: Option<&BoundCredentialIds>,
) -> Result<CallOutcome, RelayError> {
    let pool_size = { state.pool.lock().await.len() };
    let max_attempts = cross_account_attempts(pool_size);

    let mut tried_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut last_err: Option<RelayError> = None;

    for _attempt in 0..max_attempts {
        match select_and_call_once(state, req, now_unix, &tried_ids, bound).await {
            // 成功:直接返回(响应体尚未开始消费)。
            Ok((outcome, _tried_id)) => return Ok(outcome),
            // 请求级致命错误:任何账号上都会同样失败,不重试。
            // - Convert:请求无法转换(含未映射模型);
            // - InvalidModel:上游确定性拒绝(INVALID_MODEL_ID:该模型对当前档位不可用)。
            Err((e @ (RelayError::Convert(_) | RelayError::InvalidRequest(_)), _)) => {
                return Err(e);
            }
            // 池级:已无可选账号(可能是所有健康账号都被本请求试过了)。
            // 首个尝试就 NoAccount → 池确实空/全不可用,返回 NoAccount;
            // 非首个尝试 NoAccount → 说明前面的账号已试尽,返回上一次真实的账号级错误。
            Err((RelayError::NoAccount, _)) => {
                return Err(last_err.unwrap_or(RelayError::NoAccount));
            }
            // 账号级失败(Auth/Quota/Transient):记下已试账号,换下一个重试。
            Err((e @ (RelayError::Upstream(_) | RelayError::UpstreamTransient(_)), tried_id)) => {
                let transient = matches!(e, RelayError::UpstreamTransient(_));
                if let Some(id) = tried_id {
                    tried_ids.insert(id);
                }
                last_err = Some(e);
                // 生命周期日志(#7):账号级失败,即将跨账号换下一个健康账号重试。
                // 仅当还有下一轮才算真正"跨账号重试"。不含令牌/密钥/提示词。
                if tried_ids.len() < max_attempts {
                    tracing::info!(
                        event = "cross_account_retry",
                        model = %req.model,
                        tried_accounts = tried_ids.len(),
                        max_attempts = max_attempts,
                        "账号级失败,跨账号重试"
                    );
                }
                // 瞬态失败在换账号前退避(指数 + 抖动,上限 2s);账号级失败不等。
                if transient && tried_ids.len() < max_attempts {
                    tokio::time::sleep(transient_backoff(_attempt)).await;
                }
                // 还有下一轮就继续;这是最后一轮则落到循环外返回 last_err。
                continue;
            }
        }
    }

    Err(last_err.unwrap_or(RelayError::Upstream("all accounts exhausted".to_string())))
}

/// 选账号→(即将过期则内存刷新)→转换→调用 Kiro→反馈池,返回上游响应 + 凭据 id。
/// 整条选-调链路串行占池锁,网络发出后即释放(见 [`MessagesState`] 说明)。
/// 分类失败(401/403/429)在池锁释放后落进统计层(不阻塞、不跨锁)。
///
/// 单次选-调(可排除已试账号)。返回 `Ok((outcome, tried_id))` 或 `Err((error, tried_id))`,
/// `tried_id` 为本次实际选中的凭据 id(选择即失败=`NoAccount` 时为 `None`,供跨账号重试跟踪)。
///
/// `exclude_ids`:本请求已试过的凭据 id,选择时跳过(除既有 disabled/冷却/RPM 纪律外再叠加)。
pub(crate) async fn select_and_call_once(
    state: &MessagesState,
    req: &MessagesRequest,
    now_unix: u64,
    exclude_ids: &std::collections::HashSet<String>,
    bound: Option<&BoundCredentialIds>,
) -> Result<(CallOutcome, String), (RelayError, Option<String>)> {
    // 1) 选账号(整条链路串行占锁,见 MessagesState 说明);排除本请求已试过的账号。
    let mut pool = state.pool.lock().await;
    let mut cred = pool
        .select_with_exclude(now_unix, exclude_ids, bound)
        .ok_or((RelayError::NoAccount, None))?;

    // 2) 即将过期 → 集中刷新并写回活池(不各自反复轮换令牌;见 kiro::ensure_fresh)。
    //    ensure_fresh 内部先释放再重取池锁,不跨网络 .await 持锁;这里为避免持有本
    //    handler 的池锁(会与 ensure_fresh 内的重取死锁)先释放,刷新后再重新占锁。
    // 本次实际选中的凭据 id(供跨账号重试跟踪;从此处起任何 Err 都带上它)。
    let tried_id = cred.id.clone();

    // 生命周期日志(#7):账号已选中。只记数值 id/model/是否为重试选择,不含令牌/密钥/提示词。
    tracing::info!(
        event = "account_selected",
        account_id = credential_id_num(&cred.id),
        model = %req.model,
        excluded = exclude_ids.len(),
        "已选中账号"
    );

    if cred.expires_soon(now_unix, REFRESH_MARGIN_SECS) {
        // 生命周期日志(#7):令牌即将过期,触发主动刷新(控制面)。不含任何令牌明文。
        tracing::info!(
            event = "token_refresh",
            account_id = credential_id_num(&cred.id),
            reason = "expires_soon",
            "令牌即将过期,刷新中"
        );
        let cred_id = cred.id.clone();
        drop(pool);
        cred = crate::kiro::ensure_fresh::ensure_fresh(
            &state.pool,
            &cred_id,
            // #7:令牌刷新走**控制面**硬超时客户端(connect + 整请求 timeout),而非数据面流式
            // 客户端(无整请求超时)。避免上游刷新挂死拖垮 relay 热路径;与 balance/models 同设计。
            &state.control_client,
            now_unix,
            REFRESH_MARGIN_SECS,
            Some(&state.refresh_ctx),
        )
        .await
        .map_err(|e| {
            (
                RelayError::Upstream(format!("ensure_fresh: {e}")),
                Some(tried_id.clone()),
            )
        })?;
        pool = state.pool.lock().await;
    }
    let credential_id = credential_id_num(&cred.id);

    // 3) 转换请求体(未映射模型/空消息 → 400)。Convert 为请求级致命错误,不触发跨账号重试。
    let kiro_req = anthropic_to_kiro(req, cred.profile_arn.as_deref())
        .map_err(|e| (RelayError::Convert(e), Some(tried_id.clone())))?;
    let body = serde_json::to_vec(&kiro_req).map_err(|e| {
        (
            RelayError::Upstream(format!("encode: {e}")),
            Some(tried_id.clone()),
        )
    })?;

    // 4) 构造伪装身份并调用 Kiro。
    //    这里先释放池锁:数据面调用**不持锁**发出(force_refresh 内部要重取池锁,持锁会死锁),
    //    调用返回后再短暂重取锁反馈成败。
    let imp = impersonation_for(&state.cfg, &cred.refresh_token, cred.machine_id.as_deref());
    drop(pool);

    // 一次数据面尝试:成功→Ok(Response);失败→Err((kind, status))。**不反馈池**,由本函数
    // 在决定是否重试后统一反馈,避免首个 Auth 失败就把可刷新自愈的好账号立即禁用。
    let attempt = call_data_plane(state, &cred, &imp, &body).await;

    // 5) 数据面 401/403(Auth)→ 强制刷新该账号令牌、用新令牌重试一次;仅重试仍失败才反馈池。
    //    重试至多一次(无循环),不会成环。
    let outcome = match attempt {
        Ok(r) => Ok(r),
        // 两类鉴权失败(永久失效 / 歧义冷却)都先强制刷新令牌重试一次:AuthAmbiguous 常为
        // 令牌抖动/过期,刷新后即自愈;AuthInvalid 刷新多半也失败,回落原失败交由反馈池禁用。
        Err((
            orig_kind @ (FailureKind::AuthInvalid | FailureKind::AuthAmbiguous),
            status,
            orig_body,
        )) => {
            // 生命周期日志(#7):数据面鉴权失败,触发强制换新令牌重试一次。含 HTTP 状态码,无令牌明文。
            tracing::info!(
                event = "token_refresh",
                account_id = credential_id,
                reason = "auth_failure",
                status = status,
                "鉴权失败,强制刷新令牌后重试"
            );
            // 强制换新令牌(无条件刷新并写回活池)。刷新失败(refresh_token 也失效)→ 视作真失败。
            match crate::kiro::ensure_fresh::force_refresh(
                &state.pool,
                &cred.id,
                // #7:403 后的强制换新令牌同样走控制面硬超时客户端,防上游刷新挂死拖垮 relay。
                &state.control_client,
                now_unix,
                // 本次数据面请求实际用的 bearer(provider 用的就是它),即"失败令牌":
                // 池内当前值若已不是它,说明别人刚换过,直接复用、不再多刷一轮。
                &cred.access_token,
                Some(&state.refresh_ctx),
            )
            .await
            {
                Ok(fresh) => {
                    let imp2 = impersonation_for(
                        &state.cfg,
                        &fresh.refresh_token,
                        fresh.machine_id.as_deref(),
                    );
                    // 用刷新后的凭据重试一次(仍不反馈池)。
                    call_data_plane(state, &fresh, &imp2, &body).await
                }
                // 刷新失败:回落到**原**鉴权失败类别,交由下方反馈池/禁用(保留 Invalid/Ambiguous 语义)。
                Err(_) => Err((orig_kind, 0u16, orig_body)),
            }
        }
        Err(other) => Err(other),
    };

    // 5.5) 确定性请求错误(如上游 INVALID_MODEL_ID:该模型对当前账号档位不可用):
    //      **非账号故障** —— 换任何同档账号都会同样失败。故此处**不反馈池**(不冷却/不禁用/
    //      不累计 strike)、**不落账号级失败日志**,直接返回 [`RelayError::InvalidModel`],
    //      由 [`select_and_call_with_retry`] 当作致命错误**不重试**、以 400 把清晰的不可用说明回客户端。
    if let Err((FailureKind::InvalidRequest, _, body)) = &outcome {
        // 文案按 reason 码分别给:这一档现在含义不止一种(模型不可用 / 上下文超长),
        // 笼统套一句会把后者讲成前者,使用者照着去换模型,换多少次都没用。
        let msg = crate::kiro::pool::invalid_request_message(body, &req.model);
        return Err((RelayError::InvalidRequest(msg), Some(tried_id)));
    }

    // 6) 依最终结果反馈池(短暂持锁)。分类失败随后在池锁释放外落统计。
    let failure_to_record: Option<(FailureKind, u16, String)> = {
        let mut pool = state.pool.lock().await;
        match &outcome {
            Ok(_) => {
                pool.report_success(&cred.id);
                None
            }
            Err((kind, status, body)) => {
                // 顺带把这次失败的**具体原因**记进池:上游响应体在这里还在手上,分类完就丢
                // 的话,面板永远只能显示一个笼统的"异常"(封禁/限流/额度耗尽全混在一起)。
                let reason = crate::kiro::pool::status_reason_from_body(body);
                pool.report_failure_with_reason(&cred.id, *kind, reason, now_unix);
                Some((*kind, *status, body.clone()))
            }
        }
    };

    // 分类失败落库(池锁已释放,fire-and-forget,不阻塞热路径的其它请求)。
    if let Some((kind, status, body)) = failure_to_record {
        record_classified_failure(&state.stats, credential_id, kind, status, &body, now_unix).await;
        // 瞬态与账号级分开:前者值得退避后再换号(上游抖动时不放大),后者换个账号
        // 立刻就能成、等待纯属拖慢用户。此分工照观测 —— 真实客户端在 408/429/5xx 上退避、
        // 在 401/403 换账号时不退避,而它打同一个上游长期稳定。
        let msg = "data-plane request failed".to_string();
        return Err((
            if kind == FailureKind::Transient {
                RelayError::UpstreamTransient(msg)
            } else {
                RelayError::Upstream(msg)
            },
            Some(tried_id),
        ));
    }

    let resp = outcome.expect("resp 存在当且仅当无失败记录");
    Ok((
        CallOutcome {
            resp,
            credential_id,
        },
        tried_id,
    ))
}

/// 数据面调用的有效 region:优先取 profileArn 里编码的 region(账号真实 region),
/// 回落到 `cred.region`,再回落到 `us-east-1`。
///
/// 动机(#9):非 us-east-1 账号的 profileArn ARN 段带真实 region;若只看 `cred.region`
/// (默认 us-east-1)会把请求发到 us-east-1 主机 → 403/400。ARN 解析在
/// [`crate::kiro::endpoint::region_from_profile_arn`]。
fn effective_region(cred: &crate::kiro::credential::Credential) -> String {
    cred.profile_arn
        .as_deref()
        .and_then(crate::kiro::endpoint::region_from_profile_arn)
        .or_else(|| {
            let r = cred.region.trim();
            if r.is_empty() {
                None
            } else {
                Some(r.to_string())
            }
        })
        .unwrap_or_else(|| "us-east-1".to_string())
}

/// 发一次数据面请求(**不反馈池**)。成功→`Ok(Response)`;失败→`Err((FailureKind, status))`,
/// status 为可精确解析到的 HTTP 码(端点回退路径无从精确解析时回落 0)。
///
/// 覆盖两条路径:`endpoint_override`(代理/mock,单端点直调 [`provider::call`])与生产端点回退
/// [`provider::call_with_fallback_no_report`]。是否重试/反馈池由调用方 [`select_and_call_with_retry`] 决定。
///
/// 分类:override 路径能精确拿到状态码与响应体,故用 [`classify_with_body`] 依响应体升级判定
/// (真凭据失效才永久禁用);端点回退路径由 provider 内部分类,status 回落 0。
///
/// 数据面 region 取自 [`effective_region`](profileArn 优先),而非裸 `cred.region`(#9)。
async fn call_data_plane(
    state: &MessagesState,
    cred: &crate::kiro::credential::Credential,
    imp: &Impersonation,
    body: &[u8],
) -> Result<reqwest::Response, (FailureKind, u16, String)> {
    match &state.endpoint_override {
        Some(url) => {
            let ep = Endpoint {
                url: url.clone(),
                origin: "AI_EDITOR",
                target: None,
            };
            match provider::call(&state.client, &ep, cred, imp, body).await {
                Ok(r) => Ok(r),
                Err(e) => {
                    // provider::call 在非 2xx 时读体后返回 `UpstreamHttp{status, body}`
                    //(见 provider::call 的 #2 body capture):携带**数字状态码**与**真响应体**
                    // 摘要。把二者一并喂给 body-aware classify_with_body,使真凭据失效信号可达
                    // AuthInvalid(永久禁用);其余变体(纯传输层 Http / 语义标签 Upstream)无
                    // HTTP 体可解析,status 回落 0、body 空串 → classify 保守落 AuthAmbiguous/Transient。
                    let (status, body) = match &e {
                        LoginError::UpstreamHttp { status, body } => (*status, body.as_str()),
                        _ => (0u16, ""),
                    };
                    Err((classify_with_body(status, body), status, body.to_string()))
                }
            }
        }
        None => provider::call_with_fallback_no_report(
            &state.client,
            &effective_region(cred),
            "auto",
            true,
            cred,
            imp,
            body,
        )
        .await
        // 生产端点回退路径:`try_endpoints`(provider.rs)在终态(尤其 401/403)已**读响应体**
        // 并用 body-aware `classify_with_body(status, &body)` 分类,故真凭据失效信号已在此处到达
        // 分类、AuthInvalid(永久禁用)在生产路径同样可达——`FailureKind` 已反映真响应体。
        // provider 这层不回原始状态码,故由 kind 反推一个代表性状态码供统计日志用:
        // Auth→403、Quota→429、Transient→0(record_classified_failure 再兜底)。响应体则由
        // `DataPlaneFailure` 原样带上来,落进失败/限流日志的"详情"列。
        .map_err(|f| {
            let status = match f.kind {
                FailureKind::AuthInvalid | FailureKind::AuthAmbiguous => 403u16,
                FailureKind::Quota => 429u16,
                FailureKind::Transient => 0u16,
                FailureKind::InvalidRequest => 400u16,
            };
            (f.kind, status, f.body)
        }),
    }
}

/// 可测试内核(非流式):接收注入的 `now_unix`,跑完整条中转链路 → 一个 [`MessagesResponse`]。
///
/// 兼容入口:用量记录归属到无 store-key(id=0)。其它协议(openai/gemini/responses)
/// 复用此签名;需归属 store-key 时用 [`relay_core_attributed`]。
pub async fn relay_core(
    state: &MessagesState,
    req: MessagesRequest,
    now_unix: u64,
) -> Result<MessagesResponse, RelayError> {
    relay_core_attributed(state, req, 0, None, None, now_unix).await
}

/// 非流式内核(带 store-key 归属)。`api_key_id` 为鉴权闸解析出的 store-key id
/// (0 = 全局/开放模式,无归属),用量记录归属到该 key。`client_ip` 为调用方 IP
/// (由 handler 经 [`extract_client_ip`] 算出;无则 `None`),随用量记录落库。
///
/// 兼容入口:上游在 200 事件流里下发的 exception 在这里被压成 [`RelayError::Upstream`],
/// 对外即 502;需要按 429/403/400 精确回状态码的协议层改用 [`relay_core_outcome`]。
pub async fn relay_core_attributed(
    state: &MessagesState,
    req: MessagesRequest,
    api_key_id: u32,
    client_ip: Option<String>,
    bound: Option<BoundCredentialIds>,
    now_unix: u64,
) -> Result<MessagesResponse, RelayError> {
    match relay_core_outcome(state, req, api_key_id, client_ip, bound, now_unix).await? {
        CoreOutcome::Response(resp) => Ok(resp),
        CoreOutcome::Exception { e, .. } => Err(RelayError::Upstream(exception_detail(&e))),
    }
}

/// 非流式内核的完整结果。
///
/// 上游把限流/鉴权/参数错误放在 **HTTP 200** 的事件流里,以 `:message-type == "exception"`
/// 帧下发;直接把这样的帧序列交给还原函数,客户端只会拿到 200 + 空内容 + `end_turn`,
/// 既察觉不到失败也不会重试。故内核把它单独表达出来,由协议层按自己的错误体形状回错。
#[derive(Debug)]
pub enum CoreOutcome {
    /// 正常响应。
    Response(MessagesResponse),
    /// 上游事件流内的非截断 exception。`status` 已由 `exception_status` 映射为对外 HTTP 码
    /// (429 / 403 / 400 / 502)。
    Exception { status: u16, e: StreamException },
}

/// 非流式内核本体(见 [`relay_core_attributed`] 的参数语义)。
///
/// 与旧版的唯一差别:把上游事件流里的 exception 帧当作一等结果返回,而不是还原成
/// "空内容 + end_turn" 的假成功。命中 exception 时**不**落用量记录(本轮没有产出内容)。
pub async fn relay_core_outcome(
    state: &MessagesState,
    req: MessagesRequest,
    api_key_id: u32,
    client_ip: Option<String>,
    bound: Option<BoundCredentialIds>,
    now_unix: u64,
) -> Result<CoreOutcome, RelayError> {
    let started = std::time::Instant::now();
    // 跨账号重试:账号级失败自动换下一个健康账号,直到成功或用尽自适应上限;
    // 请求级致命错误(Convert/400)不重试。返回成功前不会开始读 body。
    let CallOutcome {
        resp,
        credential_id,
    } = select_and_call_with_retry(state, &req, now_unix, bound.as_ref()).await?;

    // 端到端延迟(选账号+跨账号重试+调上游,至上游响应就绪):既用于 INFO 日志,也随用量记录落库
    // 供 usage-summary 计 avgLatencyMs(与日志的 latency_ms 同源、口径一致)。
    let latency_ms = started.elapsed().as_millis() as u64;

    // 每请求一条 INFO(供 /admin 日志查看器):method/model/account/api-key/status/延迟,
    // 不含任何 token/密钥/提示词。热路径外、无锁,一行一请求。
    tracing::info!(
        method = "messages",
        model = %req.model,
        account_id = credential_id,
        api_key_id = api_key_id,
        status = "ok",
        latency_ms = latency_ms,
        "relay 已处理请求"
    );

    // 读 body → 解码事件流 → 还原 Anthropic 响应。
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| RelayError::Upstream(format!("read body: {e}")))?;
    let mut dec = StreamDecoder::new();
    dec.push(&bytes);
    let frames = dec.drain();

    // 必须先于还原响应查 exception:上游用 200 + exception 帧表达限流/鉴权/参数错误,
    // 交给 kiro_events_to_anthropic 只会得到空内容 + end_turn(截断类另有 Truncation 路径,
    // 与此互斥,不会被这里抢走)。
    if let Some(e) = extract_exception(&frames) {
        let status = exception_status(&e.kind);
        tracing::warn!(
            event = "upstream_stream_exception",
            model = %req.model,
            account_id = credential_id,
            kind = %e.kind,
            status = status,
            "上游事件流下发 exception"
        );
        return Ok(CoreOutcome::Exception { status, e });
    }

    let out = kiro_events_to_anthropic(&frames, &req.model);

    // meteringEvent(若上游发了)带真实积分消耗与缓存 token;无则 credits=None、回退字符估算。
    let metering = crate::kiro::convert::extract_metering(&frames);
    let credits = metering.as_ref().map(|m| m.credits);
    let cache_read = metering.as_ref().and_then(|m| m.cache_read_input_tokens);
    let cache_creation = metering
        .as_ref()
        .and_then(|m| m.cache_creation_input_tokens);

    // 成功中转 → 记录一条用量(池锁已释放,异步/批量存储,不阻塞热路径)。
    // token 数取自解码后的 usage:meteringEvent 带真实计量时即上游口径。上游只回报 credits、
    // 不含 token,故实际总是走回退:output = 响应字符数/4,input 此前**恒为 0**。
    // input 为 0 不是"没有输入",是解码器看不到请求;而这里请求就在手边,故补上估算——
    // 否则 USD 少算了成本里通常更大的一半,按 USD 设的上限会系统性偏松。
    let input_tokens = if out.usage.input_tokens == 0 {
        estimate_request_input_tokens(&req)
    } else {
        out.usage.input_tokens
    };
    let estimated_cost = crate::stats::pricing::calculate_cost(
        &req.model,
        input_tokens as i32,
        out.usage.output_tokens as i32,
    );
    state
        .stats
        .usage
        .record_usage_full(
            credential_id,
            api_key_id,
            req.model.clone(),
            input_tokens as i32,
            out.usage.output_tokens as i32,
            estimated_cost,
            credits,
            client_ip,
            cache_read,
            cache_creation,
            Some(latency_ms),
            now_unix as i64,
        )
        .await;

    Ok(CoreOutcome::Response(out))
}

/// 流式用量记账哨兵(#18):无论流如何结束(正常收尾 / 客户端断连 / 上游中途出错),
/// 只要 `stream!` future 被 drop,本哨兵的 [`Drop`] 就会用**当时已累计**的 token/credits 落一条用量,
/// 避免"记账代码在读循环之后、中途丢弃即漏记"。
///
/// 记账本身是异步的(写入经 `.await` 的锁),而 `Drop` 不能 `.await`,故 Drop 里 `tokio::spawn`
/// 一个短任务完成落库(持 `Arc<UsageTracker>`,与流生命周期解耦)。`recorded` 防重复:正常收尾
/// 路径显式调 [`StreamUsageGuard::flush`] 立即记一次并置位,Drop 时若已记则不再重复。
struct StreamUsageGuard {
    usage: Arc<crate::stats::usage::UsageTracker>,
    credential_id: u32,
    api_key_id: u32,
    /// 调用方 IP(由 handler 经 `extract_client_ip` 算出;无则 `None`),随用量记录落库。
    client_ip: Option<String>,
    model: String,
    now_unix: i64,
    /// 累计输出字符数(收尾按 ÷4 估算 output_tokens);随流增量更新。
    total_chars: usize,
    /// 末次 meteringEvent 的真实计费(有则优先于字符估算)。
    metering: Option<crate::kiro::convert::MeteringUsage>,
    /// 已记账标记:避免正常收尾 + Drop 双写。
    recorded: bool,
    /// 流建立延迟(毫秒):选账号+跨账号重试+调上游至 SSE 就绪(与流式 INFO 日志 latency_ms 同源)。
    /// 流式无"整体完成"墙钟(下发时长取决于客户端消费速度),故此处记的是**首字节前**的建立延迟,
    /// 与非流式 relay_core 的 latency 口径统一(都测选-调至上游响应就绪),供 usage-summary 计 avgLatencyMs。
    latency_ms: Option<u64>,
}

impl StreamUsageGuard {
    /// 本次流是否累计到**有意义的用量**:有输出字符(→ output_tokens>0)或收到过 meteringEvent
    /// (上游真实计费,可能 output 为 0 但仍有 input/credits/缓存计费)。二者皆无 = 极早取消
    /// (纯工具轮尚未产出文本、或计量前客户端断连),落库只会写一条零/近零行,应跳过。
    fn has_meaningful_usage(&self) -> bool {
        self.total_chars >= CHARS_PER_TOKEN || self.metering.is_some()
    }

    /// 把当前累计量落一条用量(同步组装参数,异步写库经 `tokio::spawn`)。幂等:仅首次生效。
    fn flush(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;

        // token 数优先取 meteringEvent 的真实计量,逐项回退:input 无从估算 → 0;
        // output → 累计字符数 ÷ 4。
        let input_tokens = self
            .metering
            .as_ref()
            .and_then(|m| m.input_tokens)
            .unwrap_or(0) as i32;
        let output_tokens = self
            .metering
            .as_ref()
            .and_then(|m| m.output_tokens)
            .map(|n| n as i32)
            .unwrap_or((self.total_chars / CHARS_PER_TOKEN) as i32);
        let credits = self.metering.as_ref().map(|m| m.credits);
        let cache_read = self
            .metering
            .as_ref()
            .and_then(|m| m.cache_read_input_tokens);
        let cache_creation = self
            .metering
            .as_ref()
            .and_then(|m| m.cache_creation_input_tokens);
        // estimated_cost 按定价表由两侧 token 换算(见 stats::pricing)。
        let estimated_cost =
            crate::stats::pricing::calculate_cost(&self.model, input_tokens, output_tokens);

        let usage = self.usage.clone();
        let model = self.model.clone();
        let (credential_id, api_key_id, now_unix) =
            (self.credential_id, self.api_key_id, self.now_unix);
        let latency_ms = self.latency_ms;
        let client_ip = self.client_ip.clone();
        tokio::spawn(async move {
            usage
                .record_usage_full(
                    credential_id,
                    api_key_id,
                    model,
                    input_tokens,
                    output_tokens,
                    estimated_cost,
                    credits,
                    client_ip,
                    cache_read,
                    cache_creation,
                    latency_ms,
                    now_unix,
                )
                .await;
        });
    }
}

impl Drop for StreamUsageGuard {
    fn drop(&mut self) {
        // 正常收尾已显式 flush 过 → recorded=true,此处 flush 立即 no-op(不双写)。
        // 未收尾即被 drop(客户端中途断连 / 上游中途出错)才走补记:但只在**已累计到有意义
        // 用量**时补记——极早取消(纯工具轮未产出文本、或计量前断连)只会写零/近零行,直接跳过
        // (#16)。`recorded` + `has_meaningful_usage` 双闸:正常路径靠前者防双写,补记路径靠后者
        // 滤零行。
        if self.recorded {
            return;
        }
        if self.has_meaningful_usage() {
            self.flush();
        }
    }
}

/// 流式内核:选-调后,把上游事件流增量编码为 Anthropic SSE。
///
/// 兼容入口:用量记录归属到无 store-key(id=0)。需归属时用 [`relay_stream_attributed`]。
pub async fn relay_stream(
    state: &MessagesState,
    req: MessagesRequest,
    now_unix: u64,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>> + use<>>, RelayError> {
    relay_stream_attributed(state, req, 0, None, None, now_unix).await
}

/// 流式内核(带 store-key 归属)。`api_key_id` 同 [`relay_core_attributed`];
/// `client_ip` 为调用方 IP(由 handler 经 [`extract_client_ip`] 算出),随用量记录落库。
pub async fn relay_stream_attributed(
    state: &MessagesState,
    req: MessagesRequest,
    api_key_id: u32,
    client_ip: Option<String>,
    bound: Option<BoundCredentialIds>,
    now_unix: u64,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>> + use<>>, RelayError> {
    let started = std::time::Instant::now();
    // 跨账号重试在**流开始之前**完成:拿到成功响应才建 SSE,失败账号的响应永不会开始下发。
    let CallOutcome {
        mut resp,
        credential_id,
    } = select_and_call_with_retry(state, &req, now_unix, bound.as_ref()).await?;
    let model = req.model.clone();

    // 流建立延迟(选账号+跨账号重试+调上游至 SSE 就绪):既用于 INFO 日志,也随用量记录落库供
    // usage-summary 计 avgLatencyMs(与非流式 relay_core 的 latency 口径统一:皆测至上游响应就绪)。
    let latency_ms = started.elapsed().as_millis() as u64;

    // 每请求一条 INFO(供 /admin 日志查看器):流已建立即记,latency = 选账号+调上游耗时。
    // 只含 method/model/account/api-key/status/延迟,不含任何 token/密钥/提示词。
    tracing::info!(
        method = "messages_stream",
        model = %req.model,
        account_id = credential_id,
        api_key_id = api_key_id,
        status = "streaming",
        latency_ms = latency_ms,
        "relay 流已建立"
    );

    // 统计层用量句柄(Arc,移入哨兵);记账经 Drop 哨兵在流**任意方式结束**时都落一条(#18)。
    let usage_handle = state.stats.usage.clone();
    let record_model = req.model.clone();

    // 块索引状态机(照 Anthropic 公开流式规范;Kiro toolUseEvent 帧序照真机观测):
    // 不变量 = 同一时刻至多一个内容块打开(先关后开);每块分配唯一递增 index;
    // 工具块按 toolUseId 键控;文本块懒开(首个文本增量到达才发 text 的 content_block_start),
    // 故纯工具轮不产出空文本块。文本块收到工具块后可重开(index 递增)。
    // 已发过 content_block_stop 的 index 记进 `closed_blocks`:该块此后不再产出任何
    // delta,也不会再发第二个 stop——上游偶发的迟到 input/stop 帧会让严格校验的官方 SDK
    // 报解析错误或把参数拼错块。
    let body = async_stream::stream! {
        let id = crate::kiro::convert::new_message_id();
        let to_event = |e: stream::SseEvent| Event::default().event(e.event).data(e.data);

        // 用量记账哨兵:累计 token/credits 存于此;正常收尾显式 flush,断连/出错时其 Drop 补记(#18)。
        // 它必须在读循环之前建立、活到 stream! future 被 drop 为止,故声明在此作用域内。
        let mut usage_guard = StreamUsageGuard {
            usage: usage_handle,
            credential_id,
            api_key_id,
            client_ip,
            model: record_model,
            now_unix: now_unix as i64,
            total_chars: 0,
            metering: None,
            recorded: false,
            latency_ms: Some(latency_ms),
        };

        yield Ok(to_event(stream::message_start(&id, &model, 0)));

        let mut dec = StreamDecoder::new();
        let mut next_index: u32 = 0;
        let mut open_block: Option<u32> = None; // 当前打开的块 index
        let mut text_index: Option<u32> = None; // 当前文本块 index(关闭后置 None 以便重开)
        let mut tool_index: HashMap<String, u32> = HashMap::new(); // toolUseId → 块 index
        let mut closed_blocks: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut any_tool = false;
        let mut truncation: Option<Truncation> = None; // 首个截断信号(max_tokens / 上下文耗尽)
        let mut stream_error: Option<StreamException> = None; // 上游中途下发的 exception
        // 传输层中断(连接重置 / TLS 中断 / 读超时 / chunked 体未收尾)。与上面的 in-band
        // exception 是两回事:那是上游"说"自己出错了,这是连接本身断了。两者都必须以 error
        // 事件收尾——若照常发 message_delta(end_turn)+message_stop,客户端会把半截回答
        // 当成正常完成,既不报错也不重试。(504=读超时,其余 502。)
        let mut transport_err: Option<(u16, String)> = None;

        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    dec.push(&chunk);
                    for frame in dec.drain() {
                        if let Some(t) = crate::kiro::convert::frame_text_delta(&frame) {
                            // assistantResponseEvent:懒开文本块,再发 text_delta。
                            if text_index.is_none() {
                                // 关闭当前工具块(若有);随后无条件开新文本块,故此处不必显式置 None。
                                if let Some(oi) = open_block && closed_blocks.insert(oi) {
                                    yield Ok(to_event(stream::content_block_stop(oi)));
                                }
                                let idx = next_index;
                                next_index += 1;
                                text_index = Some(idx);
                                yield Ok(to_event(stream::content_block_start(idx)));
                                open_block = Some(idx);
                            }
                            usage_guard.total_chars += t.chars().count();
                            yield Ok(to_event(stream::text_delta(text_index.unwrap(), &t)));
                        } else if let Some(v) = crate::kiro::convert::tool_use_frame(&frame) {
                            // toolUseEvent:open 帧开新工具块、input 帧发 input_json_delta、stop 帧关块。
                            let Some(id) = v["toolUseId"].as_str() else { continue };
                            if !tool_index.contains_key(id) {
                                // 关闭当前块(若有);随后无条件开新工具块,故此处不必显式置 None。
                                if let Some(oi) = open_block {
                                    if closed_blocks.insert(oi) {
                                        yield Ok(to_event(stream::content_block_stop(oi)));
                                    }
                                    if text_index == Some(oi) {
                                        text_index = None; // 允许后续文本重开新块
                                    }
                                }
                                let name = v["name"].as_str().unwrap_or("");
                                let idx = next_index;
                                next_index += 1;
                                tool_index.insert(id.to_string(), idx);
                                yield Ok(to_event(stream::tool_use_start(idx, id, name)));
                                open_block = Some(idx);
                                any_tool = true;
                            }
                            let idx = tool_index[id];
                            // 该块已收尾 → 迟到的 input/stop 帧一律丢弃,不再产出 delta 或第二个 stop。
                            if !closed_blocks.contains(&idx) {
                                if let Some(inp) = v["input"].as_str() {
                                    yield Ok(to_event(stream::input_json_delta(idx, inp)));
                                }
                                if v["stop"].as_bool() == Some(true) {
                                    closed_blocks.insert(idx);
                                    yield Ok(to_event(stream::content_block_stop(idx)));
                                    if open_block == Some(idx) {
                                        open_block = None;
                                    }
                                }
                            }
                        } else if let Some(m) = crate::kiro::convert::metering_frame(&frame) {
                            // meteringEvent:记住真实积分消耗(多个则末次覆盖),流收尾时落库。
                            usage_guard.metering = Some(m);
                        } else if let Some(tr) = crate::kiro::convert::frame_truncation(&frame) {
                            // 截断信号(ContentLengthExceededException / contextUsage 100%):
                            // 取首个,收尾时据此给出 stop_reason,与非流式路径同口径。
                            if truncation.is_none() {
                                truncation = Some(tr);
                            }
                        } else if let Some(e) = crate::kiro::convert::frame_exception(&frame) {
                            // 上游中途报错(限流/鉴权/参数):记下并立刻停止读帧,收尾走 error 事件。
                            stream_error = Some(e);
                            break;
                        }
                        // 忽略其它事件。
                    }
                    if stream_error.is_some() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    // 流中断:记下原因,收尾走 error 事件(不可伪装成正常完成)。
                    let status = if e.is_timeout() { 504 } else { 502 };
                    transport_err = Some((status, e.to_string()));
                    break;
                }
            }
        }

        if let Some(oi) = open_block && closed_blocks.insert(oi) {
            yield Ok(to_event(stream::content_block_stop(oi)));
        }

        if let Some((status, detail)) = transport_err {
            // 传输层中断:同 in-band exception 的收尾口径——发 error 事件后终止,
            // 不发 message_delta/message_stop。用量照常落库(上游确实已消耗)。
            tracing::warn!(
                event = "upstream_stream_interrupted",
                model = %model,
                account_id = credential_id,
                status = status,
                detail = %detail,
                "上游事件流传输中断"
            );
            yield Ok(to_event(stream_error_event(status, &format!("upstream stream interrupted: {detail}"))));
        } else if let Some(e) = stream_error {
            // 上游在 200 事件流里报错:照 Anthropic 流式规范发 `error` 事件后就此终止。
            // **不**再发 message_delta/message_stop——那会把失败伪装成正常完成,
            // 客户端既察觉不到也不会重试。
            let status = exception_status(&e.kind);
            tracing::warn!(
                event = "upstream_stream_exception",
                model = %model,
                account_id = credential_id,
                kind = %e.kind,
                status = status,
                "上游事件流下发 exception"
            );
            yield Ok(to_event(stream_error_event(status, &exception_detail(&e))));
        } else {
            // stop_reason 与非流式 kiro_events_to_anthropic 同口径:tool_use 优先级最高,
            // 其次才看截断信号,最后才是 end_turn。
            let stop_reason = if any_tool {
                "tool_use"
            } else {
                match truncation {
                    Some(Truncation::MaxTokens) => "max_tokens",
                    Some(Truncation::ContextWindow) => "model_context_window_exceeded",
                    None => "end_turn",
                }
            };
            // output_tokens 优先用 meteringEvent 的真实计量,上游没带才回退字符估算。
            let output_tokens = usage_guard
                .metering
                .as_ref()
                .and_then(|m| m.output_tokens)
                .unwrap_or((usage_guard.total_chars / CHARS_PER_TOKEN) as u32);
            yield Ok(to_event(stream::message_delta(stop_reason, output_tokens)));
            yield Ok(to_event(stream::message_stop()));
        }

        // 流收尾(正常完成或已发 error 事件)→ 立即用当前累计量记一条用量:token 优先取
        // meteringEvent 的真实计量,缺项回退估算;credits/缓存同样取末次 meteringEvent。
        // flush 幂等并置 recorded,故随后哨兵 Drop 不会重复落库。若客户端在收尾前断连,
        // 则本行不执行,由 usage_guard 的 Drop 补记同样的累计量(#18)。
        usage_guard.flush();
    };

    Ok(Sse::new(body).keep_alive(
        // SSE 保活。上游"想"得久时(长推理、长工具链)会有几十秒一个字节都不出,
        // 中间的 CDN / 反代 / 客户端读超时会把这条静默的连接掐掉,表现成"会话莫名其妙断了"。
        // 25 秒一个注释帧,既低于常见的 30/60 秒空闲阈值,又不干扰任何客户端解析
        //(`:` 开头的注释行是 SSE 规范里明确要求忽略的)。
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(25))
            .text("keep-alive"),
    ))
}

/// axum handler:计算真实 now,按 `stream` 分流到 [`relay_stream`] 或 [`relay_core`]。
///
/// 鉴权闸(见 `server::auth`)命中 store key 时会把 [`ApiKeyId`] 塞进请求扩展;
/// 全局 key/开放模式下扩展缺失,归属 id 记 0。
///
/// 请求体以 `Result<Json<..>, JsonRejection>` 提取:解析失败时由本函数回标准 Anthropic
/// 错误体(400 + `invalid_request_error`),而不是 axum 默认的纯文本 422 —— 后者 SDK 解析不了。
pub async fn messages(
    State(state): State<MessagesState>,
    connect_info: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: axum::http::HeaderMap,
    api_key_id: Option<axum::Extension<ApiKeyId>>,
    bound: Option<axum::Extension<BoundCredentialIds>>,
    payload: Result<Json<MessagesRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let req = match payload {
        Ok(Json(req)) => req,
        Err(rejection) => {
            return anthropic_error_response(
                StatusCode::BAD_REQUEST.as_u16(),
                &rejection.body_text(),
            );
        }
    };
    let api_key_id = api_key_id.and_then(|axum::Extension(k)| k.0).unwrap_or(0);
    // store-key 绑定白名单(鉴权闸解析;扩展缺席 = 不受限)。下传给选号层执行。
    let bound = bound.map(|axum::Extension(b)| b);
    // 客户端 IP:优先 X-Forwarded-For/X-Real-IP(反代场景),否则取 socket 对端地址。
    // ConnectInfo 经 make-service 塞进请求扩展,以 `Option<Extension<..>>` 读取:单测用 oneshot
    // 不带连接信息,此时为 None、回落到仅头部提取(ConnectInfo 本身在 axum 0.8 无 Option 提取器)。
    let client_ip = extract_client_ip(
        &headers,
        connect_info.map(|axum::Extension(ci)| ci.0),
        state.cfg.trusted_proxy_hops,
    );
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if req.stream == Some(true) {
        match relay_stream_attributed(&state, req, api_key_id, client_ip, bound, now_unix).await {
            Ok(sse) => sse.into_response(),
            Err(e) => e.into_response(),
        }
    } else {
        match relay_core_outcome(&state, req, api_key_id, client_ip, bound, now_unix).await {
            Ok(CoreOutcome::Response(resp)) => (StatusCode::OK, Json(resp)).into_response(),
            // 上游 200 事件流里的 exception:按映射出的状态码回错误,不再是 200 + 空内容。
            Ok(CoreOutcome::Exception { status, e }) => {
                anthropic_error_response(status, &exception_detail(&e))
            }
            Err(e) => e.into_response(),
        }
    }
}

/// `created_at` 固定串(非官方数据,仅用于形状对齐 Anthropic 公开模型列表响应)。
const MODEL_CREATED_AT: &str = "2026-01-01T00:00:00Z";

/// 每张图片的固定 token 估算(#19)。base64 数据面积与其 token 成本无线性关系,直接按
/// 字符数换算会离谱地高估;取一个保守的每图固定基数(照公开经验值量级)代替。**非官方**。
const IMAGE_TOKEN_ESTIMATE: usize = 1_600;
/// 字符→token 的粗略换算基数(英文经验粗估)。
const CHARS_PER_TOKEN: usize = 4;

/// 估算单条消息 `content` 的可计费字符数:文本块取其字符数;工具调用取其 input 的序列化长度;
/// 工具结果取其 content 的序列化长度;图片单独按固定 token 计(见 [`IMAGE_TOKEN_ESTIMATE`]),
/// 这里以等值字符数回抵进字符池,便于末尾统一除以 [`CHARS_PER_TOKEN`]。
///
/// 返回 `(chars, image_count)`:`chars` 计入字符池、`image_count` 每张按固定 token 另加。
fn count_content_chars(content: &crate::protocol::anthropic::types::ContentIn) -> (usize, usize) {
    use crate::protocol::anthropic::types::{Block, ContentIn};
    match content {
        ContentIn::Text(s) => (s.chars().count(), 0),
        ContentIn::Blocks(blocks) => {
            let mut chars = 0usize;
            let mut images = 0usize;
            for b in blocks {
                match b {
                    Block::Text { text } => chars += text.chars().count(),
                    // 工具调用:name + 序列化后的 input JSON 长度都要计。
                    Block::ToolUse { name, input, .. } => {
                        chars += name.chars().count();
                        chars += serde_json::to_string(input).map(|s| s.len()).unwrap_or(0);
                    }
                    // 工具结果:序列化后的 content 长度(字符串/数组/对象皆可)。
                    Block::ToolResult { content, .. } => {
                        chars += serde_json::to_string(content).map(|s| s.len()).unwrap_or(0);
                    }
                    // 图片:按固定 token 估算,不按 base64 字符数(见 IMAGE_TOKEN_ESTIMATE)。
                    Block::Image { .. } => images += 1,
                    // 未知/不转发的块(thinking、document、search_result…):不进上游请求,不计入估算。
                    Block::Other => {}
                }
            }
            (chars, images)
        }
    }
}

/// `POST /v1/messages/count_tokens`:粗略估算输入 token 数。计入 `system`、每条消息的文本,
/// **以及 `tools` 定义、`tool_use` 的 input、`tool_result` 的 content、图片**(#19):
/// 文本/JSON 按字符数 ÷ 4;图片按每张固定 token 估算。下限 1。**非官方 tokenizer**,仅供参考;
/// 纯函数:不选账号、不打网络。
///
/// 请求体以 `Result<Json<..>, JsonRejection>` 提取,与 `/v1/messages` 同法:解析失败时回标准
/// Anthropic 错误体的 400,而不是 axum 默认的纯文本 422 —— 后者 SDK 用 `response.json()` 读
/// 错误时只会抛解析异常,真正的失败原因被吞掉;且 422 也不在 Anthropic 的错误码契约里。
/// 由请求估算输入 token(与 `/v1/messages/count_tokens` 同一口径)。
///
/// 上游 meteringEvent 只回报 credits、不含 token 计量,故记账时输入项此前**硬编码为 0**——
/// 等于把成本里通常更大的那一半整个抹掉,USD 上限因此系统性偏松(长上下文尤甚)。
/// 这里复用 count_tokens 的统计口径,不另起一套平行实现,免得两处估算各说各话。
pub fn estimate_request_input_tokens(req: &MessagesRequest) -> u32 {
    let mut chars = req
        .system
        .as_ref()
        .map(|s| s.text().chars().count())
        .unwrap_or(0);
    let mut images = 0usize;
    for m in &req.messages {
        let (c, i) = count_content_chars(&m.content);
        chars += c;
        images += i;
    }
    if let Some(tools) = &req.tools {
        for t in tools {
            chars += t.name.chars().count();
            chars += t
                .description
                .as_deref()
                .map(|d| d.chars().count())
                .unwrap_or(0);
            chars += serde_json::to_string(&t.input_schema)
                .map(|s| s.len())
                .unwrap_or(0);
        }
    }
    (chars / CHARS_PER_TOKEN + images * IMAGE_TOKEN_ESTIMATE).max(1) as u32
}

pub async fn count_tokens(
    payload: Result<Json<MessagesRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let req = match payload {
        Ok(Json(req)) => req,
        Err(rejection) => {
            return anthropic_error_response(
                StatusCode::BAD_REQUEST.as_u16(),
                &rejection.body_text(),
            );
        }
    };
    let mut chars = req
        .system
        .as_ref()
        .map(|s| s.text().chars().count())
        .unwrap_or(0);
    let mut images = 0usize;

    // 消息内容:文本 + 工具调用/结果的序列化长度 + 图片计数。
    for m in &req.messages {
        let (c, i) = count_content_chars(&m.content);
        chars += c;
        images += i;
    }

    // 工具定义:每个工具的 name + description + input_schema 序列化长度都算进输入预算。
    if let Some(tools) = &req.tools {
        for t in tools {
            chars += t.name.chars().count();
            chars += t
                .description
                .as_deref()
                .map(|d| d.chars().count())
                .unwrap_or(0);
            chars += serde_json::to_string(&t.input_schema)
                .map(|s| s.len())
                .unwrap_or(0);
        }
    }

    let input_tokens = (chars / CHARS_PER_TOKEN + images * IMAGE_TOKEN_ESTIMATE).max(1) as u32;
    (StatusCode::OK, Json(CountTokensResponse { input_tokens })).into_response()
}

/// `GET /claude/v1/models`:固定的 Anthropic 形状模型列表(本中转支持的模型),
/// 不读时钟、不打网络。
pub async fn anthropic_models() -> Json<AnthropicModelList> {
    // 目录来自 `models_catalog::CATALOG`(与 OpenAI / Gemini 侧及管理端点同源);
    // 此前这里硬编码三条 claude,列出来的远少于实际能服务的模型。
    let data: Vec<AnthropicModel> = crate::models_catalog::CATALOG
        .iter()
        .map(|e| AnthropicModel {
            kind: "model".to_string(),
            id: e.id.to_string(),
            display_name: e.display_name.to_string(),
            created_at: MODEL_CREATED_AT.to_string(),
        })
        .collect();
    let first_id = data.first().map(|m| m.id.clone());
    let last_id = data.last().map(|m| m.id.clone());
    Json(AnthropicModelList {
        data,
        has_more: false,
        first_id,
        last_id,
    })
}

/// 带自身状态的 `/v1/messages` 子路由(供 `build_router` 合并,不影响其它路由的状态类型)。
///
/// 同时挂载 `/claude/v1` 前缀的等价变体(供统一鉴权闸按前缀识别 Anthropic 协议),
/// 以及 `count_tokens` 估算端点与只读的 `GET /claude/v1/models`。
/// 注意:不挂裸 `/v1/models`——那是 OpenAI 协议的路由,归属另一处。
pub fn messages_router(state: MessagesState) -> Router {
    Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/claude/v1/messages", post(messages))
        .route("/claude/v1/messages/count_tokens", post(count_tokens))
        .route("/claude/v1/models", axum::routing::get(anthropic_models))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};
    use crate::kiro::eventstream::crc::crc32;
    use crate::kiro::pool::LbMode;
    use axum::body::Body;

    /// `extract_client_ip` 的取值契约。
    ///
    /// **不是**取 XFF 最左那项 —— 最左恰恰是调用方自己就能写死的一项。每一跳可信反代会把它
    /// 亲眼看到的对端追加到最右,所以取"倒数第 `trusted_proxy_hops` 项"才是伪造不了的那个。
    #[test]
    fn extract_client_ip_takes_the_hop_the_trusted_proxy_observed() {
        let peer: std::net::SocketAddr = "10.0.0.9:5555".parse().unwrap();

        // 一层反代:客户端伪造了最左的 203.0.113.7,反代在其右追加真实对端 70.0.0.1。
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.7, 70.0.0.1".parse().unwrap());
        h.insert("x-real-ip", "9.9.9.9".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("70.0.0.1"),
            "一层反代应取反代自己观测到的最右项,而不是客户端可控的最左项"
        );

        // 两层(CDN → 自己的反代):倒数第 2 项 = CDN 写入的访客 IP。
        let mut h = axum::http::HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            "1.1.1.1, 198.51.100.23, 70.0.0.1".parse().unwrap(),
        );
        assert_eq!(
            extract_client_ip(&h, Some(peer), 2).as_deref(),
            Some("198.51.100.23")
        );

        // hops 比实际跳数大 → 越过最左端,回落而不是采信伪造值。
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        h.insert("x-real-ip", "198.51.100.4".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 5).as_deref(),
            Some("198.51.100.4")
        );

        // hops = 0(裸跑、前面没有反代)→ 一切转发头都不采信。
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        h.insert("cf-connecting-ip", "1.2.3.4".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 0).as_deref(),
            Some("10.0.0.9"),
            "没有反代时任何转发头都只可能是调用方自己写的"
        );

        // 无 XFF → X-Real-IP;二者皆无 → socket 对端;无对端 → None。
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-real-ip", "198.51.100.4".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("198.51.100.4")
        );
        let h = axum::http::HeaderMap::new();
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("10.0.0.9")
        );
        assert_eq!(extract_client_ip(&h, None, 1), None);
    }

    /// 回归:公网对端伪造的转发头**不得**被采信——否则任何人都能决定自己被记成哪个 IP,
    /// 污染用量记录、失败日志与面板展示。私网/回环对端(反代回源)才继续采信。
    #[test]
    fn extract_client_ip_ignores_forged_headers_from_public_peer() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("cf-connecting-ip", "1.2.3.4".parse().unwrap());
        h.insert("x-forwarded-for", "5.6.7.8".parse().unwrap());
        h.insert("x-real-ip", "9.9.9.9".parse().unwrap());

        // 公网对端:全部转发头忽略,只认 socket 对端。
        let public: std::net::SocketAddr = "203.0.113.7:5555".parse().unwrap();
        assert_eq!(
            extract_client_ip(&h, Some(public), 1).as_deref(),
            Some("203.0.113.7"),
            "公网对端的转发头必须忽略,否则可任意伪造日志 IP"
        );

        // 私网对端(反代/CDN 回源,含 docker 网关):采信反代观测到的那一跳。
        for p in ["127.0.0.1:1", "172.17.0.1:1", "10.0.0.5:1", "192.168.1.9:1"] {
            let peer: std::net::SocketAddr = p.parse().unwrap();
            assert_eq!(
                extract_client_ip(&h, Some(peer), 1).as_deref(),
                Some("5.6.7.8"),
                "私网对端({p})应采信 XFF 里反代观测到的那一项"
            );
        }
    }

    /// CDN 权威头只在**压根没有 XFF** 时才回退采用。
    ///
    /// 反代(Caddy/nginx)只管 `X-Forwarded-*`,会把客户端自带的 `CF-Connecting-IP` 原样透传,
    /// 所以它并不比反代亲眼观测到的那一跳更可信 —— 之前把它排在 XFF 之前,等于给了调用方一个
    /// 绕过 XFF 的伪造通道。
    #[test]
    fn cdn_headers_are_only_a_fallback_when_no_forwarded_for() {
        let peer: std::net::SocketAddr = "10.0.0.9:5555".parse().unwrap();

        // 有 XFF 时:以反代观测到的那一跳为准,CDN 头不得覆盖它。
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "1.2.3.4, 70.0.0.1".parse().unwrap());
        h.insert("cf-connecting-ip", "198.51.100.23".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("70.0.0.1")
        );

        // 没有 XFF 时才回退到 CF-Connecting-IP / True-Client-IP。
        let mut h = axum::http::HeaderMap::new();
        h.insert("cf-connecting-ip", "198.51.100.23".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("198.51.100.23")
        );
        let mut h = axum::http::HeaderMap::new();
        h.insert("true-client-ip", "203.0.113.99".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("203.0.113.99")
        );
    }

    /// 转发头的值解析不出 IP 就必须丢弃:审计字段之前只做 `trim()`,任意字符串(控制字符、
    /// 超长文本、伪造成日志格式的片段)都能原样落进用量记录与管理面。
    #[test]
    fn forwarded_headers_must_parse_as_ip_or_be_discarded() {
        let peer: std::net::SocketAddr = "10.0.0.9:5555".parse().unwrap();

        // XFF 该跳不是合法 IP → 丢弃,回落到下一优先级。
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "1.2.3.4, not-an-ip".parse().unwrap());
        h.insert("x-real-ip", "198.51.100.4".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("198.51.100.4")
        );

        // 垃圾值一路到底 → 回落 socket 对端,绝不把垃圾写进审计。
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "'; DROP TABLE".parse().unwrap());
        h.insert("cf-connecting-ip", "<script>x</script>".parse().unwrap());
        h.insert("x-real-ip", "……".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("10.0.0.9")
        );

        // 带端口 / 方括号的合法写法要剥成纯 IP。
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.7:44321".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("203.0.113.7")
        );
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-real-ip", "[2001:db8::1]".parse().unwrap());
        assert_eq!(
            extract_client_ip(&h, Some(peer), 1).as_deref(),
            Some("2001:db8::1")
        );
    }
    use axum::http::Request;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// 构造一条合法 AWS 事件流帧:一个 `:event-type` string header + JSON payload。
    /// (与 `eventstream::frame` 测试同法:大端 prelude + 两处 CRC。)
    fn event_frame(event_type: &str, payload: &[u8]) -> Vec<u8> {
        let name = ":event-type";
        let mut headers = Vec::new();
        headers.push(name.len() as u8);
        headers.extend_from_slice(name.as_bytes());
        headers.push(7u8); // string 类型
        headers.extend_from_slice(&(event_type.len() as u16).to_be_bytes());
        headers.extend_from_slice(event_type.as_bytes());

        let headers_len = headers.len() as u32;
        let total_len = 16 + headers_len + payload.len() as u32; // 12 prelude + headers + payload + 4 msg_crc

        let mut msg = Vec::new();
        msg.extend_from_slice(&total_len.to_be_bytes());
        msg.extend_from_slice(&headers_len.to_be_bytes());
        let prelude_crc = crc32(&msg[0..8]);
        msg.extend_from_slice(&prelude_crc.to_be_bytes());
        msg.extend_from_slice(&headers);
        msg.extend_from_slice(payload);
        let msg_crc = crc32(&msg);
        msg.extend_from_slice(&msg_crc.to_be_bytes());
        msg
    }

    /// 构造一条 exception 帧:`:message-type=exception` + `:exception-type=<kind>`
    /// 两个 string header(上游在 200 事件流里报错的真实形态)。
    fn exception_frame(kind: &str, payload: &[u8]) -> Vec<u8> {
        let mut headers = Vec::new();
        for (name, value) in [(":message-type", "exception"), (":exception-type", kind)] {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7u8); // string 类型
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }

        let headers_len = headers.len() as u32;
        let total_len = 16 + headers_len + payload.len() as u32;

        let mut msg = Vec::new();
        msg.extend_from_slice(&total_len.to_be_bytes());
        msg.extend_from_slice(&headers_len.to_be_bytes());
        let prelude_crc = crc32(&msg[0..8]);
        msg.extend_from_slice(&prelude_crc.to_be_bytes());
        msg.extend_from_slice(&headers);
        msg.extend_from_slice(payload);
        let msg_crc = crc32(&msg);
        msg.extend_from_slice(&msg_crc.to_be_bytes());
        msg
    }

    /// 发一次 `/v1/messages`,返回 (状态码, 响应体字节)。
    async fn post_messages(app: Router, body: &'static str) -> (StatusCode, Vec<u8>) {
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, bytes.to_vec())
    }

    fn cred() -> Credential {
        Credential {
            id: "a".into(),
            access_token: "AT".into(),
            refresh_token: "rt".into(),
            kiro_api_key: None,
            expires_at_unix: u64::MAX, // 永不过期 → 不触发刷新
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
        }
    }

    fn state(server_uri: &str, creds: Vec<Credential>) -> MessagesState {
        MessagesState {
            pool: Arc::new(Mutex::new(Pool::new(creds, LbMode::Priority))),
            client: reqwest::Client::new(),
            control_client: reqwest::Client::new(),
            cfg: Arc::new(Config::default()),
            runtime_cfg: crate::config::shared_runtime_config(&crate::config::Config::default()),
            endpoint_override: Some(format!("{server_uri}/generateAssistantResponse")),
            stats: StatsManager::load_from_dir(&std::env::temp_dir()),
            api_keys: crate::apikey::ApiKeyStore::load(std::env::temp_dir().join(format!(
                "kiro2api_anthropic_apikeys_{}.json",
                std::process::id()
            ))),
            balance: crate::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
            models_cache: crate::models_cache::ModelsCache::new(),
            builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            log_capture: None,
            refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(
                std::env::temp_dir()
                    .join(format!(
                        "kiro2api_refreshctx_src_protocol_anthropic_handler_rs_{}.json",
                        std::process::id()
                    ))
                    .to_string_lossy()
                    .to_string(),
            ),
        }
    }

    /// 指定 id 的凭据(多账号跨账号重试测试用)。
    fn cred_id(id: &str) -> Credential {
        let mut c = cred();
        c.id = id.into();
        c
    }

    // ==================== 跨账号重试(cross-account retry)====================

    /// 跨账号重试:第一个账号 502(瞬时),换第二个账号成功 → 整体请求 200。
    /// mock:头一次请求回 502(`up_to_n_times(1)` + 高优先级),之后回带 "pong" 的 200。
    /// Priority 轮转保证两次选到不同账号;断言最终 200 且文本为 "pong"。
    #[tokio::test]
    async fn cross_account_first_502_second_succeeds() {
        let server = MockServer::start().await;
        // 头一次:502(账号级 Transient 失败)。高优先级 + 只生效一次。
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(502))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        // 其后:200 + pong 事件流帧(默认优先级,兜底)。
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .with_priority(5)
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred_id("1"), cred_id("2")]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 第一个账号 502 → 跨账号换第二个 → 200 pong。
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["content"][0]["text"], "pong");
    }

    /// 跨账号重试用尽:两个账号全 502 → 最终 502(BAD_GATEWAY)。
    /// 断言两个账号都被尝试过(各记一次失败)。
    #[tokio::test]
    async fn cross_account_all_fail_yields_502() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(502))
            .mount(&server)
            .await;

        let st = state(&server.uri(), vec![cred_id("1"), cred_id("2")]);
        let pool = st.pool.clone();
        let app = messages_router(st);
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        // 两个账号都被试过:各累计一次失败(Transient 首次不冷却,故仍 active,但 failures==1)。
        let stats = pool.lock().await.stats(0);
        let tried: usize = stats.iter().filter(|s| s.failures >= 1).count();
        assert_eq!(
            tried, 2,
            "两个账号都应被尝试并各记一次失败;实际 stats={stats:?}"
        );
    }

    /// 请求级致命错误(未映射模型 → Convert/400)不触发跨账号重试:
    /// 即便池里有多个账号,也不换账号、直接 400,且没有任何账号被打上失败。
    #[tokio::test]
    async fn cross_account_convert_error_no_retry() {
        let server = MockServer::start().await;
        // 若真去打上游会 200;但 Convert 发生在选账号之后、调用之前,不应命中。
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let st = state(&server.uri(), vec![cred_id("1"), cred_id("2")]);
        let pool = st.pool.clone();
        let app = messages_router(st);
        // 未映射模型 → anthropic_to_kiro 返回 Convert → 400,不重试。
        let req_body = r#"{"model":"llama-3","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // Convert 不是账号级失败:不应把任何账号打成 failure。
        let stats = pool.lock().await.stats(0);
        assert!(
            stats.iter().all(|s| s.failures == 0),
            "Convert 致命错误不应记账号失败;实际 stats={stats:?}"
        );
    }

    /// 流式路径同样跨账号重试且在流开始前完成:第一个账号 502、第二个成功 →
    /// 200 + text/event-stream,不泄露首个失败账号的任何 SSE。
    #[tokio::test]
    async fn cross_account_retry_applies_to_stream() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(502))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .with_priority(5)
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred_id("1"), cred_id("2")]));
        let req_body =
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.contains("text/event-stream"), "content-type = {ct}");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);
        // 换到第二个账号后才建流:应出现成功的 pong 文本与完整收尾,无 502 泄露。
        assert!(
            s.contains("\"text\":\"pong\""),
            "SSE 应含成功账号的 pong;实际:\n{s}"
        );
        assert!(
            s.contains("event: message_stop"),
            "SSE 应正常收尾;实际:\n{s}"
        );
    }

    /// 全链路(parse→convert→call→decode→convert):mock Kiro 回 "pong" 事件流帧,
    /// 经 axum oneshot 打 `/v1/messages`,断言 200 + content[0] 文本为 "pong"。
    #[tokio::test]
    async fn full_pipeline_returns_pong() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "pong");
    }

    /// 上游错误体必须经**生产链路**落进失败日志,而不是只在单测里手工注入。
    ///
    /// 修复前 `record_classified_failure` 收到的 response_body 恒为空串:provider 明明读了
    /// 响应体、分类完就丢。于是面板上"失败/限流"详情列线上永远是 `—`,运维点开什么也看不到,
    /// 真正的上游原因(权限不足?模型不可用?令牌失效?)只存在于进程日志里。
    #[tokio::test]
    async fn upstream_error_body_reaches_the_failure_log() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(403).set_body_string(
                r#"{"__type":"AccessDeniedException","message":"no entitlement"}"#,
            ))
            .mount(&server)
            .await;

        // 专属 stats 目录,避免与共享 temp_dir 的其它测试串扰。
        let dir = std::env::temp_dir().join(format!("kiro2api_errbody_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let stats = StatsManager::load_from_dir(&dir);
        let mut st = state(&server.uri(), vec![cred()]);
        st.stats = stats.clone();

        let req: MessagesRequest = serde_json::from_str(
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .unwrap();
        let _ = relay_core(&st, req, 1000).await; // 必然失败,这里只关心落库内容

        let page = stats
            .failure_log
            .records_for_credential(credential_id_num(&cred().id), 1, 10)
            .await;
        assert_eq!(page.total, 1, "403 应落一条失败日志");
        assert!(
            page.items[0]
                .response_body
                .contains("AccessDeniedException"),
            "失败日志必须带上上游原始响应体,实际: {:?}",
            page.items[0].response_body
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 绑定必须**真的**约束选号,而不是"白名单送到了扩展里"就算修好。
    ///
    /// 上一轮的回归测试只断言扩展在场,选号层从没读过它,于是绑定形同虚设却带着"已修复"
    /// 的标签发了版。这里改为断言可观测行为:绑定指向池里没有的账号时必须回 503,
    /// 绝不能悄悄回落到一个未授权但健康的账号(那才是这条 finding 的真实危害)。
    #[tokio::test]
    async fn binding_to_absent_account_yields_503_instead_of_falling_back() {
        let server = MockServer::start().await;
        // 上游一律成功:一旦选号漏给了未授权账号,这里就会回 200 —— 断言 503 才有意义。
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;
        let mut healthy = cred();
        healthy.id = "3".into();
        let st = state(&server.uri(), vec![healthy]);

        // 未绑定:同一个池、同一发请求 → 200(证明池本身是健康可用的)。
        let app = messages_router(st.clone());
        let body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let ok = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK, "未绑定时应正常中转");

        // 绑定到池里不存在的 4242:必须 503,而不是回落到健康的 3 号。
        let app = messages_router(st).layer(axum::middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(BoundCredentialIds(vec![4242]));
                next.run(req).await
            },
        ));
        let denied = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            denied.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "绑定未命中任何账号时必须选不出账号(503),绝不能漏给未授权账号"
        );
    }

    /// `count_tokens` 的畸形请求体必须回 Anthropic 形状的 400,而不是 axum 默认的纯文本 422。
    ///
    /// 上一轮把四个中转端点都改成了 `Result<Json<..>, JsonRejection>`,唯独漏了这个端点 ——
    /// SDK 用 `response.json()` 读错误时,纯文本体只会抛解析异常,真正的原因被吞掉。
    #[tokio::test]
    async fn count_tokens_malformed_body_returns_anthropic_error_not_plain_422() {
        let server = MockServer::start().await;
        let app = messages_router(state(&server.uri(), vec![cred()]));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages/count_tokens")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":123,"messages":"not-an-array"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "畸形体应回 400,而不是 axum 的 422"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).expect("错误体必须是 JSON,否则 SDK 解析不了");
        assert_eq!(v["type"], "error");
        assert_eq!(v["error"]["type"], "invalid_request_error");
    }

    /// 空池 → 503。
    #[tokio::test]
    async fn empty_pool_yields_503() {
        let server = MockServer::start().await;
        let app = messages_router(state(&server.uri(), vec![]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// 未映射模型 → 400(契约"未映射→400"在 HTTP 层兑现)。
    #[tokio::test]
    async fn unmapped_model_yields_400() {
        let server = MockServer::start().await;
        let app = messages_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"llama-3","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 上游 5xx → 502。
    #[tokio::test]
    async fn upstream_error_yields_502() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let app = messages_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    /// 数据面 401/403(裸状态码,无失效信号 → AuthAmbiguous):handler 会尝试 force_refresh +
    /// 重试一次。测试环境下 force_refresh 打真实 AWS 刷新端点必失败(网络不可达),故重试不成立、
    /// 回落原 AuthAmbiguous 失败 → 502。**关键**:裸 403 归类 AuthAmbiguous,单次上报只记 1 strike
    /// (未达 AUTH_AMBIGUOUS_STRIKES=2),**绝不**因裸状态码就永久禁用或冷却,账号仍可用
    /// (与旧的"一次 Auth 即永久禁用"契约相区分;真机上 refresh_token 有效时会重试成功)。
    #[tokio::test]
    async fn data_plane_auth_failure_disables_after_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let st = state(&server.uri(), vec![cred()]);
        let pool = st.pool.clone();
        let app = messages_router(st);
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        // 重试后仍失败 → report_failure(AuthAmbiguous) 记 1 strike(<2),不禁用不冷却,账号仍可用。
        assert_eq!(pool.lock().await.active_count(0), 1);
    }

    /// 流式(`stream:true`):按序产出 message_start→content_block_start→若干 text_delta→
    /// content_block_stop→message_delta→message_stop。
    #[tokio::test]
    async fn streaming_emits_ordered_sse() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"po"}"#);
        body.extend(event_frame(
            "assistantResponseEvent",
            br#"{"content":"ng"}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let req_body =
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.contains("text/event-stream"), "content-type = {ct}");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);
        for needle in [
            "event: message_start",
            "event: content_block_start",
            "\"text\":\"po\"",
            "\"text\":\"ng\"",
            "event: content_block_stop",
            "event: message_delta",
            "\"stop_reason\":\"end_turn\"",
            "event: message_stop",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        // 顺序:message_start 在 message_stop 之前。
        assert!(s.find("event: message_start") < s.find("event: message_stop"));
    }

    /// 流式纯工具轮(`stream:true` + `toolUseEvent` 帧序):按序产出
    /// message_start→(tool_use 的)content_block_start→若干 input_json_delta→
    /// content_block_stop→message_delta(stop_reason=tool_use)→message_stop。
    /// 并断言不先发空文本块(首个 content_block_start 即 tool_use)。
    #[tokio::test]
    async fn streaming_tool_use_emits_input_json_delta() {
        let server = MockServer::start().await;
        // 探针实测的 6 帧 toolUseEvent 生命周期(open→input×4→stop),toolUseId="tu1"。
        let mut body = event_frame(
            "toolUseEvent",
            br#"{"name":"get_weather","toolUseId":"tu1"}"#,
        );
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"{\"ci","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"ty\": \"Paris","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"\"}","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"name":"get_weather","stop":true,"toolUseId":"tu1"}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"weather?"}],"tools":[{"name":"get_weather","input_schema":{}}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.contains("text/event-stream"), "content-type = {ct}");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);

        for needle in [
            "event: content_block_start",
            "\"type\":\"tool_use\"",
            "\"name\":\"get_weather\"",
            "\"id\":\"tu1\"",
            "event: content_block_delta",
            "\"input_json_delta\"",
            "event: content_block_stop",
            "event: message_delta",
            "\"stop_reason\":\"tool_use\"",
            "event: message_stop",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        // partial_json 片段应跨 delta 出现(累计含 city 的字面片段)。
        assert!(s.contains("partial_json"), "缺 partial_json;实际:\n{s}");
        assert!(
            s.contains("ci") && s.contains("Paris"),
            "input 片段缺失;实际:\n{s}"
        );

        // 不先发空文本块:首个 content_block_start 即 tool_use,不应出现 "type":"text" 的块。
        let tool_pos = s.find("\"type\":\"tool_use\"").expect("应有 tool_use 块");
        if let Some(text_pos) = s.find("\"type\":\"text\"") {
            assert!(
                tool_pos < text_pos,
                "tool_use 块应先于任何 text 块;实际:\n{s}"
            );
        }

        // 顺序:tool_use 的 content_block_start 在 content_block_stop 之前、后者在 message_stop 之前。
        let start_pos = s.find("event: content_block_start").unwrap();
        let stop_pos = s.find("event: content_block_stop").unwrap();
        let msg_stop_pos = s.find("event: message_stop").unwrap();
        assert!(start_pos < stop_pos && stop_pos < msg_stop_pos);
    }

    // ==================== 上游 200 事件流里的 exception ====================

    /// 状态码 → Anthropic 错误类型的对照(exception_status 只会给出 400/403/429/502)。
    #[test]
    fn anthropic_error_type_maps_status_codes() {
        assert_eq!(anthropic_error_type(429), "rate_limit_error");
        assert_eq!(anthropic_error_type(403), "permission_error");
        assert_eq!(anthropic_error_type(400), "invalid_request_error");
        assert_eq!(anthropic_error_type(502), "api_error");
        assert_eq!(anthropic_error_type(500), "api_error");
    }

    /// 非流式:上游回 200 但事件流里夹 ThrottlingException →
    /// 必须回 429 + Anthropic 错误体,而不是 200 + 空内容 + end_turn。
    #[tokio::test]
    async fn non_streaming_upstream_exception_yields_mapped_status() {
        let server = MockServer::start().await;
        let body = exception_frame("ThrottlingException", br#"{"message":"slow down"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(
            app,
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["error"]["type"], "rate_limit_error");
        let msg = v["error"]["message"].as_str().expect("message 应为字符串");
        assert!(
            msg.contains("ThrottlingException") && msg.contains("slow down"),
            "错误消息应带上游类型与说明;实际:{msg}"
        );
    }

    /// 非流式:鉴权类 exception → 403 + permission_error。
    #[tokio::test]
    async fn non_streaming_access_denied_exception_yields_403() {
        let server = MockServer::start().await;
        let body = exception_frame("AccessDeniedException", br#"{"message":"nope"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(
            app,
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "permission_error");
    }

    /// 流式:上游先发文本再发 exception → 必须发 `event: error` 并就此终止,
    /// **不得**再发 message_delta/message_stop 把失败伪装成正常完成。
    #[tokio::test]
    async fn streaming_upstream_exception_emits_error_event() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"po"}"#);
        body.extend(exception_frame(
            "ThrottlingException",
            br#"{"message":"slow down"}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(
            app,
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let s = String::from_utf8_lossy(&bytes);
        for needle in [
            "event: error",
            "\"type\":\"rate_limit_error\"",
            "ThrottlingException",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        assert!(
            !s.contains("event: message_stop"),
            "出错的流不应发 message_stop;实际:\n{s}"
        );
        assert!(
            !s.contains("event: message_delta"),
            "出错的流不应发 message_delta;实际:\n{s}"
        );
        // 已打开的文本块仍要收尾,保持内容块结构完整。
        assert!(
            s.contains("event: content_block_stop"),
            "已打开的块应先收尾;实际:\n{s}"
        );
    }

    // ==================== 流式截断信号 ====================

    /// 流式:命中 max_tokens(ContentLengthExceededException)→ stop_reason 必须是
    /// `max_tokens`,而不是按"有无工具调用"推出来的 end_turn。截断不是错误,仍正常收尾。
    #[tokio::test]
    async fn streaming_max_tokens_truncation_sets_stop_reason() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"po"}"#);
        body.extend(exception_frame("ContentLengthExceededException", b"{}"));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(
            app,
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            s.contains("\"stop_reason\":\"max_tokens\""),
            "截断应报 max_tokens;实际:\n{s}"
        );
        assert!(
            s.contains("event: message_stop"),
            "截断是正常收尾,应有 message_stop;实际:\n{s}"
        );
        assert!(
            !s.contains("event: error"),
            "截断不是错误,不应发 error 事件;实际:\n{s}"
        );
    }

    /// 流式:上下文窗口耗尽(contextUsagePercentage=100)→ stop_reason
    /// `model_context_window_exceeded`。
    #[tokio::test]
    async fn streaming_context_window_truncation_sets_stop_reason() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"po"}"#);
        body.extend(event_frame(
            "contextUsageEvent",
            br#"{"contextUsagePercentage":100}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(
            app,
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            s.contains("\"stop_reason\":\"model_context_window_exceeded\""),
            "上下文耗尽应报 model_context_window_exceeded;实际:\n{s}"
        );
    }

    // ==================== 内容块开闭状态机 ====================

    /// 工具块 stop 之后上游又发来同一块的 input/stop 帧:不得产出已关闭块的 delta,
    /// 也不得重复发 content_block_stop(严格校验的官方 SDK 会因此解析失败/参数错乱)。
    #[tokio::test]
    async fn streaming_ignores_frames_after_tool_block_closed() {
        let server = MockServer::start().await;
        let mut body = event_frame("toolUseEvent", br#"{"name":"f","toolUseId":"tu1"}"#);
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"{}","name":"f","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"name":"f","stop":true,"toolUseId":"tu1"}"#,
        ));
        // 收尾之后迟到的 input / 重复 stop:都应被丢弃。
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"LATEFRAGMENT","name":"f","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"name":"f","stop":true,"toolUseId":"tu1"}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(
            app,
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            !s.contains("LATEFRAGMENT"),
            "已关闭的块不应再产出 delta;实际:\n{s}"
        );
        assert_eq!(
            s.matches("event: content_block_stop").count(),
            1,
            "content_block_stop 应恰好一次;实际:\n{s}"
        );
        assert!(
            s.contains("\"stop_reason\":\"tool_use\""),
            "仍应正常收尾于 tool_use;实际:\n{s}"
        );
    }

    // ==================== usage 用上游真实计量 ====================

    /// 流式:meteringEvent 带真实 token 计量时,message_delta 的 usage 必须用它,
    /// 而不是"输出字符数 ÷ 4"的估算(2 个字符本会估成 0)。
    #[tokio::test]
    async fn streaming_message_delta_uses_metering_output_tokens() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"po"}"#);
        body.extend(event_frame(
            "meteringEvent",
            br#"{"usage":1.5,"input_tokens":1234,"output_tokens":777}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(
            app,
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            s.contains("\"output_tokens\":777"),
            "message_delta 应用 meteringEvent 的真实 output_tokens;实际:\n{s}"
        );
    }

    /// 非流式:meteringEvent 的真实 token 计量应原样出现在响应 usage 里
    /// (input 不再恒 0、output 不再是字符估算)。
    #[tokio::test]
    async fn non_streaming_usage_uses_metering_tokens() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        body.extend(event_frame(
            "meteringEvent",
            br#"{"usage":1.5,"input_tokens":1234,"output_tokens":777}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(
            app,
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["usage"]["input_tokens"], 1234);
        assert_eq!(v["usage"]["output_tokens"], 777);
    }

    // ==================== 请求体宽容度与错误体形状 ====================

    /// 带 thinking / document 块的请求不应整体失败(旧行为:422 纯文本),
    /// 未知块被跳过、其余内容照常转发。
    #[tokio::test]
    async fn unknown_content_blocks_do_not_fail_request() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(
            app,
            r#"{"model":"sonnet","messages":[{"role":"user","content":[
                {"type":"thinking","thinking":"hmm","signature":"s"},
                {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"JVBER"}},
                {"type":"text","text":"hi"}
            ]}]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["content"][0]["text"], "pong");
    }

    /// 请求体解析失败 → 400 + 标准 Anthropic 错误体(而非 axum 默认的纯文本 422)。
    #[tokio::test]
    async fn malformed_body_yields_anthropic_error_shape() {
        let server = MockServer::start().await;
        let app = messages_router(state(&server.uri(), vec![cred()]));
        let (status, bytes) = post_messages(app, r#"{"model":"sonnet","messages":"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).expect("错误体应为 JSON,而不是纯文本");
        assert_eq!(v["type"], "error");
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert!(
            v["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty()),
            "错误体应带说明;实际:{v}"
        );
    }

    /// `POST /v1/messages/count_tokens`:纯估算,不打网络(空池也应 200)。
    #[tokio::test]
    async fn count_tokens_returns_positive_estimate() {
        let server = MockServer::start().await;
        let app = messages_router(state(&server.uri(), vec![]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hello world"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages/count_tokens")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let n = v["input_tokens"].as_u64().expect("input_tokens 应为数字");
        assert!(n > 0, "input_tokens 应 > 0,实际 {n}");
    }

    /// 更长输入应得到更大的估算值(单调性,而非精确匹配官方 tokenizer)。
    #[tokio::test]
    async fn count_tokens_scales_with_input_length() {
        let server = MockServer::start().await;
        let app = messages_router(state(&server.uri(), vec![]));

        let short_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages/count_tokens")
                    .header("content-type", "application/json")
                    .body(Body::from(short_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let short_n = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["input_tokens"]
            .as_u64()
            .unwrap();

        let long_text = "word ".repeat(200);
        let long_body = serde_json::json!({
            "model": "sonnet",
            "messages": [{"role": "user", "content": long_text}],
        })
        .to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages/count_tokens")
                    .header("content-type", "application/json")
                    .body(Body::from(long_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let long_n = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["input_tokens"]
            .as_u64()
            .unwrap();

        assert!(long_n > short_n, "长输入 {long_n} 应大于短输入 {short_n}");
    }

    /// `GET /claude/v1/models`:固定模型列表,形状照 Anthropic 公开规范。
    #[tokio::test]
    async fn claude_models_lists_fixed_models() {
        let server = MockServer::start().await;
        let app = messages_router(state(&server.uri(), vec![]));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/claude/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let data = v["data"].as_array().expect("data 应为数组");
        assert!(!data.is_empty(), "data 不应为空");
        for entry in data {
            assert_eq!(entry["type"], "model");
            assert!(entry["id"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(
                entry["display_name"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            );
        }
        assert_eq!(v["has_more"], false);
    }

    /// store-key 归属:`relay_core_attributed` 以非 0 的 api_key_id 记录用量,
    /// 该 key 的 summary 应计入本次请求(全链路 mock Kiro 回 "pong")。
    #[tokio::test]
    async fn relay_core_attributes_usage_to_api_key_id() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        // 专属 stats 目录,避免与共享 temp_dir 的其它测试记录串扰。
        let dir = std::env::temp_dir().join(format!("kiro2api_attr_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let stats = StatsManager::load_from_dir(&dir);
        let st = MessagesState {
            pool: Arc::new(Mutex::new(Pool::new(vec![cred()], LbMode::Priority))),
            client: reqwest::Client::new(),
            control_client: reqwest::Client::new(),
            cfg: Arc::new(Config::default()),
            runtime_cfg: crate::config::shared_runtime_config(&crate::config::Config::default()),
            endpoint_override: Some(format!("{}/generateAssistantResponse", server.uri())),
            stats: stats.clone(),
            api_keys: crate::apikey::ApiKeyStore::load(dir.join("api_keys.json")),
            balance: crate::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
            models_cache: crate::models_cache::ModelsCache::new(),
            builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            log_capture: None,
            refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(
                std::env::temp_dir()
                    .join(format!(
                        "kiro2api_refreshctx_src_protocol_anthropic_handler_rs_{}.json",
                        std::process::id()
                    ))
                    .to_string_lossy()
                    .to_string(),
            ),
        };

        let req: MessagesRequest = serde_json::from_str(
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .unwrap();
        // 归属到 key id 42。
        let out = relay_core_attributed(&st, req, 42, None, None, 1000)
            .await
            .unwrap();
        assert!(
            matches!(&out.content[0], crate::protocol::anthropic::types::OutBlock::Text { text } if text == "pong")
        );

        // key 42 的 summary 计入一条;id 7(未用过)为空。
        let s42 = stats.get_summary_by_api_key(42).await;
        assert_eq!(s42.total_requests, 1);
        let s7 = stats.get_summary_by_api_key(7).await;
        assert_eq!(s7.total_requests, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `messages` handler 从请求扩展读取 `ApiKeyId` 并归属:经真实中间件塞入
    /// `ApiKeyId(Some(9))`,一次 `/v1/messages` 后 key 9 的 summary 应计入。
    #[tokio::test]
    async fn messages_reads_api_key_id_from_extension() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!("kiro2api_attr_ext_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let stats = StatsManager::load_from_dir(&dir);
        let st = MessagesState {
            pool: Arc::new(Mutex::new(Pool::new(vec![cred()], LbMode::Priority))),
            client: reqwest::Client::new(),
            control_client: reqwest::Client::new(),
            cfg: Arc::new(Config::default()),
            runtime_cfg: crate::config::shared_runtime_config(&crate::config::Config::default()),
            endpoint_override: Some(format!("{}/generateAssistantResponse", server.uri())),
            stats: stats.clone(),
            api_keys: crate::apikey::ApiKeyStore::load(dir.join("api_keys.json")),
            balance: crate::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
            models_cache: crate::models_cache::ModelsCache::new(),
            builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            log_capture: None,
            refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(
                std::env::temp_dir()
                    .join(format!(
                        "kiro2api_refreshctx_src_protocol_anthropic_handler_rs_{}.json",
                        std::process::id()
                    ))
                    .to_string_lossy()
                    .to_string(),
            ),
        };

        // 用一层中间件把 ApiKeyId(Some(9)) 塞进扩展,模拟鉴权闸命中 store key。
        let app = messages_router(st).layer(axum::middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(ApiKeyId(Some(9)));
                next.run(req).await
            },
        ));

        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let s9 = stats.get_summary_by_api_key(9).await;
        assert_eq!(s9.total_requests, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ==================== #9 数据面 region 取自 profileArn ====================

    /// profileArn 里的 region 优先于 `cred.region`:非 us-east-1 账号(ARN 带真实 region)
    /// 应据 ARN 解析出的 region,而非默认的 cred.region。
    #[test]
    fn effective_region_prefers_profile_arn() {
        let mut c = cred();
        c.region = "us-east-1".into(); // cred 默认 us-east-1
        c.profile_arn = Some("arn:aws:codewhisperer:eu-west-1:123456789012:profile/ABC".into());
        assert_eq!(effective_region(&c), "eu-west-1");
    }

    /// 无 profileArn → 回落 cred.region。
    #[test]
    fn effective_region_falls_back_to_cred_region() {
        let mut c = cred();
        c.region = "ap-northeast-1".into();
        c.profile_arn = None;
        assert_eq!(effective_region(&c), "ap-northeast-1");
    }

    /// profileArn 不含合法 region 段 → 回落 cred.region;cred.region 空 → 回落 us-east-1。
    #[test]
    fn effective_region_final_fallback_is_us_east_1() {
        let mut c = cred();
        c.region = "".into();
        c.profile_arn = Some("not-an-arn".into());
        assert_eq!(effective_region(&c), "us-east-1");
    }

    // ==================== #19 count_tokens 计入 tools/tool_result/图片 ====================

    async fn count_for(body: &str) -> u64 {
        let req: MessagesRequest = serde_json::from_str(body).unwrap();
        let resp = count_tokens(Ok(Json(req))).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        v["input_tokens"].as_u64().unwrap()
    }

    /// 带 tools 定义的请求估算应显著大于同样文本但无 tools 的请求(工具 schema 计入)。
    #[tokio::test]
    async fn count_tokens_includes_tools() {
        let base = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let with_tools = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],
            "tools":[{"name":"get_weather","description":"Get the current weather for a city",
            "input_schema":{"type":"object","properties":{"city":{"type":"string"},"units":{"type":"string"}},"required":["city"]}}]}"#;
        assert!(
            count_for(with_tools).await > count_for(base).await,
            "tools 应抬高估算"
        );
    }

    /// tool_result 内容(块数组)应计入估算,而非被 .text() 丢弃。
    #[tokio::test]
    async fn count_tokens_includes_tool_result() {
        let base = r#"{"model":"sonnet","messages":[{"role":"user","content":[{"type":"text","text":"x"}]}]}"#;
        let with_tr = r#"{"model":"sonnet","messages":[{"role":"user","content":[
            {"type":"text","text":"x"},
            {"type":"tool_result","tool_use_id":"tu1","content":"the weather in Paris is sunny and 24 degrees celsius today"}
        ]}]}"#;
        assert!(
            count_for(with_tr).await > count_for(base).await,
            "tool_result 应计入估算"
        );
    }

    /// 图片块应按固定每图 token 抬高估算(而非被丢弃)。
    #[tokio::test]
    async fn count_tokens_includes_images() {
        let base = r#"{"model":"sonnet","messages":[{"role":"user","content":[{"type":"text","text":"x"}]}]}"#;
        let with_img = r#"{"model":"sonnet","messages":[{"role":"user","content":[
            {"type":"text","text":"x"},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"aaaa"}}
        ]}]}"#;
        let n = count_for(with_img).await;
        assert!(n > count_for(base).await, "图片应抬高估算");
        assert!(n >= (IMAGE_TOKEN_ESTIMATE as u64), "每图固定估算应生效");
    }

    /// tool_use 块的 input JSON 应计入估算。
    #[tokio::test]
    async fn count_tokens_includes_tool_use_input() {
        let base = r#"{"model":"sonnet","messages":[{"role":"assistant","content":[{"type":"text","text":"x"}]}]}"#;
        let with_tu = r#"{"model":"sonnet","messages":[{"role":"assistant","content":[
            {"type":"text","text":"x"},
            {"type":"tool_use","id":"tu1","name":"search","input":{"query":"a fairly long search query string here"}}
        ]}]}"#;
        assert!(
            count_for(with_tu).await > count_for(base).await,
            "tool_use input 应计入估算"
        );
    }

    /// `POST /claude/v1/messages`:与裸 `/v1/messages` 行为一致(同一 handler,前缀变体)。
    #[tokio::test]
    async fn claude_prefixed_messages_returns_pong() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = messages_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/claude/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["content"][0]["text"], "pong");
    }
}
