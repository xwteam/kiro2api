//! IAM Identity Center 登录:授权码(RFC 6749)+ PKCE S256(RFC 7636)+ AWS SSO-OIDC。
use super::{
    LoginError, MintedCredential, SCOPES, effective_expires_in, new_state, pkce_pair,
    read_upstream_json,
};
use serde::Deserialize;
use url::Url;

const CLIENT_NAME: &str = "Kiro";
const CLIENT_TYPE: &str = "public";
const REDIRECT_URI: &str = "http://127.0.0.1/oauth/callback";
const AUTH_CODE_GRANT: &str = "authorization_code";

/// 注册后待用户授权的态。
#[derive(Debug, Clone)]
pub struct AuthStart {
    pub authorize_url: String,
    pub verifier: String,
    pub state: String,
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
struct TokenResp {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// AWS SSO-OIDC 换 token 会回带 idToken(JWT):IdC 数据面(Amazon Q)认这个 JWT,
    /// 而非 portal 会话型 accessToken。铸凭据时优先采纳(缺省才回落 accessToken)。
    #[serde(rename = "idToken", default)]
    id_token: Option<String>,
    #[serde(rename = "refreshToken", default)]
    refresh_token: String,
    #[serde(rename = "expiresIn", default)]
    expires_in: u64,
}

/// 同 builderid:登录流与刷新打同一台 `oidc.{region}.amazonaws.com`,头要一致。
fn oidc_headers(url: &str) -> reqwest::header::HeaderMap {
    use reqwest::header::HeaderValue;
    let mut h = reqwest::header::HeaderMap::new();
    crate::kiro::login::apply_sso_oidc_headers(&mut h, &crate::kiro::login::login_impersonation());
    if let Some(host) = crate::kiro::provider::host_of(url) {
        h.insert(
            "host",
            HeaderValue::from_str(&host).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
    }
    h.insert(
        "amz-sdk-invocation-id",
        HeaderValue::from_str(&crate::kiro::provider::new_invocation_id())
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    h.insert(
        "amz-sdk-request",
        HeaderValue::from_static("attempt=1; max=4"),
    );
    h.insert("connection", HeaderValue::from_static("close"));
    h
}

/// 注册客户端并构造 authorize URL。
pub async fn start(
    client: &reqwest::Client,
    base: &str,
    start_url: &str,
) -> Result<AuthStart, LoginError> {
    let url = format!("{base}/client/register");
    let resp = client
        .post(&url)
        .headers(oidc_headers(&url))
        .json(&serde_json::json!({
            "clientName": CLIENT_NAME, "clientType": CLIENT_TYPE, "scopes": SCOPES,
            "grantTypes": [AUTH_CODE_GRANT, "refresh_token"],
            "redirectUris": [REDIRECT_URI], "issuerUrl": start_url,
        }))
        .send()
        .await
        .map_err(|_| LoginError::Http)?;
    let reg: RegisterResp = read_upstream_json(resp).await?;

    let (verifier, challenge) = pkce_pair();
    let state = new_state();
    let scopes = SCOPES.join(" ");
    // 用 Url 组装,保证正确编码。
    let mut u = Url::parse(&format!("{base}/authorize")).map_err(|_| LoginError::Http)?;
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &reg.client_id)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scopes", &scopes)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");

    Ok(AuthStart {
        authorize_url: u.to_string(),
        verifier,
        state,
        client_id: reg.client_id,
        client_secret: reg.client_secret,
    })
}

/// 解析用户粘贴的回调 URL:error 优先 → state CSRF → code。
pub fn parse_callback(pasted_url: &str, expected_state: &str) -> Result<String, LoginError> {
    let u = Url::parse(pasted_url).map_err(|_| LoginError::BadCallback)?;
    let mut code = None;
    let mut state = None;
    let mut err = None;
    for (k, v) in u.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => err = Some(v.into_owned()),
            _ => {}
        }
    }
    if let Some(e) = err {
        return Err(match e.as_str() {
            "access_denied" => LoginError::Denied,
            other => LoginError::Upstream(other.to_string()),
        });
    }
    if state.as_deref() != Some(expected_state) {
        return Err(LoginError::BadCallback); // CSRF
    }
    code.ok_or(LoginError::BadCallback)
}

