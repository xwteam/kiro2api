//! OpenAI Chat Completions 请求/响应/流式 chunk/models DTO。
//!
//! 净室实现:形状照 OpenAI 公开 Chat Completions API 规范(线上 JSON 字段名/结构)
//! 由本仓从公开文档重新推导实现,不含任何第三方专有代码。
use serde::{Deserialize, Serialize};

/// `/v1/chat/completions` 请求体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Option<Vec<OpenAiTool>>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 推理档位(`minimal`/`low`/`medium`/`high`,以及明确关掉的 `none`)。
    ///
    /// 这是 Chat Completions 里唯一表达"要不要思考、思考多深"的字段。不解析它,这个协议
    /// 就完全开不出 extended thinking —— 客户端把开关拨了,链路上却没有任何一环收得到。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// 调用方给的终端用户标识。与对话开头一起做会话指纹(见 `convert::session_metadata`),
    /// 使同一场多轮对话在上游共用一个 conversationId,而不是每轮都开一段新会话。
    #[serde(default)]
    pub user: Option<String>,
}

/// 流式附加选项(照 OpenAI 公开规范)。
///
/// `include_usage:true` 时,`[DONE]` 之前多发一个 `choices:[]`、仅带 `usage` 的收尾 chunk
/// (见 [`ChatCompletionChunk::usage_only`]);为假/缺失时全流不出现 `usage` 字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: Option<bool>,
}

/// 单条消息(user/assistant/system/tool)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<OpenAiContent>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// 消息的 `content`:裸字符串或内容块数组。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenAiContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

/// 多模态内容块(照 OpenAI 公开规范:文本/图片)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

/// 图片 URL(支持 `data:image/png;base64,<data>` 形式)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

/// 工具定义(照 OpenAI 公开规范)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAiFunction,
}

/// 工具函数定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiFunction {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

fn default_function() -> String {
    "function".into()
}

/// 工具调用(assistant 消息里的 `tool_calls`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "default_function")]
    pub kind: String,
    pub function: ToolCallFunction,
}

/// 工具调用的函数体(`arguments` 是 JSON 字符串)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// `/v1/chat/completions` 非流式响应体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatCompletion {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: OpenAiUsage,
}

impl ChatCompletion {
    /// 构造响应,`object` 固定填 `"chat.completion"`。
    pub fn new(
        id: String,
        created: u64,
        model: String,
        choices: Vec<Choice>,
        usage: OpenAiUsage,
    ) -> Self {
        Self {
            id,
            object: "chat.completion".to_string(),
            created,
            model,
            choices,
            usage,
        }
    }
}

/// 非流式响应的单条 choice。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: RespMessage,
    pub finish_reason: Option<String>,
}

/// 非流式响应消息体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RespMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// token 用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// 流式响应的单个 chunk。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    /// 仅 `stream_options.include_usage` 为真时的收尾 chunk 带值;其余 chunk 整字段省略
    /// (OpenAI 规范:普通增量 chunk 不含 `usage`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAiUsage>,
}

impl ChatCompletionChunk {
    /// 构造 chunk,`object` 固定填 `"chat.completion.chunk"`(不带 `usage`)。
    pub fn new(id: String, created: u64, model: String, choices: Vec<ChunkChoice>) -> Self {
        Self {
            id,
            object: "chat.completion.chunk".to_string(),
            created,
            model,
            choices,
            usage: None,
        }
    }

    /// 构造 `include_usage` 的收尾 chunk:`choices` 为空数组,只携带 `usage`。
    pub fn usage_only(id: String, created: u64, model: String, usage: OpenAiUsage) -> Self {
        Self {
            id,
            object: "chat.completion.chunk".to_string(),
            created,
            model,
            choices: Vec::new(),
            usage: Some(usage),
        }
    }
}

/// 流式响应的单条 choice。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

