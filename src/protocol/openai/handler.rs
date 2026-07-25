//! `POST /v1/chat/completions` + `GET /v1/models` 处理器:复用中枢 [`relay_core`]。
//!
//! 非流式流程:`ChatCompletionRequest` → [`openai_to_hub`] → `relay_core`(与 `/v1/messages`
//! 同一条中转内核)→ [`hub_to_openai`] → `Json` 返回。错误以 OpenAI 错误体(而非 Anthropic
//! 错误体)向外暴露,复用 [`RelayError`] 的分类与 HTTP 状态。
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
    MessagesState, RelayError, extract_client_ip, relay_core_attributed, select_and_call_with_retry,
};
use crate::protocol::anthropic::types::MessagesRequest;
use crate::protocol::openai::convert::{finish_reason_from_stop, hub_to_openai, openai_to_hub};
use crate::protocol::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChunkChoice, Delta, ModelList, ModelObject,
    ToolCallChunk, ToolCallFunctionChunk,
};

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
        RelayError::Upstream(_) => ("upstream request failed".to_string(), "api_error"),
        // 上游确定性拒绝(INVALID_MODEL_ID:该模型对当前档位不可用)→ 400 + 清晰的不可用说明。
        RelayError::InvalidModel(msg) => (msg.clone(), "invalid_request_error"),
    };
    let code: Option<String> = None;
    let body = serde_json::json!({
        "error": { "message": message, "type": kind, "code": code },
    });
    (status, Json(body)).into_response()
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
/// - 结束发一个空 `delta:{}` + `finish_reason`(有工具 → `"tool_calls"` 否则 `"stop"`)。
/// - 最后发字面 `data: [DONE]`(无 event 名,`Event::default().data("[DONE]")`)。
///
/// 同一个 `id`(`chatcmpl-<hex>`)在本次流的所有 chunk 间保持不变,照 OpenAI 规范。
pub async fn chat_completions_stream(
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
        let id = format!("chatcmpl-{}", crate::kiro::convert::new_message_id());
        // 用量记账哨兵:累计字符存于此;正常收尾显式 flush,断连/出错时其 Drop 补记(#8/#9/#15)。
        // 必须在读循环之前建立、活到 stream! future 被 drop 为止。
        let mut usage_guard = StreamUsageGuard {
            usage: usage_handle,
            credential_id,
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

        loop {
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
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => break, // 流中断:尽力收尾
            }
        }

        let finish_reason = if any_tool { finish_reason_from_stop(Some("tool_use")) } else { finish_reason_from_stop(None) };
        yield Ok(make(ChunkChoice {
            index: 0,
            delta: Delta::default(),
            finish_reason,
        }));

        yield Ok(Event::default().data("[DONE]"));

        // 流成功收尾 → 立即用当前累计量记一条用量(input 置 0;output 为字符估算)。flush 幂等并置
        // recorded,故随后哨兵 Drop 不会重复落库。若客户端在收尾前断连/上游中途出错,则本行不执行,
        // 由 usage_guard 的 Drop 补记同样的累计量(#8/#9/#15)。
        usage_guard.flush();
    };

    Ok(Sse::new(body))
}

/// axum handler:`POST /v1/chat/completions`(与 `/openai/v1/chat/completions` 共用)。
///
/// `stream:true` → SSE(`chat.completion.chunk` + `[DONE]`,见 [`chat_completions_stream`]);
/// 否则走非流式 JSON([`relay_core`] + [`hub_to_openai`])。
pub async fn chat_completions(
    State(state): State<MessagesState>,
    connect_info: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    let now = now_unix();
    let created = now;
    let is_stream = req.stream == Some(true);
    // 客户端 IP:优先 XFF/Real-IP(反代场景),否则 socket 对端地址(见 extract_client_ip)。
    let client_ip = extract_client_ip(&headers, connect_info.map(|axum::Extension(ci)| ci.0));

    let hub_req = openai_to_hub(req);
    let model = hub_req.model.clone();

    if is_stream {
        match chat_completions_stream(state, hub_req, model, created, client_ip, now).await {
            Ok(sse) => sse.into_response(),
            Err(e) => relay_error_to_openai(e),
        }
    } else {
        match relay_core_attributed(&state, hub_req, 0, client_ip, now).await {
            Ok(resp) => Json(hub_to_openai(resp, created)).into_response(),
            Err(e) => relay_error_to_openai(e),
        }
    }
}

/// axum handler:`GET /v1/models`(与 `/openai/v1/models` 共用)。固定列表,不读时钟。
pub async fn models() -> Json<ModelList> {
    const FIXED_CREATED: u64 = 1_700_000_000;
    Json(ModelList::new(vec![
        ModelObject::new(
            "claude-sonnet-4.5".to_string(),
            FIXED_CREATED,
            "kiro2api".to_string(),
        ),
        ModelObject::new(
            "claude-opus-4.6".to_string(),
            FIXED_CREATED,
            "kiro2api".to_string(),
        ),
        ModelObject::new(
            "gpt-5.6-sol".to_string(),
            FIXED_CREATED,
            "kiro2api".to_string(),
        ),
    ]))
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
