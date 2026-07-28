//! `POST /v1beta/models/{model}:generateContent` + `GET /v1beta/models` 处理器:复用中枢 [`relay_core`]。
//!
//! Gemini 的路径把「模型名」与「动作」拼在同一段里(`{model}:generateContent`),
//! axum 只能整段捕获再自行按最后一个 `:` 拆分——模型名本身可能含 `/` 或 `.`,
//! 但绝不含 `:`(Gemini 官方模型名规范如此),故按**最后一个** `:` 切分是安全的。
//!
//! 非流式流程:`GenerateContentRequest` → [`gemini_to_hub`] → `relay_core`(与 `/v1/messages`
//! 同一条中转内核)→ [`hub_to_gemini`] → `Json` 返回。错误以 Gemini 错误体(而非 Anthropic
//! 错误体)向外暴露,复用 [`RelayError`] 的分类与 HTTP 状态。
//!
//! `streamGenerateContent`:复用 [`select_and_call_with_retry`] 取上游 `reqwest::Response`,`async_stream`
//! 里 `resp.chunk()` 喂 `StreamDecoder`,逐帧编码为 `GenerateContentResponse` chunk 下发。
//!
//! **两种线格式**:Gemini `streamGenerateContent` 官方定义了两种——默认(无 `?alt=sse` 查询参数)
//! 是「增量输出的 JSON 数组」`[{chunk},{chunk},…]`(`application/json`);带 `?alt=sse` 才是
//! `data: {chunk}\n\n` 的标准 SSE(`text/event-stream`)。官方各语言 SDK 的流式方法内部一律走
//! `alt=sse`,裸 HTTP 客户端则按默认拿数组。两者只差外层包装,chunk 本身同形,故统一由
//! [`WireFormat`] 在同一条产出流上选包装(见 [`frame_out`])。

