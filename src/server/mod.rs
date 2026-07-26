pub mod auth;

use crate::config::Config;
use crate::kiro::credential;
use crate::kiro::pool::{LbMode, Pool};
use crate::protocol::anthropic::handler::{MessagesState, messages_router};
use crate::webui;
use axum::{Json, Router, routing::get};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
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
    build_router_with_handles(cfg, log_capture).0
}

/// 与 [`build_router_with_logs`] 完全相同的路由,但额外交出内部构造的 `StatsManager` 句柄。
/// `serve()` 据此在收到停机信号后触发统计最终刷盘——统计层由 `build_router*` 内部构造,
/// 外部拿不到句柄就无法在退出前落盘。
pub fn build_router_with_handles(
    cfg: Arc<Config>,
    log_capture: Option<Arc<crate::logcap::LogCapture>>,
) -> (Router, Arc<crate::stats::StatsManager>) {
    // 捕获进程启动时刻(幂等):admin system-info 端点据此算 uptimeSecs。
    // 首次调用置入,后续调用(如测试重复建 router)不覆盖,保留最早时刻。
    let _ = SERVER_START.get_or_init(Instant::now);
    // 从配置路径加载凭据;文件不存在/解析失败均回落空池,
    // /v1/messages 遇空池返回 503(不影响 health/webui 路由,默认配置测试保持全绿)。
    let creds = credential::load(&cfg.credentials_path).unwrap_or_default();
    // 账号 id 高水位:读上次落盘值,使"已删除账号的编号"重启后也不被复用。
    // 旧安装没有该旁挂文件 → `None` → 传 0,由 `with_next_id` 平滑退化为 `max(现有 id)+1`
    // 的原语义。不接这一步的话高水位只写不读,重启即退回复用编号(旧令牌覆盖新账号凭据)。
    let persisted_next_id = credential::load_next_id(&cfg.credentials_path).unwrap_or(0);
    let mut pool_inner = Pool::with_next_id(creds, LbMode::Priority, persisted_next_id);
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

    // Admin REST API 单独一组:鉴权用 admin_api_key,未配置则回退主 api_key。
    // 与协议组各自独立 layer,互不影响——协议组用 api_key,admin 组用 admin_api_key 优先。
    // Admin 复用 messages_state:其已携带同一个 `Arc<StatsManager>`(上方 line 28-34 构造),
    // 故 admin 只读查询与 relay 记录写入共享同一内存存储,无需再建第二份。
    let admin = crate::admin::admin_api_router(messages_state)
        .layer(axum::middleware::from_fn_with_state(
            // admin 闸:admin_api_key(非空)否则回退主 api_key 的全局 key 校验,不查消费上限。
            // 期望 key 从 runtime_cfg 实时读取,使 PUT /config/auth-keys 改 admin 密码后
            // 无需重启即时生效。store 一并传入但 admin 闸**不据其判定"是否已配置鉴权"**:
            // store key 是用户级凭据、在 admin 闸永不放行,若让它把闸判成已配置,首次部署
            // 建出第一把 key 就会自锁(能开锁的管理员凭据一把都没有)。故 admin 闸只认
            // 管理员级凭据(admin_api_key,缺省回退 api_key)。
            auth::AuthState::admin(runtime_cfg.clone(), api_keys.clone()),
            auth::require_api_key,
        ))
        // 批量导入等 admin 端点可能提交大 JSON(多账号凭据集,单个 token 就上千字符)。
        // 把请求体上限从 axum 默认 2MB 提到 32MB,避免大导入被 413 拦下(体现为前端"发生错误");
        // admin 已鉴权,放宽仅限受信管理面。
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024));

    let router = Router::new()
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
        .merge(user);
    (router, stats)
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

