//! Anthropic ⇄ Kiro 转换器(转换逻辑自写;Kiro 形状照观测的数据面契约)。
//!
//! 覆盖范围:`/v1/messages` 请求体 → Kiro `KiroRequest`(含 `tool_result` /
//! `tool_use`(历史)/ `image` 内容块,以及工具规格 `tools`→`spectask`);
//! Kiro 事件流帧(已解码为 [`Message`])→ Anthropic `MessagesResponse`(响应侧
//! `tool_use` 块见后续任务)。
//!
//! 上游在 200 事件流里下发的错误(非截断类 exception 帧)不进响应体,而是经
//! [`extract_exception`] / [`exception_status`] 交给协议层转成对外错误状态码。

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
/// 由 `thinking` 配置生成上游认的指令前缀。
///
/// 上游不认 Anthropic 的 `thinking` 字段,它认的是**写在 system 文本最前面的标签**
/// (照观测)。`enabled` 带预算上限,`adaptive` 让模型自行决定深浅。
/// 未开启 thinking 时返回 None —— 绝不无故给 system 加东西。
fn thinking_directive(req: &MessagesRequest) -> Option<String> {
    let t = req.thinking.as_ref()?;
    match t.thinking_type.as_str() {
        "enabled" => Some(format!(
            "<thinking_mode>enabled</thinking_mode><max_thinking_length>{}</max_thinking_length>",
            t.budget_tokens
        )),
        "adaptive" => Some("<thinking_mode>adaptive</thinking_mode>".to_string()),
        _ => None,
    }
}

/// 定出本次请求的 `conversationId`。
///
/// 优先从客户端 `metadata.user_id` 里提取 session UUID —— 真实客户端(Claude Code)会把
/// 会话标识放在那里,于是**同一次会话的多个请求共用同一个 conversationId**,这正是真实
/// 客户端的形态。取不到才新生成一个 UUID。
///
/// 此前这里是 `random_hex_id()`:每请求一个新的 32 位无连字符十六进制。两处都不对——
/// 形状不是 UUID,而且每个请求在上游看来都是一段全新的对话。
fn conversation_id_for(req: &MessagesRequest) -> String {
    req.metadata
        .as_ref()
        .and_then(|m| m.user_id.as_deref())
        .and_then(extract_session_id)
        .unwrap_or_else(crate::kiro::uuid_v4)
}

/// 从 `metadata.user_id` 里抠出 session UUID。
///
/// 两种形态都认(照真实客户端观测):
/// - JSON 串,含 `session_id` 键;
/// - 任意串里出现 `session_<uuid>` 片段。
///
/// 两条都要求那段确实是标准 UUID,否则宁可返回 None 去新生成 —— 把一段来路不明的用户
/// 输入原样当会话标识发给上游,既可能带上隐私,也可能是个畸形值。
fn extract_session_id(user_id: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(user_id)
        && let Some(sid) = v.get("session_id").and_then(|x| x.as_str())
        && crate::kiro::is_uuid(sid)
    {
        return Some(sid.to_string());
    }
    let pos = user_id.find("session_")?;
    let rest = &user_id[pos + "session_".len()..];
    let cand = rest.get(..36)?;
    crate::kiro::is_uuid(cand).then(|| cand.to_string())
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
        // opus-5 必须在默认分支**之前**判:漏了它就会被静默降级成 opus-4.6,
        // 客户端拿到的是一个它没要过的、更弱的模型,而且毫无提示。
        } else if m.contains("opus-5") || m.contains("opus5") {
            "claude-opus-5".to_string()
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

/// 从整段对话历史里收集出现过的工具名(按首次出现顺序,去重)。
///
/// 上游硬性要求:只要消息里出现 `toolUse`/`toolResult` 内容块,`toolConfig` 就必须存在
/// (`TOOL_CONFIG_MISSING`)。而工具**可能在到达这里之前就被合法地丢掉了** —— Responses
/// 协议里的内置工具(`web_search` / `local_shell` …)由 OpenAI 服务端自己执行、中枢没有
/// 等价物,故转换时丢弃;若客户端这一轮**只带了内置工具**,`tools` 就成了空数组。
///
/// 于是请求变成「有工具调用、没有工具定义」—— 一个我们自己造出来的畸形请求,上游必拒。
/// 实测就是 codex 的 502:带工具历史 + 仅内置工具 → 502,同样历史带一个函数工具 → 200。
fn tool_names_in_history(messages: &[InMsg]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in messages {
        if let ContentIn::Blocks(blocks) = &m.content {
            for b in blocks {
                if let Block::ToolUse { name, .. } = b
                    && seen.insert(name.clone())
                {
                    out.push(name.clone());
                }
            }
        }
    }
    out
}

/// 为历史里出现过、但当前 `tools` 里没有的工具补一份最小规格。
///
/// 补的是**模型自己调用过**的工具,补上定义只是让请求自洽;不补则整个请求被上游拒掉、
/// 那一轮对话彻底失败。schema 给空对象:我们无从知道原始定义(客户端这轮没送),
/// 而上游只要求 `toolConfig` 存在且形状合法,不校验它与历史调用的参数是否吻合。
fn tool_specs_with_history_fallback(
    tools: &[ToolDef],
    messages: &[InMsg],
    map: &mut std::collections::HashMap<String, String>,
) -> Vec<ToolSpec> {
    let mut specs = map_tools(tools, map);
    let declared: std::collections::HashSet<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    for name in tool_names_in_history(messages) {
        if !declared.contains(name.as_str()) {
            specs.push(ToolSpec {
                tool_specification: ToolSpecInner {
                    description: name.clone(),
                    name: map_tool_name(&name, map),
                    input_schema: InputSchemaJson {
                        json: normalize_json_schema(&serde_json::Value::Null),
                    },
                },
            });
        }
    }
    specs
}

/// 按字符类别加权估算 token 数。
///
/// 全局 `字符数 / 4` 对中文严重低估:CJK 大约 **1.5 字/token**,而英文才 4 字符/token ——
/// 一段纯中文按 /4 算,估出来只有真实值的三分之一强。这个偏差直接落到用量统计与按 USD
/// 设的限额上:同样的钱,中文用户能超支两三倍而限额毫无察觉。
///
/// 权重:CJK 统一表意文字按 2/3 token 每字,其余按 1/4。全项目**只有这一处**口径,
/// 输入估算、输出估算、`/v1/messages/count_tokens` 三条路都走它 —— 各写一套必然各说各话。
pub fn estimate_tokens(text: &str) -> usize {
    let (cjk, other) = text.chars().fold((0usize, 0usize), |(c, o), ch| {
        if is_cjk_char(ch) {
            (c + 1, o)
        } else {
            (c, o + 1)
        }
    });
    // (cjk * 2 + 2) / 3 与 (other + 3) / 4 都是向上取整,避免短文本被抹成 0。
    (cjk * 2).div_ceil(3) + other.div_ceil(4)
}

/// 是否 CJK 字符(统一表意文字 + 扩展 A + 兼容表意 + 假名 + 谚文)。
///
/// 这些书写系统的分词密度都远高于拉丁文,按同一档加权即可 —— 再细分收益很小,
/// 而估算本来就只是估算。
pub fn is_cjk_char(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // 平假名 / 片假名
        | 0x3400..=0x4DBF // 扩展 A
        | 0x4E00..=0x9FFF // 统一表意文字
        | 0xAC00..=0xD7AF // 谚文音节
        | 0xF900..=0xFAFF // 兼容表意文字
    )
}

/// 工具名长度上限(照观测的上游约束)。
const TOOL_NAME_MAX_LEN: usize = 63;

/// 工具描述长度上限。超长描述会把请求体撑大,且上游有自己的上限;这里先行安全截断。
const TOOL_DESC_MAX_CHARS: usize = 10_000;

/// 超长工具名 → 确定性短名:`前缀 + "_" + 8 位 sha256`,总长恰好 63。
///
/// 必须**确定性**:同一个工具名在每次请求里都要缩成同一个短名,否则模型在多轮之间看到的
/// 工具会变来变去。按字符边界截前缀,避免把多字节字符切一半。
fn shorten_tool_name(name: &str) -> String {
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(name.as_bytes()));
    let prefix_max = TOOL_NAME_MAX_LEN - 1 - 8;
    let prefix = match name.char_indices().nth(prefix_max) {
        Some((idx, _)) => &name[..idx],
        None => name,
    };
    format!("{}_{}", prefix, &hash[..8])
}

/// 需要缩短就缩短,并把 `短名 → 原名` 记进映射(供把上游回来的 tool_use 名字还原)。
fn map_tool_name(name: &str, map: &mut std::collections::HashMap<String, String>) -> String {
    if name.len() <= TOOL_NAME_MAX_LEN {
        return name.to_string();
    }
    let short = shorten_tool_name(name);
    map.insert(short.clone(), name.to_string());
    short
}

/// JSON Schema 的标准类型词表(小写)。
const SCHEMA_TYPE_TOKENS: [&str; 7] = [
    "object", "array", "string", "number", "integer", "boolean", "null",
];

/// 单个类型 token 归一成小写标准形态。
///
/// 只认得出来的才改:`"OBJECT"`→`"object"`;认不出来的(自定义/未来词汇)原样留着,
/// 免得把小写化当成一次静默的语义改写。
fn normalize_type_token(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    if SCHEMA_TYPE_TOKENS.contains(&lower.as_str()) {
        lower
    } else {
        s.to_string()
    }
}

/// 归一 `type` 字段:字符串,或 JSON Schema 允许的字符串数组(联合类型)。
fn normalize_type_field(v: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) => Value::String(normalize_type_token(s)),
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|x| match x {
                    Value::String(s) => Value::String(normalize_type_token(s)),
                    other => other.clone(),
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// 递归把 schema 里的类型方言归一成小写标准形态。
///
/// 上游只认小写标准写法,而 Gemini 那套工具声明用的是 `"type":"OBJECT"` / `"STRING"`
/// 这类大写方言 —— 原样透传的话,上游拒掉的是**整条请求**,客户端只看到一个 400。
/// 必须**递归**:大写方言在嵌套层同样成立,只改顶层等于没改。这里一个字段都不新增、
/// 不删除,纯粹改类型 token 的大小写。
fn normalize_schema_dialect(schema: &serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    let Value::Object(obj) = schema else {
        return schema.clone();
    };
    let mut out = Map::new();
    for (k, v) in obj {
        let nv = match k.as_str() {
            "type" => normalize_type_field(v),
            // 值本身就是一份子 schema(`items` 另有旧式的"逐位 schema 数组"形态)。
            "items"
            | "additionalProperties"
            | "not"
            | "if"
            | "then"
            | "else"
            | "contains"
            | "propertyNames" => match v {
                Value::Array(arr) => {
                    Value::Array(arr.iter().map(normalize_schema_dialect).collect())
                }
                Value::Object(_) => normalize_schema_dialect(v),
                other => other.clone(),
            },
            // 值是子 schema 数组。
            "anyOf" | "oneOf" | "allOf" | "prefixItems" => match v {
                Value::Array(arr) => {
                    Value::Array(arr.iter().map(normalize_schema_dialect).collect())
                }
                other => other.clone(),
            },
            // 值是「名字 → 子 schema」的表。
            "properties" | "patternProperties" | "$defs" | "definitions" => match v {
                Value::Object(m) => Value::Object(
                    m.iter()
                        .map(|(name, s)| (name.clone(), normalize_schema_dialect(s)))
                        .collect(),
                ),
                other => other.clone(),
            },
            _ => v.clone(),
        };
        out.insert(k.clone(), nv);
    }
    Value::Object(out)
}

/// 上游一定收得下的最小合法 schema(客户端压根没给出对象时的兜底)。
fn fallback_object_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {},
        "required": [],
        "additionalProperties": true
    })
}

/// 把工具的 JSON Schema 规范成上游一定收得下的形状。
///
/// 客户端给的 schema 千奇百怪:`type` 缺失或不是字符串、`properties` 是 null、
/// `required` 是 null 或混进了非字符串、`additionalProperties` 给了个数字、类型写成
/// 大写方言……原样透传的话上游直接拒掉**整条请求**,而客户端那边只看到一个语焉不详的 400。
/// 这里只补形状、归一类型大小写,不改语义:已经合法的字段一律原样保留。
fn normalize_json_schema(schema: &serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    if !schema.is_object() {
        return fallback_object_schema();
    }
    // 先递归归一类型方言,再在顶层补形状 —— 补形状只对顶层成立(嵌套的
    // `{"type":"string"}` 不该被硬塞 properties/required/additionalProperties)。
    let Value::Object(mut obj) = normalize_schema_dialect(schema) else {
        return fallback_object_schema();
    };
    if !obj
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        obj.insert("type".into(), Value::String("object".into()));
    }
    if !matches!(obj.get("properties"), Some(Value::Object(_))) {
        obj.insert("properties".into(), Value::Object(Map::new()));
    }
    let required = match obj.remove("required") {
        Some(Value::Array(arr)) => Value::Array(
            arr.into_iter()
                .filter_map(|v| v.as_str().map(|s| Value::String(s.to_string())))
                .collect(),
        ),
        _ => Value::Array(Vec::new()),
    };
    obj.insert("required".into(), required);
    if !matches!(
        obj.get("additionalProperties"),
        Some(Value::Bool(_)) | Some(Value::Object(_))
    ) {
        obj.insert("additionalProperties".into(), Value::Bool(true));
    }
    Value::Object(obj)
}

