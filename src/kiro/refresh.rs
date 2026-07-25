//! Kiro token 刷新。Social 走 Kiro auth 端点;IdC 走 AWS SSO-OIDC refresh_token grant。
//! 端点为照观测的 wire 事实;请求/响应处理为本项目自写。
use crate::kiro::credential::{AuthMethod, Credential};
use crate::kiro::login::LoginError;
use serde::Deserialize;

/// 刷新/OIDC 端点使用的 region:与数据面保持一致地优先取 profileArn 里的 region,
/// 缺省(无 arn 或 arn 不含合法 region 段)才回落 `cred.region`。
///
/// #12:此前刷新端点直接用 `cred.region`,而数据面 region 来自 profileArn,二者可能
/// 不一致 → IdC 账号可能把 OIDC 刷新打到与数据面不同的 region。这里复用
/// [`crate::kiro::endpoint::region_from_profile_arn`] 统一来源。
fn refresh_region(cred: &Credential) -> String {
    cred.profile_arn
        .as_deref()
        .and_then(crate::kiro::endpoint::region_from_profile_arn)
        .unwrap_or_else(|| cred.region.clone())
}

/// 刷新端点(照观测)。region 与数据面一致地优先取 profileArn(#12)。
pub fn endpoint(cred: &Credential) -> String {
    let region = refresh_region(cred);
    match cred.auth {
        AuthMethod::Social => format!("https://prod.{region}.auth.desktop.kiro.dev/refreshToken"),
        AuthMethod::Idc => format!("https://oidc.{region}.amazonaws.com/token"),
    }
}

#[derive(Deserialize)]
struct RefreshResp {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// AWS SSO-OIDC 的 refresh 响应带 idToken(JWT):IdC 数据面(Amazon Q
    /// generateAssistantResponse)要的是这个 JWT,而非 portal 会话型 accessToken。
    #[serde(rename = "idToken", default)]
    id_token: Option<String>,
    #[serde(rename = "refreshToken", default)]
    refresh_token: Option<String>,
    #[serde(rename = "expiresIn", default)]
    expires_in: u64,
    /// 若刷新响应回带 profileArn 则采纳(通常缺省)。
    #[serde(rename = "profileArn", default)]
    profile_arn: Option<String>,
}

/// 向 `base` 发刷新请求,返回更新后的 Credential。
pub async fn refresh_at(
    client: &reqwest::Client,
    base: &str,
    cred: &Credential,
    now_unix: u64,
) -> Result<Credential, LoginError> {
    let body = match cred.auth {
        AuthMethod::Social => serde_json::json!({ "refreshToken": cred.refresh_token }),
        AuthMethod::Idc => serde_json::json!({
            "clientId": cred.client_id, "clientSecret": cred.client_secret,
            "refreshToken": cred.refresh_token, "grantType": "refresh_token",
        }),
    };
    let resp = client
        .post(base)
        .json(&body)
        .send()
        .await
        .map_err(|_| LoginError::Http)?;
    if !resp.status().is_success() {
        return Err(LoginError::Upstream(format!(
            "refresh HTTP {}",
            resp.status().as_u16()
        )));
    }
    let r: RefreshResp = resp.json().await.map_err(|_| LoginError::Http)?;
    let mut out = cred.clone();
    // IdC 数据面认 idToken(JWT);缺省才回落 accessToken。Social 一律用 accessToken。
    // 用 portal 型 accessToken 打 IdC 数据面会 403,进而账号被判死,故必须区分。
    out.access_token = match cred.auth {
        AuthMethod::Idc => r.id_token.unwrap_or(r.access_token),
        AuthMethod::Social => r.access_token,
    };
    if let Some(rt) = r.refresh_token {
        out.refresh_token = rt;
    }
    // 刷新响应若回带 profileArn 且与当前不同,则采纳(通常缺省,保留旧值)。
    if let Some(pa) = r.profile_arn
        && out.profile_arn.as_deref() != Some(pa.as_str())
    {
        out.profile_arn = Some(pa);
    }
    out.expires_at_unix = now_unix.saturating_add(r.expires_in);
    Ok(out)
}

