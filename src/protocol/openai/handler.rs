//! `POST /v1/chat/completions` + `GET /v1/models` 处理器:复用中枢 [`relay_core`]。
//!
//! 非流式流程:`ChatCompletionRequest` → [`openai_to_hub`] → [`relay_core_outcome`](与
//! `/v1/messages` 同一条中转内核)→ [`hub_to_openai`] → `Json` 返回。错误以 OpenAI 错误体
//! (而非 Anthropic 错误体)向外暴露,复用 [`RelayError`] 的分类与 HTTP 状态;上游放在
//! HTTP 200 事件流里的 exception 由内核单独回报,按 429/403/400/502 精确出错。
//!
//! 流式(`stream:true`)分支:OpenAI 流式 chunk 编码照 OpenAI 公开规范自写状态机——
//! 复用 [`select_and_call_with_retry`] 取上游 `reqwest::Response`,`async_stream` 里 `resp.chunk()`
//! 喂 `StreamDecoder`,逐帧转换为 `chat.completion.chunk`;工具调用按 `toolUseId` 首现顺序
//! 分配 `tool_calls[].index`(与 Anthropic 流式 `content_block` index 同构但含义不同:
//! 这里是 `tool_calls` 数组下标,纯文本不占用)。

use std::collections::HashMap;
use std::convert::Infallible;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::protocol::anthropic::handler::{
    CoreOutcome, MessagesState, RelayError, extract_client_ip, relay_core_outcome,
    select_and_call_with_retry,
};
use crate::protocol::anthropic::types::MessagesRequest;
use crate::protocol::openai::convert::{finish_reason_from_stop, hub_to_openai, openai_to_hub};
use crate::protocol::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChunkChoice, Delta, ModelList, ModelObject,
    OpenAiUsage, ToolCallChunk, ToolCallFunctionChunk,
};
use crate::server::auth::{ApiKeyId, BoundCredentialIds};

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 字符→token 粗估比(与 anthropic handler 私有的同名常量一致:约 4 字符/token)。
/// anthropic 侧未导出,故此处本地复刻(见文件末尾 reconciliation 备注)。
const CHARS_PER_TOKEN: usize = 4;

/// 流式用量记账哨兵(#8/#9/#15,对齐 anthropic 的 `StreamUsageGuard` #18):无论流如何结束
/// (正常收尾 / 客户端断连 / 上游中途出错),只要 `stream!` future 被 drop,本哨兵的 [`Drop`]
/// 就会用**当时已累计**的字符估算落一条用量,避免"记账代码在读循环之后、中途丢弃即漏记"。
///
/// 记账本身异步(写库经 `.await` 的锁),`Drop` 不能 `.await`,故 Drop 里 `tokio::spawn` 一个短
/// 任务完成落库(持 `Arc<UsageTracker>`,与流生命周期解耦)。`recorded` 防重复:正常收尾路径显式
/// 调 [`StreamUsageGuard::flush`] 立即记一次并置位,Drop 时若已记则不再重复。
///
/// 计费(#4):除字符估算 output 外,还捕获上游 `meteringEvent` 的真实积分消耗(credits)与缓存
/// token(cache_read/cache_creation),经 [`record_usage_full`](crate::stats::usage::UsageTracker::record_usage_full)
/// 落库——与 anthropic `relay_stream` 一致,避免这三个前端在流式路径上漏记 credits/缓存计费。
struct StreamUsageGuard {
    usage: std::sync::Arc<crate::stats::usage::UsageTracker>,
    credential_id: u32,
    /// 鉴权闸解析出的 store-key id(0 = 全局 key/开放模式,无归属)。用量记录归属到该 key,
    /// 其消费上限即按这些记录的 `estimated_cost` 累计。
    api_key_id: u32,
    /// 调用方 IP(由 handler 经 `extract_client_ip` 算出;无则 `None`),随用量记录落库。
    client_ip: Option<String>,
    model: String,
    now_unix: i64,
    /// 累计输出字符数(收尾按 ÷CHARS_PER_TOKEN 估算 output_tokens);随流增量更新。
    total_chars: usize,
    /// 末次 meteringEvent 的真实计费(#4;有则 credits/缓存 token 随之落库,多个则末次覆盖)。
    metering: Option<crate::kiro::convert::MeteringUsage>,
    /// 已记账标记:避免正常收尾 + Drop 双写。
    recorded: bool,
}

impl StreamUsageGuard {
    /// 本次流是否累计到**有意义的用量**:有输出字符(→ output_tokens>0)或收到过 meteringEvent
    /// (上游真实计费,可能 output 为 0 但仍有 credits/缓存计费——纯工具轮即属此类,#9)。二者皆无
    /// = 极早取消(计量前客户端断连、且尚未产出文本),补记只会写零行,应跳过(#16 类比)。
    fn has_meaningful_usage(&self) -> bool {
        self.total_chars >= CHARS_PER_TOKEN || self.metering.is_some()
    }

    /// 本次流的 `(input, output)` token 数:优先取 meteringEvent 的真实计量,缺哪项回退哪项
    /// (input 无从估算 → 0;output → 累计字符数 ÷ [`CHARS_PER_TOKEN`])。
    /// 记账与 `include_usage` 的流内 usage 共用同一口径。
    fn token_counts(&self) -> (u32, u32) {
        let input = self
            .metering
            .as_ref()
            .and_then(|m| m.input_tokens)
            .unwrap_or(0);
        let output = self
            .metering
            .as_ref()
            .and_then(|m| m.output_tokens)
            .unwrap_or((self.total_chars / CHARS_PER_TOKEN) as u32);
        (input, output)
    }

