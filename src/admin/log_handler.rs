//! Phase 6 实时日志端点(SSE 流 + 快照 + 下载)。
//!
//! 三个 handler 均从 [`MessagesState::log_capture`] 读数据;`None`(log_capacity=0 或未接线)
//! 时统一返回 503。鉴权由挂载处的 admin `require_api_key` 闸完成——原生 EventSource 无法设自定义
//! 头,故用 URL query `?api_key=`(或历史 `?token=`)携带 key,闸的 [`query_param_key`] 已支持。
//!
//! 前端契约(`admin-ui/src/hooks/use-log-stream.ts`):
//! - `GET /api/admin/logs/stream`:SSE。首个具名事件 `history`(data = LogEntry JSON 数组),
//!   其后每条新记录一个具名事件 `log`(data = 单个 LogEntry JSON);带周期心跳防代理断流。
//! - `GET /api/admin/logs/snapshot`:JSON,返回当前历史环形缓冲的 LogEntry 数组。
//! - `GET /api/admin/logs/download`:`text/plain` 附件,每行 `timestamp [LEVEL] target message`。
//!
//! [`query_param_key`]: crate::server::auth::query_param_key

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use tokio::sync::broadcast::error::RecvError;

use crate::logcap::LogEntry;
use crate::protocol::anthropic::handler::MessagesState;

/// SSE 心跳间隔(秒):周期性发送保活注释,避免反向代理因空闲把长连接掐断。
const KEEPALIVE_SECS: u64 = 15;

/// 日志捕获未启用时的统一 503。
fn capture_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "log capture disabled (set log_capacity > 0)",
    )
        .into_response()
}

/// `GET /api/admin/logs/stream?api_key=<key>`
///
/// SSE:先发一个 `history` 事件(当前历史环形缓冲的完整快照,JSON 数组),再持续把广播来的
/// 每条新记录作为 `log` 事件发出。为避免"订阅建立"与"读历史"之间产生的记录两头漏接,
/// **先 `subscribe()` 再 `snapshot()`**:订阅在前,则这段窗口内的新记录必进广播接收端;
/// 它们也可能已在快照里,前端按需去重(实际 UI 直接以 history 覆盖初值,随后追加 log)。
///
/// 广播接收端落后(`Lagged`)时跳过丢失批次继续跟进,不断流;发送端关闭则自然结束。
pub async fn stream_logs(State(state): State<MessagesState>) -> Response {
    let Some(capture) = state.log_capture.clone() else {
        return capture_unavailable();
    };

    // 订阅在前、快照在后(见函数文档的漏接说明)。
    let mut rx = capture.subscribe();
    let history = capture.snapshot();
    let history_json = serde_json::to_string(&history).unwrap_or_else(|_| "[]".to_string());

    let body = async_stream::stream! {
        // 1) 历史事件:一次性回放当前缓冲。
        yield Ok::<Event, Infallible>(Event::default().event("history").data(history_json));

        // 2) 实时事件:逐条转发广播来的新记录。
        loop {
            match rx.recv().await {
                Ok(entry) => {
                    match serde_json::to_string(&entry) {
                        Ok(json) => yield Ok(Event::default().event("log").data(json)),
                        Err(_) => continue,
                    }
                }
                // 本连接消费过慢,广播丢弃了 n 条:跳过、继续跟最新,不中断流。
                Err(RecvError::Lagged(_)) => continue,
                // 发送端已释放(进程收尾):正常结束流。
                Err(RecvError::Closed) => break,
            }
        }
    };

    let mut response = Sse::new(body)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(KEEPALIVE_SECS))
                .text("keep-alive"),
        )
        .into_response();

    // 显式关闭反向代理对 SSE body 的缓冲(Nginx X-Accel-Buffering)。
    let headers = response.headers_mut();
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
}

/// `GET /api/admin/logs/snapshot?api_key=<key>`
///
/// 返回当前历史环形缓冲的 LogEntry 数组(JSON)。前端在 SSE `onopen` 后另发一次 REST 拉取,
/// 规避反向代理对 SSE body 的缓冲导致 history 事件迟迟不达。
pub async fn snapshot_logs(State(state): State<MessagesState>) -> Response {
    let Some(capture) = state.log_capture.clone() else {
        return capture_unavailable();
    };
    let snapshot: Vec<LogEntry> = capture.snapshot();
    let json = serde_json::to_string(&snapshot).unwrap_or_else(|_| "[]".to_string());

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    (headers, json).into_response()
}

