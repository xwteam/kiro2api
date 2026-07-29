//! `POST /v1/responses` 处理器(非流式 + 流式):复用中枢 [`relay_core`] / [`select_and_call_with_retry`]。
//!
//! 非流式:`ResponsesRequest` → [`responses_to_hub`] → [`relay_core_outcome`](与 `/v1/messages`、
//! `/v1/chat/completions` 同一条中转内核)→ [`hub_to_responses`] → `Json` 返回。
//! `previous_response_id` 非空 → [`ResponsesConvertError::PreviousResponseUnsupported`] → 400
//! (OpenAI 错误体)。未映射模型由 `relay_core` 内部转换阶段判定,统一走
//! `RelayError::Convert` → 400,这里不重复处理;上游放在 HTTP 200 事件流里的 exception 由内核
//! 单独回报,按 429/403/400/502 精确出错。
//!
//! 流式(`stream:true`)分支:OpenAI Responses 流式**具名事件**编码照公开规范自写
//! 状态机(见 [`responses_stream`])——复用 [`select_and_call_with_retry`] 取上游 `reqwest::Response`,
//! `async_stream` 里 `resp.chunk()` 喂 `StreamDecoder`,逐帧转换为
//! `Event::default().event(<name>).data(<json 含 sequence_number>)`。`sequence_number`
//! 从 0 单调递增,`output_index` 按输出条目出现序递增(文本 message 与各 function_call
//! 各占一个 index)。**无 `[DONE]`**——流自然结束。

use std::collections::HashMap;
use std::convert::Infallible;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;

use crate::protocol::anthropic::handler::{
    CoreOutcome, MessagesState, RelayError, extract_client_ip, relay_core_outcome,
    select_and_call_with_retry,
};
use crate::protocol::anthropic::types::MessagesRequest;
use crate::protocol::openai::handler::{
    exception_detail, json_rejection_to_openai, relay_error_to_openai, upstream_exception_to_openai,
};
use crate::protocol::responses::convert::{
    ResponsesConvertError, hub_to_responses, responses_to_hub,
};
use crate::protocol::responses::types::ResponsesRequest;
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
    model: String,
    now_unix: i64,
    /// 累计输出字符数(收尾按 ÷CHARS_PER_TOKEN 估算 output_tokens);随文本帧增量更新
    /// (与状态机的 `full_text` 同步累加,故断连时 Drop 也拿得到当时累计量)。
    total_chars: usize,
    /// 末次 meteringEvent 的真实计费(#4;有则 credits/缓存 token 随之落库,多个则末次覆盖)。
    metering: Option<crate::kiro::convert::MeteringUsage>,
    /// 已记账标记:避免正常收尾 + Drop 双写。
    recorded: bool,
    /// 调用方 IP(由 handler 经 `extract_client_ip` 算出;无则 `None`),随用量记录落库。
    client_ip: Option<String>,
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
    /// 记账与 `response.completed` 事件里的 usage 共用同一口径。
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

