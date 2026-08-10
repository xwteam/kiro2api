//! AWS Builder-ID 登录:OAuth 2.0 设备授权码(RFC 8628)+ AWS SSO-OIDC 公开端点。
use super::{LoginError, MintedCredential, SCOPES, read_upstream_json};
use serde::Deserialize;

/// AWS/Kiro 强制常量(照观测记录)。
const CLIENT_NAME: &str = "Kiro";
const CLIENT_TYPE: &str = "public";
const START_URL: &str = "https://view.awsapps.com/start";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

use super::effective_expires_in;

/// 发起设备授权后的待批准态。
#[derive(Debug, Clone)]
pub struct Pending {
    pub user_code: String,
    pub verification_uri: String,
    pub device_code: String,
    pub interval_secs: u64,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Deserialize)]
struct RegisterResp {
    #[serde(rename = "clientId")]
    client_id: String,
    #[serde(rename = "clientSecret")]
    client_secret: String,
}
#[derive(Deserialize)]
struct DeviceResp {
    #[serde(rename = "deviceCode")]
    device_code: String,
    #[serde(rename = "userCode")]
    user_code: String,
    #[serde(rename = "verificationUri")]
    verification_uri: String,
    #[serde(default = "default_interval")]
    interval: u64,
}
fn default_interval() -> u64 {
    5
}
#[derive(Deserialize)]
struct TokenResp {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// AWS SSO-OIDC 设备码换 token 会回带 idToken(JWT):SSO/IdC 数据面认这个 JWT,
    /// 而非 portal 会话型 accessToken。铸凭据时优先采纳(缺省才回落 accessToken)。
    #[serde(rename = "idToken", default)]
    id_token: Option<String>,
    #[serde(rename = "refreshToken", default)]
    refresh_token: String,
    #[serde(rename = "expiresIn", default)]
    expires_in: u64,
}
#[derive(Deserialize)]
struct ErrResp {
    #[serde(default)]
    error: String,
    /// AWS SSO-OIDC 少数路径不回 `error`,只在 `__type` 里给异常名(可能带 `namespace#`
    /// 前缀),同样要认,否则 `AuthorizationPendingException` 会被当成不可解析的错误体。
    #[serde(rename = "__type", default)]
    kind: String,
}

/// 从设备码 `/token` 的错误体取 OAuth 错误码:先 `error`,再回落 `__type`(剥掉
/// `namespace#` 前缀)。两者都取不到 → `None`,表示这压根不是一份可解析的错误体。
fn device_error_code(body: &str) -> Option<String> {
    let e: ErrResp = serde_json::from_str(body).ok()?;
    for raw in [e.error.as_str(), e.kind.as_str()] {
        let code = raw.rsplit('#').next().unwrap_or(raw).trim();
        if !code.is_empty() {
            return Some(code.to_string());
        }
    }
    None
}

async fn post_json<T: serde::Serialize>(
    client: &reqwest::Client,
    url: &str,
    body: &T,
) -> Result<reqwest::Response, LoginError> {
    // 补齐 sso-oidc 伪装头:登录流与令牌刷新打的是**同一台主机**,理应长得一样。
    // 此前这里一个头都不带、连 User-Agent 都没有,而登录恰恰是账号刚被创建、
    // 上游最会看指纹的时刻(见 `apply_sso_oidc_headers` 的说明)。
    let mut h = reqwest::header::HeaderMap::new();
    crate::kiro::login::apply_sso_oidc_headers(&mut h, &crate::kiro::login::login_impersonation());
    if let Some(host) = crate::kiro::provider::host_of(url) {
        h.insert(
            "host",
            reqwest::header::HeaderValue::from_str(&host)
                .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("")),
        );
    }
    h.insert(
        "amz-sdk-invocation-id",
        reqwest::header::HeaderValue::from_str(&crate::kiro::provider::new_invocation_id())
            .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("")),
    );
    h.insert(
        "amz-sdk-request",
        reqwest::header::HeaderValue::from_static("attempt=1; max=4"),
    );
    h.insert(
        "connection",
        reqwest::header::HeaderValue::from_static("close"),
    );
    client
        .post(url)
        .headers(h)
        .json(body)
        .send()
        .await
        .map_err(|_| LoginError::Http)
}

/// 注册客户端 + 发起设备授权。
pub async fn start(client: &reqwest::Client, base: &str) -> Result<Pending, LoginError> {
    let resp = post_json(
        client,
        &format!("{base}/client/register"),
        &serde_json::json!({
            "clientName": CLIENT_NAME, "clientType": CLIENT_TYPE, "scopes": SCOPES,
            "grantTypes": [DEVICE_GRANT, "refresh_token"],
        }),
    )
    .await?;
    let reg: RegisterResp = read_upstream_json(resp).await?;

    let resp = post_json(
        client,
        &format!("{base}/device_authorization"),
        &serde_json::json!({
            "clientId": reg.client_id, "clientSecret": reg.client_secret, "startUrl": START_URL,
        }),
    )
    .await?;
    let dev: DeviceResp = read_upstream_json(resp).await?;

    Ok(Pending {
        user_code: dev.user_code,
        verification_uri: dev.verification_uri,
        device_code: dev.device_code,
        interval_secs: dev.interval,
        client_id: reg.client_id,
        client_secret: reg.client_secret,
    })
}