/// 把一批记录渲染为可下载文本:每行 `timestamp [LEVEL] target message`。
fn render_plaintext(entries: &[LogEntry]) -> String {
    let mut out = String::with_capacity(entries.len() * 96);
    for e in entries {
        out.push_str(&e.timestamp);
        out.push_str(" [");
        out.push_str(&e.level);
        out.push_str("] ");
        out.push_str(&e.target);
        out.push(' ');
        out.push_str(&e.message);
        out.push('\n');
    }
    out
}

/// `GET /api/admin/logs/download?api_key=<key>`
///
/// 把当前历史缓冲渲染为 `.txt` 附件下载,文件名带 UTC 时间戳。
pub async fn download_logs(State(state): State<MessagesState>) -> Response {
    let Some(capture) = state.log_capture.clone() else {
        return capture_unavailable();
    };
    let snapshot = capture.snapshot();
    let content = render_plaintext(&snapshot);

    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let disposition = format!("attachment; filename=\"kiro2api-logs-{stamp}.txt\"");

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition)
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    (headers, content).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::pool::{LbMode, Pool};
    use crate::logcap::LogCapture;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    fn entry(msg: &str) -> LogEntry {
        LogEntry {
            timestamp: "2026-07-20T14:00:00.000Z".into(),
            level: "INFO".into(),
            target: "kiro2api::test".into(),
            message: msg.into(),
        }
    }

    /// 造一个最小可用的 MessagesState,仅设置 `log_capture`(其余字段取空/默认,
    /// 日志 handler 不触碰它们)。
    fn state_with_log_capture(capture: Option<Arc<LogCapture>>) -> MessagesState {
        let tag = format!("kiro2api_loghandler_{}.json", std::process::id());
        MessagesState {
            pool: Arc::new(Mutex::new(Pool::new(vec![], LbMode::Priority))),
            client: reqwest::Client::new(),
            control_client: reqwest::Client::new(),
            cfg: Arc::new(crate::config::Config::default()),
            runtime_cfg: crate::config::shared_runtime_config(&crate::config::Config::default()),
            endpoint_override: None,
            stats: crate::stats::StatsManager::load_from_dir(&std::env::temp_dir()),
            api_keys: crate::apikey::ApiKeyStore::load(std::env::temp_dir().join(tag)),
            balance: crate::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
            models_cache: crate::models_cache::ModelsCache::new(),
            builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            log_capture: capture,
            refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(
                std::env::temp_dir()
                    .join(format!(
                        "kiro2api_refreshctx_src_admin_log_handler_rs_{}.json",
                        std::process::id()
                    ))
                    .to_string_lossy()
                    .to_string(),
            ),
        }
    }

    /// 造一个仅装了三个日志路由 + 给定 state 的最小 Router(不叠鉴权闸,专测 handler 行为)。
    fn logs_router(state: MessagesState) -> Router {
        Router::new()
            .route("/logs/stream", get(stream_logs))
            .route("/logs/snapshot", get(snapshot_logs))
            .route("/logs/download", get(download_logs))
            .with_state(state)
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[test]
    fn render_plaintext_shape() {
        let text = render_plaintext(&[entry("hello"), entry("world")]);
        assert_eq!(
            text,
            "2026-07-20T14:00:00.000Z [INFO] kiro2api::test hello\n\
             2026-07-20T14:00:00.000Z [INFO] kiro2api::test world\n"
        );
    }

    #[tokio::test]
    async fn snapshot_returns_json_array_of_entries() {
        let capture = Arc::new(LogCapture::new(8));
        capture.push_entry(entry("one"));
        capture.push_entry(entry("two"));
        let state = state_with_log_capture(Some(capture));

        let resp = logs_router(state)
            .oneshot(
                Request::builder()
                    .uri("/logs/snapshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json; charset=utf-8"
        );
        let parsed: Vec<LogEntry> = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].message, "one");
        assert_eq!(parsed[1].message, "two");
    }

    #[tokio::test]
    async fn snapshot_503_when_capture_disabled() {
        let state = state_with_log_capture(None);
        let resp = logs_router(state)
            .oneshot(
                Request::builder()
                    .uri("/logs/snapshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn download_sets_attachment_headers_and_body() {
        let capture = Arc::new(LogCapture::new(8));
        capture.push_entry(entry("dl"));
        let state = state_with_log_capture(Some(capture));

        let resp = logs_router(state)
            .oneshot(
                Request::builder()
                    .uri("/logs/download")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
        let cd = resp
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(cd.starts_with("attachment; filename=\"kiro2api-logs-"));
        assert!(cd.ends_with(".txt\""));
        let text = body_string(resp).await;
        assert_eq!(text, "2026-07-20T14:00:00.000Z [INFO] kiro2api::test dl\n");
    }

    #[tokio::test]
    async fn download_503_when_capture_disabled() {
        let state = state_with_log_capture(None);
        let resp = logs_router(state)
            .oneshot(
                Request::builder()
                    .uri("/logs/download")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn stream_503_when_capture_disabled() {
        let state = state_with_log_capture(None);
        let resp = logs_router(state)
            .oneshot(
                Request::builder()
                    .uri("/logs/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// 流式端点建立成功:200 + SSE content-type。history/log 事件的数据面由 logcap 的
    /// 广播/环形缓冲单元测试覆盖(见 crate::logcap::tests),此处只验证 HTTP 响应契约。
    #[tokio::test]
    async fn stream_returns_200_and_sse_content_type() {
        let capture = Arc::new(LogCapture::new(8));
        capture.push_entry(entry("hist-1"));
        let state = state_with_log_capture(Some(capture));

        let resp = logs_router(state)
            .oneshot(
                Request::builder()
                    .uri("/logs/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/event-stream"), "content-type = {ct}");
        // SSE body 不能用 to_bytes 全量读(无限流),仅验证响应头契约即可。
    }

    /// 端到端读取 SSE body:history 事件先到(含历史),随后推入的记录作为 log 事件到达。
    /// 用 axum::body::to_bytes 带上限读取——history + 一条 live 后 body 仍开着,靠 timeout 收尾。
    #[tokio::test]
    async fn stream_emits_history_then_live_log_events() {
        use tokio::time::{Duration as TD, timeout};

        let capture = Arc::new(LogCapture::new(8));
        capture.push_entry(entry("hist-1"));
        let state = state_with_log_capture(Some(capture.clone()));

        let resp = logs_router(state)
            .oneshot(
                Request::builder()
                    .uri("/logs/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let mut body = resp.into_body();

        // 逐帧累积 SSE data,直到出现 history+hist-1 与 log+live-1,或超时。
        async fn next_data_chunk(body: &mut Body) -> Option<String> {
            use axum::body::HttpBody;
            use std::pin::Pin;
            use std::task::{Context, Poll};
            std::future::poll_fn(
                |cx: &mut Context<'_>| match Pin::new(&mut *body).poll_frame(cx) {
                    Poll::Ready(Some(Ok(frame))) => {
                        if let Ok(data) = frame.into_data() {
                            Poll::Ready(Some(String::from_utf8_lossy(&data).to_string()))
                        } else {
                            Poll::Ready(Some(String::new()))
                        }
                    }
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => Poll::Ready(None),
                    Poll::Pending => Poll::Pending,
                },
            )
            .await
        }

        let mut acc = String::new();
        // 读 history。
        let chunk = timeout(TD::from_secs(2), next_data_chunk(&mut body))
            .await
            .expect("history frame timed out");
        acc.push_str(&chunk.unwrap_or_default());
        assert!(acc.contains("event: history"), "acc so far: {acc}");
        assert!(acc.contains("hist-1"), "acc so far: {acc}");

        // 推一条实时记录,应作为 `log` 事件到达。
        capture.push_entry(entry("live-1"));
        for _ in 0..8 {
            if acc.contains("event: log") && acc.contains("live-1") {
                break;
            }
            match timeout(TD::from_secs(2), next_data_chunk(&mut body)).await {
                Ok(Some(c)) => acc.push_str(&c),
                _ => break,
            }
        }
        assert!(acc.contains("event: log"), "acc: {acc}");
        assert!(acc.contains("live-1"), "acc: {acc}");
    }
}
