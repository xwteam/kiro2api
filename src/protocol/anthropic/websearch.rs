//! Anthropic 服务端内置工具 `web_search` 的接管。
//!
//! 客户端声明这个工具时,**搜索是服务端做的** —— 不像普通工具那样把调用回给客户端执行。
//! 上游把这个能力放在 `/mcp` 端点(JSON-RPC),而不是数据面。所以这条请求要在进数据面
//! 之前被截住:调 MCP 拿结果,再合成一份 Anthropic 形状的响应。
//!
//! 此前我们只是**容忍**这个工具声明(v0.11.0 让它不再 400),但从不真的搜索 —— 模型照常
//! 回一段普通文本,客户端以为搜过了,其实没有。
use crate::kiro::credential::Credential;
use crate::kiro::provider::Impersonation;
use crate::protocol::anthropic::types::MessagesRequest;
use serde::Deserialize;

/// MCP 返回的一条搜索结果。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WebSearchResult {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub snippet: Option<String>,
    /// 毫秒时间戳。
    #[serde(rename = "publishedDate", default)]
    pub published_date: Option<i64>,
}

/// MCP `web_search` 的结果载荷(它把 JSON 塞在 `content[0].text` 里)。
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub struct WebSearchResults {
    #[serde(default)]
    pub results: Vec<WebSearchResult>,
    #[serde(default)]
    pub error: Option<String>,
}

/// 这条请求是不是「纯 web_search」请求。
///
/// 判据是**只声明了 web_search 这一个工具** —— 客户端在做服务端搜索时就是这么发的。
/// 混着别的工具时不接管:那种请求要走正常数据面,由模型决定调哪个工具。
pub fn is_web_search_request(req: &MessagesRequest) -> bool {
    req.tools
        .as_ref()
        .is_some_and(|t| t.len() == 1 && t[0].name.eq_ignore_ascii_case("web_search"))
}

/// 客户端在做服务端搜索时会加的固定前缀(照观测)。
const QUERY_PREFIX: &str = "Perform a web search for the query: ";

/// 从请求里抠出搜索词。
///
/// 取**首条**消息的文本:服务端搜索是一次性请求,客户端把查询放在第一条。
/// 带着那个固定前缀就剥掉 —— 连前缀一起搜会把结果搞乱。
pub fn extract_query(req: &MessagesRequest) -> Option<String> {
    use crate::protocol::anthropic::types::{Block, ContentIn};
    let first = req.messages.first()?;
    let text = match &first.content {
        ContentIn::Text(s) => s.clone(),
        ContentIn::Blocks(blocks) => match blocks.first()? {
            Block::Text { text } => text.clone(),
            _ => return None,
        },
    };
    let q = text.strip_prefix(QUERY_PREFIX).unwrap_or(&text).trim();
    (!q.is_empty()).then(|| q.to_string())
}

/// 服务端工具调用的 id(照 Anthropic 形状:`srvtoolu_` + 32 位十六进制)。
pub fn new_server_tool_use_id() -> String {
    format!("srvtoolu_{}", crate::kiro::uuid_v4().replace('-', ""))
}

/// 搜索没拿到结果的原因。
///
/// 分两档是因为**它们对账号池的含义完全不同**:调用被上游拒了是这个账号的事(令牌失效、
/// 没额度、被限流),池子必须知道;而载荷形状不认识是我们和上游的协议对不上,账号本身
/// 好好的,罚它只会平白削掉一个能用的号。
#[derive(Debug)]
pub enum SearchError {
    /// MCP 调用本身失败(非 2xx / 传输层)。**账号级**信号,调用方须反馈账号池。
    Call(crate::kiro::login::LoginError),
    /// 调用通过了,但响应里没有我们认识的结果载荷。账号没问题。
    Malformed,
}

