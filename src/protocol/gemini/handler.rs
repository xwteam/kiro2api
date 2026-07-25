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
//! 里 `resp.chunk()` 喂 `StreamDecoder`,逐帧编码为 `GenerateContentResponse` chunk 通过 SSE 下发。
//!
//! **SSE-vs-JSON-array 取舍**:Gemini `streamGenerateContent` 官方有两种线格式——默认(无
//! `?alt=sse` 查询参数)是「增量输出的 JSON 数组」`[{chunk},{chunk},...]`;带 `?alt=sse` 才是
//! `data: {chunk}\n\n` 的标准 SSE。官方各语言 SDK 的流式方法内部一律走 `alt=sse` 这条路径。
//! 本实现**恒定返回 SSE**(即等价于默认打开 `alt=sse`),与 SDK 的流式调用兼容;裸 HTTP 客户端
//! 期望默认 JSON 数组线格式的场景**不支持**,是已知的、有意为之的遗留限制(不在本任务范围内补齐)。

use std::collections::HashMap;
use std::convert::Infallible;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::protocol::anthropic::handler::{
    MessagesState, RelayError, extract_client_ip, relay_core_attributed, select_and_call_with_retry,
};
use crate::protocol::gemini::convert::{gemini_to_hub, hub_to_gemini};
use crate::protocol::gemini::types::{
    Candidate, Content, FunctionCall, GeminiModel, GeminiModelList, GenerateContentRequest,
    GenerateContentResponse, Part, UsageMetadata,
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
    let body = serde_json::json!({
        "error": {
            "code": 400,
            "message": format!("unsupported action: {action}"),
            "status": "INVALID_ARGUMENT",
        },
    });
    (axum::http::StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// 流式内核:选-调后,把上游事件流增量编码为 Gemini `GenerateContentResponse` SSE chunk。
///
/// 帧状态机:
/// - `frame_text_delta` → 立即发一个 chunk:`candidates:[{content:{role:"model",
///   parts:[{text:<t>}]},index:0}]`(无 `finishReason`,不含 usage)。
/// - `tool_use_frame`:**Gemini 流式无参数分片标准**(不同于 OpenAI/Anthropic 逐片 delta),
///   故按 `toolUseId` 累积 `name` + 拼接 `input` 片段,直到该 id 的 `stop:true` 帧才一次性
///   发出一个含完整 `functionCall{name,args}` 的 chunk(`args` 由拼接后的 JSON 字符串解析,
///   解析失败则退化为 `{}`,面板不 panic)。
/// - 流结束(`resp.chunk()` 返回 `Ok(None)` 或 `Err`)后,发一个收尾 chunk:
///   `finishReason:"STOP"` + 近似 `usageMetadata`(MVP:`0`,与非流式 `hub_to_gemini` 的
///   "近似 usage" 取舍一致,真实值需读 `meteringEvent`,P2 遗留)。
/// - **无 `[DONE]` 哨兵**——Gemini 流式协议里流的结束就是 chunk 序列的自然终止。
pub async fn stream_generate_content(
    state: MessagesState,
    hub_req: crate::protocol::anthropic::types::MessagesRequest,
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
        let make = |resp: GenerateContentResponse| Event::default().data(serde_json::to_string(&resp).unwrap_or_default());
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

        let text_chunk = |t: String| {
            GenerateContentResponse {
                candidates: vec![Candidate {
                    content: Content {
                        role: Some("model".to_string()),
                        parts: vec![Part { text: Some(t), inline_data: None, function_call: None, function_response: None }],
                    },
                    finish_reason: None,
                    index: 0,
                }],
                usage_metadata: None,
            }
        };

        let mut dec = crate::kiro::eventstream::decoder::StreamDecoder::new();
        // toolUseId → (name, 拼接中的 input 片段);MVP 单工具轮足够,多工具并发轮各自独立累积。
        let mut tool_names: HashMap<String, String> = HashMap::new();
        let mut tool_args: HashMap<String, String> = HashMap::new();

        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    dec.push(&chunk);
                    for frame in dec.drain() {
                        if let Some(t) = crate::kiro::convert::frame_text_delta(&frame) {
                            usage_guard.total_chars += t.chars().count();
                            yield Ok(make(text_chunk(t)));
                            continue;
                        }
                        if let Some(m) = crate::kiro::convert::metering_frame(&frame) {
                            // meteringEvent(#4):记住真实积分/缓存计费(多个则末次覆盖),收尾时落库。
                            // 不产出任何 Gemini chunk——纯记账,不影响线格式。
                            usage_guard.metering = Some(m);
                            continue;
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
                                yield Ok(make(GenerateContentResponse {
                                    candidates: vec![Candidate {
                                        content: Content {
                                            role: Some("model".to_string()),
                                            parts: vec![Part {
                                                text: None,
                                                inline_data: None,
                                                function_call: Some(FunctionCall { name, args }),
                                                function_response: None,
                                            }],
                                        },
                                        finish_reason: None,
                                        index: 0,
                                    }],
                                    usage_metadata: None,
                                }));
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => break, // 流中断:尽力收尾
            }
        }

        yield Ok(make(GenerateContentResponse {
            candidates: vec![Candidate {
                content: Content { role: Some("model".to_string()), parts: vec![] },
                finish_reason: Some("STOP".to_string()),
                index: 0,
            }],
            usage_metadata: Some(UsageMetadata { prompt_token_count: 0, candidates_token_count: 0, total_token_count: 0 }),
        }));

        // 流成功收尾 → 立即用当前累计量记一条用量(input 置 0;output 为字符估算)。flush 幂等并置
        // recorded,故随后哨兵 Drop 不会重复落库。若客户端在收尾前断连/上游中途出错,则本行不执行,
        // 由 usage_guard 的 Drop 补记同样的累计量(#8/#9/#15)。
        usage_guard.flush();
    };

    Ok(Sse::new(body))
}

/// axum handler:`POST /v1beta/models/{model_action}`(与 `/gemini/v1beta/models/{model_action}` 共用)。
///
/// `model_action` 形如 `gemini-pro:generateContent`。`generateContent` 走非流式中枢;
/// `streamGenerateContent` 走 [`stream_generate_content`](真正 SSE,见其文档的 SSE-vs-JSON-array
/// 取舍说明);其它 action → 400。
pub async fn generate_content(
    State(state): State<MessagesState>,
    Path(model_action): Path<String>,
    connect_info: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<GenerateContentRequest>,
) -> Response {
    let Some((model, action)) = split_model_action(&model_action) else {
        return bad_model_action_response();
    };
    // 客户端 IP:优先 XFF/Real-IP(反代场景),否则 socket 对端地址(见 extract_client_ip)。
    let client_ip = extract_client_ip(&headers, connect_info.map(|axum::Extension(ci)| ci.0));

    match action.as_str() {
        "generateContent" => {
            let now = now_unix();
            let hub_req = gemini_to_hub(req, model);
            match relay_core_attributed(&state, hub_req, 0, client_ip, now).await {
                Ok(resp) => Json(hub_to_gemini(resp)).into_response(),
                Err(e) => relay_error_to_gemini(e),
            }
        }
        "streamGenerateContent" => {
            let now = now_unix();
            let hub_req = gemini_to_hub(req, model);
            match stream_generate_content(state, hub_req, client_ip, now).await {
                Ok(sse) => sse.into_response(),
                Err(e) => relay_error_to_gemini(e),
            }
        }
        other => unsupported_action_response(other),
    }
}

/// axum handler:`GET /v1beta/models`(与 `/gemini/v1beta/models` 共用)。固定列表,不读时钟。
pub async fn models() -> Json<GeminiModelList> {
    let methods = || {
        vec![
            "generateContent".to_string(),
            "streamGenerateContent".to_string(),
        ]
    };
    Json(GeminiModelList {
        models: vec![
            GeminiModel {
                name: "models/claude-sonnet-4.5".to_string(),
                display_name: None,
                supported_generation_methods: methods(),
            },
            GeminiModel {
                name: "models/claude-opus-4.6".to_string(),
                display_name: None,
                supported_generation_methods: methods(),
            },
            GeminiModel {
                name: "models/gpt-5.6-sol".to_string(),
                display_name: None,
                supported_generation_methods: methods(),
            },
        ],
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

    /// 流式(`streamGenerateContent`)纯文本:两帧 assistantResponseEvent "po"/"ng" →
    /// SSE `text/event-stream`,逐帧 `"role":"model"` + `"text":"po"`/`"text":"ng"`(camelCase,
    /// 在 `parts` 里),末尾一个 `"finishReason":"STOP"` 收尾 chunk;**无 `[DONE]`**;不含
    /// snake_case 键(`finish_reason`)。
    #[tokio::test]
    async fn stream_generate_content_emits_text_chunks_camel_case_no_done() {
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

    /// 流式(`streamGenerateContent`)工具轮:6 帧 get_weather toolUseEvent →
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
}