/// 授权码 + verifier 换 token。
pub async fn complete(
    client: &reqwest::Client,
    base: &str,
    region: &str,
    s: &AuthStart,
    code: &str,
) -> Result<MintedCredential, LoginError> {
    let url = format!("{base}/token");
    let resp = client
        .post(&url)
        .headers(oidc_headers(&url))
        .json(&serde_json::json!({
            "clientId": s.client_id, "clientSecret": s.client_secret,
            "grantType": AUTH_CODE_GRANT, "code": code,
            "redirectUri": REDIRECT_URI, "codeVerifier": s.verifier,
        }))
        .send()
        .await
        .map_err(|_| LoginError::Http)?;
    let t: TokenResp = read_upstream_json(resp).await?;
    // 没有 refreshToken 的凭据永远刷不动:轮到它就换号重试,池里只有它时请求直接失败;
    // 且空串会让按 refresh_token 去重失效,重复登录会堆出多条死账号。授权码此刻已被上游
    // 消费,重试也换不回来,故按终态报错而不是落库。
    if t.refresh_token.trim().is_empty() {
        return Err(LoginError::Upstream(
            "upstream returned no refreshToken".to_string(),
        ));
    }
    Ok(MintedCredential {
        // IdC 数据面认 idToken(JWT);缺省才回落 accessToken。
        access_token: t.id_token.unwrap_or(t.access_token),
        refresh_token: t.refresh_token,
        expires_in_secs: effective_expires_in(t.expires_in),
        region: region.to_string(),
        // 首刷走 SSO-OIDC refresh_token 授权须重放这对客户端凭据(auth=Idc)。
        client_id: s.client_id.clone(),
        client_secret: s.client_secret.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn start_builds_authorize_url_with_pkce_and_state() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/client/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "clientId": "cid", "clientSecret": "csec"
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let s = start(&client, &server.uri(), "https://my.awsapps.com/start")
            .await
            .unwrap();
        assert!(s.authorize_url.contains("code_challenge_method=S256"));
        assert!(s.authorize_url.contains(&format!("state={}", s.state)));
        assert!(s.authorize_url.contains("response_type=code"));
        assert_eq!(s.verifier.len(), 43);
    }

    #[test]
    fn parse_callback_checks_state_then_code() {
        // 正常
        assert_eq!(
            parse_callback("http://127.0.0.1/oauth/callback?code=abc&state=S", "S").unwrap(),
            "abc"
        );
        // state 不符 → BadCallback(CSRF)
        assert_eq!(
            parse_callback("http://x/?code=abc&state=WRONG", "S"),
            Err(LoginError::BadCallback)
        );
        // 上游 error 优先
        assert_eq!(
            parse_callback("http://x/?error=access_denied&state=S", "S"),
            Err(LoginError::Denied)
        );
        // 缺 code
        assert_eq!(
            parse_callback("http://x/?state=S", "S"),
            Err(LoginError::BadCallback)
        );
    }

    #[tokio::test]
    async fn start_maps_non_2xx_to_upstream_http_with_message() {
        // 上游 400 + AWS 风格 message → UpstreamHttp{400, "..."},不再吞成 Http。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/client/register"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "message": "issuerUrl is invalid or unreachable"
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let err = start(&client, &server.uri(), "https://bad/start")
            .await
            .unwrap_err();
        match err {
            LoginError::UpstreamHttp { status, body } => {
                assert_eq!(status, 400);
                assert_eq!(body, "issuerUrl is invalid or unreachable");
            }
            other => panic!("expected UpstreamHttp, got {other:?}"),
        }
    }

    /// IdC 授权码登录**实际发出去**的请求头必须带进程级配置的版本号。
    ///
    /// 回归:登录流的伪装身份曾经取编译期默认值,而同一台 `oidc.{region}.amazonaws.com`
    /// 上的令牌刷新取的是进程级真值 —— 同一个 clientId 注册时报一套版本、几分钟后刷新时
    /// 报另一套。IdC 这条路自己拼头(`oidc_headers`),必须单独钉住,不能靠设备码那条路的
    /// 用例代管。
    #[tokio::test]
    async fn register_request_carries_the_process_level_versions() {
        let effective = crate::kiro::login::install_test_process_config();
        assert_ne!(
            effective.system_version,
            crate::config::Config::default().system_version,
            "夹具没能把进程级配置灌进去,本用例失去区分力"
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/client/register"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"clientId":"cid","clientSecret":"csec"})),
            )
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        start(&client, &server.uri(), "https://x.awsapps.com/start")
            .await
            .unwrap();

        let reqs = server.received_requests().await.unwrap();
        let ua = reqs[0].headers["user-agent"].to_str().unwrap();
        assert!(
            ua.contains(&format!("os/{}", effective.system_version)),
            "UA 没带进程级 system_version: {ua}"
        );
        assert!(
            ua.contains(&format!("md/nodejs#{}", effective.node_version)),
            "UA 没带进程级 node_version: {ua}"
        );
    }

    #[tokio::test]
    async fn complete_exchanges_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "atok", "refreshToken": "rtok", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let s = AuthStart {
            authorize_url: "x".into(),
            verifier: "ver".into(),
            state: "S".into(),
            client_id: "cid".into(),
            client_secret: "csec".into(),
        };
        let cred = complete(&client, &server.uri(), "us-east-1", &s, "code123")
            .await
            .unwrap();
        assert_eq!(cred.access_token, "atok");
        // 客户端凭据须带出(auth=Idc 首刷依赖)。
        assert_eq!(cred.client_id, "cid");
        assert_eq!(cred.client_secret, "csec");
        assert!(!cred.client_id.is_empty() && !cred.client_secret.is_empty());
    }

    #[tokio::test]
    async fn complete_prefers_id_token_for_idc_data_plane() {
        // IdC 换 token 回带 idToken(JWT):铸凭据须采 idToken,不是 portal accessToken。
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
        let s = AuthStart {
            authorize_url: "x".into(),
            verifier: "ver".into(),
            state: "S".into(),
            client_id: "cid".into(),
            client_secret: "csec".into(),
        };
        let cred = complete(&client, &server.uri(), "us-east-1", &s, "code123")
            .await
            .unwrap();
        assert_eq!(cred.access_token, "jwt-idtok"); // 采 idToken
        assert_eq!(cred.client_id, "cid");
        assert_eq!(cred.client_secret, "csec");
    }

    /// 上游漏回 refreshToken 时必须报错,而不是铸出一条永远刷不动、且因空串去重失效
    /// 会被重复堆积的死凭据。
    #[tokio::test]
    async fn complete_requires_refresh_token() {
        for body in [
            serde_json::json!({"accessToken": "atok", "expiresIn": 3600}),
            serde_json::json!({"accessToken": "atok", "refreshToken": "", "expiresIn": 3600}),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
            let client = reqwest::Client::new();
            let s = AuthStart {
                authorize_url: "x".into(),
                verifier: "ver".into(),
                state: "S".into(),
                client_id: "cid".into(),
                client_secret: "csec".into(),
            };
            let e = complete(&client, &server.uri(), "us-east-1", &s, "code123")
                .await
                .unwrap_err();
            assert!(matches!(e, LoginError::Upstream(m) if m.contains("refreshToken")));
        }
    }

    /// `expiresIn` 缺失时 serde 默认得到 0——直接落库会让凭据一写盘就判过期,每个请求
    /// 触发一次刷新。须回落到保守短有效期。
    #[tokio::test]
    async fn complete_missing_expires_in_falls_back_to_short_ttl() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "atok", "refreshToken": "rtok"
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let s = AuthStart {
            authorize_url: "x".into(),
            verifier: "ver".into(),
            state: "S".into(),
            client_id: "cid".into(),
            client_secret: "csec".into(),
        };
        let cred = complete(&client, &server.uri(), "us-east-1", &s, "code123")
            .await
            .unwrap();
        assert_eq!(cred.expires_in_secs, super::super::FALLBACK_EXPIRES_IN_SECS);
        assert!(cred.expires_in_secs > 0);
    }
}
