//! Anthropic ⇄ Kiro 转换器(转换逻辑自写;Kiro 形状照观测的数据面契约)。
//!
//! 覆盖范围:`/v1/messages` 请求体 → Kiro `KiroRequest`(含 `tool_result` /
//! `tool_use`(历史)/ `image` 内容块,以及工具规格 `tools`→`spectask`);
//! Kiro 事件流帧(已解码为 [`Message`])→ Anthropic `MessagesResponse`(响应侧
//! `tool_use` 块见后续任务)。

use std::fmt;

use crate::kiro::eventstream::frame::Message;
use crate::kiro::wire::{
    AssistantResponseMessage, ConversationState, CurrentMessage, HistoryItem, ImageBlock,
    ImageSource, InputSchemaJson, KiroRequest, ToolResultText, ToolResultWire, ToolSpec,
    ToolSpecInner, ToolUseWire, UserInputMessage, UserInputMessageContext,
};
use crate::protocol::anthropic::types::{
    Block, ContentIn, InMsg, MessagesRequest, MessagesResponse, OutBlock, ToolDef, Usage,
};

/// 转换失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertError {
    /// 客户端请求的模型名无法映射到任何已知 Kiro modelId。
    UnknownModel(String),
    /// 请求里没有任何消息,无法确定 currentMessage。
    EmptyMessages,
    /// 图片内容块携带的是远程 http(s) URL,而 Kiro 数据面只接受内联 base64。
    /// 明确报错而非静默丢弃(否则视觉请求会丢图、模型看不到)。
    RemoteImageUrl(String),
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConvertError::UnknownModel(m) => write!(f, "无法识别的模型名: {m}"),
            ConvertError::EmptyMessages => write!(f, "messages 不能为空"),
            ConvertError::RemoteImageUrl(u) => write!(
                f,
                "不支持远程图片 URL({u});请把图片内联为 data: URL(base64)后再发送"
            ),
        }
    }
}

impl std::error::Error for ConvertError {}

/// 生成一个随机十六进制 id(16 字节 CSPRNG),用于 conversationId / 响应 id。
fn random_hex_id() -> String {
    let mut raw = [0u8; 16];
    getrandom::getrandom(&mut raw).expect("CSPRNG");
    hex::encode(raw)
}

/// 版本号 token 匹配:同时接受点分与横杠两种分隔形式(契约 §5)。
///
/// 客户端可能发点分(`4.5`,Anthropic 直连风格)也可能发横杠
/// (`4-5`,SDK / Bedrock modelId 风格),两者都要命中同一变体,
/// 否则横杠形会漏匹配而落到 DEFAULT 分支映射到错误的模型。
fn ver(m: &str, dotted: &str) -> bool {
    m.contains(dotted) || m.contains(&dotted.replace('.', "-"))
}

/// 把客户端传入的模型名(任意大小写)映射到内部 Kiro modelId。
///
/// 按"最具体优先"的子串匹配规则(契约 §5),未命中任何已知家族/变体返回 `None`。
/// 版本号 token 一律经 [`ver`] 同时接受点分(`4.5`)与横杠(`4-5`)两种写法。
pub fn map_model(client_model: &str) -> Option<String> {
    let m = client_model.to_lowercase();

    if m.contains("sonnet") {
        return Some(if ver(&m, "4.6") {
            "claude-sonnet-4.6".to_string()
        } else if m.contains("sonnet-5") {
            "claude-sonnet-5".to_string()
        } else {
            "claude-sonnet-4.5".to_string()
        });
    }
    if m.contains("fable") {
        return Some("claude-fable-5".to_string());
    }
    if m.contains("opus") {
        return Some(if ver(&m, "4.5") {
            "claude-opus-4.5".to_string()
        } else if ver(&m, "4.7") {
            "claude-opus-4.7".to_string()
        } else if ver(&m, "4.8") {
            "claude-opus-4.8".to_string()
        } else {
            "claude-opus-4.6".to_string()
        });
    }
    if m.contains("haiku") {
        return Some("claude-haiku-4.5".to_string());
    }
    if m.contains("auto") {
        return Some("auto".to_string());
    }
    if m.contains("deepseek") {
        return Some("deepseek-3.2".to_string());
    }
    if m.contains("glm") {
        return Some("glm-5".to_string());
    }
    if m.contains("minimax") {
        return Some(if ver(&m, "2.5") {
            "minimax-m2.5".to_string()
        } else {
            "minimax-m2.1".to_string()
        });
    }
    if m.contains("qwen") {
        return Some("qwen3-coder-next".to_string());
    }
    if m.contains("gpt") {
        return if m.contains("terra") {
            Some("gpt-5.6-terra".to_string())
        } else if m.contains("luna") {
            Some("gpt-5.6-luna".to_string())
        } else if m.contains("sol") || ver(&m, "5.6") {
            Some("gpt-5.6-sol".to_string())
        } else {
            None
        };
    }

    None
}

/// 空文本回退为契约 §2 规定的字面量占位符(纯 `tool_result` 轮次没有文本时用)。
fn non_empty_content(text: String) -> String {
    if text.is_empty() {
        "(tool result above)".to_string()
    } else {
        text
    }
}

/// 把请求里的 Anthropic `ToolDef` 列表映射成 Kiro `ToolSpec` 列表(照契约 §2)。
fn map_tools(tools: &[ToolDef]) -> Vec<ToolSpec> {
    tools
        .iter()
        .map(|t| ToolSpec {
            tool_specification: ToolSpecInner {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: InputSchemaJson {
                    json: t.input_schema.clone(),
                },
            },
        })
        .collect()
}

/// 把 `tool_result` 块的 `content`(字符串或文本块数组)拍平成纯文本(照观测)。
fn tool_result_text(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .concat();
    }
    String::new()
}

/// 从一条消息里提取所有 `Block::ToolResult`,映射成 Kiro `ToolResultWire` 列表。
fn message_tool_results(msg: &InMsg) -> Vec<ToolResultWire> {
    let ContentIn::Blocks(blocks) = &msg.content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some(ToolResultWire {
                tool_use_id: tool_use_id.clone(),
                content: vec![ToolResultText {
                    text: tool_result_text(content),
                }],
                status: if *is_error == Some(true) {
                    "error".to_string()
                } else {
                    "success".to_string()
                },
                is_error: *is_error,
            }),
            _ => None,
        })
        .collect()
}