/// 流式增量(全部字段按需跳过,避免多余 null)。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Delta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallChunk>>,
    /// 思考增量。上游把思考过程用 `<thinking>…</thinking>` 包在普通文本里下发,不切出来的话
    /// 客户端会把整段思考当正文显示。这条通道是业界通行的 `reasoning_content`(与 content
    /// 并列的独立字段),客户端不认得它时按缺省忽略,总好过混进正文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// 流式工具调用增量。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ToolCallChunk {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub function: ToolCallFunctionChunk,
}

/// 流式工具调用函数体增量。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ToolCallFunctionChunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// `/v1/models` 响应体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelList {
    pub object: String,
    pub data: Vec<ModelObject>,
}

impl ModelList {
    /// 构造列表,`object` 固定填 `"list"`。
    pub fn new(data: Vec<ModelObject>) -> Self {
        Self {
            object: "list".to_string(),
            data,
        }
    }
}

/// 单个模型条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelObject {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}

impl ModelObject {
    /// 构造条目,`object` 固定填 `"model"`。
    pub fn new(id: String, created: u64, owned_by: String) -> Self {
        Self {
            id,
            object: "model".to_string(),
            created,
            owned_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_chat_request() {
        let raw = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#;
        let req: ChatCompletionRequest = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(
            req.messages[0].content,
            Some(OpenAiContent::Text("hi".to_string()))
        );
        assert_eq!(req.stream, None);
        assert_eq!(req.tools, None);
        assert_eq!(req.max_tokens, None);
        assert_eq!(req.stream_options, None);
    }

    #[test]
    fn parses_stream_options_include_usage() {
        let raw = r#"{"model":"gpt-4o","messages":[],"stream":true,"stream_options":{"include_usage":true}}"#;
        let req: ChatCompletionRequest = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(
            req.stream_options,
            Some(StreamOptions {
                include_usage: Some(true)
            })
        );
    }

    #[test]
    fn parses_multimodal_content_parts() {
        let raw = r#"{"model":"gpt-4o","messages":[{"role":"user","content":[
            {"type":"text","text":"t"},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}
        ]}]}"#;
        let req: ChatCompletionRequest = serde_json::from_str(raw).expect("解析失败");
        match &req.messages[0].content {
            Some(OpenAiContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    ContentPart::Text { text } => assert_eq!(text, "t"),
                    other => panic!("应为 Text,实际: {other:?}"),
                }
                match &parts[1] {
                    ContentPart::ImageUrl { image_url } => {
                        assert_eq!(image_url.url, "data:image/png;base64,AAAA");
                    }
                    other => panic!("应为 ImageUrl,实际: {other:?}"),
                }
            }
            other => panic!("应为 Parts,实际: {other:?}"),
        }
    }

    #[test]
    fn parses_tool_role_message_with_tool_call_id() {
        let raw = r#"{"role":"tool","tool_call_id":"c1","content":"ok"}"#;
        let msg: ChatMessage = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(msg.content, Some(OpenAiContent::Text("ok".to_string())));
    }

    #[test]
    fn parses_assistant_tool_calls_message() {
        let raw = r#"{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{}"}}]}"#;
        let msg: ChatMessage = serde_json::from_str(raw).expect("解析失败");
        let calls = msg.tool_calls.expect("tool_calls 应存在");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "c1");
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.name, "f");
        assert_eq!(calls[0].function.arguments, "{}");
    }

    #[test]
    fn tool_call_kind_defaults_to_function_when_missing() {
        let raw = r#"{"id":"c1","function":{"name":"f","arguments":"{}"}}"#;
        let call: ToolCall = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(call.kind, "function");
    }

    #[test]
    fn serializes_chat_completion() {
        let resp = ChatCompletion::new(
            "chatcmpl-1".to_string(),
            1_700_000_000,
            "gpt-4o".to_string(),
            vec![Choice {
                index: 0,
                message: RespMessage {
                    role: "assistant".to_string(),
                    content: Some("hi".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            OpenAiUsage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
            },
        );
        let v = serde_json::to_value(&resp).expect("序列化失败");
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["role"], "assistant");
        assert_eq!(v["choices"][0]["message"]["content"], "hi");
        assert_eq!(v["usage"]["total_tokens"], 3);
    }

    #[test]
    fn serializes_resp_message_with_tool_calls() {
        let msg = RespMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "c1".to_string(),
                kind: "function".to_string(),
                function: ToolCallFunction {
                    name: "f".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
        };
        let v = serde_json::to_value(&msg).expect("序列化失败");
        assert_eq!(v["tool_calls"][0]["id"], "c1");
        assert_eq!(v["tool_calls"][0]["type"], "function");
        assert_eq!(v["tool_calls"][0]["function"]["name"], "f");
    }

    #[test]
    fn resp_message_omits_tool_calls_when_none() {
        let msg = RespMessage {
            role: "assistant".to_string(),
            content: Some("hi".to_string()),
            tool_calls: None,
        };
        let v = serde_json::to_value(&msg).expect("序列化失败");
        assert!(v.get("tool_calls").is_none());
    }

    #[test]
    fn serializes_chat_completion_chunk() {
        let chunk = ChatCompletionChunk::new(
            "chatcmpl-1".to_string(),
            1_700_000_000,
            "gpt-4o".to_string(),
            vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: Some("assistant".to_string()),
                    content: Some("hi".to_string()),
                    tool_calls: None,
                ..Default::default()
                },
                finish_reason: None,
            }],
        );
        let v = serde_json::to_value(&chunk).expect("序列化失败");
        assert_eq!(v["object"], "chat.completion.chunk");
        assert_eq!(v["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(v["choices"][0]["delta"]["content"], "hi");
        assert!(v.get("usage").is_none(), "普通增量 chunk 不应带 usage");
    }

    #[test]
    fn serializes_usage_only_chunk_with_empty_choices() {
        let chunk = ChatCompletionChunk::usage_only(
            "chatcmpl-1".to_string(),
            1_700_000_000,
            "gpt-4o".to_string(),
            OpenAiUsage {
                prompt_tokens: 7,
                completion_tokens: 11,
                total_tokens: 18,
            },
        );
        let v = serde_json::to_value(&chunk).expect("序列化失败");
        assert_eq!(v["object"], "chat.completion.chunk");
        assert_eq!(v["choices"].as_array().expect("choices 应为数组").len(), 0);
        assert_eq!(v["usage"]["prompt_tokens"], 7);
        assert_eq!(v["usage"]["completion_tokens"], 11);
        assert_eq!(v["usage"]["total_tokens"], 18);
    }

    #[test]
    fn delta_with_only_role_serializes_without_null_fields() {
        let delta = Delta {
            role: Some("assistant".to_string()),
            content: None,
            tool_calls: None,
        ..Default::default()
        };
        let v = serde_json::to_value(&delta).expect("序列化失败");
        assert_eq!(v.as_object().expect("应为 object").len(), 1);
        assert_eq!(v["role"], "assistant");
        assert!(v.get("content").is_none());
        assert!(v.get("tool_calls").is_none());
    }

    #[test]
    fn tool_call_chunk_omits_none_fields() {
        let chunk = ToolCallChunk {
            index: 0,
            id: None,
            kind: None,
            function: ToolCallFunctionChunk {
                name: None,
                arguments: Some("{\"a\":1}".to_string()),
            },
        };
        let v = serde_json::to_value(&chunk).expect("序列化失败");
        assert!(v.get("id").is_none());
        assert!(v.get("type").is_none());
        assert!(v["function"].get("name").is_none());
        assert_eq!(v["function"]["arguments"], "{\"a\":1}");
    }

    #[test]
    fn serializes_model_list_and_model_object() {
        let list = ModelList::new(vec![ModelObject::new(
            "gpt-4o".to_string(),
            1_700_000_000,
            "kiro2api".to_string(),
        )]);
        let v = serde_json::to_value(&list).expect("序列化失败");
        assert_eq!(v["object"], "list");
        assert_eq!(v["data"][0]["object"], "model");
        assert_eq!(v["data"][0]["id"], "gpt-4o");
    }
}