/// 调 MCP 取搜索结果。
///
/// 失败**不让整条请求失败**(由调用方合成一个"没搜到"的响应),但失败这件事必须原样
/// 交回调用方:此前这里把错误吞掉只返回 `None`,于是"这个账号刚刚被上游拒了"这个事实
/// 连传都传不出去 —— 账号池什么都学不到,同一个坏号会被一次次挑中去打同一枪。
pub async fn search(
    client: &reqwest::Client,
    region: &str,
    cred: &Credential,
    imp: &Impersonation,
    query: &str,
) -> Result<WebSearchResults, SearchError> {
    search_at(
        client,
        &crate::kiro::mcp::endpoint(region),
        cred,
        imp,
        query,
    )
    .await
}

/// 向指定 MCP base 发一次搜索(测试可注入 mock base;生产由 [`search`] 按 region 取)。
pub async fn search_at(
    client: &reqwest::Client,
    base: &str,
    cred: &Credential,
    imp: &Impersonation,
    query: &str,
) -> Result<WebSearchResults, SearchError> {
    let req = crate::kiro::mcp::tools_call("web_search", serde_json::json!({ "query": query }));
    match crate::kiro::mcp::call_at(client, base, cred, imp, &req).await {
        Ok(resp) => parse_results(&resp).ok_or(SearchError::Malformed),
        Err(e) => {
            tracing::warn!(error = ?e, "web_search MCP 调用失败,按空结果继续");
            Err(SearchError::Call(e))
        }
    }
}

/// 把 MCP 响应解析成搜索结果。载荷是**塞在 text 里的 JSON**,故要二次解析。
pub fn parse_results(resp: &crate::kiro::mcp::McpResponse) -> Option<WebSearchResults> {
    let c = resp.result.as_ref()?.content.first()?;
    if c.content_type != "text" {
        return None;
    }
    serde_json::from_str(&c.text).ok()
}

