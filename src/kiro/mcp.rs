//! Kiro MCP 端点客户端(JSON-RPC over HTTP)。
//!
//! 上游除了数据面的 `generateAssistantResponse`,还有一个 `/mcp` 端点,走 JSON-RPC 2.0,
//! 用来调用上游自带的服务端工具 —— 目前用到的是 `web_search`。
//!
//! 头的形态与数据面**不同**(照观测):这里带 `x-amzn-kiro-profile-arn`(数据面是把
//! profileArn 放进请求体),且**不带** `x-amzn-codewhisperer-optout` / `x-amzn-kiro-agent-mode`。
//! 两边各按各的来,不要图省事共用一套。
use crate::kiro::credential::Credential;
use crate::kiro::provider::Impersonation;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

/// MCP 端点(region 化)。
pub fn endpoint(region: &str) -> String {
    format!("https://q.{region}.amazonaws.com/mcp")
}

/// JSON-RPC 请求。
#[derive(Debug, Serialize)]
pub struct McpRequest {
    pub id: String,
    pub jsonrpc: &'static str,
    pub method: &'static str,
    pub params: McpParams,
}

#[derive(Debug, Serialize)]
pub struct McpParams {
    pub name: &'static str,
    pub arguments: serde_json::Value,
}

/// JSON-RPC 响应(只取我们用得上的部分)。
#[derive(Debug, Deserialize)]
pub struct McpResponse {
    #[serde(default)]
    pub result: Option<McpResult>,
}

#[derive(Debug, Deserialize)]
pub struct McpResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
}

#[derive(Debug, Deserialize)]
pub struct McpContent {
    #[serde(rename = "type", default)]
    pub content_type: String,
    #[serde(default)]
    pub text: String,
}

/// 构造一次 `tools/call`。
pub fn tools_call(name: &'static str, arguments: serde_json::Value) -> McpRequest {
    McpRequest {
        // id 只需在本次调用内唯一,用 UUID 即可(与其它 wire 字段同一形状)。
        id: crate::kiro::uuid_v4(),
        jsonrpc: "2.0",
        method: "tools/call",
        params: McpParams { name, arguments },
    }
}

/// MCP 请求头(照观测)。**插入顺序即线上顺序**,与数据面同一纪律。
fn build_headers(cred: &Credential, imp: &Impersonation, base: &str) -> HeaderMap {
    let hv = |s: &str| HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static(""));
    let mut h = HeaderMap::new();
    h.insert("content-type", HeaderValue::from_static("application/json"));
    h.insert("connection", HeaderValue::from_static("close"));
    if let Ok(name) = HeaderName::from_bytes(b"x-amz-user-agent") {
        h.insert(
            name,
            hv(&format!(
                "aws-sdk-js/1.0.34 KiroIDE-{}-{}",
                imp.kiro_version, imp.machine_id
            )),
        );
    }
    h.insert(
        reqwest::header::USER_AGENT,
        hv(&format!(
            "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererstreaming#1.0.34 m/E KiroIDE-{}-{}",
            imp.system_version, imp.node_version, imp.kiro_version, imp.machine_id
        )),
    );
    if let Some(host) = crate::kiro::provider::host_of(base) {
        h.insert("host", hv(&host));
    }
    h.insert(
        "amz-sdk-invocation-id",
        hv(&crate::kiro::provider::new_invocation_id()),
    );
    h.insert(
        "amz-sdk-request",
        HeaderValue::from_static("attempt=1; max=3"),
    );
    h.insert("authorization", hv(&format!("Bearer {}", cred.bearer())));
    // MCP 侧的 profileArn 走**头**,而数据面是放进请求体。两处形态不同,别互相套用。
    if let Some(arn) = cred.profile_arn.as_deref().filter(|a| !a.is_empty())
        && let Ok(name) = HeaderName::from_bytes(b"x-amzn-kiro-profile-arn")
    {
        h.insert(name, hv(arn));
    }
    if cred.is_api_key() {
        h.insert("tokentype", HeaderValue::from_static("API_KEY"));
    }
    h
}

