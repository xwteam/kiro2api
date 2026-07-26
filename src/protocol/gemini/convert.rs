//! Gemini ↔ 中枢转换自写;形状照 Google 公开规范/中枢既有(`anthropic::types`)。
use super::types::{
    Candidate, Content, FunctionCall, GenerateContentRequest, GenerateContentResponse, Part,
    UsageMetadata,
};
use crate::protocol::anthropic::types::{
    Block, ContentIn, InMsg, MessagesRequest, MessagesResponse, OutBlock, SystemPrompt, ToolDef,
};
use serde_json::{Value, json};

/// 把中枢 `Value` 响应(工具执行结果)转成纯文本:字符串原样,其它类型 JSON 字符串化。
fn response_value_to_text(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(s.clone()),
        other => Value::String(serde_json::to_string(other).unwrap_or_default()),
    }
}

/// 把单个 Gemini `Part` 转换成 0~1 个中枢 `Block`。
fn convert_part(part: &Part) -> Option<Block> {
    if let Some(text) = &part.text {
        return Some(Block::Text { text: text.clone() });
    }
    if let Some(inline) = &part.inline_data {
        return Some(Block::Image {
            source: json!({"type": "base64", "media_type": inline.mime_type, "data": inline.data}),
        });
    }
    if let Some(call) = &part.function_call {
        return Some(Block::ToolUse {
            id: call.name.clone(),
            name: call.name.clone(),
            input: call.args.clone(),
        });
    }
    if let Some(resp) = &part.function_response {
        return Some(Block::ToolResult {
            tool_use_id: resp.name.clone(),
            content: response_value_to_text(&resp.response),
            is_error: None,
        });
    }
    None
}

/// 把单个 Gemini `Content` 转换成中枢 `InMsg`(role 映射:`"model"`→`"assistant"`,其余→`"user"`)。
fn convert_content(content: &Content) -> InMsg {
    let role = match content.role.as_deref() {
        Some("model") => "assistant",
        _ => "user",
    };
    let blocks: Vec<Block> = content.parts.iter().filter_map(convert_part).collect();
    InMsg {
        role: role.to_string(),
        content: ContentIn::Blocks(blocks),
    }
}

/// 把 Gemini `GenerateContentRequest` 转换成中枢 `MessagesRequest`。
pub fn gemini_to_hub(req: GenerateContentRequest, model: String) -> MessagesRequest {
    let system = req.system_instruction.as_ref().map(|sys| {
        sys.parts
            .iter()
            .filter_map(|p| p.text.as_deref())
            .collect::<Vec<_>>()
            .concat()
    });

    let messages: Vec<InMsg> = req.contents.iter().map(convert_content).collect();

    let tools = req.tools.map(|tools| {
        tools
            .into_iter()
            .flat_map(|t| t.function_declarations)
            .map(|fd| ToolDef {
                name: fd.name,
                description: fd.description,
                input_schema: fd.parameters,
            })
            .collect()
    });

    let max_tokens = req
        .generation_config
        .as_ref()
        .and_then(|g| g.max_output_tokens);

    MessagesRequest {
        model,
        system: system.map(SystemPrompt::Text),
        messages,
        max_tokens,
        stream: None,
        tools,
        tool_choice: req.tool_config,
    }
}

/// 把中枢 `stop_reason` 映射成 Gemini `finishReason`,流式复用。
///
/// 上游截断(`max_tokens` / `model_context_window_exceeded`)→ Gemini `"MAX_TOKENS"`;
/// 其余(含 `tool_use` / `end_turn`)→ `"STOP"`(Gemini 的工具调用同样以 `STOP` 收尾,
/// 工具意图由 `functionCall` part 表达,不占 finishReason)。
pub fn finish_reason_gemini(stop: Option<&str>) -> Option<String> {
    Some(
        match stop {
            Some("max_tokens") | Some("model_context_window_exceeded") => "MAX_TOKENS",
            _ => "STOP",
        }
        .to_string(),
    )
}