/// 从一条消息里提取所有 `Block::ToolUse`,映射成 Kiro `ToolUseWire` 列表。
fn message_tool_uses(msg: &InMsg) -> Vec<ToolUseWire> {
    let ContentIn::Blocks(blocks) = &msg.content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::ToolUse { id, name, input } => Some(ToolUseWire {
                tool_use_id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// 从一条消息里提取所有 `Block::Image`,映射成 Kiro `ImageBlock` 列表。
///
/// Kiro 数据面只接受内联 base64(`source.type=="base64"`)。遇到远程图片
/// (Anthropic 的 `source.type=="url"`,或任何带 http(s) `url` 字段的图片源)
/// 一律 **报错**(`ConvertError::RemoteImageUrl`)而不是静默丢弃——否则视觉
/// 请求会悄悄丢图、模型收不到图片。OpenAI/Gemini 前端把无法内联的远程图片
/// 编码成 `{"type":"url","url":...}` 转到这里统一拦截。
fn message_images(msg: &InMsg) -> Result<Vec<ImageBlock>, ConvertError> {
    let ContentIn::Blocks(blocks) = &msg.content else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for b in blocks {
        let Block::Image { source } = b else { continue };
        // 远程 URL(显式 type=="url" 或带 http(s) 的 url 字段)→ 报错。
        let is_url_type = source.get("type").and_then(|t| t.as_str()) == Some("url");
        let url_field = source.get("url").and_then(|u| u.as_str());
        if is_url_type || url_field.map(is_remote_url).unwrap_or(false) {
            let u = url_field.unwrap_or("").to_string();
            return Err(ConvertError::RemoteImageUrl(u));
        }
        if source.get("type").and_then(|t| t.as_str()) != Some("base64") {
            // 既非 base64 也非可识别的远程 URL:跳过(空/未知源,无图可传)。
            continue;
        }
        let format = source
            .get("media_type")
            .and_then(|m| m.as_str())
            .map(|m| m.strip_prefix("image/").unwrap_or(m).to_string())
            .unwrap_or_default();
        let bytes = source
            .get("data")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string();
        out.push(ImageBlock {
            format,
            source: ImageSource { bytes },
        });
    }
    Ok(out)
}

/// 判断字符串是否为远程 http(s) URL(用于图片源识别)。
fn is_remote_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// 把 Anthropic `/v1/messages` 请求转换为 Kiro 数据面请求体。
///
/// - 模型映射失败 → `Err(ConvertError::UnknownModel)`。
/// - `system`(若存在)前置到首条消息的文本前面。
/// - 末条消息作为 `currentMessage.userInputMessage`,其余进入 `history`
///   (`assistant` 角色 → `AssistantResponseMessage`,否则 → `UserInputMessage`)。
/// - 有 `tools` → `agentTaskType="spectask"` 且当前消息上下文带映射后的工具规格;无 tools → `"vibe"`。
/// - `tool_result` / `tool_use` / `image` 内容块(照契约/观测)分别映射进对应消息的
///   `toolResults` / `toolUses` / `images`。
pub fn anthropic_to_kiro(
    req: &MessagesRequest,
    profile_arn: Option<&str>,
) -> Result<KiroRequest, ConvertError> {
    let model_id =
        map_model(&req.model).ok_or_else(|| ConvertError::UnknownModel(req.model.clone()))?;

    if req.messages.is_empty() {
        return Err(ConvertError::EmptyMessages);
    }

    let texts: Vec<String> = req
        .messages
        .iter()
        .enumerate()
        .map(|(i, msg)| {
            let text = msg.text();
            if i == 0 {
                match req.system.as_ref().map(|s| s.text()) {
                    Some(sys) if !sys.is_empty() => format!("{sys}\n\n{text}"),
                    _ => text,
                }
            } else {
                text
            }
        })
        .collect();

    let (history_msgs, last) = texts.split_at(texts.len() - 1);
    let last_text = last[0].clone();
    let last_msg = &req.messages[req.messages.len() - 1];

    let history: Vec<HistoryItem> = req.messages[..req.messages.len() - 1]
        .iter()
        .zip(history_msgs.iter())
        .map(|(msg, text)| {
            if msg.role == "assistant" {
                let tool_uses = message_tool_uses(msg);
                Ok(HistoryItem::AssistantResponseMessage {
                    assistant_response_message: AssistantResponseMessage {
                        content: text.clone(),
                        tool_uses: if tool_uses.is_empty() {
                            None
                        } else {
                            Some(tool_uses)
                        },
                    },
                })
            } else {
                let tool_results = message_tool_results(msg);
                let images = message_images(msg)?;
                Ok(HistoryItem::UserInputMessage {
                    user_input_message: UserInputMessage {
                        content: non_empty_content(text.clone()),
                        model_id: model_id.clone(),
                        origin: "AI_EDITOR".to_string(),
                        user_input_message_context: UserInputMessageContext {
                            tools: None,
                            tool_results: if tool_results.is_empty() {
                                None
                            } else {
                                Some(tool_results)
                            },
                        },
                        images: if images.is_empty() {
                            None
                        } else {
                            Some(images)
                        },
                    },
                })
            }
        })
        .collect::<Result<Vec<_>, ConvertError>>()?;

    let has_tools = req.tools.as_ref().is_some_and(|t| !t.is_empty());
    let agent_task_type = if has_tools { "spectask" } else { "vibe" }.to_string();

    let current_tool_results = message_tool_results(last_msg);
    let current_images = message_images(last_msg)?;

    Ok(KiroRequest {
        conversation_state: ConversationState {
            chat_trigger_type: "MANUAL".to_string(),
            agent_task_type,
            conversation_id: random_hex_id(),
            current_message: CurrentMessage {
                user_input_message: UserInputMessage {
                    content: non_empty_content(last_text),
                    model_id,
                    origin: "AI_EDITOR".to_string(),
                    user_input_message_context: UserInputMessageContext {
                        tools: if has_tools {
                            Some(map_tools(req.tools.as_ref().unwrap()))
                        } else {
                            None
                        },
                        tool_results: if current_tool_results.is_empty() {
                            None
                        } else {
                            Some(current_tool_results)
                        },
                    },
                    images: if current_images.is_empty() {
                        None
                    } else {
                        Some(current_images)
                    },
                },
            },
            history,
        },
        profile_arn: profile_arn.map(|s| s.to_string()),
    })
}

/// 从一个事件帧里取出 `:event-type` header 的字符串值(取不到则 `None`)。
fn event_type(msg: &Message) -> Option<&str> {
    use crate::kiro::eventstream::header::HeaderValue;
    msg.headers.iter().find_map(|h| {
        if h.name == ":event-type" {
            match &h.value {
                HeaderValue::Str(s) => Some(s.as_str()),
                _ => None,
            }
        } else {
            None
        }
    })
}

/// 从一个事件帧里取出某个字符串 header 的值(取不到则 `None`)。
fn header_str<'a>(msg: &'a Message, name: &str) -> Option<&'a str> {
    use crate::kiro::eventstream::header::HeaderValue;
    msg.headers.iter().find_map(|h| {
        if h.name == name {
            match &h.value {
                HeaderValue::Str(s) => Some(s.as_str()),
                _ => None,
            }
        } else {
            None
        }
    })
}

/// 上游截断信号(照 §5 观测的数据面契约)。
///
/// Kiro 事件流用两种帧表达"这轮不是正常结束":
/// - **exception 帧**:`:message-type == "exception"` 且 `:exception-type` header
///   为 `ContentLengthExceededException`(命中 `max_tokens` 预算)→ [`Truncation::MaxTokens`]。
/// - **contextUsageEvent 帧**:payload 的 `contextUsagePercentage >= 100`(上下文窗口耗尽)
///   → [`Truncation::ContextWindow`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncation {
    /// 命中 max_tokens 预算 → Anthropic `stop_reason="max_tokens"` / OpenAI `finish_reason="length"`。
    MaxTokens,
    /// 上下文窗口耗尽 → Anthropic `stop_reason="model_context_window_exceeded"`。
    ContextWindow,
}

