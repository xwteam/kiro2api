pub mod auth;

use crate::config::Config;
use crate::kiro::credential;
use crate::kiro::pool::{LbMode, Pool};
use crate::protocol::anthropic::handler::{MessagesState, messages_router};
use crate::webui;
use axum::{Json, Router, routing::get};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::sync::Mutex;

/// 进程启动时刻,首次 `build_router*` 时捕获一次(`OnceLock` 幂等)。
/// admin `system-info` 端点据此算 `uptimeSecs`(已运行秒数)。
/// 用 `Instant`(单调钟)避免系统时钟回拨影响运行时长。
static SERVER_START: OnceLock<Instant> = OnceLock::new();

/// 进程已运行秒数;若从未初始化(理论上不会,build_router 必先跑)回落 0。
pub fn server_uptime_secs() -> u64 {
    SERVER_START
        .get()
        .map(|start| start.elapsed().as_secs())
        .unwrap_or(0)
}

// server::auth 中的鉴权原语(verify/extract_key)与 require_api_key 中间件
// 统一接入下方三条协议路由(messages/openai/gemini);/health、/v1/ping 与
// /admin、/user 挂载的公开 UI 静态资源保持开放,不受鉴权闸影响。
pub fn build_router(cfg: Arc<Config>) -> Router {
    build_router_with_logs(cfg, None)
}

