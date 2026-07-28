//! Responses ↔ 中枢转换自写;形状照 OpenAI 公开 Responses 规范/中枢既有(`anthropic::types`)。
use super::types::{
    IncompleteDetails, InputContentPart, InputItem, InputItemContent, OutputContentPart,
    OutputItem, ResponseObject, ResponsesInput, ResponsesRequest, ResponsesTool, ResponsesUsage,
};
use crate::protocol::anthropic::types::{
    Block, ContentIn, InMsg, MessagesRequest, MessagesResponse, OutBlock, SystemPrompt, ToolDef,
};
use serde_json::{Value, json};
use std::fmt;

/// Responses ↔ 中枢转换失败原因。
///
/// 不含"未知模型"变体:模型映射发生在下游 `anthropic_to_kiro`(`relay_core` 调用内部),
/// 未映射模型统一由 `RelayError::Convert` → 400 表达,这里不重复判定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponsesConvertError {
    PreviousResponseUnsupported,
}

impl fmt::Display for ResponsesConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResponsesConvertError::PreviousResponseUnsupported => {
                write!(f, "previous_response_id 暂不支持")
            }
        }
    }
}

impl std::error::Error for ResponsesConvertError {}

/// 生成一个随机十六进制 id(16 字节 CSPRNG)。
///
/// `pub(crate)`:流式 handler([`super::handler::responses_stream`])复用同一 id 生成器
/// (`resp_`/`msg_`/`fc_` 前缀条目 id),避免第二份平行实现。
pub(crate) fn random_hex_id() -> String {
    let mut raw = [0u8; 16];
    getrandom::getrandom(&mut raw).expect("CSPRNG");
    hex::encode(raw)
}

/// 解析 `data:<mime>;base64,<data>` 形式的 data URL,失败返回 `None`。
fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (mime, data) = rest.split_once(";base64,")?;
    Some((mime.to_string(), data.to_string()))
}

/// 把单个 Responses 输入内容块转换成中枢 `Block`(非 data: 图片/空 image_url 跳过)。
///
/// `output_text`(上一轮 output 回灌)与 `input_text` 同样落成文本块。
fn convert_input_part(part: &InputContentPart) -> Option<Block> {
    match part {
        InputContentPart::InputText { text } | InputContentPart::OutputText { text } => {
            Some(Block::Text { text: text.clone() })
        }
        InputContentPart::InputImage { image_url } => {
            let url = image_url.as_deref()?;
            let (mime, data) = parse_data_url(url)?;
            Some(Block::Image {
                source: json!({"type": "base64", "media_type": mime, "data": data}),
            })
        }
    }
}

/// 把单条 Responses 输入条目转换成 0~1 条中枢消息。
fn convert_input_item(item: &InputItem) -> Option<InMsg> {
    match item {
        InputItem::Message { role, content } => {
            let content = match content {
                InputItemContent::Text(s) => ContentIn::Text(s.clone()),
                InputItemContent::Parts(parts) => {
                    ContentIn::Blocks(parts.iter().filter_map(convert_input_part).collect())
                }
            };
            Some(InMsg {
                role: role.clone(),
                content,
            })
        }
        InputItem::FunctionCall {
            call_id,
            name,
            arguments,
        } => {
            let input: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
            Some(InMsg {
                role: "assistant".to_string(),
                content: ContentIn::Blocks(vec![Block::ToolUse {
                    id: call_id.clone(),
                    name: name.clone(),
                    input,
                }]),
            })
        }
        InputItem::FunctionCallOutput { call_id, output } => Some(InMsg {
            role: "user".to_string(),
            content: ContentIn::Blocks(vec![Block::ToolResult {
                tool_use_id: call_id.clone(),
                content: output.clone(),
                is_error: None,
            }]),
        }),
        // 中枢没有对应槽位,整条丢。记一条:多轮里客户端回灌了什么,只有这里看得见。
        InputItem::Unsupported { kind } => {
            tracing::debug!(
                event = "responses_input_item_skipped",
                item_type = %kind,
                "输入条目无中枢对应,已跳过"
            );
            None
        }
    }
}