/// 从单个事件帧探测上游截断信号;非截断帧一律 `None`(不 panic)。
pub fn frame_truncation(frame: &Message) -> Option<Truncation> {
    // exception 帧:靠 header 判定,payload 不必是合法 JSON。
    if header_str(frame, ":message-type") == Some("exception")
        && header_str(frame, ":exception-type") == Some("ContentLengthExceededException")
    {
        return Some(Truncation::MaxTokens);
    }
    // contextUsageEvent:payload 里 contextUsagePercentage >= 100 视为窗口耗尽。
    if event_type(frame) == Some("contextUsageEvent") {
        let v: serde_json::Value = serde_json::from_slice(&frame.payload).ok()?;
        let pct = v.get("contextUsagePercentage").and_then(|p| p.as_f64())?;
        if pct >= 100.0 {
            return Some(Truncation::ContextWindow);
        }
    }
    None
}

/// 遍历一批帧,取出首个截断信号(exception 与 contextUsage 二者取先见者)。无 → `None`。
pub fn extract_truncation(frames: &[Message]) -> Option<Truncation> {
    frames.iter().find_map(frame_truncation)
}

/// 从单个事件帧取出增量文本(仅 `assistantResponseEvent` 且 payload 有 `content` 字符串时)。
pub fn frame_text_delta(frame: &Message) -> Option<String> {
    if event_type(frame) != Some("assistantResponseEvent") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&frame.payload).ok()?;
    v.get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
}

/// 新的 Anthropic 消息 id(`msg_` + 随机十六进制)。
pub fn new_message_id() -> String {
    format!("msg_{}", random_hex_id())
}

/// 从 `meteringEvent` 帧提取的真实计费数据(照观测的数据面契约)。
///
/// 字段:`credits` = 上游 payload 的 `usage`(真实积分消耗,f64);
/// `cache_read_input_tokens` / `cache_creation_input_tokens` = 若 payload 带则透传。
/// 上游 payload 的键既可能是 snake_case(`cache_read_input_tokens`)也可能是
/// camelCase(`cacheReadInputTokens`),两种都接。
#[derive(Debug, Clone, PartialEq)]
pub struct MeteringUsage {
    pub credits: f64,
    pub cache_read_input_tokens: Option<i32>,
    pub cache_creation_input_tokens: Option<i32>,
}

/// 从单个事件帧解析 `meteringEvent`:仅当 `:event-type == "meteringEvent"` 且
/// payload 是含数值 `usage` 字段的合法 JSON 时返回 `Some`;其余一律 `None`(不 panic)。
pub fn metering_frame(frame: &Message) -> Option<MeteringUsage> {
    if event_type(frame) != Some("meteringEvent") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&frame.payload).ok()?;
    let credits = v.get("usage").and_then(|u| u.as_f64())?;
    // 缓存 token 字段:snake_case 优先,回退 camelCase。
    let read = v
        .get("cache_read_input_tokens")
        .or_else(|| v.get("cacheReadInputTokens"))
        .and_then(|c| c.as_i64())
        .map(|n| n as i32);
    let creation = v
        .get("cache_creation_input_tokens")
        .or_else(|| v.get("cacheCreationInputTokens"))
        .and_then(|c| c.as_i64())
        .map(|n| n as i32);
    Some(MeteringUsage {
        credits,
        cache_read_input_tokens: read,
        cache_creation_input_tokens: creation,
    })
}

/// 遍历一批帧,取出最后一个(或唯一)`meteringEvent` 的真实计费数据。
///
/// 上游在一次响应里通常只发一个 meteringEvent;若发多个,取末次(累计后的终值)。
/// 无 meteringEvent → `None`,调用方回退到字符估算(现状)。
pub fn extract_metering(frames: &[Message]) -> Option<MeteringUsage> {
    frames.iter().rev().find_map(metering_frame)
}

