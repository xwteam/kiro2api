//! Gemini ↔ 中枢转换自写;形状照 Google 公开规范/中枢既有(`anthropic::types`)。
use std::collections::HashMap;

use super::types::{
    Candidate, Content, FunctionCall, FunctionResponse, GenerateContentRequest,
    GenerateContentResponse, Part, UsageMetadata,
};
use crate::kiro::convert::Truncation;
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

/// `functionCall` / `functionResponse` 的 `tool_use_id` 分配器。
///
/// Gemini 线格式里这两个 part 的 `id` 是可选的:带了就直接用(唯一性由客户端负责);
/// 缺失时按「函数名 + 该名在本请求内的出现序号」生成——**不能**拿函数名当 id,
/// 否则同一轮里并行调用同一个函数会产生重复 id,中枢按 id 归并时会把它们并成一条。
///
/// 调用与结果各用一套计数器:一次 `functionCall` 之后必跟一次 `functionResponse`,
/// 两侧同名序号天然对齐,故即便双方都没带 id,tool_use 与 tool_result 仍能配上对。
/// 生成式 `{name}_{n}`(`n` 为纯数字)在名字集合上是单射,不会与另一函数名撞车。
#[derive(Default)]
struct ToolIdAlloc {
    calls: HashMap<String, u32>,
    responses: HashMap<String, u32>,
}

impl ToolIdAlloc {
    /// 取该函数名的下一个序号并拼出 id(序号自 0 起,确定性、不含随机数)。
    fn next(counter: &mut HashMap<String, u32>, name: &str) -> String {
        let n = counter.entry(name.to_string()).or_insert(0);
        let id = format!("{name}_{n}");
        *n += 1;
        id
    }

    fn call_id(&mut self, call: &FunctionCall) -> String {
        match call.id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => Self::next(&mut self.calls, &call.name),
        }
    }

    fn response_id(&mut self, resp: &FunctionResponse) -> String {
        match resp.id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => Self::next(&mut self.responses, &resp.name),
        }
    }
}

/// 把单个 Gemini `Part` 转换成 0~1 个中枢 `Block`;`ids` 供工具 part 取 `tool_use_id`。
fn convert_part(part: &Part, ids: &mut ToolIdAlloc) -> Option<Block> {
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
            id: ids.call_id(call),
            name: call.name.clone(),
            input: call.args.clone(),
        });
    }
    if let Some(resp) = &part.function_response {
        return Some(Block::ToolResult {
            tool_use_id: ids.response_id(resp),
            content: response_value_to_text(&resp.response),
            is_error: None,
        });
    }
    None
}

/// 把单个 Gemini `Content` 转换成中枢 `InMsg`(role 映射:`"model"`→`"assistant"`,其余→`"user"`)。
fn convert_content(content: &Content, ids: &mut ToolIdAlloc) -> InMsg {
    let role = match content.role.as_deref() {
        Some("model") => "assistant",
        _ => "user",
    };
    let blocks: Vec<Block> = content
        .parts
        .iter()
        .filter_map(|p| convert_part(p, ids))
        .collect();
    InMsg {
        role: role.to_string(),
        content: ContentIn::Blocks(blocks),
    }
}

