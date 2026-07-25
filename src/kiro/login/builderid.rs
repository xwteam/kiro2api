//! AWS Builder-ID 登录:OAuth 2.0 设备授权码(RFC 8628)+ AWS SSO-OIDC 公开端点。
use super::{LoginError, MintedCredential, SCOPES, read_upstream_json};
use serde::Deserialize;

/// AWS/Kiro 强制常量(照观测记录)。
const CLIENT_NAME: &str = "Kiro";
const CLIENT_TYPE: &str = "public";
const START_URL: &str = "https://view.awsapps.com/start";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

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
    error: String,
}

async fn post_json<T: serde::Serialize>(
    client: &reqwest::Client,
    url: &str,
    body: &T,
) -> Result<reqwest::Response, LoginError> {
    client
        .post(url)
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
    if resp.status().is_success() {
        let status = resp.status().as_u16();
        let t: TokenResp = resp.json().await.map_err(|e| LoginError::UpstreamHttp {
            status,
            body: super::extract_upstream_message(&e.to_string()),
        })?;
        Ok(MintedCredential {
            // SSO/IdC 数据面认 idToken(JWT);缺省才回落 accessToken。
            access_token: t.id_token.unwrap_or(t.access_token),
            refresh_token: t.refresh_token,
            expires_in_secs: t.expires_in,
            region: region.to_string(),
            // 首刷走 SSO-OIDC refresh_token 授权须重放这对客户端凭据(auth=Idc)。
            // sso_token::redeem 也经此路径,故 client_id/secret 一并带出。
            client_id: p.client_id.clone(),
            client_secret: p.client_secret.clone(),
        })
    } else {
        let e: ErrResp = resp.json().await.map_err(|_| LoginError::Http)?;
        Err(match e.error.as_str() {
            "authorization_pending" => LoginError::Pending,
            "slow_down" => LoginError::SlowDown,
            "expired_token" => LoginError::Expired,
            "access_denied" => LoginError::Denied,
            other => LoginError::Upstream(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
        let p = Pending {
            user_code: "x".into(),
            verification_uri: "x".into(),
            device_code: "dev".into(),
            interval_secs: 5,
            client_id: "cid".into(),
            client_secret: "csec".into(),
        };
        assert_eq!(
            poll(&client, &server.uri(), "us-east-1", &p).await,
            Err(LoginError::Pending)
        );
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
        let p = Pending {
            user_code: "x".into(),
            verification_uri: "x".into(),
            device_code: "dev".into(),
            interval_secs: 5,
            client_id: "cid".into(),
            client_secret: "csec".into(),
        };
        let cred = poll(&client, &server.uri(), "us-east-1", &p).await.unwrap();
        assert_eq!(cred.access_token, "jwt-idtok"); // 采 idToken
        assert_eq!(cred.client_id, "cid");
        assert_eq!(cred.client_secret, "csec");
    }
}