/// 便捷刷新:用 `endpoint(cred)` 作 base。
pub async fn refresh(
    client: &reqwest::Client,
    cred: &Credential,
    now_unix: u64,
) -> Result<Credential, LoginError> {
    let base = endpoint(cred);
    refresh_at(client, &base, cred, now_unix).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cred(auth: AuthMethod) -> Credential {
        Credential {
            id: "a".into(),
            access_token: "old".into(),
            refresh_token: "rt".into(),
            expires_at_unix: 0,
            region: "us-east-1".into(),
            auth,
            client_id: Some("cid".into()),
            client_secret: Some("csec".into()),
            profile_arn: None,
            machine_id: None,
            email: None,
            nickname: None,
            weight: 1,
            label: None,
            disabled: false,
        }
    }

    #[tokio::test]
    async fn social_refresh_updates_token_and_expiry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new", "refreshToken": "rt2", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Social);
        let out = refresh_at(&client, &format!("{}/refreshToken", server.uri()), &c, 1000)
            .await
            .unwrap();
        assert_eq!(out.access_token, "new");
        assert_eq!(out.refresh_token, "rt2");
        assert_eq!(out.expires_at_unix, 1000 + 3600);
    }

    #[tokio::test]
    async fn idc_refresh_keeps_old_refresh_when_absent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new", "expiresIn": 1800
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Idc);
        let out = refresh_at(&client, &format!("{}/token", server.uri()), &c, 500)
            .await
            .unwrap();
        assert_eq!(out.access_token, "new");
        assert_eq!(out.refresh_token, "rt"); // 响应未带则保留旧的
        assert_eq!(out.expires_at_unix, 500 + 1800);
    }

    #[tokio::test]
    async fn idc_refresh_prefers_id_token_over_portal_access_token() {
        // AWS SSO-OIDC refresh 回 {accessToken: portal 会话型, idToken: 数据面 JWT}。
        // IdC 数据面(Amazon Q)要 idToken;用 portal accessToken 会 403 → 账号被判死。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "portal-session-token",
                "idToken": "jwt-data-plane",
                "refreshToken": "rt2", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Idc);
        let out = refresh_at(&client, &format!("{}/token", server.uri()), &c, 1000)
            .await
            .unwrap();
        assert_eq!(out.access_token, "jwt-data-plane"); // 采 idToken,不是 portal token
        assert_eq!(out.refresh_token, "rt2");
    }

    #[tokio::test]
    async fn idc_refresh_falls_back_to_access_token_when_no_id_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "only-access", "expiresIn": 1800
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Idc);
        let out = refresh_at(&client, &format!("{}/token", server.uri()), &c, 500)
            .await
            .unwrap();
        assert_eq!(out.access_token, "only-access"); // 无 idToken → 回落 accessToken
    }

    #[tokio::test]
    async fn social_refresh_ignores_id_token_and_uses_access_token() {
        // Social 数据面用 accessToken;即便响应误带 idToken 也不采纳。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "social-at", "idToken": "should-be-ignored",
                "refreshToken": "rt2", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Social);
        let out = refresh_at(&client, &format!("{}/refreshToken", server.uri()), &c, 1000)
            .await
            .unwrap();
        assert_eq!(out.access_token, "social-at");
    }

    #[tokio::test]
    async fn refresh_adopts_profile_arn_when_returned() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new",
                "profileArn": "arn:aws:codewhisperer:us-east-1:222222222222:profile/NEW",
                "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Social); // profile_arn 初始 None
        let out = refresh_at(&client, &format!("{}/refreshToken", server.uri()), &c, 1000)
            .await
            .unwrap();
        assert_eq!(
            out.profile_arn.as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:222222222222:profile/NEW")
        );
    }

    #[tokio::test]
    async fn refresh_keeps_profile_arn_when_absent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let mut c = cred(AuthMethod::Social);
        c.profile_arn = Some("arn:existing".into());
        let out = refresh_at(&client, &format!("{}/refreshToken", server.uri()), &c, 1000)
            .await
            .unwrap();
        assert_eq!(out.profile_arn.as_deref(), Some("arn:existing")); // 响应未带则保留旧值
    }

    #[test]
    fn endpoint_by_auth() {
        assert_eq!(
            endpoint(&cred(AuthMethod::Social)),
            "https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken"
        );
        assert_eq!(
            endpoint(&cred(AuthMethod::Idc)),
            "https://oidc.us-east-1.amazonaws.com/token"
        );
    }

    /// #12:有 profileArn 时刷新端点 region 取自 arn(与数据面一致),而非 cred.region。
    #[test]
    fn endpoint_region_prefers_profile_arn() {
        let mut c = cred(AuthMethod::Idc);
        c.region = "us-east-1".into(); // cred.region 与 arn 不同
        c.profile_arn = Some("arn:aws:codewhisperer:ap-northeast-1:123:profile/X".into());
        assert_eq!(
            endpoint(&c),
            "https://oidc.ap-northeast-1.amazonaws.com/token"
        );

        let mut s = cred(AuthMethod::Social);
        s.region = "us-east-1".into();
        s.profile_arn = Some("arn:aws:codewhisperer:eu-west-1:123:profile/X".into());
        assert_eq!(
            endpoint(&s),
            "https://prod.eu-west-1.auth.desktop.kiro.dev/refreshToken"
        );
    }

    /// #12:profileArn 缺省或不含合法 region 段时回落 cred.region。
    #[test]
    fn endpoint_region_falls_back_to_cred_region() {
        let mut c = cred(AuthMethod::Idc);
        c.region = "eu-central-1".into();
        c.profile_arn = None;
        assert_eq!(
            endpoint(&c),
            "https://oidc.eu-central-1.amazonaws.com/token"
        );

        // arn 的第 4 段不像 region → 回落 cred.region
        c.profile_arn = Some("arn:aws:codewhisperer::123:profile/X".into());
        assert_eq!(
            endpoint(&c),
            "https://oidc.eu-central-1.amazonaws.com/token"
        );
    }
}