/// 从单个事件帧取出 `toolUseEvent` 的原始 payload(解析成 JSON `Value`)。
///
/// 仅当 `:event-type == "toolUseEvent"` 且 payload 是合法 JSON 时返回 `Some`;
/// 其余情况(事件类型不符 / payload 非法 JSON)一律 `None`,不会 panic。
pub fn tool_use_frame(frame: &Message) -> Option<serde_json::Value> {
    if event_type(frame) != Some("toolUseEvent") {
        return None;
    }
    serde_json::from_slice(&frame.payload).ok()
}

/// 按真机观测的 `toolUseEvent` 帧序自写的拼接逻辑:
/// open 帧(仅 name+toolUseId)建条目、input 帧把片段追加进该 id 的缓冲串、
/// stop 帧无需特别处理(仅代表该 tool_use 收尾)。用 `order` 保留 toolUseId
/// 首次出现的顺序,`name`/`frags` 用 id 索引到 `order` 里的下标做归并。
struct ToolUseAccum {
    order: Vec<String>,
    names: std::collections::HashMap<String, String>,
    frags: std::collections::HashMap<String, String>,
}

impl ToolUseAccum {
    fn new() -> Self {
        Self {
            order: Vec::new(),
            names: std::collections::HashMap::new(),
            frags: std::collections::HashMap::new(),
        }
    }

    fn touch(&mut self, id: &str) {
        if !self.names.contains_key(id) {
            self.order.push(id.to_string());
        }
    }

    fn apply(&mut self, payload: &serde_json::Value) {
        let Some(id) = payload.get("toolUseId").and_then(|v| v.as_str()) else {
            return;
        };
        self.touch(id);
        if let Some(name) = payload.get("name").and_then(|v| v.as_str())
            && !name.is_empty()
        {
            self.names
                .entry(id.to_string())
                .or_insert_with(|| name.to_string());
        }
        if let Some(input) = payload.get("input").and_then(|v| v.as_str()) {
            self.frags
                .entry(id.to_string())
                .or_default()
                .push_str(input);
        }
    }

    /// 按首现顺序产出 `(toolUseId, name, input_json)`;`input_json` 解析失败或为空 → `{}`。
    fn into_blocks(self) -> Vec<OutBlock> {
        self.order
            .into_iter()
            .map(|id| {
                let name = self.names.get(&id).cloned().unwrap_or_default();
                let frags = self.frags.get(&id).map(String::as_str).unwrap_or("");
                let input = serde_json::from_str(frags).unwrap_or_else(|_| serde_json::json!({}));
                OutBlock::ToolUse { id, name, input }
            })
            .collect()
    }
}