/// 启动期鉴权配置体检:返回需要提示的告警文案(空 = 无隐患)。
///
/// 抽成纯函数以便单测钉住各场景的覆盖面;`serve()` 只负责逐条 warn。
/// 判据只看两个全局 key。注意两道闸的口径不同:**协议闸**认 store key(建了 API-KEY 即收口,
/// 故协议那条文案带"未创建任何 API-KEY 前"限定);**admin 闸**只认管理员级凭据,建 API-KEY
/// 不会收口,故管理面那条**不得**带该限定,否则低估暴露面。
fn auth_startup_warnings(cfg: &Config) -> Vec<String> {
    let api_key_set = !cfg.api_key.as_deref().unwrap_or("").trim().is_empty();
    let admin_key_set = !cfg.admin_api_key.as_deref().unwrap_or("").trim().is_empty();
    let mut warnings = Vec::new();
    if !api_key_set {
        warnings.push(
            "未设置 api_key:在未创建任何 API-KEY 前,四条协议端点(Anthropic/OpenAI/Responses/Gemini)开放访问"
                .to_string(),
        );
    }
    if !admin_key_set {
        if api_key_set {
            // 只有主 key:管理闸回退用它,数据面 key 持有者即管理员。
            warnings.push(
                "未设置 admin_api_key:/admin 管理面回退用主 api_key 校验,数据面 key 持有者同时具备完整管理员权限;建议设置环境变量 ADMIN_API_KEY=<独立强口令> 与数据面分离"
                    .to_string(),
            );
        } else {
            // 两个 key 都没有:管理面完全开放——可读写账号凭据、密钥与统计,危害远大于协议端点。
            // 措辞不能带"在未创建任何 API-KEY 前"这类限定:admin 闸只认管理员级凭据,建 API-KEY
            // **不会**自动收口(那样会把首次部署的运营者锁在门外,见 admin_stats_open_when_only_store_keys_exist)。
            warnings.push(
                "未设置 admin_api_key/api_key:/admin 管理面当前完全开放(任何人可读写账号凭据、API-KEY 与统计),创建 API-KEY 也不会自动收口;请在管理面设置管理员密钥,或设置环境变量 ADMIN_API_KEY=<强口令> 后重启"
                    .to_string(),
            );
        }
    }
    warnings
}

/// 停机信号:SIGTERM(`docker stop`/编排器回收)或 SIGINT(Ctrl-C)任一到达即返回。
/// 非 unix 平台只等 Ctrl-C。
///
/// 容器里本进程常是 PID 1:不处理 SIGTERM 就会一直等到宽限期结束被 SIGKILL,
/// 在途 SSE 长流被硬掐、未刷盘的统计丢失。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                let _ = sig.recv().await;
            }
            Err(e) => {
                // 注册失败(极罕见)退化为只响应 Ctrl-C,不让停机路径 panic。
                tracing::warn!(error = %e, "SIGTERM 处理器注册失败,停机仅响应 Ctrl-C");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("收到停机信号,停止接受新连接,等待在途请求结束");
}

/// 停机排空窗口:收到 SIGTERM/SIGINT 后最多再给在途请求这么久收尾,到点无论是否
/// 还有连接挂着都继续走最终刷盘并退出。
///
/// 为什么必须有上界:admin 实时日志 SSE(`/api/admin/logs/stream`)是**无限流**——它只在
/// 广播发送端释放时才结束,而发送端由路由 state 持有,服务器活着就不会释放。axum 的
/// `with_graceful_shutdown` 要等所有在途响应结束才返回,故只要有一个管理页开着日志流,
/// 停机就会永远卡住,最后被容器宽限期结束时的 SIGKILL 硬掐——在途中转流照样被切断,
/// 统计最终刷盘反而彻底跳过,和不做优雅停机相比只坏不好。
///
/// 取 8s 的理由:必须**短于**容器宽限期(docker stop 默认 10s、本仓 compose 未改;
/// 编排器一般 30s),给窗口到期后的刷盘 + 收尾留出余量;同时长于绝大多数在途一问一答
/// 请求(控制面有整请求超时护栏),正常停机仍能干净排空。
const SHUTDOWN_DRAIN_WINDOW: Duration = Duration::from_secs(8);

/// 等服务器结束,但只给"停机信号已到"之后的排空阶段有界的等待时长。
///
/// - `server`:`axum::serve(..).with_graceful_shutdown(..)` 的 future。
/// - `shutdown_started`:停机信号已触发的通知(由信号 future 顺手发出)。
/// - 返回 `Some(结果)` = 服务器自行结束(干净排空);`None` = 窗口到期仍有连接未结束,
///   调用方应放弃等待、直接收尾。
///
/// 抽成泛型函数是为了能用假 future 单测边界行为,不必真开监听 socket。
/// 取 `IntoFuture` 而非 `Future`:axum 的 `WithGracefulShutdown` 只实现前者(普通 future
/// 经空实现自动满足),这样调用点直接传 `axum::serve(..)..` 的返回值即可。
/// `biased`:优先轮询服务器,使"信号与结束同时就绪"时确定性地报干净结束。
async fn drain_with_deadline<F, S>(
    server: F,
    shutdown_started: S,
    window: Duration,
) -> Option<F::Output>
where
    F: std::future::IntoFuture,
    S: std::future::Future<Output = ()>,
{
    let server = server.into_future();
    tokio::pin!(server, shutdown_started);
    tokio::select! {
        biased;
        // 服务器先结束:要么在途请求已全部跑完,要么 accept 出错提前返回。
        out = &mut server => return Some(out),
        // 停机信号到达:进入下面的有界排空窗口。
        _ = &mut shutdown_started => {}
    }
    tokio::time::timeout(window, &mut server).await.ok()
}