/// 读出 `toolConfig.functionCallingConfig.mode`(统一成大写);缺失/形状不符 → `None`。
fn tool_calling_mode(tool_config: Option<&Value>) -> Option<String> {
    tool_config?
        .get("functionCallingConfig")?
        .get("mode")?
        .as_str()
        .map(|s| s.to_ascii_uppercase())
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

    let mut tool_ids = ToolIdAlloc::default();
    let messages: Vec<InMsg> = req
        .contents
        .iter()
        .map(|c| convert_content(c, &mut tool_ids))
        .collect();

    let mode = tool_calling_mode(req.tool_config.as_ref());

    let mut tools: Option<Vec<ToolDef>> = req.tools.map(|tools| {
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

    // `mode: "NONE"` = 本轮禁止调用函数。Kiro 数据面 wire 没有 tool_choice 之类的字段
    // (见 `kiro::convert::anthropic_to_kiro`),唯一能忠实兑现这个约束的手段就是不下发
    // 工具规格。`AUTO` 即默认行为;`ANY`(强制至少调一次)在 wire 上无从表达,只能照默认走。
    if mode.as_deref() == Some("NONE") {
        tools = None;
    }

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
        // toolConfig 故意不塞进 tool_choice:中枢的 tool_choice 是 Anthropic 形状,这里拿到的是
        // Gemini 原始形状,直通只会让下游误以为已生效;而 Kiro 数据面 wire 本就没有对应字段,
        // 转发到底也是空转。唯一可兑现的 `NONE` 已在上面以「不下发工具」如实落地。
        tool_choice: None,
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

/// 把流式读帧时探到的上游截断信号映射成 Gemini `finishReason`。
///
/// 流式没有中枢 `stop_reason` 可用(帧是逐个到的),故按 [`Truncation`] 直接判:
/// 命中任一截断 → `"MAX_TOKENS"`,否则 `"STOP"`。与 [`finish_reason_gemini`] 同一口径。
pub fn finish_reason_for_truncation(truncation: Option<Truncation>) -> String {
    match truncation {
        Some(_) => "MAX_TOKENS",
        None => "STOP",
    }
    .to_string()
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
            // 把中枢 tool_use_id 原样带进 `functionCall.id`:客户端回填 functionResponse 时
            // 带上它,就能在并行同名调用里精确配对(缺 id 才回落到按同名序号配对)。
            OutBlock::ToolUse { id, name, input } => Part {
                text: None,
                inline_data: None,
                function_call: Some(FunctionCall {
                    id: Some(id.clone()),
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
        FunctionDeclaration, GeminiTool, GenerationConfig, InlineData,
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

    /// 无 id 的 functionCall:id 由「函数名 + 序号」生成,**不**等于函数名本身。
    #[test]
    fn gemini_to_hub_function_call_part_becomes_tool_use_block() {
        let req = GenerateContentRequest {
            contents: vec![Content {
                role: Some("model".to_string()),
                parts: vec![Part {
                    text: None,
                    inline_data: None,
                    function_call: Some(FunctionCall {
                        id: None,
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
                    assert_eq!(id, "get_weather_0");
                    assert_eq!(input["city"], "Paris");
                }
                other => panic!("应为 ToolUse,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    /// 同一轮里并行调用同名函数:两次调用必须拿到**不同**的 tool_use_id,
    /// 否则中枢按 id 归并会把两次调用并成一条。
    #[test]
    fn gemini_to_hub_parallel_same_name_calls_get_distinct_ids() {
        let call = |city: &str| Part {
            text: None,
            inline_data: None,
            function_call: Some(FunctionCall {
                id: None,
                name: "get_weather".to_string(),
                args: json!({"city": city}),
            }),
            function_response: None,
        };
        let req = GenerateContentRequest {
            contents: vec![Content {
                role: Some("model".to_string()),
                parts: vec![call("Paris"), call("Rome")],
            }],
            system_instruction: None,
            tools: None,
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        let ContentIn::Blocks(blocks) = &hub.messages[0].content else {
            panic!("应为 Blocks");
        };
        let ids: Vec<&str> = blocks
            .iter()
            .map(|b| match b {
                Block::ToolUse { id, .. } => id.as_str(),
                other => panic!("应为 ToolUse,实际: {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec!["get_weather_0", "get_weather_1"]);
    }

    /// 客户端自带 id 时优先使用它(不再被函数名覆盖)。
    #[test]
    fn gemini_to_hub_prefers_client_supplied_tool_ids() {
        let req = GenerateContentRequest {
            contents: vec![
                Content {
                    role: Some("model".to_string()),
                    parts: vec![Part {
                        text: None,
                        inline_data: None,
                        function_call: Some(FunctionCall {
                            id: Some("call-42".to_string()),
                            name: "get_weather".to_string(),
                            args: json!({}),
                        }),
                        function_response: None,
                    }],
                },
                Content {
                    role: Some("user".to_string()),
                    parts: vec![Part {
                        text: None,
                        inline_data: None,
                        function_call: None,
                        function_response: Some(FunctionResponse {
                            id: Some("call-42".to_string()),
                            name: "get_weather".to_string(),
                            response: json!("ok"),
                        }),
                    }],
                },
            ],
            system_instruction: None,
            tools: None,
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        let ContentIn::Blocks(call_blocks) = &hub.messages[0].content else {
            panic!("应为 Blocks");
        };
        let ContentIn::Blocks(resp_blocks) = &hub.messages[1].content else {
            panic!("应为 Blocks");
        };
        match (&call_blocks[0], &resp_blocks[0]) {
            (Block::ToolUse { id, .. }, Block::ToolResult { tool_use_id, .. }) => {
                assert_eq!(id, "call-42");
                assert_eq!(tool_use_id, "call-42");
            }
            other => panic!("应为 ToolUse + ToolResult,实际: {other:?}"),
        }
    }

    /// 双方都没带 id 时,调用与结果各自按同名序号计数,仍能配上对。
    #[test]
    fn gemini_to_hub_pairs_generated_ids_across_turns() {
        let call = || Part {
            text: None,
            inline_data: None,
            function_call: Some(FunctionCall {
                id: None,
                name: "f".to_string(),
                args: json!({}),
            }),
            function_response: None,
        };
        let result = || Part {
            text: None,
            inline_data: None,
            function_call: None,
            function_response: Some(FunctionResponse {
                id: None,
                name: "f".to_string(),
                response: json!("ok"),
            }),
        };
        let req = GenerateContentRequest {
            contents: vec![
                Content {
                    role: Some("model".to_string()),
                    parts: vec![call(), call()],
                },
                Content {
                    role: Some("user".to_string()),
                    parts: vec![result(), result()],
                },
            ],
            system_instruction: None,
            tools: None,
            tool_config: None,
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        let ContentIn::Blocks(call_blocks) = &hub.messages[0].content else {
            panic!("应为 Blocks");
        };
        let ContentIn::Blocks(resp_blocks) = &hub.messages[1].content else {
            panic!("应为 Blocks");
        };
        for i in 0..2 {
            match (&call_blocks[i], &resp_blocks[i]) {
                (Block::ToolUse { id, .. }, Block::ToolResult { tool_use_id, .. }) => {
                    assert_eq!(id, &format!("f_{i}"));
                    assert_eq!(tool_use_id, id);
                }
                other => panic!("应为 ToolUse + ToolResult,实际: {other:?}"),
            }
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
                        id: None,
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
                    assert_eq!(tool_use_id, "get_weather_0");
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
                        id: None,
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
        // 中枢 tool_use_id 随 functionCall.id 下发,供客户端回填时精确配对。
        assert_eq!(call.id.as_deref(), Some("tu1"));
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.args["city"], "Paris");
        assert_eq!(gemini.candidates[0].finish_reason.as_deref(), Some("STOP"));
    }

    /// toolConfig 不再直通中枢 `tool_choice`(它是 Anthropic 形状且全链路无人消费,
    /// 直通只会让下游误以为已生效)。
    #[test]
    fn gemini_to_hub_does_not_forward_tool_config_as_tool_choice() {
        let req = GenerateContentRequest {
            contents: vec![user_content("hi")],
            system_instruction: None,
            tools: Some(vec![GeminiTool {
                function_declarations: vec![FunctionDeclaration {
                    name: "f".to_string(),
                    description: None,
                    parameters: json!({}),
                }],
            }]),
            tool_config: Some(json!({"functionCallingConfig": {"mode": "ANY"}})),
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        assert_eq!(hub.tool_choice, None);
        // ANY 无从在 wire 上表达,工具照常下发。
        assert_eq!(hub.tools.expect("tools 应存在").len(), 1);
    }

    /// `mode: "NONE"`(禁止调用函数)以「不下发工具规格」如实兑现,不再静默失效。
    #[test]
    fn gemini_to_hub_mode_none_drops_tools() {
        let req = GenerateContentRequest {
            contents: vec![user_content("hi")],
            system_instruction: None,
            tools: Some(vec![GeminiTool {
                function_declarations: vec![FunctionDeclaration {
                    name: "f".to_string(),
                    description: None,
                    parameters: json!({}),
                }],
            }]),
            tool_config: Some(json!({"functionCallingConfig": {"mode": "none"}})),
            generation_config: None,
        };
        let hub = gemini_to_hub(req, "m".to_string());
        assert_eq!(hub.tools, None);
        assert_eq!(hub.tool_choice, None);
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

    /// 流式收尾:探到截断信号报 MAX_TOKENS,没探到才是 STOP。
    #[test]
    fn finish_reason_for_truncation_maps_both_kinds() {
        assert_eq!(finish_reason_for_truncation(None), "STOP");
        assert_eq!(
            finish_reason_for_truncation(Some(Truncation::MaxTokens)),
            "MAX_TOKENS"
        );
        assert_eq!(
            finish_reason_for_truncation(Some(Truncation::ContextWindow)),
            "MAX_TOKENS"
        );
    }

    #[test]
    fn tool_calling_mode_reads_nested_mode() {
        let cfg = json!({"functionCallingConfig": {"mode": "any"}});
        assert_eq!(tool_calling_mode(Some(&cfg)).as_deref(), Some("ANY"));
        assert_eq!(tool_calling_mode(None), None);
        assert_eq!(tool_calling_mode(Some(&json!({}))), None);
        assert_eq!(
            tool_calling_mode(Some(&json!({"functionCallingConfig": {}}))),
            None
        );
    }
}
