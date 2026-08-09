//! Gemini ↔ 中枢转换自写;形状照 Google 公开规范/中枢既有(`anthropic::types`)。
use std::collections::{HashMap, HashSet};
use std::fmt;

use super::types::{
    Candidate, Content, FunctionCall, FunctionResponse, GenerateContentRequest,
    GenerateContentResponse, Part, UsageMetadata,
};
use crate::kiro::convert::Truncation;
use crate::protocol::anthropic::types::{
    Block, ContentIn, InMsg, MessagesRequest, MessagesResponse, OutBlock, SystemPrompt, ToolDef,
};
use serde_json::{Value, json};

/// Gemini → 中枢转换的致命错误:必须让调用方回 400,而不是硬着头皮往上游发一个注定被拒的请求。
///
/// 与中枢侧 [`ConvertError`](crate::kiro::convert::ConvertError) 同一套立场:附件送不到模型手上时
/// **明确报错、不静默丢弃**,否则调用方既看不到失败也不知道模型压根没收到东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiConvertError {
    /// `inlineData` 的 mimeType 不是 `image/*`(PDF / 音频 / 视频 …)。
    /// Gemini 规范允许这些类型,但 Kiro 数据面只有图片通道,无从承载。
    UnsupportedInlineData(String),
}

impl fmt::Display for GeminiConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GeminiConvertError::UnsupportedInlineData(mime) => write!(
                f,
                "不支持的内联附件类型({mime});上游只接受 image/* 内联图片,PDF/音频/视频请改为文本或图片后再发送"
            ),
        }
    }
}

impl std::error::Error for GeminiConvertError {}

/// `inlineData.mimeType` 是否为图片。MIME type 按 RFC 大小写不敏感,故先小写化再判。
fn is_image_mime(mime: &str) -> bool {
    mime.to_ascii_lowercase().starts_with("image/")
}

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
/// 结果侧缺 id 时**按同名调用的出现顺序回填**成那次调用的实际 id,而不是另起一套计数器。
///
/// 为什么不能各记各的:出站 `hub_to_gemini` 会把中枢 tool_use_id 带进 `functionCall.id`,而
/// Google SDK 的惯用法是把模型那条 Content **原样**塞回历史、再追加一条只有 `name`+`response`
/// 的 `functionResponse`(不回带 id)。于是调用侧取到真 id(如 `tooluse_abc`)、结果侧却生成
/// `{name}_{n}`,两边对不上 —— 中枢按 id 配对,工具结果就成了没人认领的孤儿,模型会反复重发
/// 同一个调用。改为回填后:客户端回带 id → 精确配对(并行同名调用也不串);不回带 id → 在
/// **最近一轮仍有未认领同名调用**的那轮里按顺序配(带 id 的结果也会占位,故两种结果混用不错位;
/// 按轮收窄则使"并行调用只被答了一部分"时不会被那个永不被答的旧调用一直拽住);
/// 历史里压根没有对应调用 → 才回落 `{name}_{n}`。
/// 生成式 `{name}_{n}`(`n` 为纯数字)在名字集合上是单射,不会与另一函数名撞车。
#[derive(Default)]
struct ToolIdAlloc {
    /// 每个函数名出现过的 `functionCall`,记 `(轮次, id)`;结果侧缺 id 时据此回填。
    /// 轮次 = 该调用所在 `Content` 在 `contents` 里的下标。
    calls_by_name: HashMap<String, Vec<(usize, String)>>,
    /// 已被某条 `functionResponse` 认领的调用 id。**带 id 的结果同样计入**,否则同一轮里
    /// 混用"带 id / 不带 id"两种结果时,不带 id 的那条会被错配到已认领的调用上。
    claimed: HashSet<String>,
    /// 历史里找不到可配调用时的回落序号(按函数名各自计数,保持确定性)。
    orphan_seen: HashMap<String, usize>,
    /// 当前正在转换的 `Content` 下标,由 [`gemini_to_hub`] 逐条推进。
    turn: usize,
}

