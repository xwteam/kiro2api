//! `POST /v1/responses` 处理器(非流式 + 流式):复用中枢 [`relay_core`] / [`select_and_call_with_retry`]。
//!
//! 非流式:`ResponsesRequest` → [`responses_to_hub`] → `relay_core`(与 `/v1/messages`、
//! `/v1/chat/completions` 同一条中转内核)→ [`hub_to_responses`] → `Json` 返回。
//! `previous_response_id` 非空 → [`ResponsesConvertError::PreviousResponseUnsupported`] → 400
//! (OpenAI 错误体)。未映射模型由 `relay_core` 内部转换阶段判定,统一走
//! `RelayError::Convert` → 400,这里不重复处理。
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
    MessagesState, RelayError, extract_client_ip, relay_core_attributed, select_and_call_with_retry,
};
use crate::protocol::anthropic::types::MessagesRequest;
use crate::protocol::openai::handler::relay_error_to_openai;
use crate::protocol::responses::convert::{
    ResponsesConvertError, hub_to_responses, responses_to_hub,
};
use crate::protocol::responses::types::ResponsesRequest;

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

        let output_tokens = (self.total_chars / CHARS_PER_TOKEN) as i32;
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
        let (credential_id, now_unix) = (self.credential_id, self.now_unix);
        let client_ip = self.client_ip.clone();

        // Drop 里 spawn 前先确认有 tokio 运行时:运行时之外/关停期 spawn 会 panic,Drop 绝不可 panic。
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    usage
                        .record_usage_full(
                            credential_id,
                            0,
                            model,
                            0,
                            output_tokens,
                            0.0,
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
///   `response.output_item.done`(item `message` `completed`);最后发 `response.completed`,
///   其 `response` 对象 `status:"completed"`、`output` 按 output_index 序含全部收尾条目、`usage` 近似。
/// - **无 `[DONE]`**——流自然结束。上游 chunk 出错则跳出循环、仍尽力发 `response.completed`。
pub async fn responses_stream(
    state: MessagesState,
    hub_req: MessagesRequest,
    model: String,
    created: u64,
    client_ip: Option<String>,
    now_unix: u64,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>> + use<>>, RelayError> {
    let crate::protocol::anthropic::handler::CallOutcome {
        mut resp,
        credential_id,
    } = select_and_call_with_retry(&state, &hub_req, now_unix).await?;
    // 统计层用量句柄(Arc,移入哨兵);记账经 Drop 哨兵在流**任意方式结束**时都落一条(#8/#9/#15)。
    let usage_handle = state.stats.usage.clone();
    let record_model = hub_req.model.clone();

    let body = async_stream::stream! {
        // 用量记账哨兵:累计字符存于此(与 full_text 同步累加);正常收尾显式 flush,断连/出错时其
        // Drop 补记(#8/#9/#15)。必须在读循环之前建立、活到 stream! future 被 drop 为止。
        let mut usage_guard = StreamUsageGuard {
            usage: usage_handle,
            credential_id,
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

        loop {
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
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => break, // 流中断:尽力收尾
            }
        }

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
                    "status": "completed",
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
                "status": "completed",
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

        // usage 近似:输出 token 用累计文本字符数粗估(无真实用量流)。
        let approx = full_text.chars().count() as u64;
        let usage = serde_json::json!({
            "input_tokens": 0,
            "output_tokens": approx,
            "total_tokens": approx,
        });

        yield Ok(emit!(
            "response.completed",
            serde_json::json!({ "response": response_obj("completed", serde_json::Value::from(output), usage) })
        ));

        // 流成功收尾 → 立即用当前累计量记一条用量(input 置 0;output 为字符估算,与其它协议一致)。
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
pub async fn responses(
    State(state): State<MessagesState>,
    connect_info: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ResponsesRequest>,
) -> Response {
    let now = now_unix();
    let created = now;
    let is_stream = req.stream == Some(true);
    // 客户端 IP:优先 XFF/Real-IP(反代场景),否则 socket 对端地址(见 extract_client_ip)。
    let client_ip = extract_client_ip(&headers, connect_info.map(|axum::Extension(ci)| ci.0));

    let hub_req = match responses_to_hub(req) {
        Err(ResponsesConvertError::PreviousResponseUnsupported) => {
            return previous_response_unsupported_error();
        }
        Ok(hub_req) => hub_req,
    };

    if is_stream {
        let model = hub_req.model.clone();
        match responses_stream(state, hub_req, model, created, client_ip, now).await {
            Ok(sse) => sse.into_response(),
            Err(e) => relay_error_to_openai(e),
        }
    } else {
        match relay_core_attributed(&state, hub_req, 0, client_ip, now).await {
            Ok(resp) => Json(hub_to_responses(resp, created)).into_response(),
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

    fn cred() -> Credential {
        Credential {
            id: "a".into(),
            access_token: "AT".into(),
            refresh_token: "rt".into(),
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