/// 把中枢 `MessagesResponse` 转换成 Gemini `GenerateContentResponse`。
pub fn hub_to_gemini(resp: MessagesResponse) -> GenerateContentResponse {
    let parts: Vec<Part> = resp
        .content
        .iter()
        .map(|block| match block {
            OutBlock::Text { text } => Part {
                text: Some(text.clone()),
                inline_data: None,
                function_call: None,
                function_response: None,
            },
            OutBlock::ToolUse { name, input, .. } => Part {
                text: None,
                inline_data: None,
                function_call: Some(FunctionCall {
                    name: name.clone(),
                    args: input.clone(),
                }),
                function_response: None,
            },
        })
        .collect();

    let candidate = Candidate {
        content: Content {
            role: Some("model".to_string()),
            parts,
        },
        finish_reason: finish_reason_gemini(resp.stop_reason.as_deref()),
        index: 0,
    };

    let prompt_token_count = resp.usage.input_tokens;
    let candidates_token_count = resp.usage.output_tokens;
    let usage_metadata = Some(UsageMetadata {
        prompt_token_count,
        candidates_token_count,
        total_token_count: prompt_token_count + candidates_token_count,
    });

    GenerateContentResponse {
        candidates: vec![candidate],
        usage_metadata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::anthropic::types::Usage;
    use crate::protocol::gemini::types::{
        FunctionDeclaration, FunctionResponse, GeminiTool, GenerationConfig, InlineData,
    };

    fn user_content(text: &str) -> Content {
        Content {
            role: Some("user".to_string()),
            parts: vec![Part {
                text: Some(text.to_string()),
                inline_data: None,
                function_call: None,
                function_response: None,
            }],
        }
    }

    #[test]
    fn gemini_to_hub_system_and_user() {
        let req = GenerateContentRequest {
            contents: vec![user_content("hi")],
            system_instruction: Some(Content {
                role: None,
                parts: vec![Part {
                    text: Some("s".to_string()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                }],
            }),
            tools: None,
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "claude-sonnet-4.5".to_string());
        assert_eq!(hub.system.as_ref().map(|s| s.text()).as_deref(), Some("s"));
        let last = hub.messages.last().expect("应有消息");
        assert_eq!(last.role, "user");
        assert_eq!(last.text(), "hi");
        assert_eq!(hub.model, "claude-sonnet-4.5");
    }

    #[test]
    fn gemini_to_hub_maps_tools() {
        let req = GenerateContentRequest {
            contents: vec![user_content("hi")],
            system_instruction: None,
            tools: Some(vec![GeminiTool {
                function_declarations: vec![FunctionDeclaration {
                    name: "get_weather".to_string(),
                    description: Some("d".to_string()),
                    parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                }],
            }]),
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        let tools = hub.tools.expect("tools 应存在");
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(tools[0].description.as_deref(), Some("d"));
        assert_eq!(
            tools[0].input_schema["properties"]["city"]["type"],
            "string"
        );
    }

    #[test]
    fn gemini_to_hub_model_role_becomes_assistant() {
        let req = GenerateContentRequest {
            contents: vec![Content {
                role: Some("model".to_string()),
                parts: vec![Part {
                    text: Some("hey".to_string()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                }],
            }],
            system_instruction: None,
            tools: None,
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        assert_eq!(hub.messages[0].role, "assistant");
        assert_eq!(hub.messages[0].text(), "hey");
    }

    #[test]
    fn gemini_to_hub_function_call_part_becomes_tool_use_block() {
        let req = GenerateContentRequest {
            contents: vec![Content {
                role: Some("model".to_string()),
                parts: vec![Part {
                    text: None,
                    inline_data: None,
                    function_call: Some(FunctionCall {
                        name: "get_weather".to_string(),
                        args: json!({"city": "Paris"}),
                    }),
                    function_response: None,
                }],
            }],
            system_instruction: None,
            tools: None,
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolUse { id, name, input } => {
                    assert_eq!(name, "get_weather");
                    assert_eq!(id, "get_weather");
                    assert_eq!(input["city"], "Paris");
                }
                other => panic!("应为 ToolUse,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn gemini_to_hub_function_response_part_becomes_tool_result_block() {
        let req = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part {
                    text: None,
                    inline_data: None,
                    function_call: None,
                    function_response: Some(FunctionResponse {
                        name: "get_weather".to_string(),
                        response: json!({"temp": 20}),
                    }),
                }],
            }],
            system_instruction: None,
            tools: None,
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    assert_eq!(tool_use_id, "get_weather");
                    assert_eq!(*is_error, None);
                    // 非字符串 response 应被字符串化为 JSON 文本
                    assert!(content.is_string());
                    let s = content.as_str().expect("应为字符串");
                    assert!(s.contains("temp"));
                }
                other => panic!("应为 ToolResult,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn gemini_to_hub_string_function_response_kept_as_is() {
        let req = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part {
                    text: None,
                    inline_data: None,
                    function_call: None,
                    function_response: Some(FunctionResponse {
                        name: "f".to_string(),
                        response: json!("ok"),
                    }),
                }],
            }],
            system_instruction: None,
            tools: None,
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolResult { content, .. } => assert_eq!(content, "ok"),
                other => panic!("应为 ToolResult,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn gemini_to_hub_inline_data_becomes_image_block() {
        let req = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part {
                    text: None,
                    inline_data: Some(InlineData {
                        mime_type: "image/png".to_string(),
                        data: "AAAA".to_string(),
                    }),
                    function_call: None,
                    function_response: None,
                }],
            }],
            system_instruction: None,
            tools: None,
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::Image { source } => {
                    assert_eq!(source["media_type"], "image/png");
                    assert_eq!(source["data"], "AAAA");
                }
                other => panic!("应为 Image,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn gemini_to_hub_max_output_tokens_maps_to_max_tokens() {
        let req = GenerateContentRequest {
            contents: vec![user_content("hi")],
            system_instruction: None,
            tools: None,
            tool_config: None,
            generation_config: Some(GenerationConfig {
                max_output_tokens: Some(256),
            }),
        };
        let hub = gemini_to_hub(req, "m".to_string());
        assert_eq!(hub.max_tokens, Some(256));
        assert_eq!(hub.stream, None);
    }

    #[test]
    fn hub_to_gemini_text_response() {
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
                input_tokens: 3,
                output_tokens: 5,
            },
        };
        let gemini = hub_to_gemini(resp);
        assert_eq!(
            gemini.candidates[0].content.parts[0].text.as_deref(),
            Some("hi")
        );
        assert_eq!(gemini.candidates[0].finish_reason.as_deref(), Some("STOP"));
        assert_eq!(gemini.candidates[0].content.role.as_deref(), Some("model"));
        let usage = gemini.usage_metadata.expect("usage_metadata 应存在");
        assert_eq!(usage.total_token_count, 8);
        assert_eq!(usage.prompt_token_count, 3);
        assert_eq!(usage.candidates_token_count, 5);
    }

    #[test]
    fn hub_to_gemini_tool_use_response() {
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
            usage: Usage {
                input_tokens: 10,
                output_tokens: 2,
            },
        };
        let gemini = hub_to_gemini(resp);
        let part = &gemini.candidates[0].content.parts[0];
        let call = part.function_call.as_ref().expect("function_call 应存在");
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.args["city"], "Paris");
        assert_eq!(gemini.candidates[0].finish_reason.as_deref(), Some("STOP"));
    }

    #[test]
    fn finish_reason_gemini_stop_for_normal_and_tool_use() {
        assert_eq!(
            finish_reason_gemini(Some("tool_use")).as_deref(),
            Some("STOP")
        );
        assert_eq!(
            finish_reason_gemini(Some("end_turn")).as_deref(),
            Some("STOP")
        );
        assert_eq!(finish_reason_gemini(None).as_deref(), Some("STOP"));
    }

    #[test]
    fn finish_reason_gemini_max_tokens_for_truncation() {
        // #11:上游截断 → Gemini finishReason="MAX_TOKENS"。
        assert_eq!(
            finish_reason_gemini(Some("max_tokens")).as_deref(),
            Some("MAX_TOKENS")
        );
        assert_eq!(
            finish_reason_gemini(Some("model_context_window_exceeded")).as_deref(),
            Some("MAX_TOKENS")
        );
    }
}