    /// 把当前累计量落一条用量(同步组装参数,异步写库经 `tokio::spawn`)。幂等:仅首次生效。
    ///
    /// Drop 安全(#8):`Drop` 可能在 tokio 运行时之外或运行时关停期触发,此时 `tokio::spawn` 会
    /// panic。故先用 [`Handle::try_current`](tokio::runtime::Handle::try_current) 探测运行时,仅在
    /// 有运行时时 spawn;无运行时则跳过(记 debug 日志,绝不 panic)。
    fn flush(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;

        let (input_tokens, output_tokens) = self.token_counts();
        let (input_tokens, output_tokens) = (input_tokens as i32, output_tokens as i32);
        // 按定价表换算美元等值成本;store-key 的消费上限即按它累计,不能恒记 0。
        let estimated_cost =
            crate::stats::pricing::calculate_cost(&self.model, input_tokens, output_tokens);
        let credits = self.metering.as_ref().map(|m| m.credits);
        let cache_read = self
            .metering
            .as_ref()
            .and_then(|m| m.cache_read_input_tokens);
        let cache_creation = self
            .metering
            .as_ref()
            .and_then(|m| m.cache_creation_input_tokens);
        let usage = self.usage.clone();
        let model = self.model.clone();
        let (credential_id, api_key_id, now_unix) =
            (self.credential_id, self.api_key_id, self.now_unix);
        let client_ip = self.client_ip.clone();

        // Drop 里 spawn 前先确认有 tokio 运行时:运行时之外/关停期 spawn 会 panic,Drop 绝不可 panic。
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    usage
                        .record_usage_full(
                            credential_id,
                            api_key_id,
                            model,
                            input_tokens,
                            output_tokens,
                            estimated_cost,
                            credits,
                            client_ip,
                            cache_read,
                            cache_creation,
                            None,
                            now_unix,
                        )
                        .await;
                });
            }
            Err(_) => {
                // 无运行时(极少见:进程关停/非 tokio 上下文 Drop)。异步写库无法进行,跳过并记 debug。
                tracing::debug!("StreamUsageGuard::flush 在 tokio 运行时之外触发,用量记录已跳过");
            }
        }
    }
}

impl Drop for StreamUsageGuard {
    fn drop(&mut self) {
        // 正常收尾已显式 flush → recorded=true,此处 no-op(不双写)。未收尾即被 drop
        // (客户端断连 / 上游出错)才走补记,且只在**真的有用量**时补记(#9):有输出字符或收到过
        // meteringEvent(纯工具轮消耗了 token/credits 也算);二者皆无才是零行、跳过(#16 类比)。
        if self.recorded {
            return;
        }
        if self.has_meaningful_usage() {
            self.flush();
        }
    }
}

/// 把 [`RelayError`] 映射成 OpenAI 形状的错误体(不泄露令牌/内部细节)。
///
/// `pub(crate)`:Responses 协议(`/v1/responses`)复用同一 OpenAI 错误体形状,
/// 避免第三份平行错误结构。
pub(crate) fn relay_error_to_openai(e: RelayError) -> Response {
    let status = e.status();
    let (message, kind) = match &e {
        RelayError::Convert(err) => (err.to_string(), "invalid_request_error"),
        RelayError::NoAccount => (
            "no available upstream account".to_string(),
            "overloaded_error",
        ),
        // 上游确定性拒绝(INVALID_MODEL_ID:该模型对当前档位不可用)→ 400 + 清晰的不可用说明。
        RelayError::InvalidRequest(msg) => (msg.clone(), "invalid_request_error"),
        // 其余(上游失败,以及中枢 `RelayError` 后续新增的变体)只报粗粒度文案,错误类型由
        // 状态码派生 —— 状态码是客户端 SDK 唯一据以决定重试的信号,不能因新增变体退化成 502。
        _ => (
            "upstream request failed".to_string(),
            openai_error_kind(status.as_u16()),
        ),
    };
    let code: Option<String> = None;
    let body = serde_json::json!({
        "error": { "message": message, "type": kind, "code": code },
    });
    (status, Json(body)).into_response()
}

/// 请求体解析失败(axum `Json` 提取器拒收)→ OpenAI 形状的 400 错误体。
///
/// 为什么不能用 axum 的默认拒收响应:那是 `422` + `content-type: text/plain` 的一行英文
/// ("Failed to deserialize the JSON body into the target type: ..."),而 OpenAI 生态的客户端
/// (官方 SDK / LangChain / 各类前端)一律按 `error.message` / `error.type` 解析错误体 ——
/// 拿到纯文本会在解析处二次抛错,把"少传了 messages 字段"这种一眼可修的参数问题变成
/// 客户端内部的 JSONDecodeError,调用方完全看不到真正原因。
///
/// 口径与 `/v1/messages` 的同类处理保持一致(见 `anthropic::handler::messages`):
/// 一律回 **400**(参数错误)并把 axum 的诊断文案原样放进 `message` —— 那句文案会点名具体是哪个
/// 字段/哪种类型不对,是调用方唯一能据以自查的线索,且不含任何令牌或内部实现细节。
///
/// `pub(crate)`:Responses 协议(`/v1/responses`)复用同一形状,不另起第二份。
pub(crate) fn json_rejection_to_openai(
    rejection: axum::extract::rejection::JsonRejection,
) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": rejection.body_text(),
            "type": "invalid_request_error",
            "code": null,
        },
    });
    (axum::http::StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// 由 HTTP 状态码派生 OpenAI 错误体的 `type`(客户端按它区分限流/鉴权/参数类错误)。
fn openai_error_kind(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 | 403 => "authentication_error",
        429 => "rate_limit_error",
        _ => "api_error",
    }
}

/// 上游 exception 的对外文案:带人类可读消息时拼成 `"<kind>: <message>"`,否则只报 kind。
///
/// `pub(crate)`:Responses 协议(`/v1/responses`)的 `response.failed` 事件复用同一文案。
pub(crate) fn exception_detail(e: &crate::kiro::convert::StreamException) -> String {
    if e.message.is_empty() {
        e.kind.clone()
    } else {
        format!("{}: {}", e.kind, e.message)
    }
}