/// 把 Responses `ResponsesRequest` 转换成中枢 `MessagesRequest`。
///
/// `model` 原样透传;真正的模型名映射(及未知模型判定)发生在下游
/// `anthropic_to_kiro`(relay_core 调用),这里不重复处理。
pub fn responses_to_hub(req: ResponsesRequest) -> Result<MessagesRequest, ResponsesConvertError> {
    if let Some(prev) = &req.previous_response_id
        && !prev.is_empty()
    {
        return Err(ResponsesConvertError::PreviousResponseUnsupported);
    }

    let messages: Vec<InMsg> = match req.input {
        ResponsesInput::Text(s) => vec![InMsg {
            role: "user".to_string(),
            content: ContentIn::Text(s),
        }],
        ResponsesInput::Items(items) => items.iter().filter_map(convert_input_item).collect(),
    };

    // 只有带 `name` 的工具(`function` / `custom`)能映射成中枢 ToolDef。内置工具
    // (`web_search` / `local_shell` / `file_search` …)由 OpenAI 服务端自己执行,Kiro 数据面
    // 没有等价物,转不过去也无法代为执行——丢掉是唯一诚实的处理,但必须记出来:否则模型
    // "怎么不会搜网页/不会跑命令"在日志里毫无线索,只能靠猜。
    let tools = req.tools.map(|tools| {
        tools
            .into_iter()
            .filter_map(|t: ResponsesTool| match t.name {
                Some(name) => Some(ToolDef {
                    name,
                    description: t.description,
                    // 无 `parameters` 的函数工具照中枢规范补空对象:中枢要求 input_schema 是
                    // JSON Schema 对象,直接透传 null 会被上游判成畸形工具、整轮请求失败。
                    input_schema: if t.parameters.is_null() {
                        json!({"type": "object", "properties": {}})
                    } else {
                        t.parameters
                    },
                }),
                None => {
                    tracing::warn!(
                        event = "responses_builtin_tool_dropped",
                        tool_type = %t.kind,
                        "内置工具无中枢等价物,已丢弃(模型将不具备该能力)"
                    );
                    None
                }
            })
            .collect()
    });

    Ok(MessagesRequest {
        model: req.model,
        system: req.instructions.map(SystemPrompt::Text),
        messages,
        max_tokens: req.max_output_tokens,
        stream: req.stream,
        tools,
        // Kiro 数据面 wire 无 tool_choice 对应字段(见 `kiro::convert::anthropic_to_kiro`),
        // 故只原样带进中枢请求,链路末端故意不转发,也不改写成提示词伪装成已生效。
        tool_choice: req.tool_choice,
    })
}

