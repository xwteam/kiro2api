//! OpenAI ↔ 中枢转换自写;形状照 OpenAI 公开规范/中枢既有(`anthropic::types`)。
use super::types::{
    ChatCompletion, ChatCompletionRequest, ChatMessage, Choice, ContentPart, OpenAiContent,
    OpenAiUsage, RespMessage, ToolCall, ToolCallFunction,
};
use crate::protocol::anthropic::types::{
    Block, ContentIn, InMsg, MessagesRequest, MessagesResponse, OutBlock, SystemPrompt, ToolDef,
};
use serde_json::{Value, json};

/// 生成一个随机十六进制 id(16 字节 CSPRNG),用于响应 id。
fn random_hex_id() -> String {
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

/// 把单个 OpenAI 内容块转换成中枢 `Block`。
///
/// - `data:` 图片 → 内联 base64 图片块。
/// - 远程 http(s) 图片 URL → **不静默丢弃**,而是编码成 `{"type":"url","url":...}`
///   图片源转下去,由中枢层(`anthropic_to_kiro`)统一拦成 400(Kiro 只收内联
///   base64;详见其 `message_images`)。
fn convert_content_part(part: &ContentPart) -> Option<Block> {
    match part {
        ContentPart::Text { text } => Some(Block::Text { text: text.clone() }),
        ContentPart::ImageUrl { image_url } => match parse_data_url(&image_url.url) {
            Some((mime, data)) => Some(Block::Image {
                source: json!({"type": "base64", "media_type": mime, "data": data}),
            }),
            None => Some(Block::Image {
                source: json!({"type": "url", "url": image_url.url.clone()}),
            }),
        },
    }
}

/// 把单条 OpenAI 消息转换成 0~1 条中枢消息(`role:"system"` 由调用方单独处理,这里不产出)。
fn convert_message(msg: &ChatMessage) -> Option<InMsg> {
    if msg.role == "system" {
        return None;
    }
    if let Some(tool_call_id) = &msg.tool_call_id {
        let text_val = match &msg.content {
            Some(OpenAiContent::Text(s)) => Value::String(s.clone()),
            Some(OpenAiContent::Parts(_)) | None => Value::String(String::new()),
        };
        return Some(InMsg {
            role: "user".to_string(),
            content: ContentIn::Blocks(vec![Block::ToolResult {
                tool_use_id: tool_call_id.clone(),
                content: text_val,
                is_error: None,
            }]),
        });
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut plain_text: Option<String> = None;
    let mut had_parts = false;
    match &msg.content {
        Some(OpenAiContent::Text(s)) => plain_text = Some(s.clone()),
        Some(OpenAiContent::Parts(parts)) => {
            had_parts = true;
            blocks.extend(parts.iter().filter_map(convert_content_part));
        }
        None => {}
    }

    if let Some(tool_calls) = &msg.tool_calls {
        for tc in tool_calls {
            let input: Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| json!({}));
            blocks.push(Block::ToolUse {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                input,
            });
        }
    }

    let content = if blocks.is_empty() && !had_parts {
        ContentIn::Text(plain_text.unwrap_or_default())
    } else {
        if let Some(text) = plain_text
            && !text.is_empty()
        {
            blocks.insert(0, Block::Text { text });
        }
        ContentIn::Blocks(blocks)
    };

    Some(InMsg {
        role: msg.role.clone(),
        content,
    })
}

/// 把 OpenAI `ChatCompletionRequest` 转换成中枢 `MessagesRequest`。
pub fn openai_to_hub(req: ChatCompletionRequest) -> MessagesRequest {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<InMsg> = Vec::new();

    for msg in &req.messages {
        if msg.role == "system" {
            if let Some(OpenAiContent::Text(s)) = &msg.content {
                system_parts.push(s.clone());
            }
            continue;
        }
        if let Some(hub_msg) = convert_message(msg) {
            messages.push(hub_msg);
        }
    }

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    let tools = req.tools.map(|tools| {
        tools
            .into_iter()
            .map(|t| ToolDef {
                name: t.function.name,
                description: t.function.description,
                input_schema: t.function.parameters,
            })
            .collect()
    });

    MessagesRequest {
        model: req.model,
        system: system.map(SystemPrompt::Text),
        messages,
        max_tokens: req.max_tokens,
        stream: req.stream,
        tools,
        tool_choice: req.tool_choice,
    }
}

/// 把中枢 `stop_reason` 映射成 OpenAI `finish_reason`,流式复用。
///
/// - `tool_use` → `tool_calls`
/// - `max_tokens` / `model_context_window_exceeded`(上游截断)→ `length`
/// - 其余(含 `end_turn`)→ `stop`
pub fn finish_reason_from_stop(stop: Option<&str>) -> Option<String> {
    Some(
        match stop {
            Some("tool_use") => "tool_calls",
            Some("max_tokens") | Some("model_context_window_exceeded") => "length",
            _ => "stop",
        }
        .to_string(),
    )
}

/// 把中枢 `MessagesResponse` 转换成 OpenAI `ChatCompletion`。
pub fn hub_to_openai(resp: MessagesResponse, created: u64) -> ChatCompletion {
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for block in &resp.content {
        match block {
            OutBlock::Text { text: t } => text.push_str(t),
            OutBlock::ToolUse { id, name, input } => {
                let arguments = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(ToolCall {
                    id: id.clone(),
                    kind: "function".to_string(),
                    function: ToolCallFunction {
                        name: name.clone(),
                        arguments,
                    },
                });
            }
        }
    }

    let content = if text.is_empty() && !tool_calls.is_empty() {
        None
    } else {
        Some(text)
    };
    let tool_calls = if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls)
    };

    let finish_reason = finish_reason_from_stop(resp.stop_reason.as_deref());

    let message = RespMessage {
        role: "assistant".to_string(),
        content,
        tool_calls,
    };
    let choice = Choice {
        index: 0,
        message,
        finish_reason,
    };

    let prompt_tokens = resp.usage.input_tokens;
    let completion_tokens = resp.usage.output_tokens;
    let usage = OpenAiUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
    };

    ChatCompletion::new(
        format!("chatcmpl-{}", random_hex_id()),
        created,
        resp.model,
        vec![choice],
        usage,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::openai::types::{
        ImageUrl, OpenAiFunction, OpenAiTool, ToolCallFunction as OaToolCallFunction,
    };

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(OpenAiContent::Text(text.to_string())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn system_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: "system".to_string(),
            content: Some(OpenAiContent::Text(text.to_string())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn openai_to_hub_system_and_user() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![system_msg("s"), user_msg("hi")],
            tools: None,
            tool_choice: None,
            stream: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        assert_eq!(hub.system.as_ref().map(|s| s.text()).as_deref(), Some("s"));
        assert_eq!(hub.messages.len(), 1);
        let last = hub.messages.last().expect("应有消息");
        assert_eq!(last.role, "user");
        assert_eq!(last.text(), "hi");
    }

    #[test]
    fn openai_to_hub_multiple_system_messages_joined() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![system_msg("a"), system_msg("b"), user_msg("hi")],
            tools: None,
            tool_choice: None,
            stream: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        assert_eq!(
            hub.system.as_ref().map(|s| s.text()).as_deref(),
            Some("a\n\nb")
        );
    }

    #[test]
    fn openai_to_hub_maps_tools() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![user_msg("hi")],
            tools: Some(vec![OpenAiTool {
                kind: "function".to_string(),
                function: OpenAiFunction {
                    name: "get_weather".to_string(),
                    description: Some("d".to_string()),
                    parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                },
            }]),
            tool_choice: None,
            stream: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        let tools = hub.tools.expect("tools 应存在");
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(tools[0].description.as_deref(), Some("d"));
        assert_eq!(
            tools[0].input_schema["properties"]["city"]["type"],
            "string"
        );
    }

    #[test]
    fn openai_to_hub_assistant_tool_calls_become_tool_use_block() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "c1".to_string(),
                    kind: "function".to_string(),
                    function: OaToolCallFunction {
                        name: "f".to_string(),
                        arguments: "{\"a\":1}".to_string(),
                    },
                }]),
                tool_call_id: None,
            }],
            tools: None,
            tool_choice: None,
            stream: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        let msg = &hub.messages[0];
        assert_eq!(msg.role, "assistant");
        match &msg.content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolUse { id, name, input } => {
                    assert_eq!(id, "c1");
                    assert_eq!(name, "f");
                    assert_eq!(input["a"], 1);
                }
                other => panic!("应为 ToolUse,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn openai_to_hub_malformed_tool_call_arguments_fallback_to_empty_object() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "c1".to_string(),
                    kind: "function".to_string(),
                    function: OaToolCallFunction {
                        name: "f".to_string(),
                        arguments: "not json".to_string(),
                    },
                }]),
                tool_call_id: None,
            }],
            tools: None,
            tool_choice: None,
            stream: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolUse { input, .. } => assert_eq!(*input, json!({})),
                other => panic!("应为 ToolUse,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn openai_to_hub_tool_message_becomes_user_tool_result() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage {
                role: "tool".to_string(),
                content: Some(OpenAiContent::Text("ok".to_string())),
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
            }],
            tools: None,
            tool_choice: None,
            stream: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        let msg = &hub.messages[0];
        assert_eq!(msg.role, "user");
        match &msg.content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    assert_eq!(tool_use_id, "c1");
                    assert_eq!(content, "ok");
                    assert_eq!(*is_error, None);
                }
                other => panic!("应为 ToolResult,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn openai_to_hub_image_data_url_becomes_image_block() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(OpenAiContent::Parts(vec![
                    ContentPart::Text {
                        text: "look".to_string(),
                    },
                    ContentPart::ImageUrl {
                        image_url: ImageUrl {
                            url: "data:image/png;base64,AAAA".to_string(),
                        },
                    },
                ])),
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: None,
            tool_choice: None,
            stream: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
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
    fn openai_to_hub_remote_url_image_preserved_as_url_block_not_dropped() {
        // #12:远程 http(s) 图片 URL 不再静默丢弃,而是编码成 url 型图片块,
        // 由中枢层拦成 400。
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(OpenAiContent::Parts(vec![ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "https://example.com/a.png".to_string(),
                    },
                }])),
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: None,
            tool_choice: None,
            stream: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    Block::Image { source } => {
                        assert_eq!(source["type"], "url");
                        assert_eq!(source["url"], "https://example.com/a.png");
                    }
                    other => panic!("应为 url 型 Image,实际: {other:?}"),
                }
            }
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn hub_to_openai_text_response() {
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
        let completion = hub_to_openai(resp, 1_700_000_000);
        assert_eq!(completion.choices[0].message.content.as_deref(), Some("hi"));
        assert_eq!(completion.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(completion.usage.total_tokens, 8);
        assert_eq!(completion.model, "claude-sonnet-4.5");
        assert!(completion.id.starts_with("chatcmpl-"));
    }

    #[test]
    fn hub_to_openai_tool_use_response() {
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
        let completion = hub_to_openai(resp, 1_700_000_000);
        assert_eq!(completion.choices[0].message.content, None);
        let tool_calls = completion.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("tool_calls 应存在");
        assert_eq!(tool_calls[0].function.name, "get_weather");
        let expected_args = serde_json::to_string(&json!({"city": "Paris"})).expect("序列化失败");
        assert_eq!(tool_calls[0].function.arguments, expected_args);
        assert_eq!(
            completion.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
    }

    #[test]
    fn finish_reason_from_stop_maps_correctly() {
        assert_eq!(
            finish_reason_from_stop(Some("tool_use")).as_deref(),
            Some("tool_calls")
        );
        assert_eq!(
            finish_reason_from_stop(Some("end_turn")).as_deref(),
            Some("stop")
        );
        assert_eq!(finish_reason_from_stop(None).as_deref(), Some("stop"));
    }

    #[test]
    fn finish_reason_from_stop_maps_truncation_to_length() {
        // #11:上游截断 → OpenAI finish_reason="length"。
        assert_eq!(
            finish_reason_from_stop(Some("max_tokens")).as_deref(),
            Some("length")
        );
        assert_eq!(
            finish_reason_from_stop(Some("model_context_window_exceeded")).as_deref(),
            Some("length")
        );
    }
}