use std::collections::HashMap;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use crate::kiro::convert::{StreamException, Truncation};
use crate::protocol::anthropic::handler::{
    CoreOutcome, MessagesState, RelayError, extract_client_ip, relay_core_outcome,
    select_and_call_with_retry,
};
use crate::protocol::gemini::convert::{
    GeminiConvertError, finish_reason_for_truncation, gemini_to_hub, hub_to_gemini,
};
use crate::protocol::gemini::types::{
    Candidate, Content, FunctionCall, GeminiModel, GeminiModelList, GenerateContentRequest,
    GenerateContentResponse, Part, UsageMetadata,
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
    /// 鉴权闸解析出的 store-key id(0 = 全局 key / 开放模式,无归属)。用量记录归属到它,
    /// 否则 store key 的消费上限与面板用量全部落空。
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

    /// 本次流的 `(input_tokens, output_tokens)`:优先上游 meteringEvent 的真实计量,
    /// 逐项回退(input 无从估算 → 0;output → 累计字符数 / [`CHARS_PER_TOKEN`])。
    /// 记账与收尾 chunk 的 `usageMetadata` 共用它,保证两处口径一致。
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
        let credits = self.metering.as_ref().map(|m| m.credits);
        let cache_read = self
            .metering
            .as_ref()
            .and_then(|m| m.cache_read_input_tokens);
        let cache_creation = self
            .metering
            .as_ref()
            .and_then(|m| m.cache_creation_input_tokens);
        // estimated_cost 按定价表由两侧 token 换算(见 stats::pricing);input 侧有真实计量时
        // 才非 0,与 anthropic 非流式口径一致。
        let estimated_cost =
            crate::stats::pricing::calculate_cost(&self.model, input_tokens, output_tokens);
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

/// 把 URL 捕获段 `{model}:{action}` 按**最后一个** `:` 拆成 `(model, action)`。
///
/// 模型名可能自带 `models/` 前缀或点号(如 `models/gemini-1.5-pro`),故只认最后一个冒号;
/// 找不到冒号(格式非法)返回 `None`,由调用方转 400。
fn split_model_action(seg: &str) -> Option<(String, String)> {
    let idx = seg.rfind(':')?;
    Some((seg[..idx].to_string(), seg[idx + 1..].to_string()))
}

/// 把 [`RelayError`] 映射成 Gemini 形状的错误体(不泄露令牌/内部细节)。
fn relay_error_to_gemini(e: RelayError) -> Response {
    let status = e.status();
    let (message, grpc_status) = match &e {
        RelayError::Convert(err) => (err.to_string(), "INVALID_ARGUMENT"),
        RelayError::NoAccount => ("no available upstream account".to_string(), "UNAVAILABLE"),
        RelayError::Upstream(_) => ("upstream request failed".to_string(), "INTERNAL"),
        // 上游确定性拒绝(INVALID_MODEL_ID:该模型对当前档位不可用)→ 400 INVALID_ARGUMENT + 清晰的不可用说明。
        RelayError::InvalidModel(msg) => (msg.clone(), "INVALID_ARGUMENT"),
    };
    let body = serde_json::json!({
        "error": { "code": status.as_u16(), "message": message, "status": grpc_status },
    });
    (status, Json(body)).into_response()
}

/// 格式非法(捕获段无 `:`)时的 400 Gemini 错误体。
fn bad_model_action_response() -> Response {
    let body = serde_json::json!({
        "error": {
            "code": 400,
            "message": "invalid model/action path segment",
            "status": "INVALID_ARGUMENT",
        },
    });
    (axum::http::StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// 未知 action(非 `generateContent`/`streamGenerateContent`)时的 400 Gemini 错误体。
fn unsupported_action_response(action: &str) -> Response {
    bad_request_response(&format!("unsupported action: {action}"))
}

/// 通用的 400 + Gemini 错误体(`INVALID_ARGUMENT`)。
///
/// 请求体级失败(JSON 解析不了 / 形状不符 / 缺 content-type)与转换级失败都走它:Gemini
/// 客户端遇错一律 `response.json()` 取 `error.{code,message,status}`,拿到 axum 默认的
/// `text/plain` 只会抛 JSONDecodeError,真正的失败原因反而被吞掉。
fn bad_request_response(message: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "code": 400,
            "message": message,
            "status": "INVALID_ARGUMENT",
        },
    });
    (axum::http::StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// 把 [`GeminiConvertError`] 映射成 Gemini 形状的 400(与 `RelayError::Convert` 同一档:
/// 请求本身不合法,重试无用)。
fn convert_error_to_gemini(e: GeminiConvertError) -> Response {
    bad_request_response(&e.to_string())
}

/// 把上游在 200 事件流里下发的 exception 包成 Gemini 错误体。
///
/// `code` 用 [`exception_status`](crate::kiro::convert::exception_status) 映射出的 HTTP 码,
/// `status` 保留上游类型串(`ThrottlingException` 等,比 gRPC 枚举更能指明真因);
/// 上游没带人类可读消息时用类型串顶上,不留空 message。
fn exception_error_body(status: u16, e: &StreamException) -> serde_json::Value {
    let message = if e.message.is_empty() {
        e.kind.clone()
    } else {
        e.message.clone()
    };
    serde_json::json!({
        "error": { "code": status, "message": message, "status": e.kind },
    })
}

/// 非流式路径下 exception 的对外响应:按映射出的状态码回 Gemini 错误体
/// (状态码非法时回落 502,避免 `from_u16` panic)。
fn exception_to_gemini(status: u16, e: &StreamException) -> Response {
    let code =
        axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
    (code, Json(exception_error_body(status, e))).into_response()
}

/// `streamGenerateContent` 的线格式(见模块文档)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    /// `?alt=sse`:标准 SSE,每块 `data: {chunk}\n\n`,`text/event-stream`。
    Sse,
    /// 默认:增量输出的 JSON 数组 `[{chunk},{chunk},…]`,`application/json`。
    JsonArray,
}

/// `?alt=` 查询参数。Gemini 官方只定义了 `sse` 一个取值,其余/缺省即默认 JSON 数组线格式。
#[derive(Debug, Deserialize)]
pub struct AltQuery {
    #[serde(default)]
    pub alt: Option<String>,
}

/// `alt` → 线格式(大小写无关;非 `sse` 的取值一律按默认数组处理,不报错)。
fn wire_format(alt: Option<&str>) -> WireFormat {
    match alt {
        Some(a) if a.eq_ignore_ascii_case("sse") => WireFormat::Sse,
        _ => WireFormat::JsonArray,
    }
}

/// 按线格式包装一个已序列化的 chunk:SSE 加 `data: ` 前缀与空行分隔;JSON 数组则首个元素
/// 前置 `[`、其余前置 `,`(闭合的 `]` 由收尾统一补)。`first` 是数组线格式的分隔状态。
fn frame_out(wire: WireFormat, first: &mut bool, json: &str) -> Bytes {
    match wire {
        WireFormat::Sse => Bytes::from(format!("data: {json}\n\n")),
        WireFormat::JsonArray => {
            let sep = if *first { "[" } else { "," };
            *first = false;
            Bytes::from(format!("{sep}{json}"))
        }
    }
}

/// 序列化一个 chunk;序列化失败(理论上不会)退化成空串,不 panic。
fn chunk_json(resp: &GenerateContentResponse) -> String {
    serde_json::to_string(resp).unwrap_or_default()
}

/// 纯文本增量 chunk(无 `finishReason`、不含 usage)。
fn text_chunk(t: String) -> GenerateContentResponse {
    GenerateContentResponse {
        candidates: vec![Candidate {
            content: Content {
                role: Some("model".to_string()),
                parts: vec![Part {
                    text: Some(t),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                }],
            },
            finish_reason: None,
            index: 0,
        }],
        usage_metadata: None,
    }
}

/// 流式内核:选-调后,把上游事件流增量编码为 Gemini `GenerateContentResponse` chunk,
/// 按 `wire` 选 SSE 或 JSON 数组线格式下发。
///
/// 帧状态机:
/// - `frame_text_delta` → 立即发一个 chunk:`candidates:[{content:{role:"model",
///   parts:[{text:<t>}]},index:0}]`(无 `finishReason`,不含 usage)。
/// - `metering_frame` → 只记账,不产出 chunk。
/// - `frame_truncation`(`ContentLengthExceededException` / contextUsage 100%)→ 记下截断,
///   收尾报 `MAX_TOKENS`。截断不是错误,不打断流。
/// - `frame_exception`(限流/鉴权/参数等非截断 exception)→ 停止读帧,收尾改发 Gemini 错误块。
///   与截断严格互斥,不会互相抢帧。
/// - `tool_use_frame`:**Gemini 流式无参数分片标准**(不同于 OpenAI/Anthropic 逐片 delta),
///   故按 `toolUseId` 累积 `name` + 拼接 `input` 片段,直到该 id 的 `stop:true` 帧才一次性
///   发出一个含完整 `functionCall{id,name,args}` 的 chunk(`args` 由拼接后的 JSON 字符串解析,
///   解析失败则退化为 `{}`,面板不 panic)。
/// - 流结束(`resp.chunk()` 返回 `Ok(None)` 或 `Err`)后发一个收尾 chunk:`finishReason`
///   (截断 → `MAX_TOKENS`,否则 `STOP`)+ `usageMetadata`(优先上游 meteringEvent 的真实计量)。
/// - **无 `[DONE]` 哨兵**——Gemini 流式协议里流的结束就是 chunk 序列的自然终止。
pub async fn stream_generate_content(
    state: MessagesState,
    hub_req: crate::protocol::anthropic::types::MessagesRequest,
    api_key_id: u32,
    client_ip: Option<String>,
    bound: Option<BoundCredentialIds>,
    wire: WireFormat,
    now_unix: u64,
) -> Result<Response, RelayError> {
    let crate::protocol::anthropic::handler::CallOutcome {
        mut resp,
        credential_id,
    } = select_and_call_with_retry(&state, &hub_req, now_unix, bound.as_ref()).await?;
    // 统计层用量句柄(Arc,移入哨兵);记账经 Drop 哨兵在流**任意方式结束**时都落一条(#8/#9/#15)。
    let usage_handle = state.stats.usage.clone();
    let record_model = hub_req.model.clone();

    let body = async_stream::stream! {
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

        let mut dec = crate::kiro::eventstream::decoder::StreamDecoder::new();
        // toolUseId → (name, 拼接中的 input 片段);MVP 单工具轮足够,多工具并发轮各自独立累积。
        let mut tool_names: HashMap<String, String> = HashMap::new();
        let mut tool_args: HashMap<String, String> = HashMap::new();
        // JSON 数组线格式的元素分隔状态(SSE 下不参与);见 frame_out。
        let mut first = true;
        let mut truncation: Option<Truncation> = None;
        let mut exception: Option<StreamException> = None;
        // 传输层中断(连接重置 / 读超时 / chunked 体未收尾):与 in-band exception 一样
        // 必须发错误块收尾,**绝不能报 STOP**——否则客户端把"内容缺失"当成正常完成。
        let mut transport_err: Option<(u16, String)> = None;

        'read: loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    dec.push(&chunk);
                    for frame in dec.drain() {
                        if let Some(t) = crate::kiro::convert::frame_text_delta(&frame) {
                            usage_guard.total_chars += t.chars().count();
                            let json = chunk_json(&text_chunk(t));
                            yield Ok::<Bytes, std::io::Error>(frame_out(wire, &mut first, &json));
                            continue;
                        }
                        if let Some(m) = crate::kiro::convert::metering_frame(&frame) {
                            // meteringEvent(#4):记住真实积分/缓存计费与 token 计量(多个则末次
                            // 覆盖),收尾时既填 usageMetadata 也落库。不产出任何 chunk。
                            usage_guard.metering = Some(m);
                            continue;
                        }
                        if let Some(tr) = crate::kiro::convert::frame_truncation(&frame) {
                            // 截断是正常收尾的一种,只改 finishReason,不打断流(后续帧照常处理)。
                            truncation = Some(tr);
                            continue;
                        }
                        if let Some(e) = crate::kiro::convert::frame_exception(&frame) {
                            // 上游中途报错:响应头已发出,改不了状态码,只能发流内错误块后终止。
                            exception = Some(e);
                            break 'read;
                        }
                        if let Some(v) = crate::kiro::convert::tool_use_frame(&frame) {
                            let Some(tool_use_id) = v["toolUseId"].as_str() else { continue };
                            if let Some(name) = v["name"].as_str() {
                                tool_names.insert(tool_use_id.to_string(), name.to_string());
                            }
                            if let Some(inp) = v["input"].as_str() {
                                tool_args.entry(tool_use_id.to_string()).or_default().push_str(inp);
                            }
                            if v["stop"].as_bool() == Some(true) {
                                let name = tool_names.get(tool_use_id).cloned().unwrap_or_default();
                                let args_str = tool_args.get(tool_use_id).cloned().unwrap_or_default();
                                let args = serde_json::from_str::<serde_json::Value>(&args_str)
                                    .unwrap_or_else(|_| serde_json::json!({}));
                                let json = chunk_json(&GenerateContentResponse {
                                    candidates: vec![Candidate {
                                        content: Content {
                                            role: Some("model".to_string()),
                                            parts: vec![Part {
                                                text: None,
                                                inline_data: None,
                                                function_call: Some(FunctionCall {
                                                    id: Some(tool_use_id.to_string()),
                                                    name,
                                                    args,
                                                }),
                                                function_response: None,
                                            }],
                                        },
                                        finish_reason: None,
                                        index: 0,
                                    }],
                                    usage_metadata: None,
                                });
                                yield Ok(frame_out(wire, &mut first, &json));
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    // 流中断:记下原因,收尾走错误块(不可报 STOP 伪装成正常完成)。
                    let status = if e.is_timeout() { 504 } else { 502 };
                    transport_err = Some((status, e.to_string()));
                    break;
                }
            }
        }

        // 收尾块三选一:传输中断 / 上游 exception → Gemini 错误块(绝不能报 STOP,否则客户端把
        // "内容缺失"当成正常完成);否则正常收尾块,finishReason 按是否命中截断给 MAX_TOKENS /
        // STOP,usageMetadata 用上游真实计量(缺项才回退估算)。
        let final_json = if let Some((status, detail)) = &transport_err {
            tracing::warn!(
                event = "upstream_stream_interrupted",
                status = status,
                detail = %detail,
                "上游事件流传输中断"
            );
            // 复用 exception 的错误体形状:合成一个 kind,免得再造一套错误块。
            let synthetic = StreamException {
                kind: "UpstreamStreamInterrupted".to_string(),
                message: format!("upstream stream interrupted: {detail}"),
            };
            exception_error_body(*status, &synthetic).to_string()
        } else { match &exception {
            Some(e) => {
                let status = crate::kiro::convert::exception_status(&e.kind);
                tracing::warn!(
                    event = "upstream_stream_exception",
                    kind = %e.kind,
                    status = status,
                    "上游事件流下发 exception"
                );
                exception_error_body(status, e).to_string()
            }
            None => {
                let (prompt_token_count, candidates_token_count) = usage_guard.token_counts();
                chunk_json(&GenerateContentResponse {
                    candidates: vec![Candidate {
                        content: Content { role: Some("model".to_string()), parts: vec![] },
                        finish_reason: Some(finish_reason_for_truncation(truncation)),
                        index: 0,
                    }],
                    usage_metadata: Some(UsageMetadata {
                        prompt_token_count,
                        candidates_token_count,
                        total_token_count: prompt_token_count.saturating_add(candidates_token_count),
                    }),
                })
            }
        }};
        yield Ok(frame_out(wire, &mut first, &final_json));

        // JSON 数组线格式收尾:上面恒发一个元素,故此处必是闭合括号,数组恒合法。
        if wire == WireFormat::JsonArray {
            yield Ok(Bytes::from_static(b"]"));
        }

        // 正常收尾 → 立即用当前累计量记一条用量。flush 幂等并置 recorded,故随后哨兵 Drop
        // 不会重复落库。上游中途出错的流不在此显式落库:交给哨兵 Drop,由它的
        // has_meaningful_usage 闸滤掉「一个字都没产出」的零行(#9/#16);已产出内容的则照常补记。
        // 客户端在收尾前断连时本行同样不执行,走同一条 Drop 补记路径(#8/#9/#15)。
        // 传输中断与 exception 同处理:不在此显式落库,交给哨兵 Drop——由它的
        // has_meaningful_usage 闸决定(有产出才补记,一个字没出的零行滤掉)。
        if exception.is_none() && transport_err.is_none() {
            usage_guard.flush();
        }
    };

    let content_type = match wire {
        WireFormat::Sse => "text/event-stream",
        WireFormat::JsonArray => "application/json",
    };
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
        ],
        axum::body::Body::from_stream(body),
    )
        .into_response())
}