impl ToolIdAlloc {
    fn call_id(&mut self, call: &FunctionCall) -> String {
        let turn = self.turn;
        let seen = self.calls_by_name.entry(call.name.clone()).or_default();
        let id = match call.id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            // 缺 id:按同名出现序号生成,保证并行同名调用彼此不同 id。
            None => format!("{}_{}", call.name, seen.len()),
        };
        seen.push((turn, id.clone()));
        id
    }

    fn response_id(&mut self, resp: &FunctionResponse) -> String {
        // 客户端回带了 id:直接用(最精确),并标记该调用已被认领。
        if let Some(id) = resp.id.as_deref().filter(|s| !s.is_empty()) {
            self.claimed.insert(id.to_string());
            return id.to_string();
        }
        // 缺 id:在**最近一轮仍有未认领同名调用**的那轮里,取其中第一个未认领的。
        //
        // 为何要按轮次收窄而不是全局取第一个未认领:并行调用可能只被答了一部分(用户拒绝其中
        // 一个、或历史被压缩),那个永不被答的调用会一直排在最前,把后续每一条无 id 的结果都
        // 拽回旧轮次——模型于是收不到新调用的结果、反复重发,正是本修复要消灭的症状。按轮次
        // 收窄后:同轮并行调用仍按出现顺序依次配对(常规场景不变),跨轮则优先配最近那轮。
        let pick = self.calls_by_name.get(&resp.name).and_then(|calls| {
            let latest_open = calls
                .iter()
                .filter(|(_, id)| !self.claimed.contains(id))
                .map(|(t, _)| *t)
                .next_back()?;
            calls
                .iter()
                .find(|(t, id)| *t == latest_open && !self.claimed.contains(id))
                .map(|(_, id)| id.clone())
        });
        if let Some(id) = pick {
            self.claimed.insert(id.clone());
            return id;
        }
        // 历史里没有可配的同名调用(客户端只发结果不发调用)→ 保持原生成式。
        let n = self.orphan_seen.entry(resp.name.clone()).or_insert(0);
        let idx = *n;
        *n += 1;
        format!("{}_{}", resp.name, idx)
    }
}

/// 把单个 Gemini `Part` 转换成 0~1 个中枢 `Block`;`ids` 供工具 part 取 `tool_use_id`。
///
/// `inlineData` **只认 `image/*`**:中枢的 `Block::Image` 会被
/// [`image_from_source`](crate::kiro::convert) 以「去掉 `image/` 前缀」的方式推出 Kiro 的
/// `format` 字段。若把 `application/pdf`(Gemini 规范明确支持的 inlineData 类型)也当图片转下去,
/// 上游收到的就是 `images:[{"format":"application/pdf",…}]` —— Kiro 只认 jpeg/png/gif/webp,
/// 结果是一个既不可读又无从归因的上游错误。故非图片类型在此就地报错,由 handler 回 400 说明真因。
fn convert_part(part: &Part, ids: &mut ToolIdAlloc) -> Result<Option<Block>, GeminiConvertError> {
    if let Some(text) = &part.text {
        return Ok(Some(Block::Text { text: text.clone() }));
    }
    if let Some(inline) = &part.inline_data {
        if !is_image_mime(&inline.mime_type) {
            return Err(GeminiConvertError::UnsupportedInlineData(
                inline.mime_type.clone(),
            ));
        }
        // media_type 统一小写:下游按字面量 `image/` 前缀裁出 format,大写形(`IMAGE/PNG`)
        // 裁不掉就会变成伪造的 format 值。
        let media_type = inline.mime_type.to_ascii_lowercase();
        return Ok(Some(Block::Image {
            source: json!({"type": "base64", "media_type": media_type, "data": inline.data}),
        }));
    }
    if let Some(call) = &part.function_call {
        return Ok(Some(Block::ToolUse {
            id: ids.call_id(call),
            name: call.name.clone(),
            input: call.args.clone(),
        }));
    }
    if let Some(resp) = &part.function_response {
        return Ok(Some(Block::ToolResult {
            tool_use_id: ids.response_id(resp),
            content: response_value_to_text(&resp.response),
            is_error: None,
        }));
    }
    Ok(None)
}