/// 把 Kiro 事件流帧序列还原为 Anthropic `/v1/messages` 响应。
///
/// 遍历所有帧:`assistantResponseEvent` 按序拼接 `content` 字段成全文(如前);
/// `toolUseEvent` 按 `toolUseId` 归并(open 帧建条目、input 帧拼片段、stop 帧收尾),
/// 按首次出现顺序还原成 `tool_use` 块。`content` 先文本(若非空)、再各 tool_use;
/// 两者都空则保留一个空文本块。
///
/// `stop_reason` 优先级(照 §5 契约):
/// **tool_use 优先级最高**——只要有工具调用就无条件报 `"tool_use"`,即便同批帧里
/// 带截断信号(截断是下一轮才该报告的状态,盖掉本轮 tool_use 会让客户端只渲染
/// 工具块而不执行)。无工具时,若探测到上游截断([`extract_truncation`]):
/// [`Truncation::MaxTokens`] → `"max_tokens"`、[`Truncation::ContextWindow`] →
/// `"model_context_window_exceeded"`;否则 `"end_turn"`。
///
/// `output_tokens` 仍用全文字符数 / 4 近似估算(工具参数不计入,MVP)。
pub fn kiro_events_to_anthropic(frames: &[Message], model: &str) -> MessagesResponse {
    let mut full_text = String::new();
    let mut tools = ToolUseAccum::new();

    for frame in frames {
        if let Some(chunk) = frame_text_delta(frame) {
            full_text.push_str(&chunk);
            continue;
        }
        if let Some(payload) = tool_use_frame(frame) {
            tools.apply(&payload);
        }
    }

    let truncation = extract_truncation(frames);

    // TODO(P2): 读取 meteringEvent 取真实 usage(现 input_tokens=0、output≈字符数/4 为占位)
    let output_tokens = (full_text.chars().count() / 4) as u32;

    let has_tools = !tools.order.is_empty();
    let mut content: Vec<OutBlock> = Vec::new();
    if !full_text.is_empty() {
        content.push(OutBlock::Text { text: full_text });
    }
    content.extend(tools.into_blocks());
    if content.is_empty() {
        content.push(OutBlock::Text {
            text: String::new(),
        });
    }

    let stop_reason = if has_tools {
        "tool_use"
    } else {
        match truncation {
            Some(Truncation::MaxTokens) => "max_tokens",
            Some(Truncation::ContextWindow) => "model_context_window_exceeded",
            None => "end_turn",
        }
    };

    MessagesResponse {
        id: new_message_id(),
        kind: "message".to_string(),
        role: "assistant".to_string(),
        model: model.to_string(),
        content,
        stop_reason: Some(stop_reason.to_string()),
        usage: Usage {
            input_tokens: 0,
            output_tokens,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::eventstream::header::{Header, HeaderValue};
    use crate::protocol::anthropic::types::{Block, ContentIn, InMsg, SystemPrompt, ToolDef};

    fn msg(role: &str, text: &str) -> InMsg {
        InMsg {
            role: role.to_string(),
            content: ContentIn::Text(text.to_string()),
        }
    }

    fn blocks_msg(role: &str, blocks: Vec<Block>) -> InMsg {
        InMsg {
            role: role.to_string(),
            content: ContentIn::Blocks(blocks),
        }
    }

    fn base_req(messages: Vec<InMsg>) -> MessagesRequest {
        MessagesRequest {
            model: "sonnet".to_string(),
            system: None,
            messages,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
        }
    }

    fn event_frame(event_type: &str, payload: &str) -> Message {
        Message {
            headers: vec![Header {
                name: ":event-type".to_string(),
                value: HeaderValue::Str(event_type.to_string()),
            }],
            payload: payload.as_bytes().to_vec(),
        }
    }

    fn exception_frame(exception_type: &str) -> Message {
        Message {
            headers: vec![
                Header {
                    name: ":message-type".to_string(),
                    value: HeaderValue::Str("exception".to_string()),
                },
                Header {
                    name: ":exception-type".to_string(),
                    value: HeaderValue::Str(exception_type.to_string()),
                },
            ],
            payload: b"{}".to_vec(),
        }
    }

    fn image_url_msg(url: &str) -> InMsg {
        blocks_msg(
            "user",
            vec![Block::Image {
                source: serde_json::json!({"type": "url", "url": url}),
            }],
        )
    }

    // --- map_model ---

    #[test]
    fn maps_claude_3_5_sonnet_to_claude_sonnet_4_5() {
        assert_eq!(
            map_model("claude-3-5-sonnet"),
            Some("claude-sonnet-4.5".to_string())
        );
    }

    #[test]
    fn maps_gpt_4o_to_none() {
        assert_eq!(map_model("gpt-4o"), None);
    }

    #[test]
    fn maps_sonnet_4_6_variant() {
        assert_eq!(
            map_model("claude-sonnet-4.6"),
            Some("claude-sonnet-4.6".to_string())
        );
    }

    #[test]
    fn maps_sonnet_5_variant() {
        assert_eq!(map_model("sonnet-5"), Some("claude-sonnet-5".to_string()));
    }

    #[test]
    fn maps_fable() {
        assert_eq!(map_model("Fable"), Some("claude-fable-5".to_string()));
    }

    #[test]
    fn maps_opus_variants() {
        assert_eq!(map_model("opus-4.5"), Some("claude-opus-4.5".to_string()));
        assert_eq!(map_model("opus-4.7"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("opus-4.8"), Some("claude-opus-4.8".to_string()));
        assert_eq!(map_model("opus"), Some("claude-opus-4.6".to_string()));
    }

    #[test]
    fn maps_haiku_auto_deepseek_glm() {
        assert_eq!(map_model("haiku"), Some("claude-haiku-4.5".to_string()));
        assert_eq!(map_model("auto"), Some("auto".to_string()));
        assert_eq!(map_model("deepseek"), Some("deepseek-3.2".to_string()));
        assert_eq!(map_model("glm-4"), Some("glm-5".to_string()));
    }

    #[test]
    fn maps_minimax_and_qwen() {
        assert_eq!(map_model("minimax-2.5"), Some("minimax-m2.5".to_string()));
        assert_eq!(map_model("minimax"), Some("minimax-m2.1".to_string()));
        assert_eq!(map_model("qwen3"), Some("qwen3-coder-next".to_string()));
    }

    #[test]
    fn maps_gpt_variants() {
        assert_eq!(map_model("gpt-terra"), Some("gpt-5.6-terra".to_string()));
        assert_eq!(map_model("gpt-luna"), Some("gpt-5.6-luna".to_string()));
        assert_eq!(map_model("gpt-sol"), Some("gpt-5.6-sol".to_string()));
        assert_eq!(map_model("gpt-5.6"), Some("gpt-5.6-sol".to_string()));
    }

    #[test]
    fn maps_unknown_model_to_none() {
        assert_eq!(map_model("llama-3"), None);
    }

    // --- map_model: 横杠(dash)分隔的版本号也要命中同一变体(#6) ---

    #[test]
    fn maps_dash_form_sonnet_variants() {
        // 横杠形 4-6 应命中 sonnet-4.6,而非落到默认 4.5。
        assert_eq!(
            map_model("claude-sonnet-4-6"),
            Some("claude-sonnet-4.6".to_string())
        );
        // 点分形仍然照旧。
        assert_eq!(
            map_model("claude-sonnet-4.6"),
            Some("claude-sonnet-4.6".to_string())
        );
    }

    #[test]
    fn maps_dash_form_opus_variants() {
        // 这些以前会漏匹配 dot 形而错落到默认 opus-4.6。
        assert_eq!(
            map_model("claude-opus-4-5"),
            Some("claude-opus-4.5".to_string())
        );
        assert_eq!(
            map_model("claude-opus-4-7"),
            Some("claude-opus-4.7".to_string())
        );
        assert_eq!(
            map_model("claude-opus-4-8"),
            Some("claude-opus-4.8".to_string())
        );
        // Bedrock 风格常见形。
        assert_eq!(
            map_model("anthropic.claude-opus-4-5-v1:0"),
            Some("claude-opus-4.5".to_string())
        );
    }

    #[test]
    fn maps_dash_form_minimax_and_gpt() {
        assert_eq!(map_model("minimax-2-5"), Some("minimax-m2.5".to_string()));
        assert_eq!(map_model("gpt-5-6"), Some("gpt-5.6-sol".to_string()));
    }

    #[test]
    fn dash_form_default_fallback_still_works() {
        // 没有具体版本号 → 仍落默认变体。
        assert_eq!(
            map_model("claude-opus"),
            Some("claude-opus-4.6".to_string())
        );
        assert_eq!(
            map_model("claude-sonnet"),
            Some("claude-sonnet-4.5".to_string())
        );
    }

    // --- anthropic_to_kiro ---

    #[test]
    fn converts_request_with_system_and_history() {
        let req = MessagesRequest {
            model: "sonnet".to_string(),
            system: Some(SystemPrompt::Text("S".to_string())),
            messages: vec![msg("user", "a"), msg("assistant", "b"), msg("user", "c")],
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
        };

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        assert_eq!(kiro.conversation_state.chat_trigger_type, "MANUAL");
        assert_eq!(kiro.conversation_state.agent_task_type, "vibe");
        assert!(
            kiro.conversation_state
                .current_message
                .user_input_message
                .content
                .contains('c')
        );
        assert_eq!(
            kiro.conversation_state
                .current_message
                .user_input_message
                .model_id,
            "claude-sonnet-4.5"
        );
        assert_eq!(kiro.conversation_state.history.len(), 2);

        // system 前置到首条消息文本里。
        match &kiro.conversation_state.history[0] {
            HistoryItem::UserInputMessage { user_input_message } => {
                assert!(user_input_message.content.contains('S'));
                assert!(user_input_message.content.contains('a'));
            }
            other => panic!("首条历史应为 UserInputMessage,实际: {other:?}"),
        }
        match &kiro.conversation_state.history[1] {
            HistoryItem::AssistantResponseMessage {
                assistant_response_message,
            } => {
                assert_eq!(assistant_response_message.content, "b");
            }
            other => panic!("第二条历史应为 AssistantResponseMessage,实际: {other:?}"),
        }
    }

    #[test]
    fn unknown_model_errors() {
        let req = MessagesRequest {
            model: "gpt-4o".to_string(),
            system: None,
            messages: vec![msg("user", "hi")],
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
        };
        assert_eq!(
            anthropic_to_kiro(&req, None),
            Err(ConvertError::UnknownModel("gpt-4o".to_string()))
        );
    }

    #[test]
    fn empty_messages_errors() {
        let req = MessagesRequest {
            model: "sonnet".to_string(),
            system: None,
            messages: vec![],
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
        };
        assert_eq!(
            anthropic_to_kiro(&req, None),
            Err(ConvertError::EmptyMessages)
        );
    }

    #[test]
    fn passes_through_profile_arn() {
        let req = MessagesRequest {
            model: "sonnet".to_string(),
            system: None,
            messages: vec![msg("user", "hi")],
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
        };
        let kiro = anthropic_to_kiro(&req, Some("arn:aws:x")).expect("转换应成功");
        assert_eq!(kiro.profile_arn.as_deref(), Some("arn:aws:x"));
    }

    // --- tools → spectask ---

    #[test]
    fn tools_present_sets_spectask_and_maps_tool_spec() {
        let mut req = base_req(vec![msg("user", "hi")]);
        req.tools = Some(vec![ToolDef {
            name: "get_weather".to_string(),
            description: Some("gets weather".to_string()),
            input_schema: serde_json::json!({"type": "object"}),
        }]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        assert_eq!(kiro.conversation_state.agent_task_type, "spectask");
        let ctx = &kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;
        let tools = ctx.tools.as_ref().expect("tools 应存在");
        assert_eq!(tools[0].tool_specification.name, "get_weather");
    }

    #[test]
    fn no_tools_keeps_vibe_and_none_context_tools() {
        let req = base_req(vec![msg("user", "hi")]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        assert_eq!(kiro.conversation_state.agent_task_type, "vibe");
        let ctx = &kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;
        assert!(ctx.tools.is_none());
    }

    // --- tool_result ---

    #[test]
    fn tool_result_maps_success_status_and_text() {
        let req = base_req(vec![blocks_msg(
            "user",
            vec![Block::ToolResult {
                tool_use_id: "tu1".to_string(),
                content: serde_json::json!("sunny"),
                is_error: Some(false),
            }],
        )]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        let ctx = &kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;
        let results = ctx.tool_results.as_ref().expect("tool_results 应存在");
        assert_eq!(results[0].tool_use_id, "tu1");
        assert_eq!(results[0].content[0].text, "sunny");
        assert_eq!(results[0].status, "success");
    }

    #[test]
    fn tool_result_is_error_true_maps_error_status() {
        let req = base_req(vec![blocks_msg(
            "user",
            vec![Block::ToolResult {
                tool_use_id: "tu1".to_string(),
                content: serde_json::json!("boom"),
                is_error: Some(true),
            }],
        )]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        let ctx = &kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;
        let results = ctx.tool_results.as_ref().expect("tool_results 应存在");
        assert_eq!(results[0].status, "error");
        assert_eq!(results[0].is_error, Some(true));
    }

    #[test]
    fn tool_result_array_content_flattens_text() {
        let req = base_req(vec![blocks_msg(
            "user",
            vec![Block::ToolResult {
                tool_use_id: "tu1".to_string(),
                content: serde_json::json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]),
                is_error: None,
            }],
        )]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        let ctx = &kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;
        let results = ctx.tool_results.as_ref().expect("tool_results 应存在");
        assert_eq!(results[0].content[0].text, "ab");
    }

    // --- empty-content fallback (契约 §2) ---

    #[test]
    fn tool_result_only_message_falls_back_to_tool_result_above() {
        let req = base_req(vec![blocks_msg(
            "user",
            vec![Block::ToolResult {
                tool_use_id: "tu1".to_string(),
                content: serde_json::json!("sunny"),
                is_error: None,
            }],
        )]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        let current = &kiro.conversation_state.current_message.user_input_message;
        assert_eq!(current.content, "(tool result above)");
        let results = current
            .user_input_message_context
            .tool_results
            .as_ref()
            .expect("tool_results 应存在");
        assert_eq!(results[0].tool_use_id, "tu1");
    }

    #[test]
    fn non_empty_user_message_content_unaffected() {
        let req = base_req(vec![msg("user", "hello")]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        let current = &kiro.conversation_state.current_message.user_input_message;
        assert_eq!(current.content, "hello");
    }

    // --- history tool_use ---

    #[test]
    fn history_assistant_tool_use_maps_to_tool_uses() {
        let req = base_req(vec![
            msg("user", "q"),
            blocks_msg(
                "assistant",
                vec![Block::ToolUse {
                    id: "tu1".to_string(),
                    name: "get_weather".to_string(),
                    input: serde_json::json!({"city": "Paris"}),
                }],
            ),
            msg("user", "result"),
        ]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        match &kiro.conversation_state.history[1] {
            HistoryItem::AssistantResponseMessage {
                assistant_response_message,
            } => {
                let tool_uses = assistant_response_message
                    .tool_uses
                    .as_ref()
                    .expect("tool_uses 应存在");
                assert_eq!(tool_uses[0].tool_use_id, "tu1");
                assert_eq!(tool_uses[0].name, "get_weather");
                assert_eq!(tool_uses[0].input["city"], "Paris");
            }
            other => panic!("第二条历史应为 AssistantResponseMessage,实际: {other:?}"),
        }
    }

    // --- image ---

    #[test]
    fn image_block_maps_to_images_and_keeps_text() {
        let req = base_req(vec![blocks_msg(
            "user",
            vec![
                Block::Image {
                    source: serde_json::json!({"type": "base64", "media_type": "image/png", "data": "AAAA"}),
                },
                Block::Text {
                    text: "desc".to_string(),
                },
            ],
        )]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        let current = &kiro.conversation_state.current_message.user_input_message;
        let images = current.images.as_ref().expect("images 应存在");
        assert_eq!(images[0].format, "png");
        assert_eq!(images[0].source.bytes, "AAAA");
        assert!(current.content.contains("desc"));
    }

    // --- kiro_events_to_anthropic ---

    #[test]
    fn concatenates_assistant_response_events_into_pong() {
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"po"}"#),
            event_frame("assistantResponseEvent", r#"{"content":"ng"}"#),
        ];

        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");

        assert_eq!(resp.role, "assistant");
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
        match &resp.content[0] {
            OutBlock::Text { text } => assert_eq!(text, "pong"),
            other => panic!("应为 Text,实际: {other:?}"),
        }
    }

    #[test]
    fn skips_frames_without_content_or_other_event_types() {
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"hi"}"#),
            event_frame("contextUsageEvent", r#"{"percent":42}"#),
            event_frame("assistantResponseEvent", "not json"),
            event_frame("assistantResponseEvent", r#"{"noContentField":true}"#),
        ];

        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");

        match &resp.content[0] {
            OutBlock::Text { text } => assert_eq!(text, "hi"),
            other => panic!("应为 Text,实际: {other:?}"),
        }
    }

    // --- kiro_events_to_anthropic: toolUseEvent → tool_use 块 ---

    #[test]
    fn tool_use_frames_reconstruct_tool_use_block() {
        let frames = vec![
            event_frame(
                "toolUseEvent",
                r#"{"name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"input":"","name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"input":"{\"ci","name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"input":"ty\": \"Paris","name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"input":"\"}","name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"name":"get_weather","stop":true,"toolUseId":"tu1"}"#,
            ),
        ];

        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");

        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            OutBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tu1");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "Paris");
            }
            other => panic!("应为 ToolUse,实际: {other:?}"),
        }
    }

    #[test]
    fn text_then_tool_use_mixed_content_order() {
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"Let me check. "}"#),
            event_frame(
                "toolUseEvent",
                r#"{"name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"input":"","name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"input":"{\"ci","name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"input":"ty\": \"Paris","name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"input":"\"}","name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"name":"get_weather","stop":true,"toolUseId":"tu1"}"#,
            ),
        ];

        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");

        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(resp.content.len(), 2);
        match &resp.content[0] {
            OutBlock::Text { text } => assert_eq!(text, "Let me check. "),
            other => panic!("首块应为 Text,实际: {other:?}"),
        }
        match &resp.content[1] {
            OutBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tu1");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "Paris");
            }
            other => panic!("次块应为 ToolUse,实际: {other:?}"),
        }
    }

    #[test]
    fn text_only_pong_regression_keeps_end_turn() {
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"po"}"#),
            event_frame("assistantResponseEvent", r#"{"content":"ng"}"#),
        ];

        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");

        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            OutBlock::Text { text } => assert_eq!(text, "pong"),
            other => panic!("应为 Text,实际: {other:?}"),
        }
    }

    #[test]
    fn two_distinct_tool_use_ids_produce_two_blocks_in_order() {
        let frames = vec![
            event_frame(
                "toolUseEvent",
                r#"{"name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"input":"{\"city\": \"Paris\"}","name":"get_weather","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"name":"get_weather","stop":true,"toolUseId":"tu1"}"#,
            ),
            event_frame("toolUseEvent", r#"{"name":"get_time","toolUseId":"tu2"}"#),
            event_frame(
                "toolUseEvent",
                r#"{"input":"{\"tz\": \"UTC\"}","name":"get_time","toolUseId":"tu2"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"name":"get_time","stop":true,"toolUseId":"tu2"}"#,
            ),
        ];

        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");

        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(resp.content.len(), 2);
        match &resp.content[0] {
            OutBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tu1");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "Paris");
            }
            other => panic!("首块应为 ToolUse tu1,实际: {other:?}"),
        }
        match &resp.content[1] {
            OutBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tu2");
                assert_eq!(name, "get_time");
                assert_eq!(input["tz"], "UTC");
            }
            other => panic!("次块应为 ToolUse tu2,实际: {other:?}"),
        }
    }

    #[test]
    fn tool_use_frame_helper_extracts_payload_for_tool_use_event_only() {
        let f = event_frame("toolUseEvent", r#"{"name":"x","toolUseId":"y"}"#);
        let v = tool_use_frame(&f).expect("应解析出 payload");
        assert_eq!(v["name"], "x");

        let other = event_frame("assistantResponseEvent", r#"{"content":"hi"}"#);
        assert_eq!(tool_use_frame(&other), None);

        let bad = event_frame("toolUseEvent", "not json");
        assert_eq!(tool_use_frame(&bad), None);
    }

    // --- frame_text_delta / new_message_id ---

    #[test]
    fn frame_text_delta_extracts_content_from_assistant_response_event() {
        let frame = event_frame("assistantResponseEvent", r#"{"content":"hi"}"#);
        assert_eq!(frame_text_delta(&frame), Some("hi".to_string()));
    }

    #[test]
    fn frame_text_delta_none_for_other_event_type() {
        let frame = event_frame("contextUsageEvent", r#"{"content":"hi"}"#);
        assert_eq!(frame_text_delta(&frame), None);
    }

    #[test]
    fn frame_text_delta_none_for_non_json_payload() {
        let frame = event_frame("assistantResponseEvent", "not json");
        assert_eq!(frame_text_delta(&frame), None);
    }

    #[test]
    fn frame_text_delta_none_for_missing_content_field() {
        let frame = event_frame("assistantResponseEvent", r#"{"noContentField":true}"#);
        assert_eq!(frame_text_delta(&frame), None);
    }

    #[test]
    fn new_message_id_has_msg_prefix_and_nontrivial_length() {
        let id = new_message_id();
        assert!(id.starts_with("msg_"));
        assert!(id.len() > 4);
    }

    // --- meteringEvent → 真实积分 ---

    #[test]
    fn metering_frame_extracts_nonzero_credits_snake_case() {
        let frame = event_frame(
            "meteringEvent",
            r#"{"unit":"credit","unitPlural":"credits","usage":3.5,"cache_read_input_tokens":128,"cache_creation_input_tokens":64}"#,
        );
        let m = metering_frame(&frame).expect("应解析出 meteringEvent");
        assert!(m.credits > 0.0);
        assert_eq!(m.credits, 3.5);
        assert_eq!(m.cache_read_input_tokens, Some(128));
        assert_eq!(m.cache_creation_input_tokens, Some(64));
    }

    #[test]
    fn metering_frame_accepts_camel_case_cache_keys() {
        let frame = event_frame(
            "meteringEvent",
            r#"{"usage":1.0,"cacheReadInputTokens":10,"cacheCreationInputTokens":20}"#,
        );
        let m = metering_frame(&frame).expect("应解析出 meteringEvent");
        assert_eq!(m.credits, 1.0);
        assert_eq!(m.cache_read_input_tokens, Some(10));
        assert_eq!(m.cache_creation_input_tokens, Some(20));
    }

    #[test]
    fn metering_frame_none_for_other_event_or_missing_usage() {
        // 事件类型不符 → None
        assert_eq!(
            metering_frame(&event_frame("assistantResponseEvent", r#"{"usage":1.0}"#)),
            None
        );
        // 非法 JSON → None
        assert_eq!(
            metering_frame(&event_frame("meteringEvent", "not json")),
            None
        );
        // 缺 usage → None
        assert_eq!(
            metering_frame(&event_frame("meteringEvent", r#"{"unit":"credit"}"#)),
            None
        );
    }

    #[test]
    fn extract_metering_picks_last_metering_event() {
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"hi"}"#),
            event_frame("meteringEvent", r#"{"usage":1.0}"#),
            event_frame("assistantResponseEvent", r#"{"content":" there"}"#),
            event_frame(
                "meteringEvent",
                r#"{"usage":4.25,"cache_read_input_tokens":7}"#,
            ),
        ];
        let m = extract_metering(&frames).expect("应取到末次 meteringEvent");
        assert_eq!(m.credits, 4.25);
        assert_eq!(m.cache_read_input_tokens, Some(7));
    }

    #[test]
    fn extract_metering_none_when_no_metering_frame() {
        let frames = vec![event_frame("assistantResponseEvent", r#"{"content":"hi"}"#)];
        assert_eq!(extract_metering(&frames), None);
    }

    // --- 截断信号 → stop_reason(#11) ---

    #[test]
    fn frame_truncation_detects_content_length_exceeded_exception() {
        let f = exception_frame("ContentLengthExceededException");
        assert_eq!(frame_truncation(&f), Some(Truncation::MaxTokens));
    }

    #[test]
    fn frame_truncation_ignores_other_exception_types() {
        assert_eq!(
            frame_truncation(&exception_frame("ThrottlingException")),
            None
        );
    }

    #[test]
    fn frame_truncation_detects_context_window_at_100_percent() {
        let f = event_frame("contextUsageEvent", r#"{"contextUsagePercentage":100.0}"#);
        assert_eq!(frame_truncation(&f), Some(Truncation::ContextWindow));
        // < 100 不算截断。
        let under = event_frame("contextUsageEvent", r#"{"contextUsagePercentage":42.0}"#);
        assert_eq!(frame_truncation(&under), None);
    }

    #[test]
    fn content_length_exceeded_maps_to_max_tokens_stop_reason() {
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"partial"}"#),
            exception_frame("ContentLengthExceededException"),
        ];
        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");
        assert_eq!(resp.stop_reason.as_deref(), Some("max_tokens"));
        match &resp.content[0] {
            OutBlock::Text { text } => assert_eq!(text, "partial"),
            other => panic!("应为 Text,实际: {other:?}"),
        }
    }

    #[test]
    fn context_window_exceeded_maps_to_model_context_window_exceeded() {
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"x"}"#),
            event_frame("contextUsageEvent", r#"{"contextUsagePercentage":100.0}"#),
        ];
        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");
        assert_eq!(
            resp.stop_reason.as_deref(),
            Some("model_context_window_exceeded")
        );
    }

    #[test]
    fn tool_use_wins_over_truncation_signal() {
        // 同批帧里既有 tool_use 又有截断信号:tool_use 优先(截断是下一轮的状态)。
        let frames = vec![
            event_frame("toolUseEvent", r#"{"name":"f","toolUseId":"tu1"}"#),
            event_frame(
                "toolUseEvent",
                r#"{"input":"{}","name":"f","toolUseId":"tu1"}"#,
            ),
            event_frame(
                "toolUseEvent",
                r#"{"name":"f","stop":true,"toolUseId":"tu1"}"#,
            ),
            exception_frame("ContentLengthExceededException"),
        ];
        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn no_truncation_keeps_end_turn() {
        let frames = vec![event_frame(
            "assistantResponseEvent",
            r#"{"content":"done"}"#,
        )];
        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
    }

    // --- 远程图片 URL → 明确报错而非静默丢弃(#12) ---

    #[test]
    fn remote_image_url_type_errors_not_dropped() {
        let req = base_req(vec![image_url_msg("https://example.com/a.png")]);
        let err = anthropic_to_kiro(&req, None).expect_err("远程图片 URL 应报错");
        assert_eq!(
            err,
            ConvertError::RemoteImageUrl("https://example.com/a.png".to_string())
        );
    }

    #[test]
    fn remote_image_url_in_history_also_errors() {
        let req = base_req(vec![
            image_url_msg("http://cdn.example/x.jpg"),
            msg("assistant", "ok"),
            msg("user", "continue"),
        ]);
        let err = anthropic_to_kiro(&req, None).expect_err("历史里的远程图片 URL 也应报错");
        assert!(matches!(err, ConvertError::RemoteImageUrl(_)));
    }

    #[test]
    fn base64_image_still_accepted() {
        // 回归:内联 base64 图片不受影响。
        let req = base_req(vec![blocks_msg(
            "user",
            vec![Block::Image {
                source: serde_json::json!({"type": "base64", "media_type": "image/png", "data": "AAAA"}),
            }],
        )]);
        let kiro = anthropic_to_kiro(&req, None).expect("base64 图片应成功");
        let images = kiro
            .conversation_state
            .current_message
            .user_input_message
            .images
            .as_ref()
            .expect("images 应存在");
        assert_eq!(images[0].source.bytes, "AAAA");
    }
}
