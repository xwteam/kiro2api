//! sso_token 登录:用一个已有 SSO bearer,程序化走完设备批准链换取 CodeWhisperer 凭据。
//! 注册/设备授权/换 token 复用 AWS SSO-OIDC 公开流程(见 `builderid`);中间的 portal 批准
//! 四步端点系照观测 portal.sso 流量自行设计的最小实现(非公开 API)。
use super::{LoginError, MintedCredential, builderid};

/// 批量导入上限(自设)。
const MAX_BULK: usize = 200;

/// 用一个 bearer 兑换一份凭据:注册+设备授权 → bearer 自动批准 → 换 token。
pub async fn redeem(
    client: &reqwest::Client,
    oidc: &str,
    portal: &str,
    region: &str,
    bearer: &str,
) -> Result<MintedCredential, LoginError> {
    // 1) 注册 + 发起设备授权(公开 SSO-OIDC 流程,复用 builderid::start)。
    let pending = builderid::start(client, oidc).await?;

    // 2) 携带 bearer 程序化批准(自设四步,见 approve_with_bearer)。
    approve_with_bearer(client, portal, bearer, &pending.user_code).await?;

    // 3) 批准后换 token(设备码授权,公开流程,复用 builderid::poll)。
    builderid::poll(client, oidc, region, &pending).await
}

/// 携带 bearer 走完 portal 的四步设备批准(照观测自行设计):
/// 确认身份 → 建设备会话 → 接受 userCode → 关联 token。
async fn approve_with_bearer(
    client: &reqwest::Client,
    portal: &str,
    bearer: &str,
    user_code: &str,
) -> Result<(), LoginError> {
    let auth = |req: reqwest::RequestBuilder| req.bearer_auth(bearer);

    // 确认身份。
    auth(client.get(format!("{portal}/token/whoAmI")))
        .send()
        .await
        .map_err(|_| LoginError::Http)?
        .error_for_status()
        .map_err(|_| LoginError::Denied)?;

    // 建设备会话。
    auth(client.post(format!("{portal}/session/device")))
        .send()
        .await
        .map_err(|_| LoginError::Http)?
        .error_for_status()
        .map_err(|_| LoginError::Denied)?;

    // 接受 userCode(Referer 头照观测带上)。
    auth(client.post(format!("{portal}/device_authorization/accept_user_code")))
        .header("Referer", portal)
        .json(&serde_json::json!({ "userCode": user_code }))
        .send()
        .await
        .map_err(|_| LoginError::Http)?
        .error_for_status()
        .map_err(|_| LoginError::Denied)?;

    // 关联 token。
    auth(client.post(format!("{portal}/device_authorization/associate_token")))
        .send()
        .await
        .map_err(|_| LoginError::Http)?
        .error_for_status()
        .map_err(|_| LoginError::Denied)?;

    Ok(())
}

/// 批量兑换,每行一个 bearer,超上限截断;每行独立报告成功/失败。
pub async fn redeem_bulk(
    client: &reqwest::Client,
    oidc: &str,
    portal: &str,
    region: &str,
    lines: &[&str],
) -> Vec<Result<MintedCredential, LoginError>> {
    let mut out = Vec::new();
    for bearer in lines.iter().take(MAX_BULK) {
        out.push(redeem(client, oidc, portal, region, bearer).await);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mock_full_chain() -> MockServer {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/client/register"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"clientId":"cid","clientSecret":"csec"})),
            )
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/device_authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceCode":"dev","userCode":"UC-1","verificationUri":"https://x/","interval":1,
                "verificationUriComplete":"https://x/?user_code=UC-1"})))
            .mount(&s)
            .await;
        // portal 自动批准链(端点为观测所得)
        Mock::given(method("GET"))
            .and(path("/token/whoAmI"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"tokenId":"tid"})),
            )
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/session/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok":true})))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/device_authorization/accept_user_code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok":true})))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/device_authorization/associate_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok":true})))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"accessToken":"atok","refreshToken":"rtok","expiresIn":3600}),
            ))
            .mount(&s)
            .await;
        s
    }

    #[tokio::test]
    async fn redeem_walks_chain_to_credential() {
        let s = mock_full_chain().await;
        let client = reqwest::Client::new();
        let cred = redeem(&client, &s.uri(), &s.uri(), "us-east-1", "bearer-xyz")
            .await
            .unwrap();
        assert_eq!(cred.access_token, "atok");
        assert_eq!(cred.region, "us-east-1");
        // 客户端凭据经 builderid::poll 一路带出(auth=Idc 首刷依赖)。
        assert_eq!(cred.client_id, "cid");
        assert_eq!(cred.client_secret, "csec");
        assert!(!cred.client_id.is_empty() && !cred.client_secret.is_empty());
    }

    #[tokio::test]
    async fn bulk_caps_and_reports_each() {
        let s = mock_full_chain().await;
        let client = reqwest::Client::new();
        let lines = vec!["b1", "b2"];
        let out = redeem_bulk(&client, &s.uri(), &s.uri(), "us-east-1", &lines).await;
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.is_ok()));
    }
}