/// 把结果转成 Anthropic 的 `web_search_result` 内容块数组。
pub fn to_result_blocks(results: Option<&WebSearchResults>) -> Vec<serde_json::Value> {
    let Some(r) = results else {
        return Vec::new();
    };
    r.results
        .iter()
        .map(|x| {
            let page_age = x.published_date.and_then(|ms| {
                chrono::DateTime::from_timestamp_millis(ms)
                    .map(|dt| dt.format("%B %-d, %Y").to_string())
            });
            serde_json::json!({
                "type": "web_search_result",
                "title": x.title,
                "url": x.url,
                "encrypted_content": x.snippet.clone().unwrap_or_default(),
                "page_age": page_age,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::anthropic::types::{Block, ContentIn, InMsg, ToolDef};

    fn req_with(tools: Option<Vec<ToolDef>>, first: ContentIn) -> MessagesRequest {
        MessagesRequest {
            metadata: None,
            thinking: None,
            model: "sonnet".into(),
            system: None,
            messages: vec![InMsg {
                role: "user".into(),
                content: first,
            }],
            max_tokens: Some(16),
            stream: None,
            tools,
            tool_choice: None,
        }
    }
    fn tool(name: &str) -> ToolDef {
        ToolDef {
            tool_type: Some("web_search_20250305".into()),
            name: name.into(),
            description: None,
            input_schema: serde_json::Value::Null,
        }
    }

    /// 只有「单独声明 web_search」才接管。
    ///
    /// 混着别的工具时必须走正常数据面 —— 那种请求里模型要自己决定调哪个工具,
    /// 我们截下来直接搜等于替模型做了决定。
    #[test]
    fn only_a_lone_web_search_tool_is_intercepted() {
        assert!(is_web_search_request(&req_with(
            Some(vec![tool("web_search")]),
            ContentIn::Text("x".into())
        )));
        // 大小写无关
        assert!(is_web_search_request(&req_with(
            Some(vec![tool("Web_Search")]),
            ContentIn::Text("x".into())
        )));
        // 混着别的工具 → 不接管
        assert!(!is_web_search_request(&req_with(
            Some(vec![tool("web_search"), tool("shell")]),
            ContentIn::Text("x".into())
        )));
        // 没有工具 → 不接管
        assert!(!is_web_search_request(&req_with(
            None,
            ContentIn::Text("x".into())
        )));
    }

    /// 搜索词要剥掉客户端加的固定前缀,否则连前缀一起搜会把结果搞乱。
    #[test]
    fn query_prefix_is_stripped() {
        let r = req_with(
            Some(vec![tool("web_search")]),
            ContentIn::Text(format!("{QUERY_PREFIX}rust async runtime")),
        );
        assert_eq!(extract_query(&r).as_deref(), Some("rust async runtime"));

        // 没有前缀就原样用
        let r2 = req_with(
            Some(vec![tool("web_search")]),
            ContentIn::Text("plain query".into()),
        );
        assert_eq!(extract_query(&r2).as_deref(), Some("plain query"));

        // 内容块形态同样支持
        let r3 = req_with(
            Some(vec![tool("web_search")]),
            ContentIn::Blocks(vec![Block::Text {
                text: format!("{QUERY_PREFIX}blocks form"),
            }]),
        );
        assert_eq!(extract_query(&r3).as_deref(), Some("blocks form"));

        // 空查询 → None(调用方回 400,而不是拿空串去搜)
        let r4 = req_with(
            Some(vec![tool("web_search")]),
            ContentIn::Text(QUERY_PREFIX.into()),
        );
        assert_eq!(extract_query(&r4), None);
    }

    /// MCP 把结果 JSON **塞在 text 字段里**,要二次解析。
    #[test]
    fn results_are_parsed_out_of_the_nested_json_text() {
        let resp: crate::kiro::mcp::McpResponse = serde_json::from_value(serde_json::json!({
            "result": { "content": [{
                "type": "text",
                "text": "{\"results\":[{\"title\":\"T\",\"url\":\"https://e.com\",\"snippet\":\"S\",\"publishedDate\":1700000000000}]}"
            }]}
        }))
        .unwrap();
        let r = parse_results(&resp).expect("应解析出结果");
        assert_eq!(r.results.len(), 1);
        assert_eq!(r.results[0].title, "T");

        let blocks = to_result_blocks(Some(&r));
        assert_eq!(blocks[0]["type"], "web_search_result");
        assert_eq!(blocks[0]["url"], "https://e.com");
        assert_eq!(blocks[0]["encrypted_content"], "S");
        assert!(
            blocks[0]["page_age"].is_string(),
            "有发布日期就该格式化出来"
        );

        // 没结果时给空数组而不是 null —— 客户端按数组遍历
        assert_eq!(to_result_blocks(None), Vec::<serde_json::Value>::new());
    }

    /// MCP 调用被上游拒绝时,**失败这件事必须传回调用方**。
    ///
    /// 回归:此前这里把错误吞掉只返回 `None`,调用方无从分辨"搜到了 0 条"和"这个账号
    /// 刚刚被上游拒了" —— 账号池因此什么都学不到,同一个坏号被一次次挑中去打同一枪。
    #[tokio::test]
    async fn a_rejected_mcp_call_is_reported_back_not_swallowed() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(
                ResponseTemplate::new(403).set_body_string("{\"__type\":\"AccessDenied\"}"),
            )
            .mount(&server)
            .await;

        let c = crate::kiro::credential::tests_support_cred();
        let imp = crate::kiro::provider::Impersonation::for_credential_global(&c);
        let r = search_at(
            &reqwest::Client::new(),
            &format!("{}/mcp", server.uri()),
            &c,
            &imp,
            "rust",
        )
        .await;
        match r {
            Err(SearchError::Call(crate::kiro::login::LoginError::UpstreamHttp {
                status, ..
            })) => assert_eq!(status, 403, "状态码要原样带回,调用方据它分类"),
            other => panic!("失败必须传回调用方而不是被吞成空结果;实际:{other:?}"),
        }
    }

    /// 服务端工具调用 id 的形状。
    #[test]
    fn server_tool_use_id_shape() {
        let id = new_server_tool_use_id();
        assert!(id.starts_with("srvtoolu_"));
        let hex = &id["srvtoolu_".len()..];
        assert_eq!(hex.len(), 32);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(new_server_tool_use_id(), id);
    }
}
