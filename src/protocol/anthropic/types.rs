//! Anthropic `/v1/messages` 请求/响应 DTO(文本 MVP)。
use serde::{Deserialize, Serialize};

/// `/v1/messages` 请求体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    #[serde(default)]
    pub system: Option<SystemPrompt>,
    pub messages: Vec<InMsg>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
}

/// `system` 字段:裸字符串**或**内容块数组。Anthropic 规范二者都允许——真实客户端
/// (Claude Code、带 prompt 缓存的 SDK)会把 system 发成 `[{"type":"text","text":"…",
/// "cache_control":{…}}]` 数组;只接受字符串会 422。用 `#[serde(untagged)]` 两者都吃,
/// 额外字段(cache_control 等)serde 默认忽略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

/// system 内容块;只关心 `text`,其它字段(type/cache_control…)忽略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(default)]
    pub text: Option<String>,
}

impl SystemPrompt {
    /// 拍平成纯文本(拼接所有文本块;裸字符串即自身)。
    pub fn text(&self) -> String {
        match self {
            SystemPrompt::Text(s) => s.clone(),
            SystemPrompt::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| b.text.as_deref())
                .collect::<Vec<_>>()
                .concat(),
        }
    }
}

/// 工具定义(照 Anthropic 公开规范)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

/// 单条输入消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InMsg {
    pub role: String,
    pub content: ContentIn,
}

impl InMsg {
    /// 把 `content` 拍平成纯文本(拼接所有文本块;裸字符串即自身)。
    pub fn text(&self) -> String {
        match &self.content {
            ContentIn::Text(s) => s.clone(),
            ContentIn::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text } => Some(text.as_str()),
                    Block::ToolUse { .. } | Block::ToolResult { .. } | Block::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .concat(),
        }
    }
}

/// 输入消息的 `content`:裸字符串或内容块数组。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentIn {
    Text(String),
    Blocks(Vec<Block>),
}

/// 输入内容块(照 Anthropic 公开规范:文本/工具调用/工具结果/图片)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: serde_json::Value,
        #[serde(default)]
        is_error: Option<bool>,
    },
    Image {
        source: serde_json::Value,
    },
}

/// `/v1/messages` 响应体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub model: String,
    pub content: Vec<OutBlock>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
}

/// 输出内容块(照 Anthropic 公开规范:文本/工具调用)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

/// token 用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// `POST /v1/messages/count_tokens` 响应体(照 Anthropic 公开规范形状)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CountTokensResponse {
    pub input_tokens: u32,
}

/// `GET /v1/models` 单条模型条目(照 Anthropic 公开规范形状)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnthropicModel {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub display_name: String,
    pub created_at: String,
}

/// `GET /v1/models` 响应体(照 Anthropic 公开规范形状,游标分页字段本 MVP 恒定/可选)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnthropicModelList {
    pub data: Vec<AnthropicModel>,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_messages_request_with_string_content() {
        let raw = r#"{"model":"claude-sonnet-4.5","system":"s","messages":[{"role":"user","content":"hi"}],"max_tokens":100}"#;
        let req: MessagesRequest = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(req.model, "claude-sonnet-4.5");
        assert_eq!(req.system.as_ref().map(|s| s.text()).as_deref(), Some("s"));
        assert_eq!(req.max_tokens, Some(100));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].text(), "hi");
    }

    #[test]
    fn parses_system_as_content_block_array() {
        // 真实客户端(Claude Code / 带缓存 SDK)把 system 发成带 cache_control 的块数组。
        let raw = r#"{"model":"claude-sonnet-4.5","system":[{"type":"text","text":"you are","cache_control":{"type":"ephemeral"}},{"type":"text","text":" helpful"}],"messages":[{"role":"user","content":"hi"}]}"#;
        let req: MessagesRequest = serde_json::from_str(raw).expect("system 数组应能解析");
        assert_eq!(
            req.system.as_ref().map(|s| s.text()).as_deref(),
            Some("you are helpful")
        );
    }

    #[test]
    fn parses_messages_request_with_block_content() {
        let raw = r#"{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#;
        let req: MessagesRequest = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(req.messages[0].text(), "hi");
        assert_eq!(req.system, None);
        assert_eq!(req.stream, None);
    }

    #[test]
    fn flattens_multiple_text_blocks() {
        let raw = r#"{"role":"assistant","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#;
        let msg: InMsg = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(msg.text(), "ab");
    }

    #[test]
    fn serializes_messages_response() {
        let resp = MessagesResponse {
            id: "msg_1".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            content: vec![OutBlock::Text {
                text: "hi".to_string(),
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 2,
            },
        };
        let v = serde_json::to_value(&resp).expect("序列化失败");
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "hi");
        assert_eq!(v["usage"]["input_tokens"], 1);
    }

    // --- 工具调用 / 图片(照 Anthropic 公开规范)---

    #[test]
    fn parses_messages_request_with_tools() {
        let raw = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"tools":[{"name":"get_weather","description":"d","input_schema":{"type":"object"}}]}"#;
        let req: MessagesRequest = serde_json::from_str(raw).expect("解析失败");
        let tools = req.tools.expect("tools 应存在");
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(tools[0].description.as_deref(), Some("d"));
        assert_eq!(tools[0].input_schema["type"], "object");
    }

    #[test]
    fn parses_tool_result_block() {
        let raw = r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"ok"}]}"#;
        let msg: InMsg = serde_json::from_str(raw).expect("解析失败");
        match &msg.content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    assert_eq!(tool_use_id, "tu1");
                    assert_eq!(content, "ok");
                }
                other => panic!("应为 ToolResult,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn parses_assistant_tool_use_block() {
        let raw = r#"{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"get_weather","input":{"city":"Paris"}}]}"#;
        let msg: InMsg = serde_json::from_str(raw).expect("解析失败");
        match &msg.content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolUse { id, name, input } => {
                    assert_eq!(id, "tu1");
                    assert_eq!(name, "get_weather");
                    assert_eq!(input["city"], "Paris");
                }
                other => panic!("应为 ToolUse,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn serializes_out_block_tool_use() {
        let block = OutBlock::ToolUse {
            id: "tu1".to_string(),
            name: "n".to_string(),
            input: serde_json::json!({"a": 1}),
        };
        let v = serde_json::to_value(&block).expect("序列化失败");
        assert_eq!(v["type"], "tool_use");
        assert_eq!(v["id"], "tu1");
        assert_eq!(v["input"]["a"], 1);
    }
}