/// `previous_response_id` 拒绝时的 OpenAI 形状错误体(与 `relay_error_to_openai` 同构)。
fn previous_response_unsupported_error() -> Response {
    let body = serde_json::json!({
        "error": {
            "message": "previous_response_id is not supported",
            "type": "invalid_request_error",
            "code": null,
        },
    });
    (axum::http::StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// 流式内核:选-调后,把上游事件流增量编码为 OpenAI Responses **具名事件** SSE(无 `[DONE]`)。
///
/// 状态机(照 OpenAI 公开 Responses 流式规范自写):
/// - 维护 `seq`(每发一事件填当前值再 +1,`sequence_number` 全流单调)、稳定的
///   `resp_id`(`resp_<hex>`,贯穿整条流)、`next_output_index`(每新增一个输出条目 +1)。
/// - 开头两事件:`response.created` → `response.in_progress`(同一 `response` 对象,
///   `status:"in_progress"`、`output:[]`、`usage:null`)。
/// - 文本帧(`frame_text_delta`):首现时懒开文本 message 条目——占一个 `output_index`、
///   分配 `msg_<hex>` 条目 id,发 `response.output_item.added`(item `message`,`content:[]`)
///   再发 `response.content_part.added`(`part` 空 `output_text`);随后每帧发
///   `response.output_text.delta`(`delta` 为该帧文本),并把全文累积起来。
/// - 工具帧(`tool_use_frame`):按 `toolUseId` 首现顺序各占一个 `output_index`、分配
///   `fc_<hex>` 条目 id。open 帧发 `response.output_item.added`(item `function_call`,
///   含 `call_id`/`name`/`arguments:""`);`input` 片段发
///   `response.function_call_arguments.delta`(`delta:<片段>`,原样转发,与 Kiro `input`
///   分片同形)并累积;`stop:true` 帧发 `response.function_call_arguments.done`
///   (`arguments:<全参>`)+ `response.output_item.done`(item `function_call` `completed`)。
/// - 收尾:若开过文本条目,依次发 `response.output_text.done`、`response.content_part.done`、
///   `response.output_item.done`(item `message`);最后发 `response.completed`,其 `response`
///   对象 `status:"completed"`、`output` 按 output_index 序含全部收尾条目、`usage` 取上游真实计量。
/// - 上游截断(命中 max_tokens / 上下文窗口耗尽)→ 末事件改发 `response.incomplete`
///   (`status:"incomplete"` + `incomplete_details`),与非流式 [`hub_to_responses`] 同一口径。
/// - 上游下发非截断 exception(限流/鉴权/参数)→ 末事件改发 `response.failed`
///   (`status:"failed"` + `error`),绝不报 `response.completed`。
/// - **无 `[DONE]`**——流自然结束。上游 chunk 出错则跳出循环、仍尽力发末事件。
pub async fn responses_stream(
    state: MessagesState,
    hub_req: MessagesRequest,
    created: u64,
    api_key_id: u32,
    client_ip: Option<String>,
    bound: Option<BoundCredentialIds>,
    now_unix: u64,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>> + use<>>, RelayError> {
    let crate::protocol::anthropic::handler::CallOutcome {
        mut resp,
        credential_id,
    } = select_and_call_with_retry(&state, &hub_req, now_unix, bound.as_ref()).await?;
    // 统计层用量句柄(Arc,移入哨兵);记账经 Drop 哨兵在流**任意方式结束**时都落一条(#8/#9/#15)。
    let usage_handle = state.stats.usage.clone();
    let model = hub_req.model.clone();
    let record_model = model.clone();

    let body = async_stream::stream! {
        // 用量记账哨兵:累计字符存于此(与 full_text 同步累加);正常收尾显式 flush,断连/出错时其
        // Drop 补记(#8/#9/#15)。必须在读循环之前建立、活到 stream! future 被 drop 为止。
        let mut usage_guard = StreamUsageGuard {
            usage: usage_handle,
            credential_id,
            api_key_id,
            model: record_model,
            now_unix: now_unix as i64,
            total_chars: 0,
            metering: None,
            recorded: false,
            client_ip,
        };
        let resp_id = format!("resp_{}", crate::protocol::responses::convert::random_hex_id());

        let mut seq: u64 = 0;
        let mut next_seq = || { let s = seq; seq += 1; s };

        // 组装 `response` 对象(`status`/`output`/`usage` 随阶段变化)。
        let response_obj = |status: &str, output: serde_json::Value, usage: serde_json::Value| {
            serde_json::json!({
                "id": resp_id,
                "object": "response",
                "created_at": created,
                "status": status,
                "model": model,
                "output": output,
                "usage": usage,
            })
        };

        // 发一个具名事件:`data` 的 `"type"` 恒等于 `event` 名。
        macro_rules! emit {
            ($name:expr, $val:expr) => {{
                let mut obj = $val;
                if let Some(map) = obj.as_object_mut() {
                    map.insert("type".to_string(), serde_json::Value::String($name.to_string()));
                    map.insert("sequence_number".to_string(), serde_json::Value::from(next_seq()));
                }
                Event::default().event($name).data(obj.to_string())
            }};
        }

        // 1. response.created / 2. response.in_progress
        yield Ok(emit!(
            "response.created",
            serde_json::json!({ "response": response_obj("in_progress", serde_json::json!([]), serde_json::Value::Null) })
        ));
        yield Ok(emit!(
            "response.in_progress",
            serde_json::json!({ "response": response_obj("in_progress", serde_json::json!([]), serde_json::Value::Null) })
        ));

        let mut dec = crate::kiro::eventstream::decoder::StreamDecoder::new();

        // 文本 message 条目懒开状态。
        let mut msg_open = false;
        let mut msg_index: u64 = 0;
        let mut msg_item_id = String::new();
        let mut full_text = String::new();

        // 每个 toolUseId 的条目状态。
        struct ToolState { index: u64, item_id: String, call_id: String, name: String, args: String }
        let mut tools: HashMap<String, ToolState> = HashMap::new();
        let mut tool_order: Vec<String> = Vec::new();

        let mut next_output_index: u64 = 0;
        // 上游截断信号(命中 max_tokens / 上下文窗口耗尽):末事件改发 response.incomplete。
        let mut truncation: Option<crate::kiro::convert::Truncation> = None;
        // 上游中途下发的非截断 exception:置位即停读,末事件改发 response.failed。
        let mut upstream_error: Option<crate::kiro::convert::StreamException> = None;
        // 传输层中断(连接重置 / 读超时 / chunked 体未收尾):与 in-band exception 一样
        // 必须以 response.failed 收束,否则 agent 框架会把半截回答当完整结果继续用。
        let mut transport_err: Option<(u16, String)> = None;

        'read: loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    dec.push(&chunk);
                    for frame in dec.drain() {
                        if let Some(t) = crate::kiro::convert::frame_text_delta(&frame) {
                            if !msg_open {
                                msg_open = true;
                                msg_index = next_output_index;
                                next_output_index += 1;
                                msg_item_id = format!("msg_{}", crate::protocol::responses::convert::random_hex_id());
                                yield Ok(emit!("response.output_item.added", serde_json::json!({
                                    "output_index": msg_index,
                                    "item": {
                                        "type": "message",
                                        "id": msg_item_id,
                                        "status": "in_progress",
                                        "role": "assistant",
                                        "content": [],
                                    },
                                })));
                                yield Ok(emit!("response.content_part.added", serde_json::json!({
                                    "item_id": msg_item_id,
                                    "output_index": msg_index,
                                    "content_index": 0,
                                    "part": { "type": "output_text", "text": "" },
                                })));
                            }
                            usage_guard.total_chars += t.chars().count();
                            full_text.push_str(&t);
                            yield Ok(emit!("response.output_text.delta", serde_json::json!({
                                "item_id": msg_item_id,
                                "output_index": msg_index,
                                "content_index": 0,
                                "delta": t,
                            })));
                        } else if let Some(v) = crate::kiro::convert::tool_use_frame(&frame) {
                            let Some(id) = v["toolUseId"].as_str() else { continue };
                            if !tools.contains_key(id) {
                                let index = next_output_index;
                                next_output_index += 1;
                                let item_id = format!("fc_{}", crate::protocol::responses::convert::random_hex_id());
                                let name = v["name"].as_str().unwrap_or("").to_string();
                                yield Ok(emit!("response.output_item.added", serde_json::json!({
                                    "output_index": index,
                                    "item": {
                                        "type": "function_call",
                                        "id": item_id,
                                        "call_id": id,
                                        "name": name,
                                        "arguments": "",
                                        "status": "in_progress",
                                    },
                                })));
                                tool_order.push(id.to_string());
                                tools.insert(id.to_string(), ToolState {
                                    index,
                                    item_id,
                                    call_id: id.to_string(),
                                    name,
                                    args: String::new(),
                                });
                            }
                            if let Some(frag) = v["input"].as_str()
                                && let Some(st) = tools.get_mut(id)
                            {
                                st.args.push_str(frag);
                                let item_id = st.item_id.clone();
                                let index = st.index;
                                yield Ok(emit!("response.function_call_arguments.delta", serde_json::json!({
                                    "item_id": item_id,
                                    "output_index": index,
                                    "delta": frag,
                                })));
                            }
                            if v["stop"].as_bool() == Some(true)
                                && let Some(st) = tools.get(id)
                            {
                                yield Ok(emit!("response.function_call_arguments.done", serde_json::json!({
                                    "item_id": st.item_id,
                                    "output_index": st.index,
                                    "arguments": st.args,
                                })));
                                yield Ok(emit!("response.output_item.done", serde_json::json!({
                                    "output_index": st.index,
                                    "item": {
                                        "type": "function_call",
                                        "id": st.item_id,
                                        "call_id": st.call_id,
                                        "name": st.name,
                                        "arguments": st.args,
                                        "status": "completed",
                                    },
                                })));
                            }
                        } else if let Some(m) = crate::kiro::convert::metering_frame(&frame) {
                            // meteringEvent(#4):记住真实积分/缓存计费(多个则末次覆盖),收尾时落库。
                            // 不产出任何 Responses 事件——纯记账,不影响线格式。
                            usage_guard.metering = Some(m);
                        } else if let Some(tr) = crate::kiro::convert::frame_truncation(&frame) {
                            truncation = Some(tr);
                        } else if let Some(e) = crate::kiro::convert::frame_exception(&frame) {
                            // 非截断 exception:后续帧再无意义,停读并走 response.failed 收尾。
                            upstream_error = Some(e);
                            break 'read;
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    // 流中断:记下原因,收尾走 response.failed(不可伪装成 completed)。
                    let status = if e.is_timeout() { 504 } else { 502 };
                    transport_err = Some((status, e.to_string()));
                    break;
                }
            }
        }

        // 传输层中断:与上游报错同口径,直接以 response.failed 收束(不发"条目已完成")。
        if let Some((status, detail)) = transport_err {
            tracing::warn!(
                event = "upstream_stream_interrupted",
                status = status,
                detail = %detail,
                "上游事件流传输中断"
            );
            let mut failed = response_obj("failed", serde_json::json!([]), serde_json::Value::Null);
            if let Some(map) = failed.as_object_mut() {
                map.insert("error".to_string(), serde_json::json!({
                    "code": "upstream_stream_interrupted",
                    "message": format!("upstream stream interrupted: {detail}"),
                }));
            }
            yield Ok(emit!("response.failed", serde_json::json!({ "response": failed })));
            usage_guard.flush();
            return;
        }

        // 上游报错:不发任何"条目已完成"事件,直接以 response.failed 收束整条流。
        if let Some(e) = upstream_error {
            let status = crate::kiro::convert::exception_status(&e.kind);
            tracing::warn!(
                event = "upstream_stream_exception",
                kind = %e.kind,
                status = status,
                "上游事件流下发 exception"
            );
            let mut failed = response_obj("failed", serde_json::json!([]), serde_json::Value::Null);
            if let Some(map) = failed.as_object_mut() {
                map.insert("error".to_string(), serde_json::json!({
                    "code": e.kind,
                    "message": exception_detail(&e),
                }));
            }
            yield Ok(emit!("response.failed", serde_json::json!({ "response": failed })));
            usage_guard.flush();
            return;
        }

        // 截断时文本条目并未写完,条目状态随之报 incomplete(与顶层 status 一致)。
        let item_status = if truncation.is_some() { "incomplete" } else { "completed" };

        // 文本条目收尾。
        if msg_open {
            yield Ok(emit!("response.output_text.done", serde_json::json!({
                "item_id": msg_item_id,
                "output_index": msg_index,
                "content_index": 0,
                "text": full_text,
            })));
            yield Ok(emit!("response.content_part.done", serde_json::json!({
                "item_id": msg_item_id,
                "output_index": msg_index,
                "content_index": 0,
                "part": { "type": "output_text", "text": full_text },
            })));
            yield Ok(emit!("response.output_item.done", serde_json::json!({
                "output_index": msg_index,
                "item": {
                    "type": "message",
                    "id": msg_item_id,
                    "status": item_status,
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": full_text }],
                },
            })));
        }

        // 按 output_index 序组装最终 output(文本 message 在前,各 function_call 按出现序)。
        let mut finished: Vec<(u64, serde_json::Value)> = Vec::new();
        if msg_open {
            finished.push((msg_index, serde_json::json!({
                "type": "message",
                "id": msg_item_id,
                "status": item_status,
                "role": "assistant",
                "content": [{ "type": "output_text", "text": full_text }],
            })));
        }
        for id in &tool_order {
            if let Some(st) = tools.get(id) {
                finished.push((st.index, serde_json::json!({
                    "type": "function_call",
                    "id": st.item_id,
                    "call_id": st.call_id,
                    "name": st.name,
                    "arguments": st.args,
                    "status": "completed",
                })));
            }
        }
        finished.sort_by_key(|(idx, _)| *idx);
        let output: Vec<serde_json::Value> = finished.into_iter().map(|(_, v)| v).collect();

        // usage:meteringEvent 的真实计量优先,缺项才回退字符估算(与记账同源,不再拿字符数
        // 直接冒充 output_tokens——那会比其它路径虚高约 CHARS_PER_TOKEN 倍)。
        let (input_tokens, output_tokens) = usage_guard.token_counts();
        let usage = serde_json::json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens as u64 + output_tokens as u64,
        });

        if truncation.is_some() {
            let mut incomplete = response_obj("incomplete", serde_json::Value::from(output), usage);
            if let Some(map) = incomplete.as_object_mut() {
                map.insert(
                    "incomplete_details".to_string(),
                    serde_json::json!({ "reason": "max_output_tokens" }),
                );
            }
            yield Ok(emit!("response.incomplete", serde_json::json!({ "response": incomplete })));
        } else {
            yield Ok(emit!(
                "response.completed",
                serde_json::json!({ "response": response_obj("completed", serde_json::Value::from(output), usage) })
            ));
        }

        // 流成功收尾 → 立即用当前累计量记一条用量(token 取真实计量,缺则字符估算)。
        // flush 幂等并置 recorded,故随后哨兵 Drop 不会重复落库。若客户端在收尾前断连/上游中途出错,
        // 则本行不执行,由 usage_guard 的 Drop 补记同样的累计量(#8/#9/#15)。
        usage_guard.flush();
    };

    Ok(Sse::new(body))
}