/// 与 [`build_router`] 相同,但注入实时日志捕获器句柄(Phase 6)。
/// `log_capture` 为 `Some` 时,admin 日志端点(stream/snapshot/download)可读历史 + 订阅广播;
/// `None` 时这些端点返回 503。既有测试仍走 [`build_router`](=None),行为不变。
pub fn build_router_with_logs(
    cfg: Arc<Config>,
    log_capture: Option<Arc<crate::logcap::LogCapture>>,
) -> Router {
    // 捕获进程启动时刻(幂等):admin system-info 端点据此算 uptimeSecs。
    // 首次调用置入,后续调用(如测试重复建 router)不覆盖,保留最早时刻。
    let _ = SERVER_START.get_or_init(Instant::now);
    // 从配置路径加载凭据;文件不存在/解析失败均回落空池,
    // /v1/messages 遇空池返回 503(不影响 health/webui 路由,默认配置测试保持全绿)。
    let creds = credential::load(&cfg.credentials_path).unwrap_or_default();
    let mut pool_inner = Pool::new(creds, LbMode::Priority);
    // RPM 限流与负载均衡模式均来自配置(默认无限 RPM/Priority,保持既有行为)。
    pool_inner.set_max_rpm(cfg.max_rpm_per_credential);
    if cfg.load_balancing_mode.eq_ignore_ascii_case("balanced") {
        pool_inner.set_mode(LbMode::Balanced);
    }
    let pool = Arc::new(Mutex::new(pool_inner));
    // 统计持久化层:数据目录由 credentials_path 的父目录推断。
    // relay 数据面记录用量/失败/限流,admin 只读查询,二者共用同一 Arc<StatsManager>。
    let stats = crate::stats::StatsManager::load_from_credentials_path(&cfg.credentials_path);
    // API-KEY 存储:数据目录同 stats,由 credentials_path 父目录推断 api_keys.json。
    // auth 闸 / relay 归属 / admin / user 共用同一 Arc<ApiKeyStore>(后续阶段接入消费方)。
    let api_keys =
        crate::apikey::ApiKeyStore::load(crate::apikey::api_keys_path_from(&cfg.credentials_path));
    // 余额缓存(Phase 4):与 stats 同数据目录,载入 kiro_balance_cache.json 并启动去抖刷盘。
    // admin 余额端点读缓存(5 分钟 TTL)/回填;relay 数据面不消费。
    let balance = crate::balance::BalanceCache::load_from_credentials_path(&cfg.credentials_path);
    // 运行期可变配置:仅承载可改写字段(auth key / RPM / 负载均衡模式),与不可变 cfg 分离。
    // auth 闸(协议 + admin)据此实时读期望 key(轮换即时生效);admin 设置端点写入并原子落盘。
    let runtime_cfg = crate::config::shared_runtime_config(&cfg);
    let messages_state = MessagesState {
        pool,
        // 共享出站客户端:此 state.client 既服务中转数据面(relay/provider::call 的
        // SSE 长流)又服务控制面(登录/余额/模型/刷新的一问一答)。因数据面需容忍长流,
        // 不能加整请求超时(会误杀),故统一取流式客户端——connect_timeout 界定建连、
        // read_timeout 界定读取停顿(每读一段即重置),两面消费者都得到有界超时护栏,
        // 修复上游卡死时连接无限期挂起(HTTP TIMEOUT 发现 #5)。
        client: crate::http::streaming(),
        // 控制面(一问一答)客户端:令牌刷新 / 余额 / 模型清单等短小请求经此发出。
        // 由 http::unary() 构造——connect_timeout + 整请求 timeout 硬顶,使控制面上游
        // 卡死时在上限内失败,不会无限期挂起(HTTP TIMEOUT 发现 #6)。与数据面
        // client(流式、无整请求超时)分离,因数据面需容忍 SSE 长流、加硬顶会误杀。
        control_client: crate::http::unary(),
        cfg: cfg.clone(),
        runtime_cfg: runtime_cfg.clone(),
        endpoint_override: None, // 生产走端点回退
        stats: stats.clone(),
        api_keys: api_keys.clone(),
        balance: balance.clone(),
        // 动态模型清单缓存(FIX 1):进程内新建空缓存,admin /models 按需惰性回填/并集展示。
        models_cache: crate::models_cache::ModelsCache::new(),
        // Phase 3 交互式登录会话(内存中转态,~600s TTL)。admin 登录端点独占,
        // 每次 build_router 起一份新空存储(会话短命、无需持久化)。
        builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
        iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
        // Phase 6 实时日志:注入捕获器句柄(main 据 log_capacity 建好后传入),
        // admin 日志端点据此读历史 + 订阅广播;None 时端点 503。
        log_capture,
        // 令牌刷新上下文:per-credential 单飞锁 + credentials.json 路径 + 落盘锁。
        // 三条协议路由 + admin balance/models 共用同一份,使并发刷新同一凭据单飞、
        // 刷新后原子落盘,修复级联 401(Bug A)与重启后旧 token 401(Bug B)。
        refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(cfg.credentials_path.clone()),
    };

    // 三条协议路由(/v1/messages、OpenAI 兼容、Gemini 兼容)共用同一 MessagesState,
    // 先 merge 成一组再统一 .layer() 鉴权闸——axum 的 .layer() 只作用于其之前
    // 已注册到该 Router 的路由,故必须在合并进顶层 Router 之前单独 layer,
    // 否则 health/ping/webui 也会被一并挡住。
    let protocol = messages_router(messages_state.clone())
        // OpenAI 兼容路由(/v1/chat/completions、/v1/models 等)复用同一 MessagesState,
        // 内部通过 relay_core 走同一条中转内核。
        .merge(crate::protocol::openai::openai_router(
            messages_state.clone(),
        ))
        // Gemini 兼容路由(/v1beta/models/{model}:generateContent 等)同样复用 MessagesState,
        // 走同一条 relay_core 中转内核;与 OpenAI 的 /v1/... 路径无重叠。
        .merge(crate::protocol::gemini::gemini_router(
            messages_state.clone(),
        ))
        // OpenAI Responses 兼容路由(/v1/responses 等)同样复用 MessagesState,
        // 走同一条 relay_core 中转内核;与既有路径无重叠。
        .merge(crate::protocol::responses::handler::responses_router(
            messages_state.clone(),
        ))
        .layer(axum::middleware::from_fn_with_state(
            // 协议闸:全局 key 或有效 store key 二选一;store key 命中时把解析出的
            // api-key id 塞进请求扩展供 relay 归属,并按消费上限(查 stats)裁决。
            // 期望全局 key 从 runtime_cfg 实时读取,使 PUT /config/auth-keys 轮换即时生效。
            auth::AuthState::protocol(runtime_cfg.clone(), api_keys.clone(), stats.clone()),
            auth::require_api_key,
        ));

    // 用户面 REST API 单独一组:**不叠加任何鉴权 .layer()**——这些端点由调用方随
    // 请求携带的 API-KEY 自鉴权(handler 内经 Arc<ApiKeyStore> 校验),故与 admin/协议
    // 两组的鉴权闸互不相干。复用同一 MessagesState(共享 api_keys + stats)。
    let user = crate::user::user_api_router(messages_state.clone());

    // Admin REST API 单独一组:鉴权用 admin_api_key,未配置则回退主 api_key
    // (都空则开放,复用 require_api_key 既有语义)。与协议组各自独立 layer,
    // 互不影响——协议组用 api_key,admin 组用 admin_api_key 优先。
    // Admin 复用 messages_state:其已携带同一个 `Arc<StatsManager>`(上方 line 28-34 构造),
    // 故 admin 只读查询与 relay 记录写入共享同一内存存储,无需再建第二份。
    let admin =
        crate::admin::admin_api_router(messages_state).layer(axum::middleware::from_fn_with_state(
            // admin 闸:admin_api_key(非空)否则回退主 api_key 的全局 key 校验,
            // 不接 store、不查消费上限。期望 key 从 runtime_cfg 实时读取,
            // 使 PUT /config/auth-keys 改 admin 密码后无需重启即时生效。
            auth::AuthState::admin(runtime_cfg.clone()),
            auth::require_api_key,
        ));

    Router::new()
        .route("/health", get(health))
        .route("/v1/ping", get(ping))
        .with_state(cfg)
        // 不用 .nest():axum 0.8 的 nest 对挂载路径末尾斜杠敏感,
        // "/admin" 与 "/admin/" 只能二选一匹配,会导致另一种写法 404。
        // 改为 .merge() 绝对路径路由(webui::admin_router()/user_router()
        // 内部各自注册裸路径、带斜杠、通配子路径三条),使 /admin、/admin/、
        // /admin/{*file} 都能落到 UI 处理器。
        .merge(webui::admin_router())
        .merge(webui::user_router())
        .merge(protocol)
        .merge(admin)
        .merge(user)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "kiro2api",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn ping() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "pong": true }))
}

