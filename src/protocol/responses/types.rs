//! OpenAI Responses 请求/响应/流事件 DTO。
//!
//! 净室实现:形状照 OpenAI 公开 Responses API 规范(线上 JSON 字段名/结构)
//! 由本仓从公开文档重新推导实现,不含任何第三方专有代码。
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

/// `/v1/responses` 请求体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: ResponsesInput,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<ResponsesTool>>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
}

/// 请求的 `input`:裸字符串或输入条目数组。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsesInput {
    Text(String),
    Items(Vec<InputItem>),
}

/// 单条输入条目(消息/函数调用/函数调用结果)。
///
/// 序列化恒带 `type`;**反序列化时 `type` 可缺省**——官方 SDK 的常见写法
/// `{"role":"user","content":"hi"}` 不带 `type`,缺失时按 `role` 判定为普通 message 条目。
/// 上一轮 `output` 原样回灌做多轮时,条目自带的 `id`/`status` 等额外字段一律忽略。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    Message {
        role: String,
        content: InputItemContent,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: serde_json::Value,
    },
    /// 认得出、但映射不到中枢协议的条目(`reasoning`、`local_shell_call`、`web_search_call` …)。
    ///
    /// 这些是 Responses 侧的产物,Anthropic 中枢没有对应槽位。客户端做多轮时会把上一轮的
    /// **整个** output 原样回灌,里面必然带这类条目;若判成错误,第一轮能通、第二轮必炸,
    /// 且错误信息只说"不支持的条目类型",排查要从头翻报文。故整条跳过而非拒绝整轮请求。
    /// 保留 `kind` 是为了转换阶段能记出到底跳过了什么。
    Unsupported { kind: String },
}

/// 从 JSON 对象里取一个必填字符串字段;缺失/非字符串 → `missing_field` 错误。
fn required_str<E: de::Error>(v: &serde_json::Value, name: &'static str) -> Result<String, E> {
    v.get(name)
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| de::Error::missing_field(name))
}

impl<'de> Deserialize<'de> for InputItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;
        match v.get("type").and_then(|t| t.as_str()) {
            Some("function_call") => Ok(InputItem::FunctionCall {
                call_id: required_str::<D::Error>(&v, "call_id")?,
                name: required_str::<D::Error>(&v, "name")?,
                arguments: required_str::<D::Error>(&v, "arguments")?,
            }),
            Some("function_call_output") => {
                let call_id = required_str::<D::Error>(&v, "call_id")?;
                let Some(output) = v.get("output").cloned() else {
                    return Err(de::Error::missing_field("output"));
                };
                Ok(InputItem::FunctionCallOutput { call_id, output })
            }
            // `type` 缺失时靠 `role` 认定为消息条目;两者都没有才算真的无法解析。
            Some("message") | None => {
                let role = required_str::<D::Error>(&v, "role")?;
                let Some(raw) = v.get("content") else {
                    return Err(de::Error::missing_field("content"));
                };
                let content = match InputItemContent::deserialize(raw) {
                    Ok(c) => c,
                    Err(e) => return Err(de::Error::custom(e)),
                };
                Ok(InputItem::Message { role, content })
            }
            Some(other) => Ok(InputItem::Unsupported {
                kind: other.to_string(),
            }),
        }
    }
}

/// 消息条目的 `content`:裸字符串或内容块数组。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputItemContent {
    Text(String),
    Parts(Vec<InputContentPart>),
}

/// 多模态输入内容块(照 OpenAI 公开规范:文本/图片)。
///
/// `output_text` 也认:把上一轮的 output message 原样回灌做多轮时,其内容块正是这个类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContentPart {
    InputText {
        text: String,
    },
    OutputText {
        text: String,
    },
    InputImage {
        #[serde(default)]
        image_url: Option<String>,
    },
}

/// 工具定义(Responses 是扁平 function 形)。
///
/// `name` **可缺省**:Responses 的工具数组里除了 `type:"function"`,还混着内置工具
/// (`web_search` / `local_shell` / `file_search` …),它们照规范就没有 `name`。把 `name`
/// 设成必填会让整个请求在反序列化阶段被拒——真实客户端(codex)一次就送十几个工具,其中一个
/// 内置工具就足以让整轮对话打不通,且错误只报到 `tools[13]: missing field name`,看不出是哪类。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesTool {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

/// `/v1/responses` 非流式响应体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseObject {
    pub id: String,
    pub object: String,
    pub created_at: u64,
    pub status: String,
    pub model: String,
    pub output: Vec<OutputItem>,
    pub usage: ResponsesUsage,
    /// `status:"incomplete"` 时的补充说明;`completed` 时整字段省略(照 OpenAI 规范)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_details: Option<IncompleteDetails>,
}

impl ResponseObject {
    /// 构造响应,`object` 固定填 `"response"`(不带 `incomplete_details`)。
    pub fn new(
        id: String,
        created_at: u64,
        status: String,
        model: String,
        output: Vec<OutputItem>,
        usage: ResponsesUsage,
    ) -> Self {
        Self {
            id,
            object: "response".to_string(),
            created_at,
            status,
            model,
            output,
            usage,
            incomplete_details: None,
        }
    }
}