/// axum handler:`POST /v1/responses`(与 `/openai/v1/responses` 共用)。
///
/// `stream:true` → 具名事件 SSE(见 [`responses_stream`]);否则走非流式 JSON
/// ([`relay_core`] + [`hub_to_responses`])。`previous_response_id` 非空一律先 400。
///
/// 鉴权闸(见 `server::auth`)命中 store key 时会把 [`ApiKeyId`] 塞进请求扩展;全局 key/开放
/// 模式下扩展缺失,归属 id 记 0。两条路径(流式/非流式)都必须带上它,否则该 key 的用量与
/// 消费上限形同虚设。
///
/// 请求体以 `Result<Json<..>, JsonRejection>` 提取:解析失败时回 OpenAI 形状的 400 错误体
/// (见 [`json_rejection_to_openai`]),而不是 axum 默认的纯文本 422 —— 后者 SDK 解析不了。
pub async fn responses(
    State(state): State<MessagesState>,
    connect_info: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: axum::http::HeaderMap,
    api_key_id: Option<axum::Extension<ApiKeyId>>,
    bound: Option<axum::Extension<BoundCredentialIds>>,
    payload: Result<Json<ResponsesRequest>, axum::extract::rejection::JsonRejection>,
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

    let hub_req = match responses_to_hub(req) {
        Err(ResponsesConvertError::PreviousResponseUnsupported) => {
            return previous_response_unsupported_error();
        }
        Ok(hub_req) => hub_req,
    };

    if is_stream {
        match responses_stream(state, hub_req, created, api_key_id, client_ip, bound, now).await {
            Ok(sse) => sse.into_response(),
            Err(e) => relay_error_to_openai(e),
        }
    } else {
        match relay_core_outcome(&state, hub_req, api_key_id, client_ip, bound, now).await {
            Ok(CoreOutcome::Response(resp)) => {
                Json(hub_to_responses(resp, created)).into_response()
            }
            // 上游 200 事件流里夹带的 exception:按其映射出的状态码回错误体(与 /v1/chat/completions
            // 同一形状),不能还原成 status:"completed" 的 200。
            Ok(CoreOutcome::Exception { e, .. }) => {
                let (status, body) = upstream_exception_to_openai(&e);
                (status, Json(body)).into_response()
            }
            Err(e) => relay_error_to_openai(e),
        }
    }
}