pub async fn serve(
    cfg: Config,
    log_capture: Option<Arc<crate::logcap::LogCapture>>,
) -> anyhow::Result<()> {
    for w in auth_startup_warnings(&cfg) {
        tracing::warn!("{w}");
    }
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let (app, stats) = build_router_with_handles(Arc::new(cfg), log_capture);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("kiro2api listening on {addr}");
    // `into_make_service_with_connect_info` 使各 handler 可经 `ConnectInfo<SocketAddr>`
    // 拿到 socket 对端地址(用于提取客户端 IP;有反代时优先 X-Forwarded-For/X-Real-IP)。
    //
    // `with_graceful_shutdown`:收到 SIGTERM/SIGINT 后停止 accept,已建立的连接(含在途
    // SSE 长流)跑完才返回,而不是被 SIGKILL 硬掐。
    //
    // 但这份等待必须封顶(见 SHUTDOWN_DRAIN_WINDOW):admin 日志 SSE 是无限流,单靠
    // graceful shutdown 会永远等下去。故信号 future 在放行 axum 的同时经 oneshot 通知
    // 本函数开始计时,由 drain_with_deadline 给排空阶段设上界。
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        // 接收端在 serve 结束前一直活着;即便已被丢弃也无需处理(那说明服务器已自行结束)。
        let _ = drain_tx.send(());
    });

    let outcome = drain_with_deadline(
        server,
        async {
            let _ = drain_rx.await;
        },
        SHUTDOWN_DRAIN_WINDOW,
    )
    .await;
    if outcome.is_none() {
        tracing::warn!(
            drain_window_secs = SHUTDOWN_DRAIN_WINDOW.as_secs(),
            "排空窗口到期仍有连接未结束(常见于 admin 实时日志 SSE 这类长连接),不再等待,直接刷盘退出"
        );
    }
    // 无论是干净排空还是窗口到期,最终刷盘都必须跑且只跑一次:把去抖窗口内(约 5s)
    // 未落盘的用量写下去,否则每次停机都静默丢这一段。
    stats.flush_now().await;
    tracing::info!("kiro2api 已停止");
    // 服务器自身的错误(accept 失败等,极罕见)放到刷盘之后再上抛,
    // 避免用 `?` 提前返回把统计吞掉。
    if let Some(res) = outcome {
        res?;
    }
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

    /// 仅用 store key 部署(config 里 api_key / admin_api_key 都没配)时,/admin API **仍开放**。
    ///
    /// 这是防"首次部署自锁"的回归测试,与直觉相反故写明:store key 是**用户级**凭据,在 admin
    /// 闸永远不放行。若让它把 admin 闸判成"已配置鉴权",首次部署的运营者一在开放面板里建出第一把
    /// API-KEY,下一次 /api/admin/* 就 401——而能开锁的管理员凭据一把都不存在,产品内再无自救途径。
    /// 故 admin 闸的"是否已配置"只认管理员级凭据(admin_api_key,缺省回退 api_key);运营者据此
    /// 仍能在开放面板里调 PUT /config/auth-keys 设好管理员密钥,当场收口(见 auth.rs 同名回归测试)。
    #[tokio::test]
    async fn admin_stats_open_when_only_store_keys_exist() {
        let dir =
            std::env::temp_dir().join(format!("kiro2api_srv_adminguard_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let creds = dir.join("credentials.json").to_string_lossy().into_owned();
        let store_path = crate::apikey::api_keys_path_from(&creds);
        let _ = std::fs::remove_file(&store_path);
        {
            let store = crate::apikey::ApiKeyStore::load(&store_path);
            let _ = store.create(
                "0001".into(),
                None,
                None,
                None,
                None,
                None,
                chrono::Utc::now(),
            );
            store.save_now().unwrap();
        }

        let cfg = Config {
            credentials_path: creds,
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
        assert_ne!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "store key 不得关上 admin 闸,否则首次部署建完第一把 key 即自锁"
        );
        let _ = std::fs::remove_file(&store_path);
    }

    /// 启动告警必须显式点名管理面(而非只提协议端点),并给出可操作的补救指令。
    #[test]
    fn auth_startup_warnings_name_admin_panel_and_shared_key_risk() {
        // 一个 key 都没配:协议端点 + 管理面各一条,管理面那条须点名 /admin 与 ADMIN_API_KEY。
        let none = auth_startup_warnings(&Config::default());
        assert_eq!(none.len(), 2, "{none:?}");
        assert!(none.iter().any(|w| w.contains("协议端点")));
        let admin_warn = none
            .iter()
            .find(|w| w.contains("/admin"))
            .expect("必须点名管理面同样开放");
        assert!(admin_warn.contains("ADMIN_API_KEY"), "{admin_warn}");

        // 只设主 key:管理闸回退用它,须提示数据面 key 同时具备管理员权限。
        let only_api = auth_startup_warnings(&Config {
            api_key: Some("sk-main".into()),
            ..Config::default()
        });
        assert_eq!(only_api.len(), 1, "{only_api:?}");
        assert!(only_api[0].contains("管理员权限"), "{}", only_api[0]);
        assert!(only_api[0].contains("ADMIN_API_KEY"), "{}", only_api[0]);

        // 只设 admin key:协议端点仍开放,管理面无隐患。
        let only_admin = auth_startup_warnings(&Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        });
        assert_eq!(only_admin.len(), 1, "{only_admin:?}");
        assert!(only_admin[0].contains("协议端点"));

        // 两个都设 → 无告警;空串/纯空白视同未设置。
        assert!(
            auth_startup_warnings(&Config {
                api_key: Some("sk-main".into()),
                admin_api_key: Some("adm".into()),
                ..Config::default()
            })
            .is_empty()
        );
        assert_eq!(
            auth_startup_warnings(&Config {
                api_key: Some("   ".into()),
                admin_api_key: Some(String::new()),
                ..Config::default()
            })
            .len(),
            2
        );
    }

    /// admin_api_key 与 api_key 均未设置、且 store 里一条 key 都没有时,admin API 开放访问
    /// (首次运行体验;只要出现任一鉴权材料就不再开放,见上一条测试)。
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

    /// 排空窗口必须有界,且短于容器默认宽限期(docker stop 10s),
    /// 否则窗口一到就被 SIGKILL,最终刷盘照样跑不到。
    #[test]
    fn shutdown_drain_window_is_bounded_below_container_grace() {
        assert!(SHUTDOWN_DRAIN_WINDOW >= Duration::from_secs(1));
        assert!(
            SHUTDOWN_DRAIN_WINDOW <= Duration::from_secs(9),
            "排空窗口须给刷盘留余量:{SHUTDOWN_DRAIN_WINDOW:?}"
        );
    }

    /// 服务器在停机信号之前就自行结束(如 accept 出错):原样交回结果,不进排空窗口。
    #[tokio::test]
    async fn drain_returns_server_result_when_server_finishes_first() {
        let out = drain_with_deadline(
            async { 7u32 },
            std::future::pending::<()>(),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(out, Some(7));
    }

    /// 信号已到但在途请求在窗口内跑完:等到干净结束,拿到服务器结果(正常停机路径)。
    #[tokio::test]
    async fn drain_waits_for_inflight_requests_within_window() {
        let out = drain_with_deadline(
            async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                1u32
            },
            async {},
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(out, Some(1));
    }

    /// 关键回归:连接永不结束(admin 日志 SSE 就是这种无限流)时,等待必须在窗口到期后
    /// 放弃并返回 None,由调用方继续刷盘退出——而不是永远挂着等 SIGKILL。
    #[tokio::test]
    async fn drain_gives_up_when_connection_never_ends() {
        let started = Instant::now();
        let window = Duration::from_millis(80);
        let out = drain_with_deadline(std::future::pending::<()>(), async {}, window).await;
        assert!(out.is_none(), "窗口到期必须放弃等待");
        // 确实等满了窗口(留调度余量,只校验下界)。
        assert!(started.elapsed() >= Duration::from_millis(60));
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