/// axum handler:`POST /v1beta/models/{model_action}`(与 `/gemini/v1beta/models/{model_action}` 共用)。
///
/// `model_action` 形如 `gemini-pro:generateContent`。`generateContent` 走非流式中枢;
/// `streamGenerateContent` 走 [`stream_generate_content`](线格式由 `?alt=` 决定,见模块文档);
/// 其它 action → 400。
///
/// 鉴权闸(见 `server::auth`)命中 store key 时会把 [`ApiKeyId`] 塞进请求扩展;
/// 全局 key/开放模式下扩展缺失,归属 id 记 0。两条路径都要归属,否则 store key 的消费上限
/// 会被绕过、面板上这些流量的用量显示为零。
///
/// 请求体以 `Result<Json<..>, JsonRejection>` 提取(与 `/v1/messages` 同法):解析失败时由本
/// 函数回 Gemini 形状的 400,而不是 axum 默认的 `text/plain` 400/422/415 —— 后者 SDK 用
/// `response.json()` 读错误时只会抛 JSONDecodeError,真正的失败原因被吞掉;且 422/415 也不在
/// Gemini 的错误码契约里。
// 参数个数由 axum 提取器决定(state/path/query/headers/连接信息/api_key_id/bound/payload),
// 每一项都是框架层必需的提取器,合并会失去提取失败时的精确错误,故按需放行该 lint。
#[allow(clippy::too_many_arguments)]
pub async fn generate_content(
    State(state): State<MessagesState>,
    Path(model_action): Path<String>,
    Query(query): Query<AltQuery>,
    connect_info: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    api_key_id: Option<axum::Extension<ApiKeyId>>,
    bound: Option<axum::Extension<BoundCredentialIds>>,
    headers: axum::http::HeaderMap,
    payload: Result<Json<GenerateContentRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Some((model, action)) = split_model_action(&model_action) else {
        return bad_model_action_response();
    };
    let req = match payload {
        Ok(Json(req)) => req,
        Err(rejection) => return bad_request_response(&rejection.body_text()),
    };
    let api_key_id = api_key_id.and_then(|axum::Extension(k)| k.0).unwrap_or(0);
    // store-key 绑定白名单(鉴权闸解析;扩展缺席 = 不受限)。下传给选号层执行。
    let bound = bound.map(|axum::Extension(b)| b);
    // 客户端 IP:优先 XFF/Real-IP(反代场景),否则 socket 对端地址(见 extract_client_ip)。
    let client_ip = extract_client_ip(
        &headers,
        connect_info.map(|axum::Extension(ci)| ci.0),
        state.cfg.trusted_proxy_hops,
    );

    match action.as_str() {
        "generateContent" => {
            let now = now_unix();
            let hub_req = match gemini_to_hub(req, model) {
                Ok(r) => r,
                Err(e) => return convert_error_to_gemini(e),
            };
            match relay_core_outcome(&state, hub_req, api_key_id, client_ip, bound, now).await {
                Ok(CoreOutcome::Response(resp)) => Json(hub_to_gemini(resp)).into_response(),
                // 上游把限流/鉴权/参数错误放在 HTTP 200 的事件流里,必须按其映射出的状态码回错,
                // 否则客户端拿到的是 200 + 空 candidates,既察觉不到失败也不会重试。
                Ok(CoreOutcome::Exception { status, e }) => exception_to_gemini(status, &e),
                Err(e) => relay_error_to_gemini(e),
            }
        }
        "streamGenerateContent" => {
            let now = now_unix();
            // 转换失败要在开流**之前**拦下:响应头一发出去就改不了状态码了。
            let hub_req = match gemini_to_hub(req, model) {
                Ok(r) => r,
                Err(e) => return convert_error_to_gemini(e),
            };
            let wire = wire_format(query.alt.as_deref());
            match stream_generate_content(state, hub_req, api_key_id, client_ip, bound, wire, now)
                .await
            {
                Ok(resp) => resp,
                Err(e) => relay_error_to_gemini(e),
            }
        }
        other => unsupported_action_response(other),
    }
}

