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

/// `developer` 是 OpenAI 新版规范里 `system` 的更名,语义相同;两者都并入中枢 `system` 提示词。
fn is_system_role(role: &str) -> bool {
    role == "system" || role == "developer"
}

/// 把消息 `content` 拍平成纯文本:裸字符串取自身;数组形按 parts 顺序拼接其中的文本块
/// (图片块无文本可拼,跳过)。拼接不加分隔符,与 user 角色数组形被拆成多个文本块后
/// 由中枢 `InMsg::text()` 直接 concat 的口径一致。
fn content_text(content: &OpenAiContent) -> String {
    match content {
        OpenAiContent::Text(s) => s.clone(),
        OpenAiContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .concat(),
    }
}

/// 把单条 OpenAI 消息转换成 0~1 条中枢消息(system/developer 由调用方单独处理,这里不产出)。
fn convert_message(msg: &ChatMessage) -> Option<InMsg> {
    if is_system_role(&msg.role) {
        return None;
    }
    if let Some(tool_call_id) = &msg.tool_call_id {
        // tool 角色的 content 同样可能是数组形(LangChain 等客户端即如此发),必须按 parts
        // 拼出文本;丢成空串等于把整个工具结果吞掉,模型只能反复重发同一个工具调用。
        let text_val = match &msg.content {
            Some(content) => Value::String(content_text(content)),
            None => Value::String(String::new()),
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

/// 把中枢 `ContentIn` 归一成块序列;裸字符串 → 单个文本块(空串不产块,免得凭空多出空文本块)。
fn into_blocks(content: ContentIn) -> Vec<Block> {
    match content {
        ContentIn::Text(s) if s.is_empty() => Vec::new(),
        ContentIn::Text(s) => vec![Block::Text { text: s }],
        ContentIn::Blocks(blocks) => blocks,
    }
}

/// 这条消息里是否已有非空文本块(决定合并时要不要补空行分隔)。
fn has_text(blocks: &[Block]) -> bool {
    blocks
        .iter()
        .any(|b| matches!(b, Block::Text { text } if !text.is_empty()))
}

/// 把 `next` 并进相邻的同角色消息 `prev`(块序列顺接,顺序即客户端给的顺序)。
///
/// 两边都有文本时,在 `next` 的首个非空文本块前补 `\n\n`:中枢 `InMsg::text()` 是无缝
/// concat,不补分隔会把两轮文本粘成一个词(`"hi"+"there"` → `"hithere"`);`\n\n` 与本文件
/// 多段 system 的 `join("\n\n")` 口径一致。工具结果/图片块不参与文本拍平,故纯工具结果的
/// 合并不会平白多出空行。
fn merge_same_role(prev: &mut InMsg, next: InMsg) {
    let mut blocks = into_blocks(std::mem::replace(
        &mut prev.content,
        ContentIn::Blocks(Vec::new()),
    ));
    let mut incoming = into_blocks(next.content);
    if has_text(&blocks) {
        for block in incoming.iter_mut() {
            if let Block::Text { text } = block
                && !text.is_empty()
            {
                text.insert_str(0, "\n\n");
                break;
            }
        }
    }
    blocks.append(&mut incoming);
    prev.content = ContentIn::Blocks(blocks);
}

/// 把 OpenAI `ChatCompletionRequest` 转换成中枢 `MessagesRequest`。
///
/// **相邻同角色消息必须合并成一轮**:OpenAI 线格式把一次并行工具调用的应答拆成 N 条独立的
/// `role:"tool"` 消息(一条 assistant `tool_calls:[c1,c2]` 后面跟两条 tool 消息),而中枢/
/// Anthropic 口径是「同一条用户消息里放齐所有 tool_result」,下游 `anthropic_to_kiro` 又是把
/// 中枢消息 1:1 铺进 Kiro history、末条当 currentMessage。逐条不合并的话,声明了两个 toolUses
/// 的助手轮后面只跟着带一个 toolResult 的用户轮,另一个 toolResult 被挤到 currentMessage,
/// 配对被拆散、history 还以用户轮收尾(user/assistant 交替也断了),上游会当成工具调用未被应答。
/// 合并同样兜住「工具结果后又追一条 user 文本」「客户端把助手文本与 tool_calls 拆成两条 assistant」
/// 这些同类写法。
pub fn openai_to_hub(req: ChatCompletionRequest) -> MessagesRequest {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<InMsg> = Vec::new();

    for msg in &req.messages {
        if is_system_role(&msg.role) {
            if let Some(content) = &msg.content {
                let text = content_text(content);
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            continue;
        }
        if let Some(hub_msg) = convert_message(msg) {
            // 只在角色相邻相同时合并;单条消息(happy path)原样入列,连 ContentIn::Text
            // 都不块化,形状与改动前完全一致。
            match messages.last_mut() {
                Some(prev) if prev.role == hub_msg.role => merge_same_role(prev, hub_msg),
                _ => messages.push(hub_msg),
            }
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
        // Kiro 数据面 wire 无 tool_choice 对应字段(见 `kiro::convert::anthropic_to_kiro`),
        // 故只原样带进中枢请求,链路末端故意不转发,也不改写成提示词伪装成已生效。
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

    /// 一条带若干并行 tool_calls 的 assistant 消息(OpenAI 客户端回灌上一轮时的写法)。
    fn assistant_tool_calls_msg(calls: &[(&str, &str, &str)]) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(
                calls
                    .iter()
                    .map(|(id, name, args)| ToolCall {
                        id: (*id).to_string(),
                        kind: "function".to_string(),
                        function: OaToolCallFunction {
                            name: (*name).to_string(),
                            arguments: (*args).to_string(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
        }
    }

    /// 一条 `role:"tool"` 的工具结果消息。
    fn tool_msg(call_id: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: Some(OpenAiContent::Text(text.to_string())),
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
        }
    }

    fn req_with(messages: Vec<ChatMessage>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages,
            tools: None,
            tool_choice: None,
            stream: None,
            stream_options: None,
            max_tokens: None,
        }
    }

    /// 取出一条中枢消息里的工具结果 id 列表(顺序敏感)。
    fn tool_result_ids(msg: &InMsg) -> Vec<String> {
        match &msg.content {
            ContentIn::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                    _ => None,
                })
                .collect(),
            ContentIn::Text(_) => Vec::new(),
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
            stream_options: None,
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
            stream_options: None,
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
            stream_options: None,
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
            stream_options: None,
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
            stream_options: None,
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
            stream_options: None,
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
    fn openai_to_hub_tool_message_with_content_parts_keeps_text() {
        // 数组形 content 的 tool 消息(LangChain 等客户端写法):工具结果必须原样带上,
        // 不能被拍成空串。
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage {
                role: "tool".to_string(),
                content: Some(OpenAiContent::Parts(vec![
                    ContentPart::Text {
                        text: "sunny".to_string(),
                    },
                    ContentPart::Text {
                        text: " 25C".to_string(),
                    },
                ])),
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
            }],
            tools: None,
            tool_choice: None,
            stream: None,
            stream_options: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        match &hub.messages[0].content {
            ContentIn::Blocks(blocks) => match &blocks[0] {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    assert_eq!(tool_use_id, "c1");
                    assert_eq!(content, "sunny 25C");
                }
                other => panic!("应为 ToolResult,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn openai_to_hub_system_content_parts_joined_into_system_prompt() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: Some(OpenAiContent::Parts(vec![
                        ContentPart::Text {
                            text: "be ".to_string(),
                        },
                        ContentPart::Text {
                            text: "nice".to_string(),
                        },
                    ])),
                    tool_calls: None,
                    tool_call_id: None,
                },
                user_msg("hi"),
            ],
            tools: None,
            tool_choice: None,
            stream: None,
            stream_options: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        assert_eq!(
            hub.system.as_ref().map(|s| s.text()).as_deref(),
            Some("be nice")
        );
        assert_eq!(hub.messages.len(), 1);
    }

    #[test]
    fn openai_to_hub_developer_role_becomes_system() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![
                ChatMessage {
                    role: "developer".to_string(),
                    content: Some(OpenAiContent::Text("be nice".to_string())),
                    tool_calls: None,
                    tool_call_id: None,
                },
                user_msg("hi"),
            ],
            tools: None,
            tool_choice: None,
            stream: None,
            stream_options: None,
            max_tokens: None,
        };
        let hub = openai_to_hub(req);
        assert_eq!(
            hub.system.as_ref().map(|s| s.text()).as_deref(),
            Some("be nice")
        );
        // developer 不得再作为普通消息进 messages。
        assert_eq!(hub.messages.len(), 1);
        assert_eq!(hub.messages[0].role, "user");
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
            stream_options: None,
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
            stream_options: None,
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
    fn openai_to_hub_parallel_tool_results_merge_into_one_user_message() {
        // 并行工具调用回灌:OpenAI 线格式规定「一条 assistant(tool_calls:[c1,c2]) + 两条
        // 独立的 role:"tool"」。若逐条各自成一条中枢消息,声明了两个 toolUses 的助手轮
        // 后面就只跟着带一个 toolResult 的用户轮,配对被拆散。
        let req = req_with(vec![
            user_msg("weather in Paris and Tokyo?"),
            assistant_tool_calls_msg(&[
                ("c1", "get_weather", "{\"city\":\"Paris\"}"),
                ("c2", "get_weather", "{\"city\":\"Tokyo\"}"),
            ]),
            tool_msg("c1", "sunny"),
            tool_msg("c2", "rainy"),
        ]);
        let hub = openai_to_hub(req);

        assert_eq!(hub.messages.len(), 3, "两条 tool 消息必须并成一条用户轮");
        assert_eq!(hub.messages[0].role, "user");
        assert_eq!(hub.messages[1].role, "assistant");
        assert_eq!(hub.messages[2].role, "user");
        // 顺序必须与 tool_calls 的声明顺序一致。
        assert_eq!(tool_result_ids(&hub.messages[2]), vec!["c1", "c2"]);
        // 工具结果文本不能在合并中丢失。
        match &hub.messages[2].content {
            ContentIn::Blocks(blocks) => match (&blocks[0], &blocks[1]) {
                (Block::ToolResult { content: a, .. }, Block::ToolResult { content: b, .. }) => {
                    assert_eq!(a, "sunny");
                    assert_eq!(b, "rainy");
                }
                other => panic!("应为两个 ToolResult,实际: {other:?}"),
            },
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn openai_to_hub_parallel_tool_results_keep_pairing_on_kiro_wire() {
        // 端到端复核 Kiro 数据面线上形状:history 必须 user/assistant 严格交替且以助手轮收尾,
        // 声明 2 个 toolUses 的助手轮之后,2 个 toolResults 必须同在紧随的那一轮里。
        use crate::kiro::convert::anthropic_to_kiro;
        use crate::kiro::wire::HistoryItem;

        let mut req = req_with(vec![
            user_msg("weather in Paris and Tokyo?"),
            assistant_tool_calls_msg(&[
                ("c1", "get_weather", "{\"city\":\"Paris\"}"),
                ("c2", "get_weather", "{\"city\":\"Tokyo\"}"),
            ]),
            tool_msg("c1", "sunny"),
            tool_msg("c2", "rainy"),
        ]);
        req.model = "claude-sonnet-4.5".to_string();
        let hub = openai_to_hub(req);
        let kiro = anthropic_to_kiro(&hub, None).expect("转换应成功");

        let history = &kiro.conversation_state.history;
        assert_eq!(history.len(), 2, "history 应为 [用户轮, 助手轮]");
        assert!(
            matches!(history[0], HistoryItem::UserInputMessage { .. }),
            "history[0] 应为用户轮,实际: {:?}",
            history[0]
        );
        let HistoryItem::AssistantResponseMessage {
            assistant_response_message,
        } = &history[1]
        else {
            panic!("history 必须以助手轮收尾,实际: {:?}", history[1]);
        };
        let tool_uses = assistant_response_message
            .tool_uses
            .as_ref()
            .expect("助手轮应带 toolUses");
        assert_eq!(tool_uses.len(), 2);

        let current_results = kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tool_results
            .as_ref()
            .expect("当前轮应带 toolResults");
        assert_eq!(
            current_results.len(),
            2,
            "两个 toolUses 必须由同一轮里的两个 toolResults 应答"
        );
        assert_eq!(current_results[0].tool_use_id, "c1");
        assert_eq!(current_results[1].tool_use_id, "c2");
    }

    #[test]
    fn openai_to_hub_tool_result_then_user_text_merge_into_one_turn() {
        // 工具结果之后客户端又追一条 user 文本:两者同为用户轮,必须并成一轮,
        // 否则 history 会以用户轮收尾、currentMessage 又是用户轮,交替被打破。
        let req = req_with(vec![
            user_msg("weather?"),
            assistant_tool_calls_msg(&[("c1", "get_weather", "{}")]),
            tool_msg("c1", "sunny"),
            user_msg("and Berlin?"),
        ]);
        let hub = openai_to_hub(req);

        assert_eq!(hub.messages.len(), 3);
        assert_eq!(hub.messages[2].role, "user");
        assert_eq!(tool_result_ids(&hub.messages[2]), vec!["c1"]);
        // 工具结果块本身不参与文本拍平,故这一轮的文本就是追加的那句用户输入。
        assert_eq!(hub.messages[2].text(), "and Berlin?");
    }

    #[test]
    fn openai_to_hub_consecutive_user_texts_join_with_blank_line() {
        // 相邻同角色消息合并时,两段文本之间必须留空行分隔,不能无缝粘成一个词。
        let req = req_with(vec![user_msg("hi"), user_msg("there")]);
        let hub = openai_to_hub(req);
        assert_eq!(hub.messages.len(), 1);
        assert_eq!(hub.messages[0].role, "user");
        assert_eq!(hub.messages[0].text(), "hi\n\nthere");
    }

    #[test]
    fn openai_to_hub_consecutive_assistant_messages_merge() {
        // 部分客户端把「助手文本」与「助手 tool_calls」拆成两条 assistant 消息回灌,
        // 合并后文本与 toolUses 必须同属一轮。
        let req = req_with(vec![
            user_msg("weather?"),
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(OpenAiContent::Text("let me check".to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
            assistant_tool_calls_msg(&[("c1", "get_weather", "{}")]),
        ]);
        let hub = openai_to_hub(req);

        assert_eq!(hub.messages.len(), 2);
        assert_eq!(hub.messages[1].role, "assistant");
        assert_eq!(hub.messages[1].text(), "let me check");
        match &hub.messages[1].content {
            ContentIn::Blocks(blocks) => {
                assert!(
                    blocks
                        .iter()
                        .any(|b| matches!(b, Block::ToolUse { id, .. } if id == "c1")),
                    "合并后应保留 ToolUse,实际: {blocks:?}"
                );
            }
            other => panic!("应为 Blocks,实际: {other:?}"),
        }
    }

    #[test]
    fn openai_to_hub_single_user_message_stays_plain_text() {
        // 无相邻同角色消息时不做任何改写:单条纯文本消息仍是 ContentIn::Text(不被块化)。
        let req = req_with(vec![system_msg("s"), user_msg("hi")]);
        let hub = openai_to_hub(req);
        assert_eq!(hub.messages.len(), 1);
        assert!(
            matches!(&hub.messages[0].content, ContentIn::Text(s) if s == "hi"),
            "happy path 不应被块化,实际: {:?}",
            hub.messages[0].content
        );
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