/// 把单个 Gemini `Content` 转换成中枢 `InMsg`(role 映射:`"model"`→`"assistant"`,其余→`"user"`)。
fn convert_content(content: &Content, ids: &mut ToolIdAlloc) -> Result<InMsg, GeminiConvertError> {
    let role = match content.role.as_deref() {
        Some("model") => "assistant",
        _ => "user",
    };
    // 逐个 part 转换:任一 part 致命出错就整条请求失败(不能只丢那一个 part,否则又回到静默丢附件)。
    let mut blocks: Vec<Block> = Vec::with_capacity(content.parts.len());
    for p in &content.parts {
        if let Some(b) = convert_part(p, ids)? {
            blocks.push(b);
        }
    }
    Ok(InMsg {
        role: role.to_string(),
        content: ContentIn::Blocks(blocks),
    })
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
///
/// 失败(见 [`GeminiConvertError`])时由调用方回 Gemini 形状的 400,不往上游发。
pub fn gemini_to_hub(
    req: GenerateContentRequest,
    model: String,
) -> Result<MessagesRequest, GeminiConvertError> {
    let system = req.system_instruction.as_ref().map(|sys| {
        sys.parts
            .iter()
            .filter_map(|p| p.text.as_deref())
            .collect::<Vec<_>>()
            .concat()
    });

    let mut tool_ids = ToolIdAlloc::default();
    // 逐条推进轮次下标:无 id 的 functionResponse 据此优先配"最近一轮仍未被答"的同名调用。
    let mut messages: Vec<InMsg> = Vec::with_capacity(req.contents.len());
    for (turn, c) in req.contents.iter().enumerate() {
        tool_ids.turn = turn;
        messages.push(convert_content(c, &mut tool_ids)?);
    }

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

    // `tools` 里只有内建工具条目(`googleSearch`/`codeExecution`/`urlContext`,本中转无从兑现)时
    // 映射结果为空数组 —— 应如实归成"没有函数工具",而不是下发一个空 tools 让下游去猜。
    if tools.as_ref().is_some_and(|t| t.is_empty()) {
        tools = None;
    }

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

    Ok(MessagesRequest {
        metadata: None,
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
    })
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

    /// 回归:出站 `functionCall.id` + 客户端**不回带 id** 的 `functionResponse` 必须仍能配对。
    ///
    /// 这是 Google SDK 的惯用法(模型那条 Content 原样塞回历史,结果只带 name+response)。
    /// 结果侧若另起计数器生成 `{name}_0`,就与调用侧的真 id 对不上,工具结果成孤儿、模型反复重发。
    #[test]
    fn function_response_without_id_pairs_with_echoed_call_id() {
        let hub = gemini_to_hub(
            GenerateContentRequest {
                contents: vec![
                    // 模型那条:带出站真 id(hub_to_gemini 恒发)
                    Content {
                        role: Some("model".to_string()),
                        parts: vec![Part {
                            text: None,
                            inline_data: None,
                            function_call: Some(FunctionCall {
                                id: Some("tooluse_abc123".to_string()),
                                name: "get_weather".to_string(),
                                args: json!({"city": "SF"}),
                            }),
                            function_response: None,
                        }],
                    },
                    // 客户端回填的结果:只有 name + response,**没有 id**
                    Content {
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
                    },
                ],
                system_instruction: None,
                tools: None,
                tool_config: None,
                generation_config: None,
            },
            "claude-sonnet-4.5".to_string(),
        )
        .expect("转换应成功");

        let ContentIn::Blocks(call_blocks) = &hub.messages[0].content else {
            panic!("模型消息应为块数组");
        };
        let call_id = call_blocks
            .iter()
            .find_map(|b| match b {
                Block::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .expect("应有 tool_use");
        let ContentIn::Blocks(result_blocks) = &hub.messages[1].content else {
            panic!("结果消息应为块数组");
        };
        let result_id = result_blocks
            .iter()
            .find_map(|b| match b {
                Block::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .expect("应有 tool_result");
        assert_eq!(call_id, "tooluse_abc123");
        assert_eq!(
            result_id, call_id,
            "结果侧缺 id 时必须回填成同名调用的真实 id,否则工具配对断裂"
        );
    }

    /// 回归:同一轮里**混用**"带 id / 不带 id"两种 functionResponse 时不得错位。
    ///
    /// 带 id 的结果必须同样"占位"(标记该调用已被认领),否则后面不带 id 的结果会被
    /// 错配到已被认领的那次调用上,两条结果指向同一个 tool_use_id。
    /// 回归:并行同名调用**只被答了一部分**时,后续无 id 的结果必须配到"最近一轮"的新调用,
    /// 而不是被那个永不被答的旧调用一直拽住。
    ///
    /// 场景(审查员实测过的 S2):模型并行发 A、B 两次 read_file;用户拒了 B,只答了 A;下一轮
    /// 模型又发 C,客户端答 C 但不带 id。若全局取"第一个未认领"会配到 B —— C 永远收不到结果,
    /// 模型于是反复重发 C,正是本修复要消灭的症状。
    #[test]
    fn idless_response_prefers_latest_open_turn() {
        fn call(id: &str) -> Part {
            Part {
                text: None,
                inline_data: None,
                function_call: Some(FunctionCall {
                    id: Some(id.to_string()),
                    name: "read_file".to_string(),
                    args: json!({}),
                }),
                function_response: None,
            }
        }
        fn resp(id: Option<&str>) -> Part {
            Part {
                text: None,
                inline_data: None,
                function_call: None,
                function_response: Some(FunctionResponse {
                    id: id.map(str::to_string),
                    name: "read_file".to_string(),
                    response: json!({}),
                }),
            }
        }
        let hub = gemini_to_hub(
            GenerateContentRequest {
                contents: vec![
                    // 轮 0:并行发 A、B
                    Content {
                        role: Some("model".to_string()),
                        parts: vec![call("A"), call("B")],
                    },
                    // 轮 1:只答了 A(B 被用户拒绝,没有对应结果)
                    Content {
                        role: Some("user".to_string()),
                        parts: vec![resp(Some("A"))],
                    },
                    // 轮 2:模型又发 C
                    Content {
                        role: Some("model".to_string()),
                        parts: vec![call("C")],
                    },
                    // 轮 3:答 C,但不带 id
                    Content {
                        role: Some("user".to_string()),
                        parts: vec![resp(None)],
                    },
                ],
                system_instruction: None,
                tools: None,
                tool_config: None,
                generation_config: None,
            },
            "claude-sonnet-4.5".to_string(),
        )
        .expect("转换应成功");
        let ContentIn::Blocks(blocks) = &hub.messages[3].content else {
            panic!("最后一条应为块数组");
        };
        let id = blocks
            .iter()
            .find_map(|b| match b {
                Block::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .expect("应有 tool_result");
        assert_eq!(
            id, "C",
            "无 id 的结果必须配最近一轮的 C,而不是被没人答的 B 拽住"
        );
    }

    #[test]
    fn mixed_id_and_idless_responses_do_not_collide() {
        fn call(id: &str) -> Part {
            Part {
                text: None,
                inline_data: None,
                function_call: Some(FunctionCall {
                    id: Some(id.to_string()),
                    name: "f".to_string(),
                    args: json!({}),
                }),
                function_response: None,
            }
        }
        fn resp(id: Option<&str>) -> Part {
            Part {
                text: None,
                inline_data: None,
                function_call: None,
                function_response: Some(FunctionResponse {
                    id: id.map(str::to_string),
                    name: "f".to_string(),
                    response: json!({}),
                }),
            }
        }
        let hub = gemini_to_hub(
            GenerateContentRequest {
                contents: vec![
                    // 两次并行同名调用,各带真 id
                    Content {
                        role: Some("model".to_string()),
                        parts: vec![call("x1"), call("x2")],
                    },
                    // 第一条结果回带 id=x1,第二条不带 id → 必须落到 x2
                    Content {
                        role: Some("user".to_string()),
                        parts: vec![resp(Some("x1")), resp(None)],
                    },
                ],
                system_instruction: None,
                tools: None,
                tool_config: None,
                generation_config: None,
            },
            "claude-sonnet-4.5".to_string(),
        )
        .expect("转换应成功");
        let ContentIn::Blocks(blocks) = &hub.messages[1].content else {
            panic!("结果消息应为块数组");
        };
        let ids: Vec<String> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            vec!["x1".to_string(), "x2".to_string()],
            "带 id 的结果必须占位,否则无 id 的那条会错配回 x1"
        );
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
        let hub = gemini_to_hub(req, "claude-sonnet-4.5".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
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

    /// 回归:非 `image/*` 的 `inlineData`(Gemini 规范明确支持 PDF/音频/视频)不得被当图片转下去。
    ///
    /// 中枢的 `Block::Image` 会被下游按「去掉 `image/` 前缀」推出 Kiro 的 `format`,故
    /// `application/pdf` 会变成 `images:[{"format":"application/pdf",…}]` —— Kiro 只认
    /// jpeg/png/gif/webp,调用方拿到的是一个无从归因的上游错误。必须就地报错。
    #[test]
    fn non_image_inline_data_is_rejected_not_forwarded_as_image() {
        for mime in ["application/pdf", "audio/mp3", "video/mp4", "text/plain"] {
            let req = GenerateContentRequest {
                contents: vec![Content {
                    role: Some("user".to_string()),
                    parts: vec![Part {
                        text: None,
                        inline_data: Some(InlineData {
                            mime_type: mime.to_string(),
                            data: "JVBERi0".to_string(),
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
            let err = gemini_to_hub(req, "m".to_string())
                .expect_err("非图片内联附件必须报错,而不是伪装成图片发上游");
            assert_eq!(
                err,
                GeminiConvertError::UnsupportedInlineData(mime.to_string())
            );
            // 错误文案要点名真凶(mimeType),否则调用方无从归因。
            assert!(err.to_string().contains(mime), "错误文案应含 {mime}");
        }
    }

    /// 图片 `inlineData` 的 mimeType 大小写不敏感:`IMAGE/PNG` 既不能被误判成"非图片"而报错,
    /// 也不能原样带着大写前缀下发(下游按字面量 `image/` 裁 format,裁不掉就成了伪造值)。
    #[test]
    fn image_inline_data_mime_is_case_insensitive_and_normalized() {
        let req = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part {
                    text: None,
                    inline_data: Some(InlineData {
                        mime_type: "IMAGE/PNG".to_string(),
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
        let hub = gemini_to_hub(req, "m".to_string()).expect("图片附件应转换成功");
        let ContentIn::Blocks(blocks) = &hub.messages[0].content else {
            panic!("应为 Blocks");
        };
        match &blocks[0] {
            Block::Image { source } => assert_eq!(source["media_type"], "image/png"),
            other => panic!("应为 Image,实际: {other:?}"),
        }
    }

    /// 回归:`tools` 里只有内建工具条目(`googleSearch` 等)时,函数工具应归成 `None`,
    /// 而不是一个空数组。空数组会让下游得靠"长度是否为 0"去猜意图。
    #[test]
    fn builtin_only_tools_map_to_no_tools() {
        let raw = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"googleSearch":{}}]}"#;
        let req: GenerateContentRequest = serde_json::from_str(raw).expect("内建工具条目应能解析");
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
        assert_eq!(hub.tools, None);
    }

    /// 回归:内建工具与函数声明混发时,函数声明必须完整保留。
    #[test]
    fn builtin_mixed_with_function_declarations_keeps_functions() {
        let raw = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"functionDeclarations":[{"name":"f"}]},{"codeExecution":{}}]}"#;
        let req: GenerateContentRequest = serde_json::from_str(raw).expect("混发应能解析");
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
        let tools = hub.tools.expect("函数工具应保留");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "f");
    }

    /// 回归:snake_case(proto 原名)请求全链路走通 —— 系统提示、图片、maxOutputTokens
    /// 都必须真正落到中枢请求上,而不是被 serde 当未知字段静默丢掉。
    #[test]
    fn snake_case_request_survives_conversion() {
        let raw = r#"{
            "contents":[{"role":"user","parts":[
                {"text":"hi"},
                {"inline_data":{"mime_type":"image/png","data":"AAAA"}}
            ]}],
            "system_instruction":{"parts":[{"text":"be brief"}]},
            "generation_config":{"max_output_tokens":128}
        }"#;
        let req: GenerateContentRequest = serde_json::from_str(raw).expect("解析失败");
        let hub = gemini_to_hub(req, "m".to_string()).expect("转换应成功");
        assert_eq!(
            hub.system.as_ref().map(|s| s.text()).as_deref(),
            Some("be brief"),
            "snake_case system_instruction 必须落到中枢 system,否则系统提示整段丢失"
        );
        assert_eq!(hub.max_tokens, Some(128));
        let ContentIn::Blocks(blocks) = &hub.messages[0].content else {
            panic!("应为 Blocks");
        };
        assert_eq!(blocks.len(), 2, "文本 + 图片两块都要在;实际:{blocks:?}");
        match &blocks[1] {
            Block::Image { source } => {
                assert_eq!(source["media_type"], "image/png");
                assert_eq!(source["data"], "AAAA");
            }
            other => panic!("应为 Image,实际: {other:?}"),
        }
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
