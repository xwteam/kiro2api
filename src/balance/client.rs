//! 上游 `getUsageLimits` 客户端。
//!
//! 请求形态(照观测的 wire 事实):
//!   `GET https://q.{region}.amazonaws.com/getUsageLimits`
//!   `?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST[&profileArn=<pct-encoded>]`
//!   头:`Authorization: Bearer <access_token>`、`amz-sdk-invocation-id`、
//!       `amz-sdk-request: attempt=1; max=1`、`x-amz-user-agent`、`user-agent`。
//! 伪装身份(machine_id/kiro_version 等)复用与数据面一致的口径。
//! URL 组装、header 构造、响应解析、错误分类均为本项目自写。

use crate::balance::model::UsageLimitsResponse;
use crate::kiro::credential::Credential;
use crate::kiro::provider::Impersonation;

/// 上游错误体里回带的说明文字截断上限(字符数),避免超长响应刷爆日志/响应。
const UPSTREAM_MSG_MAX: usize = 300;

/// 查询余额时的失败原因。
#[derive(Debug)]
pub enum BalanceError {
    /// 网络/传输错误。
    Http,
    /// 上游非 2xx:携带状态码 + 从响应体解析出的说明文字(供管理员看清真因,
    /// 如 AWS "Your User ID ... temporarily is suspended")。
    Upstream { status: u16, message: String },
    /// 响应体解析失败。
    Decode,
}

impl std::fmt::Display for BalanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BalanceError::Http => write!(f, "balance http error"),
            BalanceError::Upstream { status, message } => {
                write!(f, "balance upstream HTTP {status}: {message}")
            }
            BalanceError::Decode => write!(f, "balance decode error"),
        }
    }
}
impl std::error::Error for BalanceError {}

impl BalanceError {
    /// 是否为上游鉴权失败(401/403)——供"强制刷新令牌 + 重试一次"判定。
    /// 只看状态码,与错误体文字无关。
    pub fn is_auth(&self) -> bool {
        matches!(
            self,
            BalanceError::Upstream {
                status: 401 | 403,
                ..
            }
        )
    }
}

/// 从上游 amz-json 错误体里抽 `message` 字段;拿不到则回落到原始文本(去空白)。
/// 结果按 [`UPSTREAM_MSG_MAX`] 字符截断,绝不含任何令牌(错误体本身不含令牌)。
fn extract_amz_message(body: &str) -> String {
    let raw = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("Message"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string());
    truncate_chars(&raw, UPSTREAM_MSG_MAX)
}