/// 把请求里的 Anthropic `ToolDef` 列表映射成 Kiro `ToolSpec` 列表(照契约 §2)。
///
/// `map` 收集"短名 → 原名",供上游把 tool_use 回来时还原成客户端认识的名字。
fn map_tools(
    tools: &[ToolDef],
    map: &mut std::collections::HashMap<String, String>,
) -> Vec<ToolSpec> {
    tools
        .iter()
        .map(|t| {
            // 描述**不能是空串**:上游对空描述回 `Invalid tool use format` /
            // `REQUEST_BODY_INVALID`,拒掉的是整条请求(线上实测:同一个工具带描述 200、
            // 去掉描述 400)。客户端没给时用工具名兜底 —— 非空、且是此处能给出的最有信息量
            // 的值,不会误导模型。
            let desc = t.description.clone().unwrap_or_default();
            let desc = if desc.trim().is_empty() {
                t.name.clone()
            } else {
                desc
            };
            let desc = match desc.char_indices().nth(TOOL_DESC_MAX_CHARS) {
                Some((i, _)) => desc[..i].to_string(),
                None => desc,
            };
            ToolSpec {
                tool_specification: ToolSpecInner {
                    name: map_tool_name(&t.name, map),
                    description: desc,
                    input_schema: InputSchemaJson {
                        json: normalize_json_schema(&t.input_schema),
                    },
                },
            }
        })
        .collect()
}

/// 把 `tool_result` 块的 `content`(字符串或内容块数组)拍平成纯文本(照观测)。
///
/// 数组元素的处理:带 `text` 的块取其文本;裸字符串取自身;图片块在此跳过——
/// 图片另走 [`message_images`] 提到消息级 `images`(Kiro 的 `toolResults.content`
/// 只收文本,塞不进图片,更不能把 base64 当正文喂给模型);其余无法识别的块按
/// 紧凑 JSON 原样带上,不静默丢。
fn tool_result_text(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    let Some(arr) = content.as_array() else {
        return String::new();
    };
    arr.iter()
        .filter_map(|v| {
            if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
                return Some(t.to_string());
            }
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
            if v.is_null() || is_image_block(v) {
                return None;
            }
            Some(v.to_string())
        })
        .collect::<Vec<_>>()
        .concat()
}