/// 把上游在 200 事件流里夹带的 exception 映射成 `(对外状态码, OpenAI 形状错误体)`。
///
/// 状态码取 [`exception_status`](crate::kiro::convert::exception_status)(限流 429 / 鉴权 403 /
/// 参数 400 / 其余 502);`code` 填该状态码,便于只读 body 的客户端也能分流。
///
/// `pub(crate)`:Responses 协议(`/v1/responses`)的非流式路径复用同一映射。
pub(crate) fn upstream_exception_to_openai(
    e: &crate::kiro::convert::StreamException,
) -> (axum::http::StatusCode, serde_json::Value) {
    let status = axum::http::StatusCode::from_u16(crate::kiro::convert::exception_status(&e.kind))
        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
    let body = serde_json::json!({
        "error": {
            "message": exception_detail(e),
            "type": e.kind,
            "code": status.as_u16(),
        },
    });
    (status, body)
}

/// 流式内核:选-调后,把上游事件流增量编码为 OpenAI `chat.completion.chunk` SSE + `[DONE]`。
///
/// 帧状态机(与 Anthropic `relay_stream` 同构,仅编码不同):
/// - 首个 chunk 先发 `delta:{role:"assistant"}`(不含 content/tool_calls)。
/// - `frame_text_delta` → `delta:{content:<t>}`。
/// - `tool_use_frame` 按 `toolUseId` 首现顺序分配从 0 递增的 `tool_calls[].index`
///   (纯文本不占用此 index;与 Anthropic 内容块 index 是两套独立编号)。
///   open 帧(无 input/stop)→ `delta:{tool_calls:[{index,id,type:"function",function:{name}}]}`;
///   input 帧 → `delta:{tool_calls:[{index,function:{arguments:<片段>}}]}`
///   (OpenAI 工具参数流式即分片字符串,与 Kiro `input` 片段同形,原样转发);
///   stop 帧不产出 chunk(OpenAI 流式无逐工具收尾事件)。
/// - 结束发一个空 `delta:{}` + `finish_reason`(有工具 → `"tool_calls"`;否则命中上游截断
///   → `"length"`,再否则 `"stop"`;与非流式 [`hub_to_openai`] 同一口径)。
/// - `include_usage` 为真时,`[DONE]` 之前再发一个 `choices:[]` 且带 `usage` 的收尾 chunk。
/// - 最后发字面 `data: [DONE]`(无 event 名,`Event::default().data("[DONE]")`)。
/// - 上游在 200 事件流里下发非截断 exception(限流/鉴权/参数):立即发一个 `{"error":{…}}`
///   chunk 后收束,**不发** `finish_reason` 与 `[DONE]` —— 那会把上游故障伪装成正常完成。
///
/// 同一个 `id`(`chatcmpl-<hex>`)在本次流的所有 chunk 间保持不变,照 OpenAI 规范。
// 参数个数照中枢 relay 的既定形状(state/req/created/api_key_id/client_ip/bound/now/include_usage)
// 逐项对齐;拆成 struct 会让四个协议的流式入口彼此不一致,故按需放行该 lint。
#[allow(clippy::too_many_arguments)]
pub async fn chat_completions_stream(
    state: MessagesState,
    hub_req: MessagesRequest,
    created: u64,
    api_key_id: u32,
    client_ip: Option<String>,
    bound: Option<BoundCredentialIds>,
    now_unix: u64,
    include_usage: bool,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>> + use<>>, RelayError> {
    let crate::protocol::anthropic::handler::CallOutcome {
        mut resp,
        credential_id,
        tool_name_map: _,
    } = select_and_call_with_retry(&state, &hub_req, now_unix, bound.as_ref()).await?;
    // 统计层用量句柄(Arc,移入哨兵);记账经 Drop 哨兵在流**任意方式结束**时都落一条(#8/#9/#15)。
    let usage_handle = state.stats.usage.clone();
    let model = hub_req.model.clone();
    let record_model = model.clone();

    let body = async_stream::stream! {
        let id = format!("chatcmpl-{}", crate::kiro::convert::new_message_id());
        // 用量记账哨兵:累计字符存于此;正常收尾显式 flush,断连/出错时其 Drop 补记(#8/#9/#15)。
        // 必须在读循环之前建立、活到 stream! future 被 drop 为止。
        let mut usage_guard = StreamUsageGuard {
            usage: usage_handle,
            credential_id,
            api_key_id,
            client_ip,
            model: record_model,
            now_unix: now_unix as i64,
            total_chars: 0,
            metering: None,
            recorded: false,
        };

        let make = |choice: ChunkChoice| {
            let chunk = ChatCompletionChunk::new(id.clone(), created, model.clone(), vec![choice]);
            Event::default().data(serde_json::to_string(&chunk).unwrap_or_default())
        };

        // 首个 chunk:仅 role,无 content/tool_calls。
        yield Ok(make(ChunkChoice {
            index: 0,
            delta: Delta { role: Some("assistant".to_string()), content: None, tool_calls: None },
            finish_reason: None,
        }));

        let mut dec = crate::kiro::eventstream::decoder::StreamDecoder::new();
        let mut tool_index: HashMap<String, u32> = HashMap::new();
        let mut next_tool_index: u32 = 0;
        let mut any_tool = false;
        // 上游截断信号(命中 max_tokens / 上下文窗口耗尽):收尾 finish_reason 须报 "length"。
        let mut truncation: Option<crate::kiro::convert::Truncation> = None;
        // 上游中途下发的非截断 exception:置位即停读,收尾走错误分支。
        let mut upstream_error: Option<crate::kiro::convert::StreamException> = None;
        // 传输层中断(连接重置 / 读超时 / chunked 体未收尾):与 in-band exception 一样
        // 必须以错误 chunk 收尾且**不发 [DONE]**,否则客户端把半截回答当正常完成。
        let mut transport_err: Option<(u16, String)> = None;

        'read: loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    dec.push(&chunk);
                    for frame in dec.drain() {
                        if let Some(t) = crate::kiro::convert::frame_text_delta(&frame) {
                            usage_guard.total_chars += t.chars().count();
                            yield Ok(make(ChunkChoice {
                                index: 0,
                                delta: Delta { role: None, content: Some(t), tool_calls: None },
                                finish_reason: None,
                            }));
                        } else if let Some(v) = crate::kiro::convert::tool_use_frame(&frame) {
                            let Some(tool_use_id) = v["toolUseId"].as_str() else { continue };
                            let is_new = !tool_index.contains_key(tool_use_id);
                            if is_new {
                                let idx = next_tool_index;
                                next_tool_index += 1;
                                tool_index.insert(tool_use_id.to_string(), idx);
                                any_tool = true;
                                let name = v["name"].as_str().unwrap_or("");
                                yield Ok(make(ChunkChoice {
                                    index: 0,
                                    delta: Delta {
                                        role: None,
                                        content: None,
                                        tool_calls: Some(vec![ToolCallChunk {
                                            index: idx,
                                            id: Some(tool_use_id.to_string()),
                                            kind: Some("function".to_string()),
                                            function: ToolCallFunctionChunk { name: Some(name.to_string()), arguments: None },
                                        }]),
                                    },
                                    finish_reason: None,
                                }));
                            }
                            let idx = tool_index[tool_use_id];
                            if let Some(inp) = v["input"].as_str() {
                                yield Ok(make(ChunkChoice {
                                    index: 0,
                                    delta: Delta {
                                        role: None,
                                        content: None,
                                        tool_calls: Some(vec![ToolCallChunk {
                                            index: idx,
                                            id: None,
                                            kind: None,
                                            function: ToolCallFunctionChunk { name: None, arguments: Some(inp.to_string()) },
                                        }]),
                                    },
                                    finish_reason: None,
                                }));
                            }
                            // stop 帧:无需产出(OpenAI 流式没有逐工具收尾事件)。
                        } else if let Some(m) = crate::kiro::convert::metering_frame(&frame) {
                            // meteringEvent(#4):记住真实积分/缓存计费(多个则末次覆盖),收尾时落库。
                            // 不产出任何 OpenAI chunk——纯记账,不影响线格式。
                            usage_guard.metering = Some(m);
                        } else if let Some(tr) = crate::kiro::convert::frame_truncation(&frame) {
                            truncation = Some(tr);
                        } else if let Some(e) = crate::kiro::convert::frame_exception(&frame) {
                            // 非截断 exception:后续帧再无意义,停读并走错误收尾。
                            upstream_error = Some(e);
                            break 'read;
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    // 流中断:记下原因,收尾走错误 chunk(不可伪装成正常完成)。
                    let status = if e.is_timeout() { 504 } else { 502 };
                    transport_err = Some((status, e.to_string()));
                    break;
                }
            }
        }

        if let Some((status, detail)) = transport_err {
            // 与 exception 分支同口径:发错误 chunk 后终止,不补 [DONE]。
            tracing::warn!(
                event = "upstream_stream_interrupted",
                status = status,
                detail = %detail,
                "上游事件流传输中断"
            );
            let body = serde_json::json!({
                "error": {
                    "message": format!("upstream stream interrupted: {detail}"),
                    "type": "upstream_error",
                    "code": status,
                }
            });
            yield Ok(Event::default().data(body.to_string()));
        } else if let Some(e) = upstream_error {
            let (status, err_body) = upstream_exception_to_openai(&e);
            tracing::warn!(
                event = "upstream_stream_exception",
                kind = %e.kind,
                status = status.as_u16(),
                "上游事件流下发 exception"
            );
            yield Ok(Event::default().data(err_body.to_string()));
        } else {
            // 截断优先级低于工具调用(与 kiro_events_to_anthropic 一致):有工具就报 tool_calls。
            let stop = if any_tool {
                Some("tool_use")
            } else {
                match truncation {
                    Some(crate::kiro::convert::Truncation::MaxTokens) => Some("max_tokens"),
                    Some(crate::kiro::convert::Truncation::ContextWindow) => {
                        Some("model_context_window_exceeded")
                    }
                    None => None,
                }
            };
            yield Ok(make(ChunkChoice {
                index: 0,
                delta: Delta::default(),
                finish_reason: finish_reason_from_stop(stop),
            }));

            if include_usage {
                // stream_options.include_usage:照 OpenAI 规范,[DONE] 前多发一个 choices 为空、
                // 只带 usage 的 chunk;token 数与记账同源(meteringEvent 真实计量优先)。
                let (prompt_tokens, completion_tokens) = usage_guard.token_counts();
                let chunk = ChatCompletionChunk::usage_only(
                    id.clone(),
                    created,
                    model.clone(),
                    OpenAiUsage {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens: prompt_tokens + completion_tokens,
                    },
                );
                yield Ok(Event::default().data(serde_json::to_string(&chunk).unwrap_or_default()));
            }

            yield Ok(Event::default().data("[DONE]"));
        }

        // 收尾(正常或上游报错)→ 立即用当前累计量记一条用量。flush 幂等并置 recorded,故随后哨兵
        // Drop 不会重复落库。若客户端在收尾前断连,则本行不执行,由 usage_guard 的 Drop 补记(#8/#9/#15)。
        usage_guard.flush();
    };

    Ok(Sse::new(body).keep_alive(
        // SSE 保活。上游"想"得久时(长推理、长工具链)会有几十秒一个字节都不出,
        // 中间的 CDN / 反代 / 客户端读超时会把这条静默的连接掐掉,表现成"会话莫名其妙断了"。
        // 25 秒一个注释帧,既低于常见的 30/60 秒空闲阈值,又不干扰任何客户端解析
        //(`:` 开头的注释行是 SSE 规范里明确要求忽略的)。
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(25))
            .text("keep-alive"),
    ))
}