/// 按字符(非字节)截断,避免切断多字节 UTF-8;超长时以 `…` 收尾。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// 最小百分号编码:对非 `unreserved`(ALPHA/DIGIT/`-._~`)字节一律 `%XX`。
/// 足以安全编码 ARN(含 `:`、`/`)进 query value。
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if unreserved {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// 组装 getUsageLimits 的完整 URL(`base` 形如 `https://q.us-east-1.amazonaws.com`)。
pub fn build_url(base: &str, profile_arn: Option<&str>) -> String {
    let mut url = format!(
        "{}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST",
        base.trim_end_matches('/')
    );
    if let Some(arn) = profile_arn
        && !arn.is_empty()
    {
        url.push_str(&format!("&profileArn={}", pct_encode(arn)));
    }
    url
}

/// 生产用 base:`https://q.{region}.amazonaws.com`。
fn region_base(region: &str) -> String {
    format!("https://q.{region}.amazonaws.com")
}

/// 由凭据 + 配置构造伪装身份(machine_id 优先显式,否则由 refresh_token 派生)。
fn impersonation_for(cred: &Credential, cfg: &crate::config::Config) -> Impersonation {
    // 收口到唯一入口。此前这里自写了一份**两级** resolve,不认配置级 machineId,
    // 于是配置里设了 machineId 时,同一个账号在数据面报一个机器、在这里报另一个。
    Impersonation::for_credential(cred, cfg)
}

/// 向 `base` 发一次 getUsageLimits 并解析。测试可注入 mock base;生产用 [`fetch_usage_limits`]。
pub async fn fetch_at(
    client: &reqwest::Client,
    base: &str,
    cred: &Credential,
    imp: &Impersonation,
) -> Result<UsageLimitsResponse, BalanceError> {
    let url = build_url(base, cred.profile_arn.as_deref());
    let inv = new_invocation_id();
    let user_agent = format!(
        "aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}",
        imp.system_version, imp.node_version, imp.kiro_version, imp.machine_id
    );
    let amz_user_agent = format!(
        "aws-sdk-js/1.0.0 KiroIDE-{}-{}",
        imp.kiro_version, imp.machine_id
    );
    let mut req = client
        .get(&url)
        .header("x-amz-user-agent", amz_user_agent)
        .header(reqwest::header::USER_AGENT, user_agent);
    if let Some(host) = crate::kiro::provider::host_of(&url) {
        req = req.header("host", host);
    }
    // bearer 必须走 `cred.bearer()`:ksk 凭据的令牌在 `kiro_api_key` 里,直接读
    // `access_token` 会发出一个**空 Bearer**,额度查询必然 401/403。
    // 同理 tokentype 必须跟着走,否则上游按 OAuth 令牌解析这枚 ksk。
    req = req
        .header("amz-sdk-invocation-id", inv)
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("authorization", format!("Bearer {}", cred.bearer()));
    if cred.is_api_key() {
        req = req.header("tokentype", "API_KEY");
    }
    // 控制面同样每请求一条新连接:与数据面自相矛盾的连接行为本身就是可观测差异。
    let resp = req
        .header("connection", "close")
        .send()
        .await
        .map_err(|_| BalanceError::Http)?;
    let status = resp.status();
    if !status.is_success() {
        // 读错误体(有界)并解析 amz-json 的 message,带进错误里供管理员看清真因。
        let body = resp.text().await.unwrap_or_default();
        let message = extract_amz_message(&body);
        return Err(BalanceError::Upstream {
            status: status.as_u16(),
            message,
        });
    }
    resp.json::<UsageLimitsResponse>()
        .await
        .map_err(|_| BalanceError::Decode)
}

/// 生产入口:按凭据 region 组 base + 伪装头,查询该账号剩余额度。
pub async fn fetch_usage_limits(
    client: &reqwest::Client,
    cfg: &crate::config::Config,
    cred: &Credential,
) -> Result<UsageLimitsResponse, BalanceError> {
    let base = region_base(&cred.region);
    let imp = impersonation_for(cred, cfg);
    fetch_at(client, &base, cred, &imp).await
}

/// 集中保鲜版:调 getUsageLimits 前先经 [`crate::kiro::ensure_fresh`] 确保 access_token
/// 新鲜(即将过期则刷新并写回活池,令牌与 relay/models 共享),再用刷新后的凭据实拉。
///
/// 令牌过期是 balance/models 独立 403 的根因:此前它们直接用池内(可能已过期)token 调上游。
/// 保鲜失败(池内已无该 id / 刷新失败)映射为 [`BalanceError::Http`]。不记录任何令牌明文。
pub async fn fetch_usage_limits_fresh(
    client: &reqwest::Client,
    cfg: &crate::config::Config,
    pool: &std::sync::Arc<tokio::sync::Mutex<crate::kiro::pool::Pool>>,
    cred_id: &str,
    now_unix: u64,
    ctx: Option<&crate::kiro::ensure_fresh::RefreshCtx>,
) -> Result<UsageLimitsResponse, BalanceError> {
    // 出口按该账号自己的代理取:它的数据面走哪个出口,余额/模型清单就得走哪个。
    // 只有配了代理才覆盖调用方传入的客户端,没配时行为与此前完全一致。
    let per_cred = {
        let g = pool.lock().await;
        g.snapshot_credentials()
            .into_iter()
            .find(|c| c.id == cred_id)
    }
    .filter(|c| {
        c.effective_proxy(crate::kiro::provider::config_proxy_url().as_deref())
            .is_some()
    })
    .map(|c| crate::http::unary_for(&c));
    let client = per_cred.as_ref().unwrap_or(client);
    let cred = crate::kiro::ensure_fresh::ensure_fresh(pool, cred_id, client, now_unix, 300, ctx)
        .await
        .map_err(|_| BalanceError::Http)?;
    match fetch_usage_limits(client, cfg, &cred).await {
        // 401/403:该 access_token 可能已被服务端失效(哪怕尚未过期)。强制刷新令牌后重试一次。
        Err(e) if e.is_auth() => {
            // 传本次失败请求实际用的 access_token 作双检基线:池内若已换过新的,
            // 说明别人刚刷完,直接复用而不再轮换一次(轮换会作废他人在用的令牌)。
            let fresh = match crate::kiro::ensure_fresh::force_refresh(
                pool,
                cred_id,
                client,
                now_unix,
                &cred.access_token,
                ctx,
            )
            .await
            {
                Ok(c) => c,
                // 强制刷新失败(refresh_token 也失效/网络):回传原 401/403,不再重试。
                Err(_) => return Err(e),
            };
            fetch_usage_limits(client, cfg, &fresh).await
        }
        other => other,
    }
}

/// 随机请求关联 id(16 字节 CSPRNG 十六进制),供 amz-sdk-invocation-id 使用。
fn new_invocation_id() -> String {
    let mut raw = [0u8; 16];
    getrandom::getrandom(&mut raw).expect("CSPRNG");
    hex::encode(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cred() -> Credential {
        Credential {
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            id: "7".into(),
            access_token: "AT".into(),
            refresh_token: "rt".into(),
            kiro_api_key: None,
            expires_at_unix: u64::MAX,
            region: "us-east-1".into(),
            auth: AuthMethod::Social,
            client_id: None,
            client_secret: None,
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:111:profile/ABC".into()),
            machine_id: Some("mid".into()),
            email: None,
            nickname: None,
            weight: 1,
            label: None,
            disabled: false,
            status_reason: None,
        }
    }
    fn imp() -> Impersonation {
        Impersonation {
            machine_id: "mid".into(),
            kiro_version: "0.0.1".into(),
            agent_mode: "vibe".into(),
            system_version: "win32#10.0.22631".into(),
            node_version: "22.22.0".into(),
        }
    }

    #[test]
    fn url_encodes_profile_arn() {
        let u = build_url("https://q.us-east-1.amazonaws.com", Some("arn:aws:x/y"));
        assert!(u.starts_with("https://q.us-east-1.amazonaws.com/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST"));
        assert!(u.contains("&profileArn=arn%3Aaws%3Ax%2Fy"));
    }

    #[test]
    fn url_omits_profile_arn_when_absent() {
        let u = build_url("https://q.us-east-1.amazonaws.com/", None);
        assert!(!u.contains("profileArn"));
        assert!(u.ends_with("resourceType=AGENTIC_REQUEST"));
    }

    #[tokio::test]
    async fn fetch_at_parses_response_and_sends_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getUsageLimits"))
            .and(query_param("origin", "AI_EDITOR"))
            .and(header("authorization", "Bearer AT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subscriptionInfo": { "subscriptionTitle": "KIRO PRO+" },
                "usageBreakdownList": [{
                    "currentUsageWithPrecision": 5.0,
                    "usageLimitWithPrecision": 50.0
                }]
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = fetch_at(&client, &server.uri(), &cred(), &imp())
            .await
            .unwrap();
        assert_eq!(r.usage_limit(), 50.0);
        assert_eq!(r.current_usage(), 5.0);
        assert_eq!(r.remaining(), 45.0);
        assert_eq!(r.subscription_title(), Some("KIRO PRO+"));
    }

    #[tokio::test]
    async fn fetch_at_maps_non_success_to_upstream() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = fetch_at(&client, &server.uri(), &cred(), &imp()).await;
        assert!(matches!(r, Err(BalanceError::Upstream { status: 403, .. })));
    }

    #[tokio::test]
    async fn fetch_at_carries_amz_message_in_error_display() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "__type": "AccessDeniedException",
                "message": "Your User ID abc temporarily is suspended"
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = fetch_at(&client, &server.uri(), &cred(), &imp()).await;
        let e = r.unwrap_err();
        assert!(e.is_auth());
        let d = e.to_string();
        assert!(d.contains("403"), "display should carry status: {d}");
        assert!(
            d.contains("temporarily is suspended"),
            "display should carry message: {d}"
        );
    }

    #[test]
    fn extract_amz_message_prefers_message_field_and_falls_back_to_raw() {
        assert_eq!(
            extract_amz_message(r#"{"__type":"X","message":"boom"}"#),
            "boom"
        );
        assert_eq!(extract_amz_message("plain text error"), "plain text error");
    }
}