/// 带自身状态的 Responses 兼容子路由(供 `build_router` 合并;与 `/v1/messages` 同一 `MessagesState`)。
pub fn responses_router(state: MessagesState) -> Router {
    Router::new()
        .route("/v1/responses", post(responses))
        .route("/openai/v1/responses", post(responses))
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

    /// 构造一条合法 AWS 事件流帧(照 anthropic/openai handler 测试同法)。
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
                "kiro2api_responses_apikeys_{}.json",
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
                        "kiro2api_refreshctx_src_protocol_responses_handler_rs_{}.json",
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

    /// 关键回归:请求体解析不了时必须回 **OpenAI 形状的 JSON 错误体**(400 +
    /// `invalid_request_error`),不能是 axum 默认的 `422 text/plain` —— Responses 客户端
    /// (官方 SDK / Codex 一类)按 `error.message`/`error.type` 解析错误体,拿到纯文本
    /// 只会在解析处二次抛错,真正的原因("少传 input")一个字都传不到调用方。
    #[tokio::test]
    async fn responses_malformed_body_returns_openai_json_error() {
        let server = MockServer::start().await;
        // 上游不挂 Mock:坏请求本就不该抵达上游。
        let bad_bodies = [
            // 缺必填 input。
            r#"{"model":"claude-sonnet-4.5"}"#,
            // input 条目里的 type 不在受支持的分支内。
            r#"{"model":"claude-sonnet-4.5","input":[{"type":"computer_call","action":{}}]}"#,
            // 整体不是合法 JSON。
            r#"{"model":"claude-sonnet-4.5","input":["#,
        ];
        for body in bad_bodies {
            let app = responses_router(state(&server.uri(), vec![cred()]));
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/responses")
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
    async fn responses_returns_pong() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":"hi"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["object"], "response");
        assert_eq!(v["status"], "completed");
        assert_eq!(v["output"][0]["type"], "message");
        assert_eq!(v["output"][0]["content"][0]["text"], "pong");
    }

    #[tokio::test]
    async fn responses_with_previous_response_id_returns_400() {
        let server = MockServer::start().await;
        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"gpt-4o","input":"hi","previous_response_id":"resp_x"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert!(v["error"]["message"].is_string());
    }

    /// 流式(`stream:true`)纯文本:两帧 assistantResponseEvent "po"/"ng" →
    /// 命名事件按序 `response.created` → `response.in_progress` → `response.output_item.added`
    /// → `response.content_part.added` → 两处 `response.output_text.delta`("po"/"ng")
    /// → `response.output_text.done` → `response.completed`;`sequence_number` 单调;无 `[DONE]`。
    #[tokio::test]
    async fn responses_stream_emits_named_text_events() {
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

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":"hi","stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
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
            "event: response.created",
            "event: response.in_progress",
            "event: response.output_item.added",
            "event: response.content_part.added",
            "\"delta\":\"po\"",
            "\"delta\":\"ng\"",
            "event: response.output_text.done",
            "event: response.completed",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }

        // 事件按序出现。
        let created_pos = s.find("event: response.created").unwrap();
        let added_pos = s.find("event: response.output_item.added").unwrap();
        let part_pos = s.find("event: response.content_part.added").unwrap();
        let po_pos = s.find("\"delta\":\"po\"").unwrap();
        let ng_pos = s.find("\"delta\":\"ng\"").unwrap();
        let text_done_pos = s.find("event: response.output_text.done").unwrap();
        let completed_pos = s.find("event: response.completed").unwrap();
        assert!(
            created_pos < added_pos
                && added_pos < part_pos
                && part_pos < po_pos
                && po_pos < ng_pos
                && ng_pos < text_done_pos
                && text_done_pos < completed_pos,
            "事件序错;实际:\n{s}"
        );

        // sequence_number 单调:0 在 1 之前出现。
        let seq0 = s.find("\"sequence_number\":0").unwrap();
        let seq1 = s.find("\"sequence_number\":1").unwrap();
        assert!(seq0 < seq1, "sequence_number 应单调;实际:\n{s}");

        // 无 [DONE]。
        assert!(
            !s.contains("[DONE]"),
            "Responses 流不应含 [DONE];实际:\n{s}"
        );
    }

    /// 流式(`stream:true`)工具轮:6 帧 get_weather toolUseEvent →
    /// 含 `response.output_item.added`(function_call/name)、`response.function_call_arguments.delta`、
    /// args 拼出 city、`response.function_call_arguments.done`、`response.completed`;无 `[DONE]`。
    #[tokio::test]
    async fn responses_stream_emits_named_tool_events() {
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

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":"weather?","tools":[{"type":"function","name":"get_weather","parameters":{}}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
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
            "event: response.output_item.added",
            "\"type\":\"function_call\"",
            "\"name\":\"get_weather\"",
            "event: response.function_call_arguments.delta",
            "event: response.function_call_arguments.done",
            "event: response.completed",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        // args 片段拼出 city。
        assert!(
            s.contains("ci") && s.contains("Paris"),
            "工具参数片段缺失;实际:\n{s}"
        );
        // 无 [DONE]。
        assert!(
            !s.contains("[DONE]"),
            "Responses 流不应含 [DONE];实际:\n{s}"
        );
    }

    /// #4 + #9:纯工具轮流(无文本帧)+ meteringEvent → 用量落库,credits/缓存来自 meteringEvent。
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
        let app = responses_router(st);
        let req_body = r#"{"model":"sonnet","input":"weather?","tools":[{"type":"function","name":"get_weather","parameters":{}}],"stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let _ = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();

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

    /// 非流式:上游 200 事件流里夹带 exception(鉴权)→ 403 + 错误体,
    /// 而不是 200 + `status:"completed"` 的空回答。
    #[tokio::test]
    async fn responses_upstream_exception_maps_to_status_code() {
        let server = MockServer::start().await;
        let frame = exception_frame("AccessDeniedException", br#"{"message":"nope"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":"hi"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "AccessDeniedException");
        assert_eq!(v["error"]["code"], 403);
    }

    /// 上游在 200 事件流中途下发非截断 exception(限流):末事件必须是 `response.failed`
    /// (带 error),**不得**报 `response.completed` 把故障伪装成正常完成。
    #[tokio::test]
    async fn responses_stream_emits_failed_event_on_upstream_exception() {
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

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":"hi","stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
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
            "event: response.failed",
            "\"status\":\"failed\"",
            "ThrottlingException",
            "slow down",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        assert!(
            !s.contains("event: response.completed"),
            "上游报错不得报 response.completed;实际:\n{s}"
        );
    }

    /// 截断类 exception 不是错误而是截断信号:末事件是 `response.incomplete`
    /// (`status:"incomplete"` + `incomplete_details`),而非 `response.completed`。
    #[tokio::test]
    async fn responses_stream_emits_incomplete_event_on_truncation() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"half"}"#);
        body.extend(exception_frame("ContentLengthExceededException", b"{}"));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":"hi","stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
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
            "event: response.incomplete",
            "\"status\":\"incomplete\"",
            "\"reason\":\"max_output_tokens\"",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        assert!(
            !s.contains("event: response.completed"),
            "截断不得报 response.completed;实际:\n{s}"
        );
        assert!(
            !s.contains("event: response.failed"),
            "截断不是错误;实际:\n{s}"
        );
    }

    /// `response.completed` 的 usage 取 meteringEvent 的真实计量,而不是把字符数当 output_tokens。
    #[tokio::test]
    async fn responses_stream_usage_uses_real_metering_tokens() {
        let server = MockServer::start().await;
        // 文本 16 字符:字符数直填会得到 16,真实计量应为 7。
        let mut body = event_frame(
            "assistantResponseEvent",
            br#"{"content":"0123456789abcdef"}"#,
        );
        body.extend(event_frame(
            "meteringEvent",
            br#"{"usage":1.5,"input_tokens":100,"output_tokens":7}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":"hi","stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
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
            s.contains("\"input_tokens\":100")
                && s.contains("\"output_tokens\":7")
                && s.contains("\"total_tokens\":107"),
            "response.completed 的 usage 应取真实计量;实际:\n{s}"
        );
        assert!(
            !s.contains("\"output_tokens\":16"),
            "不应把字符数当 output_tokens;实际:\n{s}"
        );
    }

    /// 非流式:handler 从请求扩展读 `ApiKeyId` 并归属用量(硬编码 0 会让 store key 的
    /// 消费上限被绕过)。
    #[tokio::test]
    async fn responses_attributes_usage_to_api_key_id() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let dir =
            std::env::temp_dir().join(format!("kiro2api_responses_attr_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let st = state_with_stats_dir(&server.uri(), vec![cred()], &dir);
        let stats = st.stats.clone();
        let app = responses_router(st).layer(axum::middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(ApiKeyId(Some(13)));
                next.run(req).await
            },
        ));

        let req_body = r#"{"model":"sonnet","input":"hi"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let s13 = stats.get_summary_by_api_key(13).await;
        assert_eq!(s13.total_requests, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 流式:同样要归属到 `ApiKeyId`,且 `estimated_cost` 按定价表换算(不能恒 0)。
    #[tokio::test]
    async fn responses_stream_attributes_usage_to_api_key_id() {
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
            "kiro2api_responses_attr_stream_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let st = state_with_stats_dir(&server.uri(), vec![cred()], &dir);
        let stats = st.stats.clone();
        let app = responses_router(st).layer(axum::middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(ApiKeyId(Some(15)));
                next.run(req).await
            },
        ));

        let req_body = r#"{"model":"sonnet","input":"hi","stream":true}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
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
            let s = stats.get_summary_by_api_key(15).await;
            if s.total_requests == 1 {
                summary = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let s15 = summary.expect("流式用量应归属到 api_key_id=15");
        assert_eq!(s15.total_input_tokens, 100);
        assert_eq!(s15.total_output_tokens, 7);
        assert!(s15.total_cost > 0.0, "estimated_cost 不应恒为 0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 非流式截断:`status:"incomplete"` + `incomplete_details`,不能报 completed。
    #[tokio::test]
    async fn responses_truncated_returns_incomplete_status() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"half"}"#);
        body.extend(exception_frame("ContentLengthExceededException", b"{}"));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":"hi"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "incomplete");
        assert_eq!(v["incomplete_details"]["reason"], "max_output_tokens");
        assert_eq!(v["output"][0]["status"], "incomplete");
    }

    /// 官方 SDK 写法:input 条目不带 `type`(靠 role 推断),须正常中转而非 422。
    #[tokio::test]
    async fn responses_accepts_input_items_without_type() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["output"][0]["content"][0]["text"], "pong");
    }

    #[tokio::test]
    async fn responses_tool_call_round_trip() {
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

        let app = responses_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"model":"sonnet","input":"weather?","tools":[{"type":"function","name":"get_weather","parameters":{}}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let output = v["output"].as_array().expect("output 应为数组");
        assert!(
            output
                .iter()
                .any(|item| item["type"] == "function_call" && item["name"] == "get_weather"),
            "output 应含 function_call;实际: {output:?}"
        );
    }
}