/// `status:"incomplete"` 的原因(照 OpenAI 规范,取值为 `max_output_tokens` / `content_filter`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncompleteDetails {
    pub reason: String,
}

/// 非流式响应的单条输出条目(消息/函数调用)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputItem {
    Message {
        id: String,
        status: String,
        role: String,
        content: Vec<OutputContentPart>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        status: String,
    },
}

/// 输出消息的内容块。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContentPart {
    OutputText { text: String },
}

/// token 用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsesUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

/// 流式事件的框架无关封装(handler 层用 `serde_json::json!` 拼 `data`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEvent {
    pub event: &'static str,
    pub data: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_with_text_input() {
        let raw = r#"{"model":"gpt-4o","input":"hi"}"#;
        let req: ResponsesRequest = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.input, ResponsesInput::Text("hi".to_string()));
        assert_eq!(req.instructions, None);
        assert_eq!(req.tools, None);
        assert_eq!(req.tool_choice, None);
        assert_eq!(req.stream, None);
        assert_eq!(req.max_output_tokens, None);
        assert_eq!(req.previous_response_id, None);
    }

    #[test]
    fn parses_request_with_input_items_array() {
        let raw = r#"{"model":"gpt-4o","input":[
            {"type":"message","role":"user","content":"hi"},
            {"type":"function_call","call_id":"c1","name":"f","arguments":"{}"},
            {"type":"function_call_output","call_id":"c1","output":{"ok":true}}
        ]}"#;
        let req: ResponsesRequest = serde_json::from_str(raw).expect("解析失败");
        match &req.input {
            ResponsesInput::Items(items) => {
                assert_eq!(items.len(), 3);
                match &items[0] {
                    InputItem::Message { role, content } => {
                        assert_eq!(role, "user");
                        assert_eq!(content, &InputItemContent::Text("hi".to_string()));
                    }
                    other => panic!("应为 Message,实际: {other:?}"),
                }
                match &items[1] {
                    InputItem::FunctionCall {
                        call_id,
                        name,
                        arguments,
                    } => {
                        assert_eq!(call_id, "c1");
                        assert_eq!(name, "f");
                        assert_eq!(arguments, "{}");
                    }
                    other => panic!("应为 FunctionCall,实际: {other:?}"),
                }
                match &items[2] {
                    InputItem::FunctionCallOutput { call_id, output } => {
                        assert_eq!(call_id, "c1");
                        assert_eq!(output["ok"], true);
                    }
                    other => panic!("应为 FunctionCallOutput,实际: {other:?}"),
                }
            }
            other => panic!("应为 Items,实际: {other:?}"),
        }
    }

    #[test]
    fn parses_message_input_item_with_content_parts() {
        let raw = r#"{"type":"message","role":"user","content":[
            {"type":"input_text","text":"t"},
            {"type":"input_image","image_url":"http://x/y.png"}
        ]}"#;
        let item: InputItem = serde_json::from_str(raw).expect("解析失败");
        match item {
            InputItem::Message { role, content } => {
                assert_eq!(role, "user");
                match content {
                    InputItemContent::Parts(parts) => {
                        assert_eq!(parts.len(), 2);
                        match &parts[0] {
                            InputContentPart::InputText { text } => assert_eq!(text, "t"),
                            other => panic!("应为 InputText,实际: {other:?}"),
                        }
                        match &parts[1] {
                            InputContentPart::InputImage { image_url } => {
                                assert_eq!(image_url.as_deref(), Some("http://x/y.png"));
                            }
                            other => panic!("应为 InputImage,实际: {other:?}"),
                        }
                    }
                    other => panic!("应为 Parts,实际: {other:?}"),
                }
            }
            other => panic!("应为 Message,实际: {other:?}"),
        }
    }

    /// 官方 SDK 的常见写法:input 条目不带 `type`,靠 `role` 认定为 message 条目。
    #[test]
    fn parses_input_items_without_type_field() {
        let raw = r#"{"model":"gpt-4o","input":[{"role":"user","content":"hi"}]}"#;
        let req: ResponsesRequest = serde_json::from_str(raw).expect("解析失败");
        match &req.input {
            ResponsesInput::Items(items) => {
                assert_eq!(items.len(), 1);
                match &items[0] {
                    InputItem::Message { role, content } => {
                        assert_eq!(role, "user");
                        assert_eq!(content, &InputItemContent::Text("hi".to_string()));
                    }
                    other => panic!("应为 Message,实际: {other:?}"),
                }
            }
            other => panic!("应为 Items,实际: {other:?}"),
        }
    }

    /// 上一轮 output 原样回灌做多轮:message 条目带 `id`/`status` 且内容块是 `output_text`,
    /// function_call 条目带 `id`/`status`——多余字段忽略,不得整体 422。
    #[test]
    fn parses_previous_output_items_fed_back_as_input() {
        let raw = r#"{"model":"gpt-4o","input":[
            {"role":"user","content":"hi"},
            {"type":"message","id":"msg_1","status":"completed","role":"assistant",
             "content":[{"type":"output_text","text":"hello"}]},
            {"type":"function_call","id":"fc_1","status":"completed","call_id":"c1","name":"f","arguments":"{}"},
            {"type":"function_call_output","call_id":"c1","output":"ok"}
        ]}"#;
        let req: ResponsesRequest = serde_json::from_str(raw).expect("解析失败");
        match &req.input {
            ResponsesInput::Items(items) => {
                assert_eq!(items.len(), 4);
                match &items[1] {
                    InputItem::Message { role, content } => {
                        assert_eq!(role, "assistant");
                        match content {
                            InputItemContent::Parts(parts) => match &parts[0] {
                                InputContentPart::OutputText { text } => assert_eq!(text, "hello"),
                                other => panic!("应为 OutputText,实际: {other:?}"),
                            },
                            other => panic!("应为 Parts,实际: {other:?}"),
                        }
                    }
                    other => panic!("应为 Message,实际: {other:?}"),
                }
                match &items[2] {
                    InputItem::FunctionCall { call_id, name, .. } => {
                        assert_eq!(call_id, "c1");
                        assert_eq!(name, "f");
                    }
                    other => panic!("应为 FunctionCall,实际: {other:?}"),
                }
            }
            other => panic!("应为 Items,实际: {other:?}"),
        }
    }

    /// 既无 `type` 又无 `role`:确实无从判定,仍须报错(而不是猜成空消息)。
    #[test]
    fn rejects_input_item_without_type_and_role() {
        let raw = r#"{"model":"gpt-4o","input":[{"content":"hi"}]}"#;
        assert!(serde_json::from_str::<ResponsesRequest>(raw).is_err());
    }

    #[test]
    fn parses_request_with_instructions_previous_response_id_and_tools() {
        let raw = r#"{
            "model":"gpt-4o",
            "input":"hi",
            "instructions":"be nice",
            "previous_response_id":"resp_1",
            "tools":[{"type":"function","name":"f","description":"d","parameters":{"a":1}}]
        }"#;
        let req: ResponsesRequest = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(req.instructions.as_deref(), Some("be nice"));
        assert_eq!(req.previous_response_id.as_deref(), Some("resp_1"));
        let tools = req.tools.expect("tools 应存在");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].kind, "function");
        assert_eq!(tools[0].name.as_deref(), Some("f"));
        assert_eq!(tools[0].description.as_deref(), Some("d"));
        assert_eq!(tools[0].parameters["a"], 1);
    }

    #[test]
    fn serializes_response_object_with_message_output() {
        let resp = ResponseObject::new(
            "resp_1".to_string(),
            1_700_000_000,
            "completed".to_string(),
            "gpt-4o".to_string(),
            vec![OutputItem::Message {
                id: "msg_1".to_string(),
                status: "completed".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentPart::OutputText {
                    text: "hi".to_string(),
                }],
            }],
            ResponsesUsage {
                input_tokens: 1,
                output_tokens: 2,
                total_tokens: 3,
            },
        );
        let v = serde_json::to_value(&resp).expect("序列化失败");
        assert_eq!(v["object"], "response");
        assert_eq!(v["status"], "completed");
        assert_eq!(v["output"][0]["type"], "message");
        assert_eq!(v["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(v["output"][0]["content"][0]["text"], "hi");
        assert_eq!(v["usage"]["total_tokens"], 3);
        assert!(
            v.get("incomplete_details").is_none(),
            "completed 响应不应带 incomplete_details"
        );
    }

    #[test]
    fn serializes_incomplete_response_object_with_details() {
        let mut resp = ResponseObject::new(
            "resp_1".to_string(),
            1_700_000_000,
            "incomplete".to_string(),
            "gpt-4o".to_string(),
            Vec::new(),
            ResponsesUsage {
                input_tokens: 1,
                output_tokens: 2,
                total_tokens: 3,
            },
        );
        resp.incomplete_details = Some(IncompleteDetails {
            reason: "max_output_tokens".to_string(),
        });
        let v = serde_json::to_value(&resp).expect("序列化失败");
        assert_eq!(v["status"], "incomplete");
        assert_eq!(v["incomplete_details"]["reason"], "max_output_tokens");
    }

    #[test]
    fn serializes_function_call_output_item() {
        let item = OutputItem::FunctionCall {
            id: "item_1".to_string(),
            call_id: "call_1".to_string(),
            name: "f".to_string(),
            arguments: "{\"a\":1}".to_string(),
            status: "completed".to_string(),
        };
        let v = serde_json::to_value(&item).expect("序列化失败");
        assert_eq!(v["type"], "function_call");
        assert_eq!(v["call_id"], "call_1");
        assert_eq!(v["arguments"], "{\"a\":1}");
    }

    #[test]
    fn stream_event_fields_are_readable() {
        let ev = StreamEvent {
            event: "response.created",
            data: "{}".to_string(),
        };
        assert_eq!(ev.event, "response.created");
        assert_eq!(ev.data, "{}");
    }
}