/// 把中枢 `MessagesResponse` 转换成 Responses `ResponseObject`。
///
/// `stop_reason` 决定顶层 `status`:上游截断(`max_tokens` / `model_context_window_exceeded`)
/// → `"incomplete"` + `incomplete_details.reason="max_output_tokens"`,文本条目同样标
/// `"incomplete"`;其余(含 `end_turn`/`tool_use`)→ `"completed"`。Responses 规范只有
/// `max_output_tokens` / `content_filter` 两种 reason,故两类截断共用前者——真正要紧的是
/// `status` 不能报 completed,否则 agent 框架会把半截回答当完整结果继续用。
pub fn hub_to_responses(resp: MessagesResponse, created_at: u64) -> ResponseObject {
    let truncated = matches!(
        resp.stop_reason.as_deref(),
        Some("max_tokens") | Some("model_context_window_exceeded")
    );
    let status = if truncated { "incomplete" } else { "completed" };

    let mut output: Vec<OutputItem> = Vec::new();
    let mut text = String::new();

    for block in &resp.content {
        match block {
            OutBlock::Text { text: t } => text.push_str(t),
            OutBlock::ToolUse { id, name, input } => {
                let arguments = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
                output.push(OutputItem::FunctionCall {
                    id: format!("fc_{}", random_hex_id()),
                    call_id: id.clone(),
                    name: name.clone(),
                    arguments,
                    status: "completed".to_string(),
                });
            }
        }
    }

    if !text.is_empty() {
        output.insert(
            0,
            OutputItem::Message {
                id: format!("msg_{}", random_hex_id()),
                status: status.to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentPart::OutputText { text }],
            },
        );
    }

    let input_tokens = resp.usage.input_tokens;
    let output_tokens = resp.usage.output_tokens;
    let usage = ResponsesUsage {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
    };

    let mut out = ResponseObject::new(
        format!("resp_{}", random_hex_id()),
        created_at,
        status.to_string(),
        resp.model,
        output,
        usage,
    );
    if truncated {
        out.incomplete_details = Some(IncompleteDetails {
            reason: "max_output_tokens".to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_to_hub_text_input_and_instructions() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Text("hi".to_string()),
            instructions: Some("s".to_string()),
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: None,
        };
        let hub = responses_to_hub(req).expect("应转换成功");
        assert_eq!(hub.system.as_ref().map(|s| s.text()).as_deref(), Some("s"));
        assert_eq!(hub.messages.len(), 1);
        assert_eq!(hub.messages[0].role, "user");
        assert_eq!(hub.messages[0].text(), "hi");
    }

    #[test]
    fn responses_to_hub_previous_response_id_rejected() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Text("hi".to_string()),
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: Some("resp_x".to_string()),
        };
        let err = responses_to_hub(req).expect_err("应拒绝");
        assert_eq!(err, ResponsesConvertError::PreviousResponseUnsupported);
    }

    #[test]
    fn responses_to_hub_empty_previous_response_id_allowed() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Text("hi".to_string()),
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: Some(String::new()),
        };
        let hub = responses_to_hub(req).expect("空字符串应放行");
        assert_eq!(hub.messages.len(), 1);
    }

    #[test]
    fn responses_to_hub_message_item_becomes_hub_message() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Items(vec![InputItem::Message {
                role: "user".to_string(),
                content: InputItemContent::Text("hi".to_string()),
            }]),
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: None,
        };
        let hub = responses_to_hub(req).expect("应转换成功");
        assert_eq!(hub.messages.len(), 1);
        assert_eq!(hub.messages[0].role, "user");
        assert_eq!(hub.messages[0].text(), "hi");
    }

    #[test]
    fn responses_to_hub_function_call_item_becomes_tool_use_block() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Items(vec![InputItem::FunctionCall {
                call_id: "call_1".to_string(),
                name: "get_weather".to_string(),
                arguments: "{\"city\":\"Paris\"}".to_string(),
            }]),
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: None,
        };
        let hub = responses_to_hub(req).expect("应转换成功");
        assert_eq!(hub.messages.len(), 1);
        assert_eq!(hub.messages[0].role, "assistant");
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolUse { id, name, input } => {
                    assert_eq!(id, "call_1");
                    assert_eq!(name, "get_weather");
                    assert_eq!(input["city"], "Paris");
                }
                other => panic!("应为 ToolUse,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn responses_to_hub_function_call_malformed_arguments_fallback_to_empty_object() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Items(vec![InputItem::FunctionCall {
                call_id: "call_1".to_string(),
                name: "f".to_string(),
                arguments: "not json".to_string(),
            }]),
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: None,
        };
        let hub = responses_to_hub(req).expect("应转换成功");
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolUse { input, .. } => assert_eq!(*input, json!({})),
                other => panic!("应为 ToolUse,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn responses_to_hub_function_call_output_item_becomes_tool_result_block() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Items(vec![InputItem::FunctionCallOutput {
                call_id: "call_1".to_string(),
                output: json!({"ok": true}),
            }]),
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: None,
        };
        let hub = responses_to_hub(req).expect("应转换成功");
        assert_eq!(hub.messages[0].role, "user");
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    assert_eq!(tool_use_id, "call_1");
                    assert_eq!(content["ok"], true);
                    assert_eq!(*is_error, None);
                }
                other => panic!("应为 ToolResult,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn responses_to_hub_message_item_with_parts_and_image() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Items(vec![InputItem::Message {
                role: "user".to_string(),
                content: InputItemContent::Parts(vec![
                    InputContentPart::InputText {
                        text: "look".to_string(),
                    },
                    InputContentPart::InputImage {
                        image_url: Some("data:image/png;base64,AAAA".to_string()),
                    },
                    InputContentPart::InputImage {
                        image_url: Some("https://example.com/a.png".to_string()),
                    },
                    InputContentPart::InputImage { image_url: None },
                ]),
            }]),
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: None,
        };
        let hub = responses_to_hub(req).expect("应转换成功");
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                match &blocks[0] {
                    Block::Text { text } => assert_eq!(text, "look"),
                    other => panic!("应为 Text,实际: {other:?}"),
                }
                match &blocks[1] {
                    Block::Image { source } => {
                        assert_eq!(source["media_type"], "image/png");
                        assert_eq!(source["data"], "AAAA");
                    }
                    other => panic!("应为 Image,实际: {other:?}"),
                }
            }
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn responses_to_hub_maps_tools() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Text("hi".to_string()),
            instructions: None,
            tools: Some(vec![ResponsesTool {
                kind: "function".to_string(),
                name: Some("get_weather".to_string()),
                description: Some("d".to_string()),
                parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            }]),
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: None,
        };
        let hub = responses_to_hub(req).expect("应转换成功");
        let tools = hub.tools.expect("tools 应存在");
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(tools[0].description.as_deref(), Some("d"));
        assert_eq!(
            tools[0].input_schema["properties"]["city"]["type"],
            "string"
        );
    }

    #[test]
    fn responses_to_hub_max_output_tokens_becomes_max_tokens() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Text("hi".to_string()),
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: Some(true),
            max_output_tokens: Some(256),
            previous_response_id: None,
        };
        let hub = responses_to_hub(req).expect("应转换成功");
        assert_eq!(hub.max_tokens, Some(256));
        assert_eq!(hub.stream, Some(true));
        assert_eq!(hub.model, "gpt-4o");
    }

    #[test]
    fn hub_to_responses_text_response() {
        let resp = MessagesResponse {
            id: "msg_1".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            content: vec![OutBlock::Text {
                text: "hi".to_string(),
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: crate::protocol::anthropic::types::Usage {
                input_tokens: 3,
                output_tokens: 5,
            },
        };
        let out = hub_to_responses(resp, 1_700_000_000);
        assert_eq!(out.status, "completed");
        assert_eq!(out.usage.total_tokens, 8);
        assert!(out.id.starts_with("resp_"));
        match &out.output[0] {
            OutputItem::Message {
                status, content, ..
            } => {
                assert_eq!(status, "completed");
                match &content[0] {
                    OutputContentPart::OutputText { text } => assert_eq!(text, "hi"),
                }
            }
            other => panic!("应为 Message,实际: {other:?}"),
        }
    }

    #[test]
    fn hub_to_responses_tool_use_response() {
        let resp = MessagesResponse {
            id: "msg_1".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            content: vec![OutBlock::ToolUse {
                id: "tu1".to_string(),
                name: "get_weather".to_string(),
                input: json!({"city": "Paris"}),
            }],
            stop_reason: Some("tool_use".to_string()),
            usage: crate::protocol::anthropic::types::Usage {
                input_tokens: 10,
                output_tokens: 2,
            },
        };
        let out = hub_to_responses(resp, 1_700_000_000);
        assert_eq!(out.output.len(), 1);
        let expected_args = serde_json::to_string(&json!({"city": "Paris"})).expect("序列化失败");
        match &out.output[0] {
            OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                status,
                ..
            } => {
                assert_eq!(call_id, "tu1");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, &expected_args);
                assert_eq!(status, "completed");
            }
            other => panic!("应为 FunctionCall,实际: {other:?}"),
        }
    }

    #[test]
    fn responses_to_hub_output_text_part_becomes_text_block() {
        // 上一轮 output 回灌:assistant 消息的 output_text 块必须落成文本块,不能被丢掉。
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: ResponsesInput::Items(vec![InputItem::Message {
                role: "assistant".to_string(),
                content: InputItemContent::Parts(vec![InputContentPart::OutputText {
                    text: "hello".to_string(),
                }]),
            }]),
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            previous_response_id: None,
        };
        let hub = responses_to_hub(req).expect("应转换成功");
        assert_eq!(hub.messages[0].role, "assistant");
        assert_eq!(hub.messages[0].text(), "hello");
    }

    #[test]
    fn hub_to_responses_truncated_response_is_incomplete() {
        for stop in ["max_tokens", "model_context_window_exceeded"] {
            let resp = MessagesResponse {
                id: "msg_1".to_string(),
                kind: "message".to_string(),
                role: "assistant".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                content: vec![OutBlock::Text {
                    text: "half".to_string(),
                }],
                stop_reason: Some(stop.to_string()),
                usage: crate::protocol::anthropic::types::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                },
            };
            let out = hub_to_responses(resp, 1_700_000_000);
            assert_eq!(out.status, "incomplete", "{stop} 应报 incomplete");
            assert_eq!(
                out.incomplete_details.as_ref().map(|d| d.reason.as_str()),
                Some("max_output_tokens"),
                "{stop} 应带 incomplete_details"
            );
            match &out.output[0] {
                OutputItem::Message { status, .. } => assert_eq!(status, "incomplete"),
                other => panic!("应为 Message,实际: {other:?}"),
            }
        }
    }

    #[test]
    fn hub_to_responses_end_turn_stays_completed() {
        let resp = MessagesResponse {
            id: "msg_1".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            content: vec![OutBlock::Text {
                text: "hi".to_string(),
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: crate::protocol::anthropic::types::Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        };
        let out = hub_to_responses(resp, 1_700_000_000);
        assert_eq!(out.status, "completed");
        assert_eq!(out.incomplete_details, None);
    }

    #[test]
    fn hub_to_responses_no_text_no_message_item() {
        let resp = MessagesResponse {
            id: "msg_1".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            content: vec![OutBlock::ToolUse {
                id: "tu1".to_string(),
                name: "f".to_string(),
                input: json!({}),
            }],
            stop_reason: Some("tool_use".to_string()),
            usage: crate::protocol::anthropic::types::Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        };
        let out = hub_to_responses(resp, 1_700_000_000);
        assert!(
            !out.output
                .iter()
                .any(|item| matches!(item, OutputItem::Message { .. }))
        );
    }

    /// codex 的工具数组里混着内置工具(照规范无 `name`)。曾把 `name` 设成必填,导致
    /// 整轮请求在反序列化阶段就被拒:`tools[13]: missing field name`——一个内置工具
    /// 就废掉整个会话,且错误看不出是哪类工具。内置工具须能解析、被丢弃,函数工具照常留下。
    #[test]
    fn builtin_tools_without_name_are_dropped_not_rejected() {
        let raw = serde_json::json!({
            "model": "gpt-4o",
            "input": "hi",
            "tools": [
                {"type": "function", "name": "shell", "parameters": {"type": "object"}},
                {"type": "web_search"},
                {"type": "local_shell"},
                {"type": "file_search", "vector_store_ids": ["vs_1"]}
            ]
        });
        let req: ResponsesRequest = serde_json::from_value(raw).expect("内置工具不得导致整轮请求被拒");
        let hub = responses_to_hub(req).expect("应转换成功");
        let tools = hub.tools.expect("tools 应存在");
        assert_eq!(tools.len(), 1, "只有带 name 的函数工具能映射到中枢");
        assert_eq!(tools[0].name, "shell");
    }

    /// 函数工具可以不带 `parameters`。中枢要求 input_schema 是 JSON Schema 对象,
    /// 透传 null 会被上游判成畸形工具、整轮失败,故补空对象。
    #[test]
    fn function_tool_without_parameters_gets_empty_object_schema() {
        let raw = serde_json::json!({
            "model": "gpt-4o",
            "input": "hi",
            "tools": [{"type": "function", "name": "now"}]
        });
        let req: ResponsesRequest = serde_json::from_value(raw).expect("应解析成功");
        let hub = responses_to_hub(req).expect("应转换成功");
        let tools = hub.tools.expect("tools 应存在");
        assert_eq!(tools[0].input_schema, json!({"type": "object", "properties": {}}));
    }

    /// 多轮时客户端把上一轮 output 整个回灌,里面带 `reasoning` 等中枢没有的条目。
    /// 曾判成错误,结果第一轮能通、第二轮必炸。这类条目须整条跳过,可映射的条目不受影响。
    #[test]
    fn unmappable_input_items_are_skipped_not_rejected() {
        let raw = serde_json::json!({
            "model": "gpt-4o",
            "input": [
                {"type": "message", "role": "user", "content": "hi"},
                {"type": "reasoning", "id": "rs_1", "summary": []},
                {"type": "local_shell_call", "id": "ls_1", "call_id": "c1",
                 "action": {"type": "exec", "command": ["ls"]}},
                {"type": "message", "role": "assistant", "content": "there"}
            ]
        });
        let req: ResponsesRequest = serde_json::from_value(raw).expect("未知条目不得导致整轮请求被拒");
        let hub = responses_to_hub(req).expect("应转换成功");
        assert_eq!(hub.messages.len(), 2, "两条 message 保留,其余跳过");
        assert_eq!(hub.messages[0].role, "user");
        assert_eq!(hub.messages[1].role, "assistant");
    }
}