/// axum handler:`POST /v1/chat/completions`(与 `/openai/v1/chat/completions` 共用)。
///
/// `stream:true` → SSE(`chat.completion.chunk` + `[DONE]`,见 [`chat_completions_stream`]);
/// 否则走非流式 JSON([`relay_core`] + [`hub_to_openai`])。
///
/// 鉴权闸(见 `server::auth`)命中 store key 时会把 [`ApiKeyId`] 塞进请求扩展;全局 key/开放
/// 模式下扩展缺失,归属 id 记 0。两条路径(流式/非流式)都必须带上它,否则该 key 的用量与
/// 消费上限形同虚设。
///
/// 请求体以 `Result<Json<..>, JsonRejection>` 提取:解析失败时回 OpenAI 形状的 400 错误体
/// (见 [`json_rejection_to_openai`]),而不是 axum 默认的纯文本 422 —— 后者 SDK 解析不了。
pub async fn chat_completions(
    State(state): State<MessagesState>,
    connect_info: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: axum::http::HeaderMap,
    api_key_id: Option<axum::Extension<ApiKeyId>>,
    bound: Option<axum::Extension<BoundCredentialIds>>,
    payload: Result<Json<ChatCompletionRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let req = match payload {
        Ok(Json(req)) => req,
        Err(rejection) => return json_rejection_to_openai(rejection),
    };
    let api_key_id = api_key_id.and_then(|axum::Extension(k)| k.0).unwrap_or(0);
    // store-key 绑定白名单(鉴权闸解析;扩展缺席 = 不受限)。下传给选号层执行。
    let bound = bound.map(|axum::Extension(b)| b);
    let now = now_unix();
    let created = now;
    let is_stream = req.stream == Some(true);
    // 客户端 IP:优先 XFF/Real-IP(反代场景),否则 socket 对端地址(见 extract_client_ip)。
    let client_ip = extract_client_ip(
        &headers,
        connect_info.map(|axum::Extension(ci)| ci.0),
        state.cfg.trusted_proxy_hops,
    );
    // stream_options 只对流式有意义(非流式响应恒带 usage)。
    let include_usage = req
        .stream_options
        .as_ref()
        .and_then(|o| o.include_usage)
        .unwrap_or(false);

    let hub_req = openai_to_hub(req);

    if is_stream {
        match chat_completions_stream(
            state,
            hub_req,
            created,
            api_key_id,
            client_ip,
            bound,
            now,
            include_usage,
        )
        .await
        {
            Ok(sse) => sse.into_response(),
            Err(e) => relay_error_to_openai(e),
        }
    } else {
        match relay_core_outcome(&state, hub_req, api_key_id, client_ip, bound, now).await {
            Ok(CoreOutcome::Response(resp)) => Json(hub_to_openai(resp, created)).into_response(),
            // 上游 200 事件流里夹带的 exception:按其映射出的状态码回 OpenAI 错误体,
            // 不能还原成"空回答 + stop"的 200。
            Ok(CoreOutcome::Exception { e, .. }) => {
                let (status, body) = upstream_exception_to_openai(&e);
                (status, Json(body)).into_response()
            }
            Err(e) => relay_error_to_openai(e),
        }
    }
}