pub async fn serve(
    cfg: Config,
    log_capture: Option<Arc<crate::logcap::LogCapture>>,
) -> anyhow::Result<()> {
    if cfg.api_key.as_deref().unwrap_or("").is_empty() {
        tracing::warn!("未设置 api_key,协议端点开放访问");
    }
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let app = build_router_with_logs(Arc::new(cfg), log_capture);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("kiro2api listening on {addr}");
    // `into_make_service_with_connect_info` 使各 handler 可经 `ConnectInfo<SocketAddr>`
    // 拿到 socket 对端地址(用于提取客户端 IP;有反代时优先 X-Forwarded-For/X-Real-IP)。
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_ok_json() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["service"], "kiro2api");
    }

    #[tokio::test]
    async fn admin_index_served() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("kiro2api") && html.contains("js/app.js"));
    }

    #[tokio::test]
    async fn admin_index_served_without_trailing_slash() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("kiro2api") && html.contains("js/app.js"));
    }

    #[tokio::test]
    async fn user_index_served_without_trailing_slash() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(Request::builder().uri("/user").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("kiro2api") && html.contains("/user/assets/"));
    }

    #[tokio::test]
    async fn user_index_served_with_trailing_slash() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/user/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("kiro2api") && html.contains("/user/assets/"));
    }

    /// /v1/messages 已挂载:默认配置(凭据文件不存在 → 空池)下返回 503,
    /// 证明路由接线成功且默认配置服务器测试不受影响。
    #[tokio::test]
    async fn messages_route_mounted_returns_503_on_empty_pool() {
        let app = build_router(Arc::new(Config::default()));
        let body = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    const MESSAGES_BODY: &str = r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#;

    fn messages_request(uri: &str, auth_header: Option<(&str, &str)>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some((name, value)) = auth_header {
            builder = builder.header(name, value);
        }
        builder.body(Body::from(MESSAGES_BODY)).unwrap()
    }

    /// 鉴权闸开启(api_key = Some("secret"))时,/v1/messages 无 Authorization → 401。
    #[tokio::test]
    async fn protected_messages_without_auth_returns_401() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request("/v1/messages", None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 鉴权闸开启时,错误的 Bearer key → 401。
    #[tokio::test]
    async fn protected_messages_with_wrong_bearer_returns_401() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request(
                "/v1/messages",
                Some(("authorization", "Bearer wrong")),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 鉴权闸开启时,正确的 Bearer key → 通过鉴权(空池 503,但不是 401)。
    #[tokio::test]
    async fn protected_messages_with_correct_bearer_passes_auth() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request(
                "/v1/messages",
                Some(("authorization", "Bearer secret")),
            ))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// 鉴权闸开启时,正确的 x-api-key → 通过鉴权。
    #[tokio::test]
    async fn protected_messages_with_correct_x_api_key_passes_auth() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request(
                "/v1/messages",
                Some(("x-api-key", "secret")),
            ))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 鉴权闸开启时,正确的 ?token= query 参数 → 通过鉴权。
    #[tokio::test]
    async fn protected_messages_with_correct_query_token_passes_auth() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(messages_request("/v1/messages?token=secret", None))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 鉴权闸开启时,/health 仍然公开,不受影响。
    #[tokio::test]
    async fn health_stays_open_when_api_key_set() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// api_key 未配置(None)时为开放模式:/v1/messages 无 auth 头 → 非 401(空池 503)。
    #[tokio::test]
    async fn open_mode_without_api_key_allows_unauthenticated_messages() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(messages_request("/v1/messages", None))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// admin_api_key 设置时,/admin/api/stats 无 key → 401。
    #[tokio::test]
    async fn admin_stats_without_key_returns_401_when_admin_key_set() {
        let cfg = Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// admin_api_key 设置时,正确的 Bearer key → 通过鉴权(非 401)。
    #[tokio::test]
    async fn admin_stats_with_correct_admin_key_passes_auth() {
        let cfg = Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .header("authorization", "Bearer adm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// admin_api_key 未设置但 api_key 设置时,admin 鉴权回退到 api_key。
    #[tokio::test]
    async fn admin_auth_falls_back_to_api_key_when_admin_key_unset() {
        let cfg = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app = build_router(Arc::new(cfg));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let cfg2 = Config {
            api_key: Some("secret".into()),
            ..Config::default()
        };
        let app2 = build_router(Arc::new(cfg2));
        let resp2 = app2
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp2.status(), StatusCode::UNAUTHORIZED);
    }

    /// admin_api_key 与 api_key 均未设置时,admin API 开放访问。
    #[tokio::test]
    async fn admin_stats_open_when_no_keys_configured() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// Phase 6:日志流路由挂在 admin 组下,`?api_key=<admin key>` 经 admin 闸放行,
    /// 且日志捕获已接线时返回 200 + SSE content-type(EventSource 无法设头,只能 query 携带 key)。
    #[tokio::test]
    async fn log_stream_authenticates_via_query_api_key_and_streams() {
        let cfg = Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        };
        let capture = Arc::new(crate::logcap::LogCapture::new(16));
        let app = build_router_with_logs(Arc::new(cfg), Some(capture));

        // 无 key → 401(admin 闸拦截)。
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/logs/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 错误 key → 401。
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/logs/stream?api_key=wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 正确 key via ?api_key= → 200 + SSE。
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/logs/stream?api_key=adm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/event-stream"), "content-type = {ct}");
    }

    /// 日志捕获未接线(build_router 默认 None)时,日志端点在通过鉴权后返回 503。
    #[tokio::test]
    async fn log_snapshot_503_when_capture_not_wired() {
        let app = build_router(Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/logs/snapshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