/// 内容块是否为图片块(Anthropic 的 `image`,或 OpenAI 形状的 `image_url`)。
fn is_image_block(v: &serde_json::Value) -> bool {
    matches!(
        v.get("type").and_then(|t| t.as_str()),
        Some("image" | "image_url")
    )
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
fn message_tool_uses(
    msg: &InMsg,
    map: &mut std::collections::HashMap<String, String>,
) -> Vec<ToolUseWire> {
    let ContentIn::Blocks(blocks) = &msg.content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::ToolUse { id, name, input } => Some(ToolUseWire {
                tool_use_id: id.clone(),
                // **历史里的工具名也要走同一套缩短**。
                //
                // 此前只有 `tools` 列表里的名字被缩短,history 里的 `toolUses` 发的还是原名 ——
                // 于是上游看到"声明的工具叫短名、历史里调用的却是另一个名字",两者对不上。
                // 缩短是确定性的,所以同一个名字在两处必然缩成同一个短名。
                name: map_tool_name(name, map),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// 把一个 Anthropic 图片 `source` 对象映射成 Kiro `ImageBlock`。
///
/// Kiro 数据面只接受内联 base64(`source.type=="base64"`)。遇到远程图片
/// (Anthropic 的 `source.type=="url"`,或任何带 http(s) `url` 字段的图片源)
/// 一律 **报错**(`ConvertError::RemoteImageUrl`)而不是静默丢弃——否则视觉
/// 请求会悄悄丢图、模型收不到图片。OpenAI/Gemini 前端把无法内联的远程图片
/// 编码成 `{"type":"url","url":...}` 转到这里统一拦截。
/// 既非 base64 也非可识别远程 URL 的源(空/未知)→ `Ok(None)`,无图可传。
fn image_from_source(source: &serde_json::Value) -> Result<Option<ImageBlock>, ConvertError> {
    let is_url_type = source.get("type").and_then(|t| t.as_str()) == Some("url");
    let url_field = source.get("url").and_then(|u| u.as_str());
    if is_url_type || url_field.map(is_remote_url).unwrap_or(false) {
        let u = url_field.unwrap_or("").to_string();
        return Err(ConvertError::RemoteImageUrl(u));
    }
    if source.get("type").and_then(|t| t.as_str()) != Some("base64") {
        return Ok(None);
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
    Ok(Some(ImageBlock {
        format,
        source: ImageSource { bytes },
    }))
}

/// 解析 `data:<mime>;base64,<data>` 形式的 data URL,失败返回 `None`。
fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (mime, data) = rest.split_once(";base64,")?;
    Some((mime.to_string(), data.to_string()))
}

/// 从 `tool_result` 块的 `content` 里挑出内嵌的图片块,追加进 `out`。
///
/// 与顶层图片同一套策略(见 [`image_from_source`]):内联 base64 透传、远程 URL 报错。
/// Kiro 的 `toolResults.content` 只有文本字段,故图片提到消息级 `images` 通道下发。
/// 认两种形状:Anthropic 的 `{"type":"image","source":{…}}` 与 OpenAI 的
/// `{"type":"image_url","image_url":{"url":…}}`(后者 data URL 就地内联)。
fn collect_tool_result_images(
    content: &serde_json::Value,
    out: &mut Vec<ImageBlock>,
) -> Result<(), ConvertError> {
    let Some(arr) = content.as_array() else {
        return Ok(());
    };
    for v in arr {
        if !is_image_block(v) {
            continue;
        }
        let source = match v.get("source") {
            Some(s) => s.clone(),
            None => {
                let Some(url) = v
                    .get("image_url")
                    .and_then(|iu| iu.get("url").or(Some(iu)))
                    .and_then(|u| u.as_str())
                else {
                    continue;
                };
                match parse_data_url(url) {
                    Some((mime, data)) => {
                        serde_json::json!({"type": "base64", "media_type": mime, "data": data})
                    }
                    None => serde_json::json!({"type": "url", "url": url}),
                }
            }
        };
        if let Some(img) = image_from_source(&source)? {
            out.push(img);
        }
    }
    Ok(())
}

/// 从一条消息里提取所有图片(顶层 `Block::Image` + `tool_result` 内嵌的 `image` 块),
/// 映射成 Kiro `ImageBlock` 列表。两者走同一处理路径:能内联就透传、远程 URL 就报错。
fn message_images(msg: &InMsg) -> Result<Vec<ImageBlock>, ConvertError> {
    let ContentIn::Blocks(blocks) = &msg.content else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Image { source } => {
                if let Some(img) = image_from_source(source)? {
                    out.push(img);
                }
            }
            Block::ToolResult { content, .. } => collect_tool_result_images(content, &mut out)?,
            _ => {}
        }
    }
    Ok(out)
}

/// 判断字符串是否为远程 http(s) URL(用于图片源识别)。
fn is_remote_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// 末条消息是 assistant 预填(prefill)时补给 `currentMessage` 的续写指令。
///
/// Kiro 的 `currentMessage` 只能是 `userInputMessage`,而预填属于助手轮次,
/// 只能落在 `history` 里;为了让上游接着预填往下写,当前轮补一条最小指令。
const PREFILL_CONTINUATION: &str = "Continue.";

/// 把 Anthropic `/v1/messages` 请求转换为 Kiro 数据面请求体。
///
/// - 模型映射失败 → `Err(ConvertError::UnknownModel)`。
/// - `system`(若存在)前置到首条消息的文本前面。
/// - 末条消息作为 `currentMessage.userInputMessage`,其余进入 `history`
///   (`assistant` 角色 → `AssistantResponseMessage`,否则 → `UserInputMessage`)。
/// - 末条消息若是 `assistant`(预填/prefill):整条进 `history` 的
///   `assistantResponseMessage`(连同其 `tool_use` 块),`currentMessage` 用
///   `PREFILL_CONTINUATION` 续写指令占位——助手文本不能冒充用户输入。
/// - 切分之前先做一遍消息整型(见 [`normalize_hub_messages`]):剔掉配不上对的工具块、
///   把相邻同角色消息并成一轮。两件事都放在这里做,四个协议入口才都能吃到。
/// - 有 `tools` → `agentTaskType="spectask"` 且当前消息上下文带映射后的工具规格;无 tools → `"vibe"`。
/// - `tool_result` / `tool_use` / `image` 内容块(照契约/观测)分别映射进对应消息的
///   `toolResults` / `toolUses` / `images`。
/// - `max_tokens` / `tool_choice`:Kiro 数据面 wire(见 `kiro::wire`)没有任何对应字段,
///   故意不转发;上游只在自己命中预算时用 `ContentLengthExceededException` 帧回报截断。
/// 一次转换的产物:上行请求体 + 工具名还原表。
///
/// 之所以要把表带出来:超长工具名在发给上游前会被缩短,上游回来的 `tool_use` 用的就是
/// 短名。不还原的话,客户端收到一个**它自己没声明过**的工具名,那一轮工具调用直接作废。
#[derive(Debug, Clone)]
pub struct Converted {
    pub request: KiroRequest,
    /// `短名 → 原名`。工具名都没超长时为空。
    pub tool_name_map: std::collections::HashMap<String, String>,
}

/// 同 [`anthropic_to_kiro`],但一并返回工具名还原表。
pub fn anthropic_to_kiro_full(
    req: &MessagesRequest,
    profile_arn: Option<&str>,
) -> Result<Converted, ConvertError> {
    let mut tool_name_map = std::collections::HashMap::new();
    let request = anthropic_to_kiro_inner(req, profile_arn, &mut tool_name_map)?;
    Ok(Converted {
        request,
        tool_name_map,
    })
}

/// 只要请求体、不关心工具名还原表时用它(测试与不涉工具的路径)。
pub fn anthropic_to_kiro(
    req: &MessagesRequest,
    profile_arn: Option<&str>,
) -> Result<KiroRequest, ConvertError> {
    anthropic_to_kiro_full(req, profile_arn).map(|c| c.request)
}

/// 把 `ContentIn` 归一成块序列;裸字符串 → 单个文本块(空串不产块,免得凭空多出空文本块)。
fn content_into_blocks(content: ContentIn) -> Vec<Block> {
    match content {
        ContentIn::Text(s) if s.is_empty() => Vec::new(),
        ContentIn::Text(s) => vec![Block::Text { text: s }],
        ContentIn::Blocks(blocks) => blocks,
    }
}

/// 这条消息里是否已有非空文本块(决定合并时要不要补空行分隔)。
fn blocks_have_text(blocks: &[Block]) -> bool {
    blocks
        .iter()
        .any(|b| matches!(b, Block::Text { text } if !text.is_empty()))
}

/// 这个块对上行请求体还有没有贡献:纯空白文本、以及无法转发给上游的未知块都算没有。
fn block_is_inert(b: &Block) -> bool {
    match b {
        Block::Text { text } => text.trim().is_empty(),
        Block::Other => true,
        Block::ToolUse { .. } | Block::ToolResult { .. } | Block::Image { .. } => false,
    }
}

/// 把 `next` 并进相邻的同角色消息 `prev`(块序列顺接,顺序即客户端给的顺序)。
///
/// 两边都有文本时,在 `next` 的首个非空文本块前补 `\n\n`:消息文本是无缝 concat,不补分隔
/// 会把两轮文本粘成一个词(`"hi"+"there"` → `"hithere"`)。工具结果/图片块不参与文本拍平,
/// 故纯工具结果的合并不会平白多出空行。
fn merge_into_previous_turn(prev: &mut InMsg, next: InMsg) {
    let mut blocks = content_into_blocks(std::mem::replace(
        &mut prev.content,
        ContentIn::Blocks(Vec::new()),
    ));
    let mut incoming = content_into_blocks(next.content);
    if blocks_have_text(&blocks) {
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

/// 相邻同角色消息并成一轮。
///
/// 上游要的是「一问一答严格交替、工具调用与其应答同处相邻两轮」的形状:这里的消息是 1:1
/// 铺进 Kiro history、末条当 currentMessage 的,连续两条同角色消息一旦原样铺开,history
/// 就不再交替,而并行工具调用的多个应答还会被拆到两轮里去 —— 上游据此判定"有工具调用没被
/// 应答",拒掉整条请求。
///
/// **幂等**:一趟之后不再有相邻同角色,再过一遍逐条原样入列,产物一模一样;没有相邻同角色
/// 的输入(绝大多数请求)连块化都不做,形状与不经此步完全一致。
fn merge_adjacent_same_role(messages: Vec<InMsg>) -> Vec<InMsg> {
    let mut out: Vec<InMsg> = Vec::with_capacity(messages.len());
    for msg in messages {
        match out.last_mut() {
            Some(prev) if same_turn_side(&prev.role, &msg.role) => {
                merge_into_previous_turn(prev, msg)
            }
            _ => out.push(msg),
        }
    }
    out
}

/// 两条消息是否属于同一侧的轮次。
///
/// 判据**必须与下游一致**:下游只分「assistant」与「其余」两侧(见 history 构造处的
/// `msg.role == "assistant"`)。此处若按角色字符串精确相等来判,`system` 紧跟 `user`、
/// 或 `developer` 紧跟 `user` 就不会被合并 —— 可它们到了下游全都折成用户轮,于是相邻的
/// 用户轮原封不动送到上游,整条请求被打回。Responses 入口的 role 是裸字符串透传的,
/// system/developer 能直接进 messages,这条路是活的。
fn same_turn_side(a: &str, b: &str) -> bool {
    (a == "assistant") == (b == "assistant")
}

/// 剔除配不上对的工具块:没有 `tool_result` 应答的 `tool_use`,以及反过来找不到
/// `tool_use` 的孤立 `tool_result`。
///
/// 悬空的工具调用在真实会话里很常见 —— 用户中途打断、上一轮超时、客户端自己裁剪了历史,
/// 留下的都是"调了工具但没有结果"的一轮。这种历史原样发上去,上游 400 掉的是**整条请求**,
/// 客户端只看到一次莫名其妙的失败,而这一轮本来完全可以正常作答。
///
/// 剔到整条消息只剩空壳时把该消息一并删掉(留着只会变成一轮无意义的占位文本);末条不删 ——
/// 它决定 currentMessage 的角色与历史切分,删了会改变整个请求的语义。
fn prune_unpaired_tool_blocks(messages: Vec<InMsg>) -> Vec<InMsg> {
    let mut use_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in &messages {
        if let ContentIn::Blocks(blocks) = &m.content {
            for b in blocks {
                match b {
                    Block::ToolUse { id, .. } => {
                        use_ids.insert(id.clone());
                    }
                    Block::ToolResult { tool_use_id, .. } => {
                        result_ids.insert(tool_use_id.clone());
                    }
                    _ => {}
                }
            }
        }
    }
    if use_ids.iter().all(|id| result_ids.contains(id))
        && result_ids.iter().all(|id| use_ids.contains(id))
    {
        return messages;
    }

    let last = messages.len().saturating_sub(1);
    let mut out = Vec::with_capacity(messages.len());
    for (i, msg) in messages.into_iter().enumerate() {
        let InMsg { role, content } = msg;
        let ContentIn::Blocks(blocks) = content else {
            out.push(InMsg { role, content });
            continue;
        };
        let before = blocks.len();
        let kept: Vec<Block> = blocks
            .into_iter()
            .filter(|b| match b {
                Block::ToolUse { id, .. } => result_ids.contains(id),
                Block::ToolResult { tool_use_id, .. } => use_ids.contains(tool_use_id),
                _ => true,
            })
            .collect();
        if kept.len() != before && i != last && kept.iter().all(block_is_inert) {
            continue;
        }
        out.push(InMsg {
            role,
            content: ContentIn::Blocks(kept),
        });
    }
    out
}

/// 中枢侧的消息整型:先剔掉配不上对的工具块,再把相邻同角色消息并成一轮。
///
/// 顺序不能反:剔除会整条删掉只剩空壳的轮次,删完才可能露出新的相邻同角色。
///
/// 这一步放在中枢而不是某个适配器里 —— 原生 Anthropic、OpenAI、Gemini、Responses 四个入口
/// 都要经过这里,只在一处适配器上做,另外三条路照样会把上游不接受的形状发出去。
fn normalize_hub_messages(messages: &[InMsg]) -> Vec<InMsg> {
    merge_adjacent_same_role(prune_unpaired_tool_blocks(messages.to_vec()))
}

fn anthropic_to_kiro_inner(
    req: &MessagesRequest,
    profile_arn: Option<&str>,
    tool_name_map: &mut std::collections::HashMap<String, String>,
) -> Result<KiroRequest, ConvertError> {
    let model_id =
        map_model(&req.model).ok_or_else(|| ConvertError::UnknownModel(req.model.clone()))?;

    let messages = normalize_hub_messages(&req.messages);
    if messages.is_empty() {
        return Err(ConvertError::EmptyMessages);
    }

    let texts: Vec<String> = messages
        .iter()
        .enumerate()
        .map(|(i, msg)| {
            let text = msg.text();
            if i == 0 {
                // thinking 指令必须排在 system 最前面(照观测:上游按前缀识别)。
                // 未开 thinking 时 `directive` 为 None,一个字符都不加。
                let sys = req.system.as_ref().map(|s| s.text()).unwrap_or_default();
                match (thinking_directive(req), sys.is_empty()) {
                    (Some(d), true) => format!("{d}\n\n{text}"),
                    (Some(d), false) => format!("{d}\n{sys}\n\n{text}"),
                    (None, true) => text,
                    (None, false) => format!("{sys}\n\n{text}"),
                }
            } else {
                text
            }
        })
        .collect();

    // 末条为 assistant 预填 → 它也进 history(助手轮次),当前轮改用续写指令;
    // 否则照旧:末条即本轮用户输入,前面的进 history。
    let last_msg = &messages[messages.len() - 1];
    let is_prefill = last_msg.role == "assistant";
    let history_len = if is_prefill {
        messages.len()
    } else {
        messages.len() - 1
    };

    let (history_msgs, tail) = texts.split_at(history_len);
    let last_text = match tail.first() {
        Some(t) => t.clone(),
        None => PREFILL_CONTINUATION.to_string(),
    };

    let history: Vec<HistoryItem> = messages[..history_len]
        .iter()
        .zip(history_msgs.iter())
        .map(|(msg, text)| {
            if msg.role == "assistant" {
                let tool_uses = message_tool_uses(msg, tool_name_map);
                Ok(HistoryItem::AssistantResponseMessage {
                    assistant_response_message: AssistantResponseMessage {
                        // 助手轮的内容**不能是空串**。
                        //
                        // 纯工具调用的那一轮(只有 tool_use、没有文本)在这里就是空,而上游
                        // 对空的 assistantResponseMessage.content 会拒掉整条请求。用户轮早有
                        // `non_empty_content` 兜底,助手轮一直漏着 —— 于是"上一轮只调了工具、
                        // 没说话"的对话再发一次就必挂,而那恰恰是工具链里最常见的形态。
                        // 占位用单个空格:既非空,又不给模型塞进任何它没说过的内容。
                        content: if text.trim().is_empty() {
                            " ".to_string()
                        } else {
                            text.clone()
                        },
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

    // 工具规格:声明的工具 + 历史里出现过却没被声明的(补最小规格)。
    // 只看 `req.tools` 是不够的 —— 历史带工具调用而 tools 为空时,上游会以
    // `TOOL_CONFIG_MISSING` 拒掉整个请求(见 `tool_specs_with_history_fallback`)。
    let declared_tools: &[ToolDef] = req.tools.as_deref().unwrap_or(&[]);
    // 用整型后的消息:被剔掉的悬空 tool_use 已经不在请求里,不该再为它补一份工具规格。
    let tool_specs = tool_specs_with_history_fallback(declared_tools, &messages, tool_name_map);
    let has_tools = !tool_specs.is_empty();
    let agent_task_type = if has_tools { "spectask" } else { "vibe" }.to_string();

    // 预填时末条消息已归入 history,当前轮只有续写指令,不带工具结果/图片。
    let (current_tool_results, current_images) = if is_prefill {
        (Vec::new(), Vec::new())
    } else {
        (message_tool_results(last_msg), message_images(last_msg)?)
    };

    Ok(KiroRequest {
        conversation_state: ConversationState {
            agent_continuation_id: Some(crate::kiro::uuid_v4()),
            agent_task_type,
            chat_trigger_type: "MANUAL".to_string(),
            conversation_id: conversation_id_for(req),
            current_message: CurrentMessage {
                user_input_message: UserInputMessage {
                    content: non_empty_content(last_text),
                    model_id,
                    origin: "AI_EDITOR".to_string(),
                    user_input_message_context: UserInputMessageContext {
                        tools: if has_tools { Some(tool_specs) } else { None },
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

/// 由 [`frame_truncation`] 认领的截断类 `:exception-type`——它们是正常的截断信号,
/// 不是错误,故一律不进 [`frame_exception`](两者严格互斥)。
const TRUNCATION_EXCEPTION_TYPES: [&str; 1] = ["ContentLengthExceededException"];

/// 上游在 200 事件流中下发的非截断类 exception 帧。
///
/// AWS event-stream 的错误在响应头(200)之后才以 `:message-type == "exception"` 帧下发,
/// 因此"HTTP 200 + 空内容"其实可能是限流/鉴权/参数错误。协议层必须在把帧序列还原成
/// 响应之前先查这个契约,按 [`exception_status`] 映射出对外状态码,而不是回 200 + `end_turn`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamException {
    /// `:exception-type` 头(如 `"ThrottlingException"`);头缺失时退而取帧体的
    /// `__type` / `code`,再取不到则为 `"UnknownException"`。
    pub kind: String,
    /// 帧体里的人类可读消息(`message` / `Message` 字段),取不到则为空串。
    pub message: String,
}

/// 从帧 payload 里尽力取人类可读消息;非法 JSON / 无该字段 → 空串(不 panic)。
fn exception_message(payload: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("Message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

/// 从单个事件帧探测非截断类 exception;非 exception 帧、或截断类 exception 一律 `None`。
///
/// 与 [`frame_truncation`] 严格互斥:凡是 [`frame_truncation`] 认下的帧(含
/// `ContentLengthExceededException` 与 contextUsage 判出的窗口耗尽)都在这里放行,
/// 由既有 Truncation 路径处理。payload 非法 JSON 不影响判定(仅消息取空串)。
pub fn frame_exception(frame: &Message) -> Option<StreamException> {
    // `exception` 与 `error` 两类都要认。
    //
    // event-stream 协议里 `:message-type` 除了 `event` / `exception`,还有 `error` ——
    // 那是**框架级**错误(类型在 `:error-code` 头里),与业务异常走的不是同一个头。
    // 此前只认 `exception`,于是一个 error 帧会被整个流循环忽略,还原出"没有任何内容"
    // 的成功响应:上游明明报了错,客户端却拿到 200 + 空消息,完全无从判断发生了什么。
    let mt = header_str(frame, ":message-type");
    if mt != Some("exception") && mt != Some("error") {
        return None;
    }
    if frame_truncation(frame).is_some() {
        return None;
    }
    let kind = header_str(frame, ":exception-type")
        .or_else(|| header_str(frame, ":error-code"))
        .map(|s| s.to_string())
        .or_else(|| {
            let v: serde_json::Value = serde_json::from_slice(&frame.payload).ok()?;
            v.get("__type")
                .or_else(|| v.get("code"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "UnknownException".to_string());
    // 头缺失、类型从帧体里读出来的情况下也要挡住截断类,保证与 Truncation 路径不重叠。
    if TRUNCATION_EXCEPTION_TYPES.contains(&kind.as_str()) {
        return None;
    }
    Some(StreamException {
        kind,
        message: exception_message(&frame.payload),
    })
}

/// 遍历一批帧,取出首个非截断类 exception。无 → `None`。
pub fn extract_exception(frames: &[Message]) -> Option<StreamException> {
    frames.iter().find_map(frame_exception)
}

/// 把 exception 类型映射为对外 HTTP 状态码(本模块不依赖 axum,返回裸 `u16`)。
///
/// 限流类 → 429、鉴权类 → 403、参数校验类 → 400,其余一律 502(上游故障)。
/// 匹配按大小写无关的子串判定,兼容 `ThrottlingException` / `ThrottledException`
/// 之类的同族命名。
pub fn exception_status(kind: &str) -> u16 {
    let k = kind.to_lowercase();
    if k.contains("throttl") {
        429
    } else if k.contains("accessdenied") || k.contains("unauthorized") {
        403
    } else if k.contains("validation") {
        400
    } else {
        502
    }
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

/// 新的 Anthropic 消息 id(`msg_` + 去连字符的 UUID)。
///
/// 这是**回给客户端**的 id,不上 wire,故形状只需与 Anthropic 官方一致(32 位十六进制)。
pub fn new_message_id() -> String {
    format!("msg_{}", crate::kiro::uuid_v4().replace('-', ""))
}

/// 从 `meteringEvent` 帧提取的真实计费数据(照观测的数据面契约)。
///
/// 字段:`credits` = 上游 payload 的 `usage`(真实积分消耗,f64);
/// `input_tokens` / `output_tokens` = 上游真实 token 计量(带则透传,用于回填响应 usage);
/// `cache_read_input_tokens` / `cache_creation_input_tokens` = 若 payload 带则透传。
/// 上游 payload 的键既可能是 snake_case(`cache_read_input_tokens`)也可能是
/// camelCase(`cacheReadInputTokens`),两种都接。
#[derive(Debug, Clone, PartialEq)]
pub struct MeteringUsage {
    pub credits: f64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read_input_tokens: Option<i32>,
    pub cache_creation_input_tokens: Option<i32>,
}

/// 取一个非负整数字段:snake_case 优先、回退 camelCase;负数/非数字/缺失 → `None`。
fn u32_field(v: &serde_json::Value, snake: &str, camel: &str) -> Option<u32> {
    v.get(snake)
        .or_else(|| v.get(camel))
        .and_then(|n| n.as_u64())
        .map(|n| n.min(u32::MAX as u64) as u32)
}

/// 从单个事件帧解析 `meteringEvent`:仅当 `:event-type == "meteringEvent"` 且
/// payload 是含数值 `usage` 字段的合法 JSON 时返回 `Some`;其余一律 `None`(不 panic)。
pub fn metering_frame(frame: &Message) -> Option<MeteringUsage> {
    if event_type(frame) != Some("meteringEvent") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&frame.payload).ok()?;
    let credits = v.get("usage").and_then(|u| u.as_f64())?;
    // 真实 token 计量:同样 snake_case 优先、回退 camelCase;缺失则由调用方回退估算。
    let input_tokens = u32_field(&v, "input_tokens", "inputTokens");
    let output_tokens = u32_field(&v, "output_tokens", "outputTokens");
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
        input_tokens,
        output_tokens,
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
/// `usage` 优先用 `meteringEvent` 的真实计量([`extract_metering`]);上游没发或没带
/// token 字段时才回退到 `input_tokens=0`、`output_tokens=` 全文字符数 / 4 的估算
/// (工具参数不计入)。
///
/// **本函数不表达错误**:上游在 200 事件流里下发的非截断 exception 帧(限流/鉴权/参数)
/// 会让帧序列既无文本也无工具,这里只会还原成空内容 + `end_turn`。协议层必须先查
/// [`extract_exception`],命中就按 [`exception_status`] 回错误,不要把它当正常响应下发。
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

    // usage:meteringEvent 的真实计量优先,缺哪项就单独回退该项的估算
    // (input 无从估算 → 0;output → 全文字符数 / 4)。
    let metering = extract_metering(frames);
    let input_tokens = metering.as_ref().and_then(|m| m.input_tokens).unwrap_or(0);
    let output_tokens = metering
        .as_ref()
        .and_then(|m| m.output_tokens)
        // 按字符类别加权,而不是全局 /4:后者对中文低估约三倍(见 `estimate_tokens`)。
        .unwrap_or_else(|| estimate_tokens(&full_text) as u32);

    let has_tools = !tools.order.is_empty();
    let mut content: Vec<OutBlock> = Vec::new();
    if !full_text.is_empty() {
        // 把 `<thinking>…</thinking>` 切成独立的 thinking 块,其余按文本。
        // 没有标签时 `split_thinking` 原样返回一段 Text,与此前行为一致。
        for piece in split_thinking(&full_text) {
            match piece {
                Piece::Thinking(t) if !t.is_empty() => {
                    content.push(OutBlock::Thinking { thinking: t })
                }
                Piece::Text(t) if !t.is_empty() => content.push(OutBlock::Text { text: t }),
                _ => {}
            }
        }
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
            input_tokens,
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

    /// 一条"发起过 `id` 这次工具调用"的助手轮。
    ///
    /// 真实会话里 `tool_result` 永远跟在自己那次 `tool_use` 后面;配不上对的工具块会被剔除
    /// (悬空工具块的剔除本身另有专门用例),所以凡是构造 `tool_result` 的用例都要把发起
    /// 调用的那一轮补齐 —— 断言才落在真实会话真正发出去的形状上。
    fn tool_call_msg(id: &str) -> InMsg {
        blocks_msg(
            "assistant",
            vec![Block::ToolUse {
                id: id.to_string(),
                name: "t".to_string(),
                input: serde_json::json!({}),
            }],
        )
    }

    fn base_req(messages: Vec<InMsg>) -> MessagesRequest {
        MessagesRequest {
            thinking: None,
            metadata: None,
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

    fn exception_frame_with(exception_type: &str, payload: &str) -> Message {
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
            payload: payload.as_bytes().to_vec(),
        }
    }

    fn exception_frame(exception_type: &str) -> Message {
        exception_frame_with(exception_type, "{}")
    }

    /// 只有 `:message-type` 没有 `:exception-type` 的 exception 帧(类型只能从帧体里读)。
    fn untyped_exception_frame(payload: &str) -> Message {
        Message {
            headers: vec![Header {
                name: ":message-type".to_string(),
                value: HeaderValue::Str("exception".to_string()),
            }],
            payload: payload.as_bytes().to_vec(),
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

    /// 历史里助手轮的 `content` **不能是空串**。
    ///
    /// 回归:纯工具调用的那一轮(只有 tool_use、没有文本)在这里就是空,而上游对空的
    /// `assistantResponseMessage.content` 会拒掉整条请求。用户轮早有兜底,助手轮一直漏着——
    /// 于是"上一轮只调了工具、没说话"的对话再发一次就必挂,而那正是工具链里最常见的形态。
    #[test]
    fn assistant_history_content_is_never_empty() {
        let req = base_req(vec![
            InMsg {
                role: "assistant".into(),
                content: ContentIn::Blocks(vec![Block::ToolUse {
                    id: "t1".into(),
                    name: "shell".into(),
                    input: serde_json::json!({"cmd": "ls"}),
                }]),
            },
            InMsg {
                role: "user".into(),
                content: ContentIn::Blocks(vec![
                    Block::ToolResult {
                        tool_use_id: "t1".into(),
                        content: serde_json::json!("a.txt"),
                        is_error: None,
                    },
                    Block::Text {
                        text: "继续".into(),
                    },
                ]),
            },
        ]);
        let kiro = anthropic_to_kiro(&req, None).expect("应转换成功");
        let item = &kiro.conversation_state.history[0];
        match item {
            HistoryItem::AssistantResponseMessage {
                assistant_response_message,
            } => {
                assert!(
                    !assistant_response_message.content.is_empty(),
                    "助手轮内容不得为空串"
                );
                // 占位不得夹带模型没说过的内容
                assert_eq!(assistant_response_message.content, " ");
                assert!(
                    assistant_response_message.tool_uses.is_some(),
                    "工具调用必须保留"
                );
            }
            other => panic!("首条应是助手轮,实得 {other:?}"),
        }
    }

    /// 开启 thinking 时,上游认的指令必须排在 system 最前面;不开则一个字符都不加。
    ///
    /// 回归:`thinking` 字段此前被静默丢弃 —— 客户端开了扩展思考,上游根本收不到指令,
    /// 于是既没有思考过程,客户端也拿不到 `thinking` 内容块。
    #[test]
    fn thinking_config_becomes_a_system_directive() {
        use crate::protocol::anthropic::types::ThinkingConfig;
        let first_text = |r: &MessagesRequest| {
            anthropic_to_kiro(r, None)
                .unwrap()
                .conversation_state
                .current_message
                .user_input_message
                .content
        };

        // 不开 thinking:内容不含任何指令
        let plain = base_req(vec![msg("user", "hi")]);
        assert!(!first_text(&plain).contains("thinking_mode"));

        // enabled:带预算上限
        let mut on = base_req(vec![msg("user", "hi")]);
        on.thinking = Some(ThinkingConfig {
            thinking_type: "enabled".into(),
            budget_tokens: 4096,
        });
        let t = first_text(&on);
        assert!(
            t.starts_with("<thinking_mode>enabled</thinking_mode>"),
            "指令必须在最前:{t}"
        );
        assert!(t.contains("<max_thinking_length>4096</max_thinking_length>"));

        // adaptive:让模型自行决定深浅
        let mut ad = base_req(vec![msg("user", "hi")]);
        ad.thinking = Some(ThinkingConfig {
            thinking_type: "adaptive".into(),
            budget_tokens: 0,
        });
        assert!(first_text(&ad).starts_with("<thinking_mode>adaptive</thinking_mode>"));

        // 未知取值按未开启处理,不瞎加东西
        let mut bad = base_req(vec![msg("user", "hi")]);
        bad.thinking = Some(ThinkingConfig {
            thinking_type: "whatever".into(),
            budget_tokens: 0,
        });
        assert!(!first_text(&bad).contains("thinking_mode"));

        // 有 system 时:指令在 system 之前,且 system 原文保留
        let mut with_sys = base_req(vec![msg("user", "hi")]);
        with_sys.system = Some(crate::protocol::anthropic::types::SystemPrompt::Text(
            "你是助手".into(),
        ));
        with_sys.thinking = Some(ThinkingConfig {
            thinking_type: "enabled".into(),
            budget_tokens: 1024,
        });
        let t2 = first_text(&with_sys);
        assert!(t2.starts_with("<thinking_mode>"), "{t2}");
        assert!(t2.contains("你是助手"), "system 原文不得丢:{t2}");
    }

    /// token 估算必须按字符类别加权:中文约 1.5 字/token,英文约 4 字符/token。
    ///
    /// 回归:此前全局 `字符数 / 4`,一段纯中文估出来只有真实值的三分之一强。这个偏差直接
    /// 落到用量统计和按 USD 设的限额上 —— 同样的钱,中文用户能超支两三倍而限额毫无察觉。
    #[test]
    fn token_estimation_is_weighted_by_script() {
        // 30 个汉字:按 1.5 字/token 约 20;旧口径 30/4 = 7,差了近 3 倍
        let zh = "中".repeat(30);
        let t = estimate_tokens(&zh);
        assert!((18..=22).contains(&t), "30 个汉字应约 20 token,实得 {t}");
        assert!(t > 30 / 4 * 2, "必须显著高于旧的 /4 口径");

        // 40 个 ASCII:约 10
        let en = "a".repeat(40);
        assert_eq!(estimate_tokens(&en), 10);

        // 混排:两部分之和
        let mixed = format!("{zh}{en}");
        assert_eq!(estimate_tokens(&mixed), t + 10);

        // 短文本不得被抹成 0(向上取整)
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("中"), 1);
        assert_eq!(estimate_tokens(""), 0);

        // 日文假名 / 韩文同档加权(它们的分词密度同样远高于拉丁文)
        assert!(estimate_tokens(&"あ".repeat(30)) > 10);
        assert!(estimate_tokens(&"한".repeat(30)) > 10);
    }

    /// `description` 恒为字符串,**绝不为 null**。
    ///
    /// 回归:此前是 `Option<String>`,客户端没给描述时序列化成 `"description": null`,
    /// 而真实客户端在这个位置永远是字符串(没有就是空串)。
    #[test]
    fn tool_description_is_never_null_nor_empty() {
        let mut map = std::collections::HashMap::new();
        let specs = map_tools(
            &[ToolDef {
                tool_type: None,
                name: "t".into(),
                description: None,
                input_schema: serde_json::json!({"type": "object"}),
            }],
            &mut map,
        );
        // 既不是 null,**也不能是空串**:上游对空描述回 `Invalid tool use format` /
        // `REQUEST_BODY_INVALID`,拒的是整条请求(线上实测:同一个工具带描述 200、
        // 去掉描述 400)。客户端没给时用工具名兜底。
        assert_eq!(specs[0].tool_specification.description, "t");
        let v = serde_json::to_value(&specs[0]).unwrap();
        assert!(
            !v["toolSpecification"]["description"].is_null(),
            "绝不能是 null"
        );
        assert_ne!(v["toolSpecification"]["description"], "", "绝不能是空串");

        // 只有空白的描述同样按"没给"处理
        let specs = map_tools(
            &[ToolDef {
                tool_type: None,
                name: "u".into(),
                description: Some("   ".into()),
                input_schema: serde_json::json!({}),
            }],
            &mut std::collections::HashMap::new(),
        );
        assert_eq!(specs[0].tool_specification.description, "u");
    }

    /// 客户端给的 schema 千奇百怪,规范化后必须一定是上游收得下的形状。
    ///
    /// 回归:此前原样透传,`properties: null` / `required: null` 这类会让上游拒掉**整条请求**,
    /// 而客户端只看到一个语焉不详的 400。
    #[test]
    fn input_schema_is_normalized_into_a_shape_upstream_accepts() {
        // 完全不是对象 → 给一份合法空 schema
        let v = normalize_json_schema(&serde_json::json!("garbage"));
        assert_eq!(v["type"], "object");
        assert!(v["properties"].is_object());
        assert!(v["required"].is_array());

        // 各字段类型都不对 → 逐项补形状
        let v = normalize_json_schema(&serde_json::json!({
            "type": 123, "properties": null, "required": null, "additionalProperties": 7
        }));
        assert_eq!(v["type"], "object");
        assert!(v["properties"].is_object());
        assert_eq!(v["required"], serde_json::json!([]));
        assert_eq!(v["additionalProperties"], true);

        // required 里混了非字符串 → 只留字符串
        let v = normalize_json_schema(&serde_json::json!({"required": ["a", 1, null, "b"]}));
        assert_eq!(v["required"], serde_json::json!(["a", "b"]));

        // 本来就合法 → 原样保留,不擅自改语义
        let ok = serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"],
            "additionalProperties": false
        });
        assert_eq!(normalize_json_schema(&ok), ok);
    }

    /// 服务端内置工具(web_search 等)没有 `input_schema`,不得让整条请求失败。
    ///
    /// 回归:`input_schema` 曾是必填字段,客户端一带官方内置工具,请求就在我们这层 400。
    #[test]
    fn server_side_tools_without_a_schema_do_not_fail_the_request() {
        let req: MessagesRequest = serde_json::from_str(
            r#"{"model":"sonnet","max_tokens":16,
                "messages":[{"role":"user","content":"hi"}],
                "tools":[{"type":"web_search_20250305","name":"web_search"}]}"#,
        )
        .expect("带内置工具的请求必须能解析");
        let kiro = anthropic_to_kiro(&req, None).expect("不得因缺 input_schema 而失败");
        let specs = kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools
            .expect("应带上工具");
        let s = &specs[0].tool_specification;
        assert_eq!(s.name, "web_search");
        assert_eq!(s.input_schema.json["type"], "object");
    }

    /// 超长工具名缩短到 63,且**确定性**;短名 → 原名的映射要带出来。
    #[test]
    fn overlong_tool_names_are_shortened_deterministically_and_mapped_back() {
        let long = "x".repeat(120);
        let req = {
            let mut r = base_req(vec![msg("user", "hi")]);
            r.tools = Some(vec![ToolDef {
                tool_type: None,
                name: long.clone(),
                description: Some("d".into()),
                input_schema: serde_json::json!({"type": "object"}),
            }]);
            r
        };
        let a = anthropic_to_kiro_full(&req, None).unwrap();
        let b = anthropic_to_kiro_full(&req, None).unwrap();
        let short = a
            .request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools
            .as_ref()
            .unwrap()[0]
            .tool_specification
            .name
            .clone();
        assert_eq!(short.len(), TOOL_NAME_MAX_LEN, "必须恰好压到上限");
        assert_ne!(short, long);
        // 确定性:同名每次缩成同一个,否则模型在多轮之间看到的工具会变来变去
        let short_b = b
            .request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools
            .as_ref()
            .unwrap()[0]
            .tool_specification
            .name
            .clone();
        assert_eq!(short, short_b);
        // 映射能还原回原名
        assert_eq!(a.tool_name_map.get(&short), Some(&long));

        // 没超长的名字不进映射,也不改名
        let mut r2 = base_req(vec![msg("user", "hi")]);
        r2.tools = Some(vec![ToolDef {
            tool_type: None,
            name: "short_name".into(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
        }]);
        let c = anthropic_to_kiro_full(&r2, None).unwrap();
        assert!(c.tool_name_map.is_empty());
    }

    /// history 里的工具名也必须走同一套缩短,否则与 tools 列表里的短名对不上。
    ///
    /// 回归:此前只有 `tools` 列表被缩短,history 的 `toolUses` 发的还是原名 —— 上游看到的是
    /// "声明的工具叫短名、历史里调用的却是另一个名字",两者对不上。
    #[test]
    fn history_tool_names_use_the_same_shortening_as_the_tool_list() {
        let long = "y".repeat(120);
        let mut r = base_req(vec![
            InMsg {
                role: "assistant".into(),
                content: ContentIn::Blocks(vec![Block::ToolUse {
                    id: "t1".into(),
                    name: long.clone(),
                    input: serde_json::json!({}),
                }]),
            },
            InMsg {
                role: "user".into(),
                content: ContentIn::Blocks(vec![
                    Block::ToolResult {
                        tool_use_id: "t1".into(),
                        content: serde_json::json!("done"),
                        is_error: None,
                    },
                    Block::Text {
                        text: "继续".into(),
                    },
                ]),
            },
        ]);
        r.tools = Some(vec![ToolDef {
            tool_type: None,
            name: long.clone(),
            description: Some("d".into()),
            input_schema: serde_json::json!({"type": "object"}),
        }]);
        let c = anthropic_to_kiro_full(&r, None).unwrap();

        let declared = c
            .request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools
            .as_ref()
            .unwrap()[0]
            .tool_specification
            .name
            .clone();
        let in_history = match &c.request.conversation_state.history[0] {
            HistoryItem::AssistantResponseMessage {
                assistant_response_message,
            } => assistant_response_message.tool_uses.as_ref().unwrap()[0]
                .name
                .clone(),
            other => panic!("首条应是助手轮:{other:?}"),
        };
        assert_eq!(
            in_history, declared,
            "history 里的工具名必须与声明的短名一致"
        );
        assert_ne!(in_history, long, "超长名必须被缩短");
        assert_eq!(
            c.tool_name_map.get(&declared),
            Some(&long),
            "映射能还原回原名"
        );
    }

    /// 同一次会话的多个请求必须共用同一个 `conversationId`。
    ///
    /// 回归:此前每请求生成一个新的 32 位无连字符十六进制 —— 上游看到的是"这个账号每发
    /// 一句话就开一段全新对话",与真实客户端形态不符;形状也不是 UUID。
    #[test]
    fn conversation_id_is_stable_within_a_session_and_is_a_uuid() {
        let sid = "550e8400-e29b-41d4-a716-446655440000";
        let mk = |uid: Option<&str>| {
            let mut r = base_req(vec![msg("user", "hi")]);
            r.metadata = uid.map(|u| crate::protocol::anthropic::types::RequestMetadata {
                user_id: Some(u.to_string()),
            });
            anthropic_to_kiro(&r, None).unwrap().conversation_state
        };

        // JSON 形态的 user_id
        let a = mk(Some(&format!(r#"{{"session_id":"{sid}"}}"#)));
        let b = mk(Some(&format!(r#"{{"session_id":"{sid}"}}"#)));
        assert_eq!(a.conversation_id, sid);
        assert_eq!(a.conversation_id, b.conversation_id, "同会话必须稳定");

        // `session_<uuid>` 片段形态
        let c = mk(Some(&format!("user_abc__session_{sid}")));
        assert_eq!(c.conversation_id, sid);

        // 没有 metadata → 新生成,但必须是 UUID 形状
        let d = mk(None);
        assert!(
            crate::kiro::is_uuid(&d.conversation_id),
            "{}",
            d.conversation_id
        );
        let e = mk(None);
        assert_ne!(
            d.conversation_id, e.conversation_id,
            "无会话标识时应各自新生成"
        );

        // 畸形/非 UUID 的 session 值不得原样发出去
        let f = mk(Some(r#"{"session_id":"not-a-uuid"}"#));
        assert_ne!(f.conversation_id, "not-a-uuid");
        assert!(crate::kiro::is_uuid(&f.conversation_id));
    }

    /// `agentContinuationId` 每请求一个新 UUID,且必须真的发出去。
    #[test]
    fn agent_continuation_id_is_present_and_fresh() {
        let a = anthropic_to_kiro(&base_req(vec![msg("user", "hi")]), None)
            .unwrap()
            .conversation_state;
        let b = anthropic_to_kiro(&base_req(vec![msg("user", "hi")]), None)
            .unwrap()
            .conversation_state;
        let ida = a.agent_continuation_id.expect("此前根本不发这个字段");
        let idb = b.agent_continuation_id.expect("此前根本不发这个字段");
        assert!(crate::kiro::is_uuid(&ida), "{ida}");
        assert_ne!(ida, idb, "每请求应各不相同");
    }

    #[test]
    fn maps_opus_variants() {
        assert_eq!(map_model("opus-4.5"), Some("claude-opus-4.5".to_string()));
        assert_eq!(map_model("opus-4.7"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("opus-4.8"), Some("claude-opus-4.8".to_string()));
        // opus-5 曾被漏判,静默降级成 opus-4.6:请求方要的是 5,拿到的是 4.6 且毫无提示。
        assert_eq!(
            map_model("claude-opus-5"),
            Some("claude-opus-5".to_string())
        );
        assert_eq!(
            map_model("claude-opus-5-20260115"),
            Some("claude-opus-5".to_string())
        );
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
            thinking: None,
            metadata: None,
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
            thinking: None,
            metadata: None,
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
            thinking: None,
            metadata: None,
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
            thinking: None,
            metadata: None,
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
            tool_type: None,
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
        let req = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!("sunny"),
                    is_error: Some(false),
                }],
            ),
        ]);

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
        let req = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!("boom"),
                    is_error: Some(true),
                }],
            ),
        ]);

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
        let req = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]),
                    is_error: None,
                }],
            ),
        ]);

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
        let req = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!("sunny"),
                    is_error: None,
                }],
            ),
        ]);

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
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!("result"),
                    is_error: None,
                }],
            ),
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
        // 非截断类 exception 不是截断信号,归 frame_exception 认领(见错误契约用例)。
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

    // --- tool_result 内嵌图片/非文本内容:与顶层图片同策略,不静默丢弃 ---

    #[test]
    fn tool_result_nested_base64_image_goes_to_message_images() {
        let req = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!([
                        {"type": "text", "text": "shot:"},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
                    ]),
                    is_error: None,
                }],
            ),
        ]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        let current = &kiro.conversation_state.current_message.user_input_message;
        let images = current.images.as_ref().expect("内嵌图片应提到 images");
        assert_eq!(images[0].format, "png");
        assert_eq!(images[0].source.bytes, "AAAA");
        // 图片走 images 通道,文本侧只保留文本块(不把 base64 塞进 toolResults)。
        let results = current
            .user_input_message_context
            .tool_results
            .as_ref()
            .expect("tool_results 应存在");
        assert_eq!(results[0].content[0].text, "shot:");
    }

    #[test]
    fn tool_result_nested_image_in_history_also_goes_to_images() {
        let req = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!([
                        {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "BBBB"}}
                    ]),
                    is_error: None,
                }],
            ),
            msg("assistant", "ok"),
            msg("user", "continue"),
        ]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        match &kiro.conversation_state.history[1] {
            HistoryItem::UserInputMessage { user_input_message } => {
                let images = user_input_message
                    .images
                    .as_ref()
                    .expect("历史里的内嵌图片也应保留");
                assert_eq!(images[0].format, "jpeg");
                assert_eq!(images[0].source.bytes, "BBBB");
            }
            other => panic!("带图片的那条历史应为 UserInputMessage,实际: {other:?}"),
        }
    }

    #[test]
    fn tool_result_nested_remote_image_url_errors_not_dropped() {
        let req = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!([
                        {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}}
                    ]),
                    is_error: None,
                }],
            ),
        ]);

        let err = anthropic_to_kiro(&req, None).expect_err("tool_result 内的远程图片也应报错");
        assert_eq!(
            err,
            ConvertError::RemoteImageUrl("https://example.com/a.png".to_string())
        );
    }

    #[test]
    fn tool_result_nested_openai_image_url_block_handled_like_top_level() {
        // data URL → 就地内联;远程 http(s) → 报错(与顶层图片同策略,都不静默丢)。
        let inline = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!([
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,CCCC"}}
                    ]),
                    is_error: None,
                }],
            ),
        ]);
        let kiro = anthropic_to_kiro(&inline, None).expect("转换应成功");
        let images = kiro
            .conversation_state
            .current_message
            .user_input_message
            .images
            .as_ref()
            .expect("data URL 图片应内联进 images");
        assert_eq!(images[0].format, "png");
        assert_eq!(images[0].source.bytes, "CCCC");

        let remote = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!([
                        {"type": "image_url", "image_url": {"url": "https://example.com/b.png"}}
                    ]),
                    is_error: None,
                }],
            ),
        ]);
        assert_eq!(
            anthropic_to_kiro(&remote, None).expect_err("远程图片应报错"),
            ConvertError::RemoteImageUrl("https://example.com/b.png".to_string())
        );
    }

    #[test]
    fn tool_result_unknown_block_kept_as_json_text() {
        let req = base_req(vec![
            tool_call_msg("tu1"),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!([{"type": "json", "data": {"rows": 2}}, "裸串"]),
                    is_error: None,
                }],
            ),
        ]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        let results = kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tool_results
            .as_ref()
            .expect("tool_results 应存在");
        assert!(results[0].content[0].text.contains("rows"));
        assert!(results[0].content[0].text.contains("裸串"));
    }

    // --- 末条 assistant 预填(prefill)→ 进 history,不当用户输入 ---

    /// 末条 assistant 预填整条进 history,`currentMessage` 只放续写指令。
    ///
    /// 预填里那次 `tool_use` **永远配不到 tool_result**(它后面已经没有消息了),带着它发
    /// 上去,上游按"工具调用没被应答"拒掉整条请求 —— 客户端只看到一次莫名其妙的失败,
    /// 而这一轮本来可以照常续写。故只保留预填的文本,把配不上对的那次调用剔掉。
    #[test]
    fn trailing_assistant_prefill_goes_to_history_and_drops_its_dangling_tool_use() {
        let req = base_req(vec![
            msg("user", "q"),
            blocks_msg(
                "assistant",
                vec![
                    Block::Text {
                        text: "开头".to_string(),
                    },
                    Block::ToolUse {
                        id: "tu1".to_string(),
                        name: "get_weather".to_string(),
                        input: serde_json::json!({"city": "Paris"}),
                    },
                ],
            ),
        ]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        assert_eq!(kiro.conversation_state.history.len(), 2);
        match &kiro.conversation_state.history[1] {
            HistoryItem::AssistantResponseMessage {
                assistant_response_message,
            } => {
                assert_eq!(assistant_response_message.content, "开头");
                assert!(
                    assistant_response_message.tool_uses.is_none(),
                    "配不到 tool_result 的调用必须剔除,实得 {:?}",
                    assistant_response_message.tool_uses
                );
            }
            other => panic!("末条预填应进 history 的 AssistantResponseMessage,实际: {other:?}"),
        }
        // currentMessage 只剩续写指令:助手文本不能冒充用户输入。
        let current = &kiro.conversation_state.current_message.user_input_message;
        assert_eq!(current.content, PREFILL_CONTINUATION);
        assert!(!current.content.contains("开头"));
    }

    /// 预填里配得上对的工具往返照旧全须全尾地进 history(上一条用例剔的只是配不上的那种)。
    #[test]
    fn trailing_assistant_prefill_keeps_answered_tool_uses() {
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
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!("sunny"),
                    is_error: None,
                }],
            ),
            msg("assistant", "巴黎"),
        ]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");
        let HistoryItem::AssistantResponseMessage {
            assistant_response_message,
        } = &kiro.conversation_state.history[1]
        else {
            panic!(
                "history[1] 应为助手轮:{:?}",
                kiro.conversation_state.history
            );
        };
        let tool_uses = assistant_response_message
            .tool_uses
            .as_ref()
            .expect("配对齐全的 tool_use 必须保留");
        assert_eq!(tool_uses[0].tool_use_id, "tu1");
        assert_eq!(tool_uses[0].name, "get_weather");
        assert_eq!(tool_uses[0].input["city"], "Paris");
        assert_eq!(
            kiro.conversation_state
                .current_message
                .user_input_message
                .content,
            PREFILL_CONTINUATION
        );
    }

    #[test]
    fn single_assistant_prefill_message_still_converts() {
        let req = base_req(vec![msg("assistant", "只有预填")]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        assert_eq!(kiro.conversation_state.history.len(), 1);
        assert_eq!(
            kiro.conversation_state
                .current_message
                .user_input_message
                .content,
            PREFILL_CONTINUATION
        );
    }

    #[test]
    fn trailing_user_message_still_becomes_current_message() {
        // 回归:末条是 user 时切分不变(history 少一条,当前轮为末条文本)。
        let req = base_req(vec![
            msg("user", "a"),
            msg("assistant", "b"),
            msg("user", "c"),
        ]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");

        assert_eq!(kiro.conversation_state.history.len(), 2);
        assert_eq!(
            kiro.conversation_state
                .current_message
                .user_input_message
                .content,
            "c"
        );
    }

    // --- meteringEvent 真实 token 计量 → 回填 usage ---

    #[test]
    fn metering_frame_extracts_real_token_counts() {
        let snake = event_frame(
            "meteringEvent",
            r#"{"usage":1.0,"input_tokens":1200,"output_tokens":34}"#,
        );
        let m = metering_frame(&snake).expect("应解析出 meteringEvent");
        assert_eq!(m.input_tokens, Some(1200));
        assert_eq!(m.output_tokens, Some(34));

        let camel = event_frame(
            "meteringEvent",
            r#"{"usage":1.0,"inputTokens":7,"outputTokens":8}"#,
        );
        let m = metering_frame(&camel).expect("应解析出 meteringEvent");
        assert_eq!(m.input_tokens, Some(7));
        assert_eq!(m.output_tokens, Some(8));

        // 不带 token 字段 → None,由调用方回退估算。
        let bare = event_frame("meteringEvent", r#"{"usage":1.0}"#);
        let m = metering_frame(&bare).expect("应解析出 meteringEvent");
        assert_eq!(m.input_tokens, None);
        assert_eq!(m.output_tokens, None);
    }

    #[test]
    fn response_usage_backfilled_from_metering_event() {
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"pong"}"#),
            event_frame(
                "meteringEvent",
                r#"{"usage":2.0,"input_tokens":1234,"output_tokens":56}"#,
            ),
        ];

        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");

        assert_eq!(resp.usage.input_tokens, 1234);
        assert_eq!(resp.usage.output_tokens, 56);
    }

    #[test]
    fn response_usage_falls_back_to_char_estimate_without_metering() {
        let frames = vec![event_frame(
            "assistantResponseEvent",
            r#"{"content":"pongpong"}"#,
        )];

        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");

        assert_eq!(resp.usage.input_tokens, 0);
        assert_eq!(resp.usage.output_tokens, 2);
    }

    // --- 200 事件流内的非截断 exception 帧(错误契约) ---

    /// `:message-type == "error"` 的**框架级**错误帧必须被报出来。
    ///
    /// 回归:此前只认 `exception`,error 帧被整个流循环忽略,于是还原出"没有任何内容"的
    /// 成功响应 —— 上游明明报了错,客户端拿到的是 200 + 空消息,完全无从判断出了什么事。
    #[test]
    fn error_typed_frames_are_reported_not_silently_dropped() {
        let f = Message {
            headers: vec![
                Header {
                    name: ":message-type".to_string(),
                    value: HeaderValue::Str("error".to_string()),
                },
                Header {
                    name: ":error-code".to_string(),
                    value: HeaderValue::Str("InternalServerError".to_string()),
                },
            ],
            payload: br#"{"message":"boom"}"#.to_vec(),
        };
        let e = frame_exception(&f).expect("error 帧不得被吞掉");
        assert_eq!(e.kind, "InternalServerError");
        assert_eq!(e.message, "boom");
        assert_eq!(exception_status(&e.kind), 502);

        // 混在正常帧里也要被 extract_exception 捞出来
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"hi"}"#),
            f,
        ];
        assert!(extract_exception(&frames).is_some());
    }

    #[test]
    fn frame_exception_detects_throttling_with_message() {
        let f = exception_frame_with("ThrottlingException", r#"{"message":"Too many requests"}"#);
        assert_eq!(
            frame_exception(&f),
            Some(StreamException {
                kind: "ThrottlingException".to_string(),
                message: "Too many requests".to_string(),
            })
        );
    }

    #[test]
    fn frame_exception_accepts_capital_message_and_tolerates_bad_payload() {
        let cap = exception_frame_with("ValidationException", r#"{"Message":"bad input"}"#);
        assert_eq!(
            frame_exception(&cap).expect("应识别 exception").message,
            "bad input"
        );

        // payload 不是合法 JSON:消息留空,但异常本身不能被吞掉。
        let bad = exception_frame_with("InternalServerException", "not json");
        let e = frame_exception(&bad).expect("payload 非法也要报出异常");
        assert_eq!(e.kind, "InternalServerException");
        assert_eq!(e.message, "");
    }

    #[test]
    fn frame_exception_excludes_truncation_exception() {
        // 截断类 exception 归 frame_truncation 管,两条路径互斥、不重叠。
        let f = exception_frame("ContentLengthExceededException");
        assert_eq!(frame_exception(&f), None);
        assert_eq!(frame_truncation(&f), Some(Truncation::MaxTokens));

        // 头缺失、类型写在帧体里的截断类同样要挡住。
        let untyped = untyped_exception_frame(r#"{"__type":"ContentLengthExceededException"}"#);
        assert_eq!(frame_exception(&untyped), None);
    }

    #[test]
    fn frame_exception_none_for_non_exception_frames() {
        assert_eq!(
            frame_exception(&event_frame(
                "assistantResponseEvent",
                r#"{"content":"hi"}"#
            )),
            None
        );
        assert_eq!(
            frame_exception(&event_frame(
                "contextUsageEvent",
                r#"{"contextUsagePercentage":100.0}"#
            )),
            None
        );
    }

    #[test]
    fn frame_exception_falls_back_to_payload_type_then_unknown() {
        let typed =
            untyped_exception_frame(r#"{"__type":"ThrottlingException","message":"slow down"}"#);
        let e = frame_exception(&typed).expect("应从帧体读出类型");
        assert_eq!(e.kind, "ThrottlingException");
        assert_eq!(e.message, "slow down");
        assert_eq!(exception_status(&e.kind), 429);

        let anon = untyped_exception_frame("not json");
        let e = frame_exception(&anon).expect("类型未知也不能吞掉");
        assert_eq!(e.kind, "UnknownException");
        assert_eq!(exception_status(&e.kind), 502);
    }

    #[test]
    fn extract_exception_finds_first_exception_in_frames() {
        let frames = vec![
            event_frame("assistantResponseEvent", r#"{"content":"hi"}"#),
            exception_frame_with("AccessDeniedException", r#"{"message":"no"}"#),
            exception_frame_with("ThrottlingException", "{}"),
        ];
        let e = extract_exception(&frames).expect("应取到首个 exception");
        assert_eq!(e.kind, "AccessDeniedException");
        assert_eq!(exception_status(&e.kind), 403);

        // 只有截断类 exception 时不算错误。
        assert_eq!(
            extract_exception(&[exception_frame("ContentLengthExceededException")]),
            None
        );
    }

    #[test]
    fn exception_status_maps_known_kinds() {
        assert_eq!(exception_status("ThrottlingException"), 429);
        assert_eq!(exception_status("ThrottledException"), 429);
        assert_eq!(exception_status("AccessDeniedException"), 403);
        assert_eq!(exception_status("UnauthorizedException"), 403);
        assert_eq!(exception_status("ValidationException"), 400);
        assert_eq!(exception_status("InternalServerException"), 502);
        assert_eq!(exception_status(""), 502);
    }

    #[test]
    fn exception_only_stream_is_not_a_silent_empty_success() {
        // 200 + 只有 exception 帧:帧循环还原不出任何内容,契约必须把错误交给协议层,
        // 否则客户端收到的是"空响应"而不是 429。
        let frames = vec![exception_frame_with(
            "ThrottlingException",
            r#"{"message":"rate exceeded"}"#,
        )];

        let e = extract_exception(&frames).expect("应报出 exception");
        assert_eq!(exception_status(&e.kind), 429);
        assert_eq!(e.message, "rate exceeded");

        let resp = kiro_events_to_anthropic(&frames, "claude-sonnet-4.5");
        assert!(matches!(&resp.content[0], OutBlock::Text { text } if text.is_empty()));
    }

    /// 上游硬性要求:消息里出现 `toolUse`/`toolResult` 就必须有 `toolConfig`,否则整个请求
    /// 被拒(`TOOL_CONFIG_MISSING`)。而工具可能在到达这里前就被合法丢弃 —— Responses 的
    /// 内置工具(`web_search`/`local_shell`)中枢无等价物,客户端若这轮只带内置工具,
    /// `tools` 就成了空数组,于是我们自己造出「有工具调用、没有工具定义」的畸形请求。
    /// 线上实测正是 codex 的 502。
    #[test]
    fn history_tool_calls_force_a_tool_config_even_when_tools_were_dropped() {
        let req = MessagesRequest {
            thinking: None,
            metadata: None,
            model: "claude-sonnet-4.5".into(),
            system: None,
            messages: vec![
                InMsg {
                    role: "assistant".into(),
                    content: ContentIn::Blocks(vec![Block::ToolUse {
                        id: "c1".into(),
                        name: "shell".into(),
                        input: serde_json::json!({"cmd": "ls"}),
                    }]),
                },
                InMsg {
                    role: "user".into(),
                    content: ContentIn::Blocks(vec![
                        Block::ToolResult {
                            tool_use_id: "c1".into(),
                            content: serde_json::json!("a.txt"),
                            is_error: None,
                        },
                        Block::Text {
                            text: "thanks".into(),
                        },
                    ]),
                },
            ],
            max_tokens: Some(32),
            stream: Some(false),
            // 内置工具已在协议层被丢掉 → 空数组
            tools: Some(vec![]),
            tool_choice: None,
        };
        let out = anthropic_to_kiro(&req, None).expect("应转换成功");
        let ctx = &out
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;
        let specs = ctx
            .tools
            .as_ref()
            .expect("历史有工具调用时 toolConfig 不得缺席");
        assert!(
            specs.iter().any(|s| s.tool_specification.name == "shell"),
            "补出的规格里必须有历史调用过的 shell"
        );
        assert_eq!(
            out.conversation_state.agent_task_type, "spectask",
            "有工具规格时任务类型须为 spectask"
        );
    }

    /// 客户端**显式声明**的工具不得被历史补全覆盖或重复:同名只留声明的那份。
    #[test]
    fn declared_tools_are_not_duplicated_by_history_fallback() {
        let req = MessagesRequest {
            thinking: None,
            metadata: None,
            model: "claude-sonnet-4.5".into(),
            system: None,
            messages: vec![
                InMsg {
                    role: "assistant".into(),
                    content: ContentIn::Blocks(vec![Block::ToolUse {
                        id: "c1".into(),
                        name: "shell".into(),
                        input: serde_json::json!({}),
                    }]),
                },
                InMsg {
                    role: "user".into(),
                    content: ContentIn::Blocks(vec![Block::ToolResult {
                        tool_use_id: "c1".into(),
                        content: serde_json::json!("a.txt"),
                        is_error: None,
                    }]),
                },
            ],
            max_tokens: Some(32),
            stream: Some(false),
            tools: Some(vec![ToolDef {
                tool_type: None,
                name: "shell".into(),
                description: Some("real one".into()),
                input_schema: serde_json::json!({"type": "object"}),
            }]),
            tool_choice: None,
        };
        let out = anthropic_to_kiro(&req, None).expect("应转换成功");
        let specs = out
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools
            .as_ref()
            .unwrap();
        let shells: Vec<_> = specs
            .iter()
            .filter(|s| s.tool_specification.name == "shell")
            .collect();
        assert_eq!(shells.len(), 1, "同名工具不得重复");
        assert_eq!(
            shells[0].tool_specification.description, "real one",
            "须保留客户端声明的那份,而不是补出来的空壳"
        );
    }

    /// 没有工具、历史也没有工具调用 → 照旧不发 toolConfig、任务类型 vibe。
    #[test]
    fn plain_chat_still_sends_no_tool_config() {
        let req = MessagesRequest {
            thinking: None,
            metadata: None,
            model: "claude-sonnet-4.5".into(),
            system: None,
            messages: vec![InMsg {
                role: "user".into(),
                content: ContentIn::Text("hi".into()),
            }],
            max_tokens: Some(32),
            stream: Some(false),
            tools: None,
            tool_choice: None,
        };
        let out = anthropic_to_kiro(&req, None).expect("应转换成功");
        assert!(
            out.conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tools
                .is_none()
        );
        assert_eq!(out.conversation_state.agent_task_type, "vibe");
    }

    // --- 悬空工具块剔除 / 相邻同角色合并 / schema 类型方言 ---

    /// 没有配对 `tool_result` 的 `tool_use`(以及反过来的孤立 `tool_result`)必须在上行
    /// 请求体里消失。
    ///
    /// 用户中断、上一轮超时、客户端自己裁剪历史,都会留下一个悬空的工具调用;原样透传的话
    /// 上游 400 掉**整条请求**,客户端只看到一次莫名其妙的失败 —— 而这一轮本可以正常作答。
    /// 断言直接落在序列化后的上行请求体上:那才是真正发出去的字节。
    #[test]
    fn unpaired_tool_blocks_are_stripped_from_the_outgoing_body() {
        let req = base_req(vec![
            msg("user", "查天气"),
            blocks_msg(
                "assistant",
                vec![
                    Block::Text {
                        text: "这就查".to_string(),
                    },
                    Block::ToolUse {
                        id: "dangling".to_string(),
                        name: "get_weather".to_string(),
                        input: serde_json::json!({"city": "Paris"}),
                    },
                ],
            ),
            blocks_msg(
                "user",
                vec![
                    Block::ToolResult {
                        tool_use_id: "never_called".to_string(),
                        content: serde_json::json!("sunny"),
                        is_error: None,
                    },
                    Block::Text {
                        text: "接着说".to_string(),
                    },
                ],
            ),
        ]);

        let body = serde_json::to_value(anthropic_to_kiro(&req, None).expect("转换应成功"))
            .expect("序列化应成功");
        let wire = body.to_string();
        assert!(
            !wire.contains("dangling"),
            "没有 tool_result 配对的 tool_use 必须剔除:{wire}"
        );
        assert!(
            !wire.contains("never_called"),
            "没有 tool_use 配对的 tool_result 必须剔除:{wire}"
        );
        // 剔掉的只该是配不上的那一块:同轮的文本一个字都不能少。
        assert!(wire.contains("这就查"), "助手轮的文本不得连坐:{wire}");
        assert_eq!(
            body["conversationState"]["currentMessage"]["userInputMessage"]["content"],
            "接着说"
        );
    }

    /// 配对齐全的工具往返一块都不许动 —— 剔除逻辑不能顺手把正常请求也削了。
    #[test]
    fn paired_tool_blocks_survive_the_pruning() {
        let req = base_req(vec![
            msg("user", "查天气"),
            blocks_msg(
                "assistant",
                vec![Block::ToolUse {
                    id: "tu1".to_string(),
                    name: "get_weather".to_string(),
                    input: serde_json::json!({"city": "Paris"}),
                }],
            ),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    content: serde_json::json!("sunny"),
                    is_error: None,
                }],
            ),
        ]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");
        let HistoryItem::AssistantResponseMessage {
            assistant_response_message,
        } = &kiro.conversation_state.history[1]
        else {
            panic!(
                "history[1] 应为助手轮:{:?}",
                kiro.conversation_state.history
            );
        };
        assert_eq!(
            assistant_response_message.tool_uses.as_ref().unwrap()[0].tool_use_id,
            "tu1"
        );
        assert_eq!(
            kiro.conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results
                .as_ref()
                .expect("配对的 tool_result 必须留下")[0]
                .tool_use_id,
            "tu1"
        );
    }

    /// 相邻同角色消息必须在**中枢**并成一轮 —— 中枢是四个入口(原生 Anthropic / OpenAI /
    /// Gemini / Responses)的必经之路,只在某一个适配器里合并,另外三条路照样会构造出
    /// 上游不接受的形状(history 角色不交替、工具调用与其应答被拆到两轮里)。
    ///
    /// 合并的判据必须与下游的「assistant / 其余」二元折叠一致。
    ///
    /// 回归:此前按角色字符串精确相等判,于是 `system` 紧跟 `user` 不合并 —— 可它们到下游
    /// 全折成用户轮,相邻用户轮照样送上去被打回。Responses 入口 role 是裸串透传,
    /// system/developer 能直接进 messages,这条路是活的。
    #[test]
    fn non_assistant_roles_count_as_one_side_when_merging() {
        let msgs = vec![
            InMsg {
                role: "system".into(),
                content: ContentIn::Text("S".into()),
            },
            InMsg {
                role: "user".into(),
                content: ContentIn::Text("U".into()),
            },
            InMsg {
                role: "assistant".into(),
                content: ContentIn::Text("A".into()),
            },
            InMsg {
                role: "developer".into(),
                content: ContentIn::Text("D".into()),
            },
            InMsg {
                role: "user".into(),
                content: ContentIn::Text("U2".into()),
            },
        ];
        let out = merge_adjacent_same_role(msgs);
        assert_eq!(out.len(), 3, "非 assistant 的连续几条应合成一轮: {out:?}");
        assert_eq!(out[0].role, "system");
        assert_eq!(out[1].role, "assistant");
        assert_eq!(out[2].role, "developer");
        // 幂等:再过一遍不变。
        let again = merge_adjacent_same_role(out.clone());
        assert_eq!(again.len(), out.len());
    }

    #[test]
    fn adjacent_same_role_messages_are_merged_into_one_turn() {
        let split = base_req(vec![
            msg("user", "前半句"),
            msg("user", "后半句"),
            msg("assistant", "答一"),
            msg("assistant", "答二"),
            msg("user", "再问"),
        ]);
        let k = anthropic_to_kiro(&split, None).expect("转换应成功");

        assert_eq!(
            k.conversation_state.history.len(),
            2,
            "五条消息应并成两轮历史 + 当前轮,实得 {:?}",
            k.conversation_state.history
        );
        match &k.conversation_state.history[0] {
            HistoryItem::UserInputMessage { user_input_message } => {
                assert_eq!(user_input_message.content, "前半句\n\n后半句");
            }
            other => panic!("history[0] 应为用户轮:{other:?}"),
        }
        match &k.conversation_state.history[1] {
            HistoryItem::AssistantResponseMessage {
                assistant_response_message,
            } => assert_eq!(assistant_response_message.content, "答一\n\n答二"),
            other => panic!("history[1] 应为助手轮:{other:?}"),
        }

        // 幂等:把已经合并好的形状再喂一遍,产物必须逐字节一致。
        let merged = base_req(vec![
            msg("user", "前半句\n\n后半句"),
            msg("assistant", "答一\n\n答二"),
            msg("user", "再问"),
        ]);
        let m = anthropic_to_kiro(&merged, None).expect("转换应成功");
        assert_eq!(
            k.conversation_state.history, m.conversation_state.history,
            "合并必须幂等"
        );
        assert_eq!(
            k.conversation_state.current_message,
            m.conversation_state.current_message
        );
    }

    /// 并行工具结果被客户端拆成两条 user 消息时,必须并回同一轮:
    /// 否则声明了两个 toolUses 的助手轮后面只跟着一个 toolResult,另一个被挤到当前轮,
    /// 配对被拆散,上游当成"工具调用没被应答"。
    #[test]
    fn split_tool_results_merge_back_into_one_turn() {
        let req = base_req(vec![
            msg("user", "两地天气"),
            blocks_msg(
                "assistant",
                vec![
                    Block::ToolUse {
                        id: "c1".to_string(),
                        name: "get_weather".to_string(),
                        input: serde_json::json!({"city": "Paris"}),
                    },
                    Block::ToolUse {
                        id: "c2".to_string(),
                        name: "get_weather".to_string(),
                        input: serde_json::json!({"city": "Tokyo"}),
                    },
                ],
            ),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "c1".to_string(),
                    content: serde_json::json!("sunny"),
                    is_error: None,
                }],
            ),
            blocks_msg(
                "user",
                vec![Block::ToolResult {
                    tool_use_id: "c2".to_string(),
                    content: serde_json::json!("rainy"),
                    is_error: None,
                }],
            ),
        ]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");
        assert_eq!(
            kiro.conversation_state.history.len(),
            2,
            "history 应为 [用户轮, 助手轮] 并以助手轮收尾:{:?}",
            kiro.conversation_state.history
        );
        let results = kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tool_results
            .as_ref()
            .expect("当前轮应带 toolResults");
        assert_eq!(
            results.len(),
            2,
            "两个 toolUses 必须由同一轮里的两个 toolResults 应答"
        );
        assert_eq!(results[0].tool_use_id, "c1");
        assert_eq!(results[1].tool_use_id, "c2");
    }

    /// 大写类型方言(`"type":"OBJECT"` / `"STRING"`,Gemini 入口的工具声明就是这个形状)
    /// 必须统一成小写标准形态,且**递归**覆盖嵌套 schema —— 只改顶层等于没改。
    #[test]
    fn uppercase_schema_type_dialect_is_lowercased_recursively() {
        let mut req = base_req(vec![msg("user", "hi")]);
        req.tools = Some(vec![ToolDef {
            tool_type: None,
            name: "get_weather".to_string(),
            description: Some("d".to_string()),
            input_schema: serde_json::json!({
                "type": "OBJECT",
                "properties": {
                    "city": {"type": "STRING"},
                    "tags": {"type": "ARRAY", "items": {"type": "STRING"}},
                    "nested": {
                        "type": "OBJECT",
                        "properties": {"n": {"type": "INTEGER"}}
                    },
                    "either": {"anyOf": [{"type": "STRING"}, {"type": "NUMBER"}]},
                    "pair": {"type": ["STRING", "NULL"]}
                },
                "required": ["city"]
            }),
        }]);

        let kiro = anthropic_to_kiro(&req, None).expect("转换应成功");
        let schema = &kiro
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools
            .as_ref()
            .expect("应带工具")[0]
            .tool_specification
            .input_schema
            .json;

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["city"]["type"], "string");
        assert_eq!(schema["properties"]["tags"]["type"], "array");
        assert_eq!(schema["properties"]["tags"]["items"]["type"], "string");
        assert_eq!(schema["properties"]["nested"]["type"], "object");
        assert_eq!(
            schema["properties"]["nested"]["properties"]["n"]["type"],
            "integer"
        );
        assert_eq!(schema["properties"]["either"]["anyOf"][0]["type"], "string");
        assert_eq!(schema["properties"]["either"]["anyOf"][1]["type"], "number");
        assert_eq!(
            schema["properties"]["pair"]["type"],
            serde_json::json!(["string", "null"])
        );
        // 语义一个字都不能改:required 与其它字段照旧。
        assert_eq!(schema["required"], serde_json::json!(["city"]));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// thinking 切分
// ─────────────────────────────────────────────────────────────────────────────

/// 切出来的一段内容。
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    /// 普通文本。
    Text(String),
    /// 思考内容(来自 `<thinking>…</thinking>`)。
    Thinking(String),
}

const THINK_OPEN: &str = "<thinking>";
const THINK_CLOSE: &str = "</thinking>";

/// 把上游下发的文本切成「普通文本」与「思考内容」两类。
///
/// 上游把思考过程用 `<thinking>…</thinking>` 包在**普通文本里**下发。此前我们原样透传,
/// 于是客户端把整段思考当成正文显示 —— Anthropic 协议里它本该是独立的 `thinking` 内容块。
///
/// **增量式**:流式逐块喂进来,非流式一次喂完整文本,两条路共用同一份实现,不会各切各的。
///
/// 跨块的半截标签靠"尾部保留"处理:输出时最多留下 `</thinking>` 的长度不发,等下一块到齐
/// 再判定。没有这一步,一个恰好被切成 `<think` + `ing>` 的标签就会被当成正文吐出去。
///
/// 模型在思考里**提到**标签(通常写成 `` `</thinking>` ``)不算结束:被反引号或引号紧贴
/// 包裹的一律跳过 —— 少了这一条,一段讨论标签本身的思考会被从中间截断,后半段当正文吐出去。
#[derive(Debug, Default)]
pub struct ThinkingSplitter {
    buf: String,
    in_thinking: bool,
}

impl ThinkingSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一段新内容,拿到此刻能确定的所有片段。
    pub fn feed(&mut self, chunk: &str) -> Vec<Piece> {
        self.buf.push_str(chunk);
        self.drain(false)
    }

    /// 流结束:把缓冲区里剩下的全部吐出去(半截标签按普通文本处理)。
    pub fn finish(&mut self) -> Vec<Piece> {
        self.drain(true)
    }

    fn drain(&mut self, eof: bool) -> Vec<Piece> {
        let mut out = Vec::new();
        loop {
            if self.in_thinking {
                match find_tag(&self.buf, THINK_CLOSE) {
                    Some(i) => {
                        let inner = self.buf[..i].to_string();
                        if !inner.is_empty() {
                            out.push(Piece::Thinking(inner));
                        }
                        self.buf = self.buf[i + THINK_CLOSE.len()..].to_string();
                        self.in_thinking = false;
                    }
                    None => {
                        // 结束标签还没到:先吐出"绝对不可能是半截标签"的那部分。
                        let keep = if eof {
                            0
                        } else {
                            holdback(&self.buf, THINK_CLOSE)
                        };
                        let cut = safe_cut(&self.buf, keep);
                        if cut > 0 {
                            let s: String = self.buf.drain(..cut).collect();
                            out.push(Piece::Thinking(s));
                        }
                        if eof && !self.buf.is_empty() {
                            out.push(Piece::Thinking(std::mem::take(&mut self.buf)));
                        }
                        break;
                    }
                }
            } else {
                match find_tag(&self.buf, THINK_OPEN) {
                    Some(i) => {
                        if i > 0 {
                            out.push(Piece::Text(self.buf[..i].to_string()));
                        }
                        self.buf = self.buf[i + THINK_OPEN.len()..].to_string();
                        // 开标签后紧跟的那个换行是格式,不是思考内容。
                        if let Some(rest) = self.buf.strip_prefix('\n') {
                            self.buf = rest.to_string();
                        }
                        self.in_thinking = true;
                    }
                    None => {
                        let keep = if eof {
                            0
                        } else {
                            holdback(&self.buf, THINK_OPEN)
                        };
                        let cut = safe_cut(&self.buf, keep);
                        if cut > 0 {
                            let s: String = self.buf.drain(..cut).collect();
                            out.push(Piece::Text(s));
                        }
                        if eof && !self.buf.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut self.buf)));
                        }
                        break;
                    }
                }
            }
        }
        out
    }
}