/// 向 `base` 发一次 MCP 调用。测试可注入 mock base;生产用 [`endpoint`]。
pub async fn call_at(
    client: &reqwest::Client,
    base: &str,
    cred: &Credential,
    imp: &Impersonation,
    req: &McpRequest,
) -> Result<McpResponse, crate::kiro::login::LoginError> {
    let resp = client
        .post(base)
        .headers(build_headers(cred, imp, base))
        .json(req)
        .send()
        .await
        .map_err(|_| crate::kiro::login::LoginError::Http)?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = crate::kiro::provider::read_body_capped(resp).await;
        return Err(crate::kiro::login::LoginError::UpstreamHttp { status, body });
    }
    resp.json()
        .await
        .map_err(|_| crate::kiro::login::LoginError::Http)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cred() -> Credential {
        let mut c = crate::kiro::credential::tests_support_cred();
        c.profile_arn = Some("arn:aws:codewhisperer:us-east-1:1:profile/A".into());
        c
    }
    fn imp() -> Impersonation {
        Impersonation {
            machine_id: "mid".into(),
            kiro_version: "0.11.107".into(),
            agent_mode: "vibe".into(),
            system_version: "win32#10.0.22631".into(),
            node_version: "22.22.0".into(),
        }
    }

    #[test]
    fn endpoint_is_regionalised() {
        assert_eq!(
            endpoint("eu-central-1"),
            "https://q.eu-central-1.amazonaws.com/mcp"
        );
    }

    /// MCP 的 profileArn 走**头**(数据面走请求体),两处形态不同不得互相套用。
    #[tokio::test]
    async fn profile_arn_goes_in_a_header_not_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": "x",
                "result": { "content": [{ "type": "text", "text": "{\"results\":[]}" }] }
            })))
            .mount(&server)
            .await;
        let base = format!("{}/mcp", server.uri());
        let req = tools_call("web_search", serde_json::json!({"query": "rust"}));
        call_at(&reqwest::Client::new(), &base, &cred(), &imp(), &req)
            .await
            .expect("应成功");

        let reqs = server.received_requests().await.unwrap();
        let h = &reqs[0].headers;
        assert_eq!(
            h["x-amzn-kiro-profile-arn"].to_str().unwrap(),
            "arn:aws:codewhisperer:us-east-1:1:profile/A"
        );
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert!(
            body.get("profileArn").is_none(),
            "MCP 请求体不该带 profileArn"
        );
        assert_eq!(body["method"], "tools/call");
        assert_eq!(body["params"]["name"], "web_search");
        assert_eq!(body["params"]["arguments"]["query"], "rust");
        // 数据面专属的两个头不该出现在 MCP 请求上
        assert!(!h.contains_key("x-amzn-codewhisperer-optout"));
        assert!(!h.contains_key("x-amzn-kiro-agent-mode"));
    }

    /// ksk 凭据要带 ksk 本身作 bearer 并声明 tokentype(与其它链路同一纪律)。
    #[tokio::test]
    async fn api_key_credentials_use_the_ksk_and_declare_token_type() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": "x", "result": { "content": [] }
            })))
            .mount(&server)
            .await;
        let mut c = cred();
        c.auth = crate::kiro::credential::AuthMethod::ApiKey;
        c.kiro_api_key = Some("ksk_K".into());
        c.access_token = String::new();
        let req = tools_call("web_search", serde_json::json!({"query": "q"}));
        call_at(&reqwest::Client::new(), &server.uri(), &c, &imp(), &req)
            .await
            .unwrap();
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(
            reqs[0].headers["authorization"].to_str().unwrap(),
            "Bearer ksk_K"
        );
        assert_eq!(reqs[0].headers["tokentype"].to_str().unwrap(), "API_KEY");
    }
}