/// 轮询一次设备码换 token。
pub async fn poll(
    client: &reqwest::Client,
    base: &str,
    region: &str,
    p: &Pending,
) -> Result<MintedCredential, LoginError> {
    let resp = post_json(
        client,
        &format!("{base}/token"),
        &serde_json::json!({
            "clientId": p.client_id, "clientSecret": p.client_secret,
            "grantType": DEVICE_GRANT, "deviceCode": p.device_code,
        }),
    )
    .await?;
    let status = resp.status().as_u16();
    if resp.status().is_success() {
        let t: TokenResp = resp.json().await.map_err(|e| LoginError::UpstreamHttp {
            status,
            body: super::extract_upstream_message(&e.to_string()),
        })?;
        // refreshToken 是这条凭据后续续期的唯一凭证:缺了就是一条永远不可用的死凭据;
        // 且落库去重按 refresh_token 比对,空串会让去重失效,重复登录堆出多条死账号。
        // 设备码此刻已被上游消费,重试也换不回来,故按终态报错而不是铸出来。
        if t.refresh_token.trim().is_empty() {
            return Err(LoginError::Upstream(
                "upstream returned no refreshToken".to_string(),
            ));
        }
        Ok(MintedCredential {
            // SSO/IdC 数据面认 idToken(JWT);缺省才回落 accessToken。
            access_token: t.id_token.unwrap_or(t.access_token),
            refresh_token: t.refresh_token,
            expires_in_secs: effective_expires_in(t.expires_in),
            region: region.to_string(),
            // 首刷走 SSO-OIDC refresh_token 授权须重放这对客户端凭据(auth=Idc)。
            // sso_token::redeem 也经此路径,故 client_id/secret 一并带出。
            client_id: p.client_id.clone(),
            client_secret: p.client_secret.clone(),
        })
    } else {
        let body = resp.text().await.unwrap_or_default();
        // 只有上游明确回了 OAuth 错误码才敢判终态。拿不到可解析错误体(代理塞回 HTML
        // 错误页、应答被截断等)必须归瞬态:调用方把终态错误当作"销毁登录会话",一次
        // 网络抖动就会把用户已在浏览器点过授权的整条设备码流杀掉。
        let Some(code) = device_error_code(&body) else {
            return Err(LoginError::Transient {
                status,
                detail: super::extract_upstream_message(&body),
            });
        };
        Err(match code.as_str() {
            "authorization_pending" | "AuthorizationPendingException" => LoginError::Pending,
            "slow_down" | "SlowDownException" => LoginError::SlowDown,
            "expired_token" | "ExpiredTokenException" => LoginError::Expired,
            "access_denied" | "AccessDeniedException" => LoginError::Denied,
            other => LoginError::Upstream(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::FALLBACK_EXPIRES_IN_SECS;
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_pending() -> Pending {
        Pending {
            user_code: "x".into(),
            verification_uri: "x".into(),
            device_code: "dev".into(),
            interval_secs: 5,
            client_id: "cid".into(),
            client_secret: "csec".into(),
        }
    }

    /// 让 `/token` 以给定状态码 + 原始体应答,返回 poll 的错误。
    async fn poll_error(status: u16, body: &str) -> LoginError {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        poll(&client, &server.uri(), "us-east-1", &test_pending())
            .await
            .unwrap_err()
    }

    /// 让 `/token` 以 200 + 给定 JSON 应答,返回 poll 结果。
    async fn poll_with_token_body(body: serde_json::Value) -> Result<MintedCredential, LoginError> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        poll(&client, &server.uri(), "us-east-1", &test_pending()).await
    }

    #[tokio::test]
    async fn start_then_poll_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/client/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "clientId": "cid", "clientSecret": "csec"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/device_authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceCode": "dev", "userCode": "ABCD-1234",
                "verificationUri": "https://view.awsapps.com/start/#/device",
                "expiresIn": 600, "interval": 5
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "atok", "refreshToken": "rtok", "expiresIn": 3600
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let p = start(&client, &server.uri()).await.unwrap();
        assert_eq!(p.user_code, "ABCD-1234");
        let cred = poll(&client, &server.uri(), "us-east-1", &p).await.unwrap();
        assert_eq!(cred.access_token, "atok");
        assert_eq!(cred.refresh_token, "rtok");
        assert_eq!(cred.region, "us-east-1");
        // 客户端凭据须带出(auth=Idc 首刷依赖)。
        assert_eq!(cred.client_id, "cid");
        assert_eq!(cred.client_secret, "csec");
        assert!(!cred.client_id.is_empty() && !cred.client_secret.is_empty());
    }

    #[tokio::test]
    async fn poll_authorization_pending_maps_to_pending() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending"
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let p = test_pending();
        assert_eq!(
            poll(&client, &server.uri(), "us-east-1", &p).await,
            Err(LoginError::Pending)
        );
    }

    #[tokio::test]
    async fn poll_recognizes_device_flow_wait_and_terminal_codes() {
        // 标准 snake_case 错误码。
        assert_eq!(
            poll_error(400, r#"{"error":"authorization_pending"}"#).await,
            LoginError::Pending
        );
        assert_eq!(
            poll_error(400, r#"{"error":"slow_down"}"#).await,
            LoginError::SlowDown
        );
        assert_eq!(
            poll_error(400, r#"{"error":"expired_token"}"#).await,
            LoginError::Expired
        );
        assert_eq!(
            poll_error(400, r#"{"error":"access_denied"}"#).await,
            LoginError::Denied
        );
        // AWS 少数路径只在 __type 里给异常名(带 namespace# 前缀),同样要认作"继续等待"。
        assert_eq!(
            poll_error(
                400,
                r#"{"__type":"com.amazonaws.oidc#AuthorizationPendingException"}"#
            )
            .await,
            LoginError::Pending
        );
        assert_eq!(
            poll_error(400, r#"{"__type":"SlowDownException"}"#).await,
            LoginError::SlowDown
        );
    }

    #[tokio::test]
    async fn poll_unparseable_error_body_is_transient_not_terminal() {
        // 代理塞回 HTML 错误页:判不出终态,必须归瞬态,否则调用方会销毁登录会话,
        // 用户即使已在浏览器点过授权也得从头再来。
        match poll_error(502, "<html><body>Bad Gateway</body></html>").await {
            LoginError::Transient { status, detail } => {
                assert_eq!(status, 502);
                assert!(detail.contains("Bad Gateway"), "detail={detail}");
            }
            other => panic!("expected Transient, got {other:?}"),
        }
        // JSON 但无 error/__type:同样判不出终态。
        match poll_error(400, "{}").await {
            LoginError::Transient { status, .. } => assert_eq!(status, 400),
            other => panic!("expected Transient, got {other:?}"),
        }
        // 空体:也不能塌缩成 Http/终态。
        assert!(poll_error(503, "").await.is_retryable());
    }

    #[tokio::test]
    async fn poll_requires_refresh_token() {
        // 缺 refreshToken → 明确报错,而不是铸出一条永远刷不动的死凭据。
        let err = poll_with_token_body(serde_json::json!({
            "accessToken": "atok", "expiresIn": 3600
        }))
        .await
        .unwrap_err();
        assert!(
            matches!(&err, LoginError::Upstream(m) if m.contains("refreshToken")),
            "got {err:?}"
        );
        // 空串同样拒绝:去重按 refresh_token 比对,空串会让去重失效堆出多条死账号。
        let err = poll_with_token_body(serde_json::json!({
            "accessToken": "atok", "refreshToken": "", "expiresIn": 3600
        }))
        .await
        .unwrap_err();
        assert!(
            matches!(&err, LoginError::Upstream(m) if m.contains("refreshToken")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn poll_missing_expires_in_falls_back_to_short_ttl() {
        // 缺 expiresIn 不能落 0:凭据一落盘即被判过期,会引发每请求刷新一次的风暴。
        let cred = poll_with_token_body(serde_json::json!({
            "accessToken": "atok", "refreshToken": "rtok"
        }))
        .await
        .unwrap();
        assert_eq!(cred.expires_in_secs, FALLBACK_EXPIRES_IN_SECS);
        assert!(cred.expires_in_secs > 0);
        // 上游给了值就照用。
        let cred = poll_with_token_body(serde_json::json!({
            "accessToken": "atok", "refreshToken": "rtok", "expiresIn": 3600
        }))
        .await
        .unwrap();
        assert_eq!(cred.expires_in_secs, 3600);
    }

    #[tokio::test]
    async fn poll_prefers_id_token_for_sso_data_plane() {
        // 设备码换 token 回带 idToken(JWT):铸凭据须采 idToken,不是 portal accessToken。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "portal-atok", "idToken": "jwt-idtok",
                "refreshToken": "rtok", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let p = test_pending();
        let cred = poll(&client, &server.uri(), "us-east-1", &p).await.unwrap();
        assert_eq!(cred.access_token, "jwt-idtok"); // 采 idToken
        assert_eq!(cred.client_id, "cid");
        assert_eq!(cred.client_secret, "csec");
    }
}