/// 把相邻的同类片段合并成一条。
///
/// 增量切分天然会把一段文本拆成多条(尾部保留的缘故),流式正需要这样逐条发;
/// 非流式则要的是"整段",故在这里合并。
pub fn merge_pieces(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for p in pieces {
        match (out.last_mut(), &p) {
            (Some(Piece::Text(a)), Piece::Text(b)) => a.push_str(b),
            (Some(Piece::Thinking(a)), Piece::Thinking(b)) => a.push_str(b),
            _ => out.push(p),
        }
    }
    out
}

/// 一次性切分整段文本(非流式路径)。相邻同类已合并。
pub fn split_thinking(text: &str) -> Vec<Piece> {
    let mut sp = ThinkingSplitter::new();
    let mut v = sp.feed(text);
    v.extend(sp.finish());
    merge_pieces(v)
}

/// 找到**真正的**标签位置:被反引号/引号紧贴包裹的当作正文里的引用,跳过。
fn find_tag(hay: &str, tag: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(tag) {
        let at = from + rel;
        let before = hay[..at].chars().next_back();
        let after = hay[at + tag.len()..].chars().next();
        let quoted = matches!(before, Some('`' | '"' | '\'' | '‘' | '“'))
            || matches!(after, Some('`' | '"' | '\'' | '’' | '”'));
        if !quoted {
            return Some(at);
        }
        from = at + tag.len();
    }
    None
}