/// axum handler:`GET /v1/models`(与 `/openai/v1/models` 共用)。固定列表,不读时钟。
pub async fn models() -> Json<ModelList> {
    // 目录来自 `models_catalog::CATALOG`,与 Anthropic / Gemini 侧 `/models` 及管理端点的
    // 静态回落同源。此前这里硬编码三条、且三个协议各列各的,客户端"先列模型再按 id 调用"
    // 会拿到一个残缺且随协议而异的子集。
    const FIXED_CREATED: u64 = 1_700_000_000;
    Json(ModelList::new(
        crate::models_catalog::CATALOG
            .iter()
            .map(|e| ModelObject::new(e.id.to_string(), FIXED_CREATED, "kiro2api".to_string()))
            .collect(),
    ))
}

/// 带自身状态的 OpenAI 兼容子路由(供 `build_router` 合并;与 `/v1/messages` 同一 `MessagesState`)。
pub fn openai_router(state: MessagesState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/openai/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(models))
        .route("/openai/v1/models", get(models))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::kiro::credential::{AuthMethod, Credential};
    use crate::kiro::eventstream::crc::crc32;
    use crate::kiro::pool::{LbMode, Pool};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// 构造一条合法 AWS 事件流帧(照 anthropic handler 测试同法)。
    fn event_frame(event_type: &str, payload: &[u8]) -> Vec<u8> {
        let name = ":event-type";
        let mut headers = Vec::new();
        headers.push(name.len() as u8);
        headers.extend_from_slice(name.as_bytes());
        headers.push(7u8);
        headers.extend_from_slice(&(event_type.len() as u16).to_be_bytes());
        headers.extend_from_slice(event_type.as_bytes());

        let headers_len = headers.len() as u32;
        let total_len = 16 + headers_len + payload.len() as u32;

        let mut msg = Vec::new();
        msg.extend_from_slice(&total_len.to_be_bytes());
        msg.extend_from_slice(&headers_len.to_be_bytes());
        let prelude_crc = crc32(&msg[0..8]);
        msg.extend_from_slice(&prelude_crc.to_be_bytes());
        msg.extend_from_slice(&headers);
        msg.extend_from_slice(payload);
        let msg_crc = crc32(&msg);
        msg.extend_from_slice(&msg_crc.to_be_bytes());
        msg
    }

    /// 构造一条带任意字符串头的事件流帧([`event_frame`] 的通用版:exception 帧要
    /// `:message-type` + `:exception-type` 两个头)。
    fn frame_with_headers(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
        let mut hdr = Vec::new();
        for (name, value) in headers {
            hdr.push(name.len() as u8);
            hdr.extend_from_slice(name.as_bytes());
            hdr.push(7u8);
            hdr.extend_from_slice(&(value.len() as u16).to_be_bytes());
            hdr.extend_from_slice(value.as_bytes());
        }

        let headers_len = hdr.len() as u32;
        let total_len = 16 + headers_len + payload.len() as u32;

        let mut msg = Vec::new();
        msg.extend_from_slice(&total_len.to_be_bytes());
        msg.extend_from_slice(&headers_len.to_be_bytes());
        let prelude_crc = crc32(&msg[0..8]);
        msg.extend_from_slice(&prelude_crc.to_be_bytes());
        msg.extend_from_slice(&hdr);
        msg.extend_from_slice(payload);
        let msg_crc = crc32(&msg);
        msg.extend_from_slice(&msg_crc.to_be_bytes());
        msg
    }

    /// 上游 200 事件流里的 exception 帧(截断类与非截断类都用它构造)。
    fn exception_frame(kind: &str, payload: &[u8]) -> Vec<u8> {
        frame_with_headers(
            &[(":message-type", "exception"), (":exception-type", kind)],
            payload,
        )
    }

    fn cred() -> Credential {
        Credential {
            priority: 999,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            id: "a".into(),
            access_token: "AT".into(),
            refresh_token: "rt".into(),
            kiro_api_key: None,
            expires_at_unix: u64::MAX,
            region: "us-east-1".into(),
            auth: AuthMethod::Social,
            client_id: None,
            client_secret: None,
            profile_arn: None,
            machine_id: None,
            email: None,
            nickname: None,
            weight: 1,
            label: None,
            disabled: false,
            status_reason: None,
        }
    }

    fn state(server_uri: &str, creds: Vec<Credential>) -> MessagesState {
        MessagesState {
            pool: Arc::new(Mutex::new(Pool::new(creds, LbMode::Priority))),
            client: reqwest::Client::new(),
            control_client: reqwest::Client::new(),
            cfg: Arc::new(Config::default()),
            runtime_cfg: crate::config::shared_runtime_config(&crate::config::Config::default()),
            endpoint_override: Some(format!("{server_uri}/generateAssistantResponse")),
            stats: crate::stats::StatsManager::load_from_dir(&std::env::temp_dir()),
            api_keys: crate::apikey::ApiKeyStore::load(std::env::temp_dir().join(format!(
                "kiro2api_openai_apikeys_{}.json",
                std::process::id()
            ))),
            balance: crate::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
            models_cache: crate::models_cache::ModelsCache::new(),
            builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            log_capture: None,
            refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(
                std::env::temp_dir()
                    .join(format!(
                        "kiro2api_refreshctx_src_protocol_openai_handler_rs_{}.json",
                        std::process::id()
                    ))
                    .to_string_lossy()
                    .to_string(),
            ),
        }
    }

    /// 与 [`state`] 相同,但统计存储指向独立目录:按 api-key 归属的断言不能被共享
    /// temp-dir 里其它用例的记录串扰。
    fn state_with_stats_dir(
        server_uri: &str,
        creds: Vec<Credential>,
        dir: &std::path::Path,
    ) -> MessagesState {
        let mut st = state(server_uri, creds);
        st.stats = crate::stats::StatsManager::load_from_dir(dir);
        st
    }

    /// 关键回归:请求体解析不了时必须回 **OpenAI 形状的 JSON 错误体**,不能是 axum 默认的
    /// `422 text/plain`。OpenAI 生态的客户端一律按 `error.message`/`error.type` 解析错误,
    /// 拿到纯文本会在解析处二次抛错(JSONDecodeError),把"少传 messages"这种一眼可修的
    /// 参数问题变成客户端内部错误,调用方看不到真正原因。
    ///
    /// 覆盖三种真实坏法:缺必填字段、tagged enum 落到未知分支(OpenAI 新内容类型 /
    /// 误贴 Anthropic 的 block)、以及整体不是合法 JSON。
    #[tokio::test]
    async fn chat_completions_malformed_body_returns_openai_json_error() {
        let server = MockServer::start().await;
        // 上游不挂任何 Mock:请求本就不该抵达上游。
        let bad_bodies = [
            // 缺必填 messages。
            r#"{"model":"claude-sonnet-4.5"}"#,
            // content part 是 tagged enum,没有兜底分支:未知 type 直接反序列化失败。
            r#"{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{"data":"AA","format":"wav"}}]}]}"#,
            // 整体不是合法 JSON。
            r#"{"model":"claude-sonnet-4.5","messages":["#,
        ];
        for body in bad_bodies {
            let app = openai_router(state(&server.uri(), vec![cred()]));
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body = {body}");
            let ct = resp
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(
                ct.starts_with("application/json"),
                "错误体必须是 JSON,实际 content-type = {ct}(body = {body})"
            );
            let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("错误体应能被 JSON 解析: {e}(body = {body})"));
            assert_eq!(v["error"]["type"], "invalid_request_error", "body = {body}");
            assert!(
                v["error"]["message"]
                    .as_str()
                    .is_some_and(|m| !m.is_empty()),
                "错误体须带可自查的文案: {v}(body = {body})"
            );
        }
    }

    #[tokio::test]
    async fn chat_completions_returns_pong() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = openai_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["content"], "pong");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
    }

    #[tokio::test]
    async fn chat_completions_tool_call_round_trip() {
        let server = MockServer::start().await;
        let mut body = event_frame(
            "toolUseEvent",
            br#"{"name":"get_weather","toolUseId":"tu1"}"#,
        );
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"{\"city\": \"Paris\"}","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"name":"get_weather","stop":true,"toolUseId":"tu1"}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = openai_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"weather?"}],"tools":[{"type":"function","function":{"name":"get_weather","parameters":{}}}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            v["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
    }

    #[tokio::test]
    async fn models_returns_nonempty_list() {
        let server = MockServer::start().await;
        let app = openai_router(state(&server.uri(), vec![]));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["object"], "list");
        let data = v["data"].as_array().expect("data 应为数组");
        assert!(!data.is_empty());
        for item in data {
            assert_eq!(item["object"], "model");
        }
    }

    /// 流式(`stream:true`)纯文本:两帧 assistantResponseEvent "po"/"ng" →
    /// 首帧 `delta.role="assistant"`,中间逐帧 `delta.content`,末帧 `finish_reason:"stop"`,
    /// 以字面 `data: [DONE]` 收尾。
    #[tokio::test]
    async fn chat_completions_stream_emits_text_chunks_and_done() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"po"}"#);
        body.extend(event_frame(
            "assistantResponseEvent",
            br#"{"content":"ng"}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = openai_router(state(&server.uri(), vec![cred()]));
        let req_body =
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.contains("text/event-stream"), "content-type = {ct}");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);

        for needle in [
            "\"object\":\"chat.completion.chunk\"",
            "\"role\":\"assistant\"",
            "\"content\":\"po\"",
            "\"content\":\"ng\"",
            "\"finish_reason\":\"stop\"",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        assert!(
            s.trim_end().ends_with("data: [DONE]"),
            "应以 data: [DONE] 收尾;实际:\n{s}"
        );

        let role_pos = s.find("\"role\":\"assistant\"").unwrap();
        let po_pos = s.find("\"content\":\"po\"").unwrap();
        let ng_pos = s.find("\"content\":\"ng\"").unwrap();
        let finish_pos = s.find("\"finish_reason\":\"stop\"").unwrap();
        let done_pos = s.find("data: [DONE]").unwrap();
        assert!(
            role_pos < po_pos && po_pos < ng_pos && ng_pos < finish_pos && finish_pos < done_pos
        );
    }

    /// 流式(`stream:true`)工具轮:6 帧 get_weather toolUseEvent(toolUseId="tu1") →
    /// 含 `tool_calls`/`name`/`index:0`/`arguments` 片段,末帧 `finish_reason:"tool_calls"`,
    /// 以 `data: [DONE]` 收尾。
    #[tokio::test]
    async fn chat_completions_stream_emits_tool_call_chunks_and_done() {
        let server = MockServer::start().await;
        let mut body = event_frame(
            "toolUseEvent",
            br#"{"name":"get_weather","toolUseId":"tu1"}"#,
        );
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"{\"ci","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"ty\": \"Paris","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"\"}","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"name":"get_weather","stop":true,"toolUseId":"tu1"}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = openai_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"weather?"}],"tools":[{"type":"function","function":{"name":"get_weather","parameters":{}}}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.contains("text/event-stream"), "content-type = {ct}");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);

        for needle in [
            "\"tool_calls\"",
            "\"name\":\"get_weather\"",
            "\"index\":0",
            "\"arguments\"",
            "\"finish_reason\":\"tool_calls\"",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        assert!(
            s.trim_end().ends_with("data: [DONE]"),
            "应以 data: [DONE] 收尾;实际:\n{s}"
        );
        assert!(
            s.contains("ci") && s.contains("Paris"),
            "input 片段缺失;实际:\n{s}"
        );
    }

    /// #4 + #9:纯工具轮流(无任何文本帧)+ meteringEvent(带 credits/缓存)→ 流成功收尾后
    /// 用量必须落库,且 `credits_used`/缓存 token 来自 meteringEvent,而非被"零文本"跳过。
    #[tokio::test]
    async fn stream_tool_only_with_metering_records_credits() {
        let server = MockServer::start().await;
        let mut body = event_frame(
            "toolUseEvent",
            br#"{"name":"get_weather","toolUseId":"tu1"}"#,
        );
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"input":"{}","name":"get_weather","toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "toolUseEvent",
            br#"{"name":"get_weather","stop":true,"toolUseId":"tu1"}"#,
        ));
        body.extend(event_frame(
            "meteringEvent",
            br#"{"usage":3.5,"cache_read_input_tokens":128,"cache_creation_input_tokens":64}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let st = state(&server.uri(), vec![cred()]);
        let usage = st.stats.usage.clone();
        let app = openai_router(st);
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"weather?"}],"tools":[{"type":"function","function":{"name":"get_weather","parameters":{}}}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // 消费完整个流(flush 在流收尾时触发)。
        let _ = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();

        // flush 内经 tokio::spawn 异步写库;轮询等待记录出现(纯工具轮 credits 不能被 #9 的跳过闸吞掉)。
        // 共享 temp-dir 的 usage 存储可能含其它测试的记录,故按本用例特征值(credits==3.5)定位,
        // 不取"首条",避免跨测试污染导致误判。
        let mut found = None;
        for _ in 0..50 {
            let page = usage.records_for_credential(0, 0, 100).await;
            if let Some(r) = page.items.into_iter().find(|r| r.credits_used == Some(3.5)) {
                found = Some(r);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let rec = found.expect("纯工具轮 + meteringEvent 应落一条含 credits 的用量(#4/#9)");
        assert_eq!(
            rec.output_tokens, 0,
            "纯工具轮无文本 → output_tokens 为 0,但仍须落库"
        );
        assert_eq!(rec.cache_read_input_tokens, Some(128));
        assert_eq!(rec.cache_creation_input_tokens, Some(64));
    }

    /// 非流式:上游 200 事件流里夹带 exception(限流)→ 429 + OpenAI 错误体,
    /// 而不是 200 + 空回答。
    #[tokio::test]
    async fn chat_completions_upstream_exception_maps_to_status_code() {
        let server = MockServer::start().await;
        let frame = exception_frame("ThrottlingException", br#"{"message":"slow down"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = openai_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "ThrottlingException");
        assert_eq!(v["error"]["code"], 429);
        assert!(
            v["error"]["message"]
                .as_str()
                .expect("message 应为字符串")
                .contains("slow down")
        );
    }

    /// 上游在 200 事件流中途下发非截断 exception(限流):必须发 OpenAI error chunk 并就此收束,
    /// **不得**再补 `finish_reason` 与 `[DONE]` 把故障伪装成正常完成。
    #[tokio::test]
    async fn chat_completions_stream_emits_error_chunk_without_done_on_exception() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"po"}"#);
        body.extend(exception_frame(
            "ThrottlingException",
            br#"{"message":"slow down"}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = openai_router(state(&server.uri(), vec![cred()]));
        let req_body =
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);

        for needle in [
            "\"content\":\"po\"",
            "\"error\"",
            "ThrottlingException",
            "slow down",
            "\"code\":429",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        assert!(
            !s.contains("[DONE]"),
            "上游报错不得以 [DONE] 收尾;实际:\n{s}"
        );
        assert!(
            !s.contains("\"finish_reason\":\"stop\""),
            "上游报错不得报 finish_reason=stop;实际:\n{s}"
        );
    }

    /// 截断类 exception(`ContentLengthExceededException`)不是错误而是截断信号:
    /// 流式收尾须报 `finish_reason:"length"`(与非流式一致),且照常以 `[DONE]` 收尾。
    #[tokio::test]
    async fn chat_completions_stream_reports_length_on_truncation() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"po"}"#);
        body.extend(exception_frame("ContentLengthExceededException", b"{}"));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = openai_router(state(&server.uri(), vec![cred()]));
        let req_body =
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);

        assert!(
            s.contains("\"finish_reason\":\"length\""),
            "截断应报 finish_reason=length;实际:\n{s}"
        );
        assert!(
            !s.contains("\"finish_reason\":\"stop\""),
            "截断不得报 stop;实际:\n{s}"
        );
        assert!(!s.contains("\"error\""), "截断不是错误;实际:\n{s}");
        assert!(
            s.trim_end().ends_with("data: [DONE]"),
            "应以 data: [DONE] 收尾;实际:\n{s}"
        );
    }

    /// `stream_options.include_usage`:`[DONE]` 之前多发一个 `choices:[]` 且带 `usage` 的 chunk,
    /// token 数取 meteringEvent 的真实计量(而非字符估算)。
    #[tokio::test]
    async fn chat_completions_stream_include_usage_emits_usage_chunk() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        body.extend(event_frame(
            "meteringEvent",
            br#"{"usage":1.5,"input_tokens":100,"output_tokens":7}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = openai_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true,"stream_options":{"include_usage":true}}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);

        assert!(
            s.contains("\"prompt_tokens\":100") && s.contains("\"completion_tokens\":7"),
            "usage chunk 应带 meteringEvent 的真实计量;实际:\n{s}"
        );
        let usage_pos = s.find("\"usage\"").expect("应有 usage chunk");
        let done_pos = s.find("data: [DONE]").expect("应有 [DONE]");
        assert!(
            usage_pos < done_pos,
            "usage chunk 应在 [DONE] 之前;实际:\n{s}"
        );
    }

    /// 不带 `stream_options` 时流内不出现 usage 字段(照 OpenAI 规范默认不下发)。
    #[tokio::test]
    async fn chat_completions_stream_omits_usage_without_include_usage() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        body.extend(event_frame("meteringEvent", br#"{"usage":1.5}"#));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = openai_router(state(&server.uri(), vec![cred()]));
        let req_body =
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            !s.contains("\"usage\""),
            "未请求 usage 时不应下发;实际:\n{s}"
        );
    }

    /// 非流式:handler 从请求扩展读 `ApiKeyId` 并归属用量(硬编码 0 会让 store key 的
    /// 消费上限被绕过)。
    #[tokio::test]
    async fn chat_completions_attributes_usage_to_api_key_id() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!("kiro2api_openai_attr_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let st = state_with_stats_dir(&server.uri(), vec![cred()], &dir);
        let stats = st.stats.clone();
        let app = openai_router(st).layer(axum::middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(ApiKeyId(Some(9)));
                next.run(req).await
            },
        ));

        let req_body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let s9 = stats.get_summary_by_api_key(9).await;
        assert_eq!(s9.total_requests, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 流式:同样要归属到 `ApiKeyId`,且 `estimated_cost` 按定价表换算(不能恒 0,
    /// 否则该 key 的消费上限永远撞不到)。
    #[tokio::test]
    async fn chat_completions_stream_attributes_usage_to_api_key_id() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        body.extend(event_frame(
            "meteringEvent",
            br#"{"usage":1.5,"input_tokens":100,"output_tokens":7}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!(
            "kiro2api_openai_attr_stream_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let st = state_with_stats_dir(&server.uri(), vec![cred()], &dir);
        let stats = st.stats.clone();
        let app = openai_router(st).layer(axum::middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(ApiKeyId(Some(11)));
                next.run(req).await
            },
        ));

        let req_body =
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // 消费完整个流(flush 在收尾时触发,经 tokio::spawn 异步写库)。
        let _ = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();

        let mut summary = None;
        for _ in 0..50 {
            let s = stats.get_summary_by_api_key(11).await;
            if s.total_requests == 1 {
                summary = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let s11 = summary.expect("流式用量应归属到 api_key_id=11");
        assert_eq!(s11.total_input_tokens, 100);
        assert_eq!(s11.total_output_tokens, 7);
        assert!(s11.total_cost > 0.0, "estimated_cost 不应恒为 0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn empty_pool_yields_openai_error_body() {
        let server = MockServer::start().await;
        let app = openai_router(state(&server.uri(), vec![]));
        let req_body = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "overloaded_error");
        assert!(v["error"]["message"].is_string());
    }
}