/// axum handler:`GET /v1beta/models`(与 `/gemini/v1beta/models` 共用)。固定列表,不读时钟。
pub async fn models() -> Json<GeminiModelList> {
    // 目录来自 `models_catalog::CATALOG`(与 OpenAI / Anthropic 侧及管理端点同源);
    // 此前这里硬编码三条,与另外两个协议列的还各不相同。
    Json(GeminiModelList {
        models: crate::models_catalog::CATALOG
            .iter()
            .map(|e| GeminiModel {
                name: format!("models/{}", e.id),
                display_name: None,
                supported_generation_methods: vec![
                    "generateContent".to_string(),
                    "streamGenerateContent".to_string(),
                ],
            })
            .collect(),
    })
}

/// 带自身状态的 Gemini 兼容子路由(供 `build_router` 合并;与 `/v1/messages` 同一 `MessagesState`)。
pub fn gemini_router(state: MessagesState) -> Router {
    Router::new()
        .route("/v1beta/models/{model_action}", post(generate_content))
        .route(
            "/gemini/v1beta/models/{model_action}",
            post(generate_content),
        )
        .route("/v1beta/models", get(models))
        .route("/gemini/v1beta/models", get(models))
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

    /// 构造一条 `:message-type: exception` 帧(带 `:exception-type` 头)——上游把限流/鉴权/
    /// 参数错误放在 HTTP 200 的事件流里就是这个形状。
    fn exception_frame(exception_type: &str, payload: &[u8]) -> Vec<u8> {
        let mut headers = Vec::new();
        for (name, value) in [
            (":message-type", "exception"),
            (":exception-type", exception_type),
        ] {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7u8);
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }

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

    /// 把流式响应体读成「元素 JSON 列表」:SSE 取 `data: ` 行,JSON 数组线格式整体解析。
    fn stream_elements(body: &str, wire: WireFormat) -> Vec<serde_json::Value> {
        match wire {
            WireFormat::Sse => body
                .lines()
                .filter_map(|l| l.strip_prefix("data: "))
                .map(|d| serde_json::from_str(d).expect("每个 data 行都应是合法 JSON"))
                .collect(),
            WireFormat::JsonArray => serde_json::from_str::<serde_json::Value>(body)
                .expect("默认线格式应是一个合法 JSON 数组")
                .as_array()
                .expect("应为数组")
                .clone(),
        }
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
                "kiro2api_gemini_apikeys_{}.json",
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
                        "kiro2api_refreshctx_src_protocol_gemini_handler_rs_{}.json",
                        std::process::id()
                    ))
                    .to_string_lossy()
                    .to_string(),
            ),
        }
    }

    /// 与 [`state`] 同构,但统计层落在指定目录。按 api-key 归属的断言必须与其它用例隔离:
    /// 共享 temp-dir 的用量存储会被并发用例写入的记录污染。
    fn state_with_stats_dir(
        server_uri: &str,
        creds: Vec<Credential>,
        dir: &std::path::Path,
    ) -> MessagesState {
        let mut st = state(server_uri, creds);
        st.stats = crate::stats::StatsManager::load_from_dir(dir);
        st
    }

    /// 用一层中间件把 `ApiKeyId(Some(id))` 塞进请求扩展,模拟鉴权闸命中 store key。
    fn with_api_key_id(app: Router, id: u32) -> Router {
        app.layer(axum::middleware::from_fn(
            move |mut req: Request<Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(ApiKeyId(Some(id)));
                next.run(req).await
            },
        ))
    }

    #[test]
    fn split_model_action_splits_on_last_colon() {
        assert_eq!(
            split_model_action("gemini-pro:generateContent"),
            Some(("gemini-pro".to_string(), "generateContent".to_string()))
        );
        assert_eq!(
            split_model_action("models/gemini-1.5-pro:streamGenerateContent"),
            Some((
                "models/gemini-1.5-pro".to_string(),
                "streamGenerateContent".to_string()
            ))
        );
        assert_eq!(split_model_action("no-colon-here"), None);
    }

    #[tokio::test]
    async fn generate_content_returns_pong() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["candidates"][0]["content"]["parts"][0]["text"], "pong");
        assert_eq!(v["candidates"][0]["finishReason"], "STOP");
        assert_eq!(v["candidates"][0]["content"]["role"], "model");
    }

    #[tokio::test]
    async fn generate_content_tool_call_round_trip() {
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

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"weather?"}]}],"tools":[{"functionDeclarations":[{"name":"get_weather","parameters":{}}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v["candidates"][0]["content"]["parts"][0]["functionCall"]["name"],
            "get_weather"
        );
        assert_eq!(
            v["candidates"][0]["content"]["parts"][0]["functionCall"]["args"]["city"],
            "Paris"
        );
    }

    /// 流式(`streamGenerateContent?alt=sse`)纯文本:两帧 assistantResponseEvent "po"/"ng" →
    /// SSE `text/event-stream`,逐帧 `"role":"model"` + `"text":"po"`/`"text":"ng"`(camelCase,
    /// 在 `parts` 里),末尾一个 `"finishReason":"STOP"` 收尾 chunk;**无 `[DONE]`**;不含
    /// snake_case 键(`finish_reason`)。SSE 需显式 `alt=sse`,默认是 JSON 数组线格式。
    #[tokio::test]
    async fn stream_generate_content_alt_sse_emits_text_chunks_camel_case_no_done() {
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

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse")
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
            "\"role\":\"model\"",
            "\"text\":\"po\"",
            "\"text\":\"ng\"",
            "\"finishReason\":\"STOP\"",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        assert!(
            !s.contains("finish_reason"),
            "不应有 snake_case `finish_reason`;实际:\n{s}"
        );
        assert!(
            !s.contains("[DONE]"),
            "Gemini 流式不应有 [DONE] 哨兵;实际:\n{s}"
        );

        let po_pos = s.find("\"text\":\"po\"").unwrap();
        let ng_pos = s.find("\"text\":\"ng\"").unwrap();
        let finish_pos = s.find("\"finishReason\":\"STOP\"").unwrap();
        assert!(po_pos < ng_pos && ng_pos < finish_pos);
    }

    /// 流式(`streamGenerateContent?alt=sse`)工具轮:6 帧 get_weather toolUseEvent →
    /// Gemini 流式工具参数无分片标准,累积到 stop 帧才发一个完整 `functionCall` chunk;
    /// 末尾 `"finishReason":"STOP"`;无 `[DONE]`。
    #[tokio::test]
    async fn stream_generate_content_emits_function_call_chunk_at_stop() {
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

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"weather?"}]}],"tools":[{"functionDeclarations":[{"name":"get_weather","parameters":{}}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse")
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
            "\"functionCall\"",
            "\"name\":\"get_weather\"",
            "\"finishReason\":\"STOP\"",
        ] {
            assert!(s.contains(needle), "SSE 缺 `{needle}`;实际:\n{s}");
        }
        assert!(
            !s.contains("[DONE]"),
            "Gemini 流式不应有 [DONE] 哨兵;实际:\n{s}"
        );

        // args 应从拼接片段解析出 city:Paris
        let v: serde_json::Value = s
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .find_map(|d| {
                let parsed: serde_json::Value = serde_json::from_str(d).ok()?;
                if parsed["candidates"][0]["content"]["parts"][0]["functionCall"].is_object() {
                    Some(parsed)
                } else {
                    None
                }
            })
            .expect("应有一个含 functionCall 的 chunk");
        assert_eq!(
            v["candidates"][0]["content"]["parts"][0]["functionCall"]["args"]["city"],
            "Paris"
        );
        // 上游 toolUseId 随 functionCall.id 下发,客户端回填 functionResponse 时可精确配对。
        assert_eq!(
            v["candidates"][0]["content"]["parts"][0]["functionCall"]["id"],
            "tu1"
        );
    }

    #[tokio::test]
    async fn unsupported_action_yields_400() {
        let server = MockServer::start().await;
        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:countTokens")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["status"], "INVALID_ARGUMENT");
    }

    // ==================== 请求体级失败:必须是 Gemini 错误体,不是 axum 纯文本 ====================

    /// 断言:400 + `application/json` + Gemini 形状的 `error{code,message,status}`。
    async fn assert_gemini_400(resp: Response) -> serde_json::Value {
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let ct = resp
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();
        assert!(
            ct.contains("application/json"),
            "错误响应必须是 JSON(SDK 用 response.json() 读错误);实际 content-type = {ct}"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes)
            .expect("错误体必须是合法 JSON,否则 SDK 抛 JSONDecodeError");
        assert_eq!(v["error"]["code"], 400);
        assert_eq!(v["error"]["status"], "INVALID_ARGUMENT");
        assert!(v["error"]["message"].is_string());
        v
    }

    /// 回归:请求体 JSON 截断 → 此前是 axum 默认的 `text/plain` 400,SDK 的 `response.json()`
    /// 会抛 JSONDecodeError,真正的失败原因被吞掉。必须回 Gemini 错误体。
    #[tokio::test]
    async fn malformed_json_body_yields_gemini_error_body() {
        let server = MockServer::start().await;
        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"contents":[{"role":"user""#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_gemini_400(resp).await;
    }

    /// 回归:形状不符(`contents` 不是数组)→ 此前是 `422 text/plain`。422 不在 Gemini 的
    /// 错误码契约里,SDK 也解析不了纯文本。
    #[tokio::test]
    async fn shape_mismatch_body_yields_400_gemini_error_not_422() {
        let server = MockServer::start().await;
        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"contents":"not-an-array"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_gemini_400(resp).await;
    }

    /// 回归:缺 `content-type` → 此前是 `415 text/plain`。同样收编成 Gemini 400 错误体。
    #[tokio::test]
    async fn missing_content_type_yields_gemini_error_body() {
        let server = MockServer::start().await;
        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .body(Body::from(
                        r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_gemini_400(resp).await;
    }

    /// 回归:`tools` 里的内建工具条目(google-genai 的 `types.Tool(google_search=...)`、
    /// gemini-cli 默认就发)没有 `functionDeclarations` 键,此前整个请求被打成 422 —— 这些
    /// 客户端一句话都发不出去。必须照常放行(内建工具本身无从兑现,安静忽略)。
    #[tokio::test]
    async fn builtin_tool_entry_does_not_reject_request() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"functionDeclarations":[{"name":"f"}]},{"codeExecution":{}},{"googleSearch":{}}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "内建工具条目不应把整个请求打成 422"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["candidates"][0]["content"]["parts"][0]["text"], "pong");
    }

    /// 回归:snake_case(proto 原名)请求体全链路走通 —— 此前 `system_instruction` /
    /// `inline_data` 被 serde 当未知字段静默丢弃,请求照样 200 但内容已经缺斤少两。
    #[tokio::test]
    async fn snake_case_request_body_is_accepted_end_to_end() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"},{"inline_data":{"mime_type":"image/png","data":"AAAA"}}]}],"system_instruction":{"parts":[{"text":"be brief"}]},"generation_config":{"max_output_tokens":64}}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["candidates"][0]["content"]["parts"][0]["text"], "pong");
    }

    /// 回归:非 `image/*` 的 `inlineData`(PDF 等)此前被当图片发上游、`format` 是伪造值,
    /// 调用方只能收到一个无从归因的上游错误。现在应在开流前就回 400 并点名 mimeType。
    #[tokio::test]
    async fn non_image_inline_data_yields_gemini_400() {
        let server = MockServer::start().await;
        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"application/pdf","data":"JVBERi0="}}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = assert_gemini_400(resp).await;
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("application/pdf"),
            "错误文案应点名 mimeType;实际:{}",
            v["error"]["message"]
        );
    }

    /// 流式路径同样必须在**开流之前**拦下:响应头一发出去状态码就改不了了。
    #[tokio::test]
    async fn non_image_inline_data_yields_gemini_400_on_stream_path() {
        let server = MockServer::start().await;
        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"audio/mp3","data":"AAAA"}}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_gemini_400(resp).await;
    }

    #[tokio::test]
    async fn models_returns_nonempty_list() {
        let server = MockServer::start().await;
        let app = gemini_router(state(&server.uri(), vec![]));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1beta/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let models = v["models"].as_array().expect("models 应为数组");
        assert!(!models.is_empty());
        for m in models {
            let name = m["name"].as_str().expect("name 应为字符串");
            assert!(name.starts_with("models/"), "name = {name}");
        }
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
        let app = gemini_router(st);
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"weather?"}]}],"tools":[{"functionDeclarations":[{"name":"get_weather","parameters":{}}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent")
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
    async fn empty_pool_yields_gemini_error_body() {
        let server = MockServer::start().await;
        let app = gemini_router(state(&server.uri(), vec![]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["status"], "UNAVAILABLE");
        assert!(v["error"]["message"].is_string());
    }

    // ==================== 线格式:默认 JSON 数组 vs `?alt=sse` ====================

    #[test]
    fn wire_format_defaults_to_json_array() {
        assert_eq!(wire_format(None), WireFormat::JsonArray);
        assert_eq!(wire_format(Some("")), WireFormat::JsonArray);
        assert_eq!(wire_format(Some("json")), WireFormat::JsonArray);
        assert_eq!(wire_format(Some("sse")), WireFormat::Sse);
        assert_eq!(wire_format(Some("SSE")), WireFormat::Sse);
    }

    #[test]
    fn frame_out_wraps_per_wire_format() {
        let text = |b: &Bytes| String::from_utf8_lossy(b).to_string();

        let mut first = true;
        assert_eq!(
            text(&frame_out(WireFormat::Sse, &mut first, "{}")),
            "data: {}\n\n"
        );
        // SSE 不消费数组分隔状态。
        assert!(first);

        let mut first = true;
        assert_eq!(
            text(&frame_out(WireFormat::JsonArray, &mut first, "{}")),
            "[{}"
        );
        assert!(!first);
        assert_eq!(
            text(&frame_out(WireFormat::JsonArray, &mut first, "{}")),
            ",{}"
        );
    }

    /// 默认(无 `alt`)线格式:`application/json` + 一个完整的 JSON 数组,末元素带 finishReason。
    /// 裸 HTTP 客户端按 Gemini 默认契约解析,不应看到 SSE 的 `data: ` 前缀。
    #[tokio::test]
    async fn stream_generate_content_default_wire_format_is_json_array() {
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

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent")
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
        assert!(ct.contains("application/json"), "content-type = {ct}");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            !s.contains("data: "),
            "默认线格式不应带 SSE 前缀;实际:\n{s}"
        );

        let items = stream_elements(&s, WireFormat::JsonArray);
        assert_eq!(items.len(), 3, "两个文本块 + 一个收尾块;实际:\n{s}");
        assert_eq!(
            items[0]["candidates"][0]["content"]["parts"][0]["text"],
            "po"
        );
        assert_eq!(
            items[1]["candidates"][0]["content"]["parts"][0]["text"],
            "ng"
        );
        assert_eq!(items[2]["candidates"][0]["finishReason"], "STOP");
    }

    // ==================== 收尾:exception / 截断 / 真实 usage ====================

    /// 上游中途下发非截断 exception(限流):流内发一个 Gemini 错误块并终止,
    /// **不得**伪装成 `finishReason:"STOP"` 的正常完成。
    #[tokio::test]
    async fn stream_exception_frame_yields_error_element_not_stop() {
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

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse")
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
            !s.contains("\"finishReason\""),
            "上游出错的流不应带任何 finishReason;实际:\n{s}"
        );

        let items = stream_elements(&s, WireFormat::Sse);
        let last = items.last().expect("应至少有一个块");
        assert_eq!(last["error"]["code"], 429);
        assert_eq!(last["error"]["status"], "ThrottlingException");
        assert_eq!(last["error"]["message"], "slow down");
    }

    /// 上游没带人类可读消息时,错误块的 message 用类型串顶上,不留空串。
    #[tokio::test]
    async fn stream_exception_without_message_falls_back_to_kind() {
        let server = MockServer::start().await;
        let body = exception_frame("AccessDeniedException", b"not-json");
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent")
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
        let items = stream_elements(&s, WireFormat::JsonArray);
        let last = items.last().expect("应至少有一个块");
        assert_eq!(last["error"]["code"], 403);
        assert_eq!(last["error"]["message"], "AccessDeniedException");
    }

    /// 上游截断(`ContentLengthExceededException`)不是错误,但收尾必须报 `MAX_TOKENS`,
    /// 否则客户端把"被截断的半截内容"当成完整回答。
    #[tokio::test]
    async fn stream_truncation_frame_yields_max_tokens() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"po"}"#);
        body.extend(exception_frame("ContentLengthExceededException", b"{}"));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse")
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
        let items = stream_elements(&s, WireFormat::Sse);
        let last = items.last().expect("应至少有一个块");
        assert_eq!(last["candidates"][0]["finishReason"], "MAX_TOKENS");
        // 截断走 Truncation 路径,不得被当成 exception 报错。
        assert!(last["error"].is_null(), "截断不是错误;实际:\n{s}");
    }

    /// 流式收尾的 usageMetadata 取自上游 meteringEvent 的真实计量,不再恒为 0。
    #[tokio::test]
    async fn stream_usage_metadata_uses_metering_tokens() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        body.extend(event_frame(
            "meteringEvent",
            br#"{"usage":1.5,"input_tokens":123,"output_tokens":45}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse")
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
        let items = stream_elements(&s, WireFormat::Sse);
        let last = items.last().expect("应至少有一个块");
        assert_eq!(last["usageMetadata"]["promptTokenCount"], 123);
        assert_eq!(last["usageMetadata"]["candidatesTokenCount"], 45);
        assert_eq!(last["usageMetadata"]["totalTokenCount"], 168);
    }

    /// 非流式 `promptTokenCount` 同样来自 meteringEvent(核心层回填),不再恒为 0。
    #[tokio::test]
    async fn generate_content_usage_metadata_uses_metering_tokens() {
        let server = MockServer::start().await;
        let mut body = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        body.extend(event_frame(
            "meteringEvent",
            br#"{"usage":1.5,"input_tokens":11,"output_tokens":22}"#,
        ));
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["usageMetadata"]["promptTokenCount"], 11);
        assert_eq!(v["usageMetadata"]["candidatesTokenCount"], 22);
        assert_eq!(v["usageMetadata"]["totalTokenCount"], 33);
    }

    /// 非流式路径:上游 200 事件流里夹带 exception → 按映射出的状态码回 Gemini 错误体,
    /// 而不是 200 + 空 candidates。
    #[tokio::test]
    async fn generate_content_upstream_exception_yields_mapped_status() {
        let server = MockServer::start().await;
        let body = exception_frame("ThrottlingException", br#"{"message":"slow down"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let app = gemini_router(state(&server.uri(), vec![cred()]));
        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], 429);
        assert_eq!(v["error"]["status"], "ThrottlingException");
        assert_eq!(v["error"]["message"], "slow down");
        assert!(v["candidates"].is_null());
    }

    // ==================== store-key 归属 ====================

    /// 非流式:handler 从请求扩展读 `ApiKeyId` 并归属用量(此前硬编码 0,
    /// 导致 store key 的消费上限被绕过、面板用量为零)。
    #[tokio::test]
    async fn generate_content_attributes_usage_to_api_key_id() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!("kiro2api_gemini_attr_{}", std::process::id()));
        // 上一轮遗留的记录会污染"某 key 计了几条"的断言,先清干净。
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let st = state_with_stats_dir(&server.uri(), vec![cred()], &dir);
        let stats = st.stats.clone();
        let app = with_api_key_id(gemini_router(st), 9);

        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:generateContent")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        assert_eq!(stats.get_summary_by_api_key(9).await.total_requests, 1);
        assert_eq!(stats.get_summary_by_api_key(0).await.total_requests, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 流式:记账哨兵同样归属到扩展里的 `ApiKeyId`(记账异步落库,故轮询等待)。
    #[tokio::test]
    async fn stream_generate_content_attributes_usage_to_api_key_id() {
        let server = MockServer::start().await;
        let frame = event_frame("assistantResponseEvent", br#"{"content":"pong pong"}"#);
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir().join(format!(
            "kiro2api_gemini_attr_stream_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let st = state_with_stats_dir(&server.uri(), vec![cred()], &dir);
        let stats = st.stats.clone();
        let app = with_api_key_id(gemini_router(st), 11);

        let req_body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/claude-sonnet-4.5:streamGenerateContent?alt=sse")
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

        let mut ok = false;
        for _ in 0..50 {
            if stats.get_summary_by_api_key(11).await.total_requests == 1 {
                ok = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(ok, "流式用量应归属到 api_key_id=11");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