/// 末尾需要压住多少字节,才不会把一个半截标签当正文吐出去。
///
/// 关键是**别无脑压固定长度**:标签必以 `<` 开头,所以只有从最后一个 `<` 起算的那一小段
/// 才可能是半截标签,其余可以立刻发走。一段不含 `<` 的普通文本因此**零延迟**透传 ——
/// 无脑压 `标签长度-1` 会让每一块下行文本都滞后近十个字节,客户端看着就是"字在卡"。
fn holdback(buf: &str, tag: &str) -> usize {
    match buf.rfind('<') {
        // 从这个 `<` 到结尾还不足一个完整标签 → 它可能是半截,压住。
        Some(i) if buf.len() - i < tag.len() => buf.len() - i,
        _ => 0,
    }
}

/// 从末尾保留 `keep` 字节后,可安全切出的字节数(落在字符边界上)。
fn safe_cut(s: &str, keep: usize) -> usize {
    let target = s.len().saturating_sub(keep);
    let mut i = target.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod thinking_tests {
    use super::*;

    /// 按块喂入后**合并同类**再比较:增量切分天然会把一段拆成多条(尾部保留的缘故),
    /// 流式正需要那样逐条发,而这些用例关心的是切分结果本身。
    fn all(chunks: &[&str]) -> Vec<Piece> {
        let mut sp = ThinkingSplitter::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(sp.feed(c));
        }
        out.extend(sp.finish());
        merge_pieces(out)
    }

    /// 一次喂完整文本(非流式路径)。
    #[test]
    fn splits_thinking_from_text_in_one_shot() {
        let v = all(&["前言<thinking>\n我在想事情</thinking>结论"]);
        assert_eq!(
            v,
            vec![
                Piece::Text("前言".into()),
                Piece::Thinking("我在想事情".into()),
                Piece::Text("结论".into()),
            ]
        );
    }

    /// **标签被切成两半**(流式最容易错的地方)。
    ///
    /// 没有"尾部保留"的话,`<think` 会被当成正文先吐出去,客户端就看到一段裸标签。
    #[test]
    fn tags_split_across_chunks_are_not_leaked_as_text() {
        let v = all(&["前言<think", "ing>思考中</think", "ing>结论"]);
        assert_eq!(
            v,
            vec![
                Piece::Text("前言".into()),
                Piece::Thinking("思考中".into()),
                Piece::Text("结论".into()),
            ]
        );
        // 逐字符喂同样要对
        let one_by_one: Vec<&str> = vec![
            "a", "<", "t", "h", "i", "n", "k", "i", "n", "g", ">", "x", "<", "/", "t", "h", "i",
            "n", "k", "i", "n", "g", ">", "b",
        ];
        let v2 = all(&one_by_one);
        let think: String = v2
            .iter()
            .filter_map(|p| match p {
                Piece::Thinking(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        let text: String = v2
            .iter()
            .filter_map(|p| match p {
                Piece::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(think, "x");
        assert_eq!(text, "ab");
    }

    /// 模型在思考里**提到**结束标签(用反引号包着)不算结束。
    ///
    /// 少了这条,一段讨论标签本身的思考会被从中间截断,后半截漏成正文。
    #[test]
    fn quoted_tags_inside_thinking_do_not_end_the_block() {
        let v = all(&["<thinking>要输出 `</thinking>` 这个标签</thinking>done"]);
        assert_eq!(
            v,
            vec![
                Piece::Thinking("要输出 `</thinking>` 这个标签".into()),
                Piece::Text("done".into()),
            ]
        );
    }

    /// 不含 `<` 的普通文本必须**当场发走**,不许压在缓冲区里。
    ///
    /// 回归:切分器最初无脑压住"标签长度 - 1"个字节,于是每一块下行文本都滞后近十个字节,
    /// 客户端看着就是"字在卡"。标签必以 `<` 开头,故只有从最后一个 `<` 起算的那一小段
    /// 才可能是半截标签。
    #[test]
    fn ordinary_text_is_emitted_immediately_without_holdback() {
        let mut sp = ThinkingSplitter::new();
        assert_eq!(
            sp.feed("po"),
            vec![Piece::Text("po".into())],
            "第一块就该发出去"
        );
        assert_eq!(sp.feed("ng"), vec![Piece::Text("ng".into())]);
        assert!(sp.finish().is_empty(), "收尾时不该还压着东西");

        // 出现 `<` 之后才压,且只压那一小段
        let mut sp2 = ThinkingSplitter::new();
        assert_eq!(
            sp2.feed("abc<th"),
            vec![Piece::Text("abc".into())],
            "`<` 之前的照发"
        );
        // 补齐成开标签 → 前面的文本已发过,这里进入 thinking
        let out = sp2.feed("inking>x</thinking>");
        assert_eq!(out, vec![Piece::Thinking("x".into())]);
    }

    /// 没有 thinking 标签时原样透传,不得改动内容。
    #[test]
    fn plain_text_passes_through_unchanged() {
        assert_eq!(
            all(&["hello ", "world"]),
            vec![Piece::Text("hello world".into())]
        );
        assert!(all(&[""]).is_empty());
    }

    /// 流在 thinking 中途结束:剩余内容按思考内容吐出,不吞。
    #[test]
    fn unterminated_thinking_is_flushed_as_thinking() {
        let v = all(&["<thinking>没写完就断了"]);
        assert_eq!(v, vec![Piece::Thinking("没写完就断了".into())]);
    }

    /// 多字节字符**不得**被从中间切开。
    #[test]
    fn multibyte_characters_are_never_split() {
        let v = all(&["中文很长的一段话没有标签"]);
        let text: String = v
            .iter()
            .map(|p| match p {
                Piece::Text(s) | Piece::Thinking(s) => s.as_str(),
            })
            .collect();
        assert_eq!(text, "中文很长的一段话没有标签");
    }
}
