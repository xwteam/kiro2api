//! Admin REST API:账号用量统计、配置只读展示、账号手动启停,以及 Phase 1 统计只读端点。
//! 鉴权由 `server::auth::require_api_key` 在挂载处单独 `.layer()`,本模块只负责路由与业务。
pub mod handler;
pub mod log_handler;
pub mod login_session;

use axum::Router;
use axum::routing::{get, post, put};

use crate::protocol::anthropic::handler::MessagesState;

/// Admin API 子路由,`.with_state(state)` 后可直接 `.merge()` 进顶层 Router。
///
/// 统计只读端点从 `MessagesState.stats`(与 relay 数据面共享的同一 `Arc<StatsManager>`)
/// 读取,故 admin 查询与 relay 写入共享内存,无需单独注入统计层。
///
/// 前端(adopted React SPA)以 `/api/admin` 为 baseURL,故 day-1 端点
/// 全部挂在 `/api/admin/*` 下并按前端契约命名/取形(camelCase)。
/// 隐式"登录"= 前端存 key 后首个拉 `GET /api/admin/credentials`,鉴权层放行即视为有效。
///
/// 旧 `/admin/api/*` 端点(stats/config/accounts enable|disable)保留,便于既有
/// 集成与内部只读观测不被打断;两组共用同一 `MessagesState`。
pub fn admin_api_router(state: MessagesState) -> Router {
    Router::new()
        // ---- 前端契约端点(/api/admin/*)----
        .route(
            "/api/admin/credentials",
            get(handler::credentials).post(handler::add_credential),
        )
        .route(
            "/api/admin/credentials/{id}",
            put(handler::update_credential).delete(handler::delete_credential),
        )
        .route(
            "/api/admin/credentials/{id}/disabled",
            post(handler::set_disabled),
        )
        // ---- Phase 3 批量/KAM 导入端点(server-side,逐项韧性)----
        // 静态段 `batch-import` 与 `{id}` 不冲突(axum 0.8 静态优先);接受数组/
        // KAM `{accounts}`/单对象,逐条规整+校验+入池落盘,返回逐项结果与计数。
        .route(
            "/api/admin/credentials/batch-import",
            post(handler::import_credentials_batch),
        )
        .route(
            "/api/admin/credentials/{id}/priority",
            post(handler::set_credential_priority),
        )
        .route(
            "/api/admin/credentials/{id}/reset",
            post(handler::reset_credential_failure),
        )
        // ---- Phase 3 交互式登录流(/api/admin/login/*)----
        .route(
            "/api/admin/login/builderid/start",
            post(handler::login_builderid_start),
        )
        .route(
            "/api/admin/login/builderid/poll",
            post(handler::login_builderid_poll),
        )
        .route(
            "/api/admin/login/iam-sso/start",
            post(handler::login_iam_sso_start),
        )
        .route(
            "/api/admin/login/iam-sso/complete",
            post(handler::login_iam_sso_complete),
        )
        .route("/api/admin/login/sso-token", post(handler::login_sso_token))
        .route("/api/admin/config", get(handler::config_view))
        .route("/api/admin/models", get(handler::models))
        // ---- FIX 1 动态模型清单刷新端点 ----
        // 静态段 `models/refresh` 与 `{id}/models/refresh` 不冲突(axum 0.8 静态优先);
        // 前端当前只拉 GET /models,刷新端点为按需手动触发(实拉上游 ListAvailableModels + 回填缓存)。
        .route(
            "/api/admin/credentials/models/refresh",
            post(handler::refresh_all_models),
        )
        .route(
            "/api/admin/credentials/{id}/models/refresh",
            post(handler::refresh_credential_models),
        )
        // ---- Phase 5 运行期可变配置端点(设置面板)----
        .route(
            "/api/admin/config/load-balancing",
            get(handler::get_load_balancing).put(handler::set_load_balancing),
        )
        .route(
            "/api/admin/config/auth-keys",
            get(handler::get_auth_keys).put(handler::set_auth_keys),
        )
        .route("/api/admin/server-info", get(handler::server_info))
        // 版本检查 / 更新 / 重启(对齐 gemini2api)。
        .route("/api/admin/check-update", get(handler::check_update))
        .route("/api/admin/update", post(handler::perform_update))
        .route("/api/admin/restart", post(handler::restart_server))
        // ---- Phase 6 实时日志端点(/api/admin/logs/*)----
        // stream=SSE(history 事件后逐条 log 事件,带心跳);snapshot=JSON 数组;download=txt 附件。
        // 鉴权走 admin `require_api_key` 闸:EventSource 无法设头,故用 `?api_key=` query 携带 key。
        .route("/api/admin/logs/stream", get(log_handler::stream_logs))
        .route("/api/admin/logs/snapshot", get(log_handler::snapshot_logs))
        .route("/api/admin/logs/download", get(log_handler::download_logs))
        // ---- Phase 4 实时 RPM 只读端点(/api/admin/rpm)----
        .route("/api/admin/rpm", get(handler::rpm_snapshot))
        // ---- Phase 1 统计只读端点(/api/admin/*)----
        .route(
            "/api/admin/credentials/{id}/usage/records",
            get(handler::credential_usage_records),
        )
        .route(
            "/api/admin/credentials/{id}/usage/today",
            get(handler::credential_today_summary),
        )
        .route(
            "/api/admin/credentials/{id}/failure-logs",
            get(handler::credential_failure_logs),
        )
        .route(
            "/api/admin/credentials/{id}/throttle-logs",
            get(handler::credential_throttle_logs),
        )
        // ---- Phase 4 余额端点(/api/admin/credentials/{id}/balance)----
        .route(
            "/api/admin/credentials/{id}/balance",
            get(handler::credential_balance),
        )
        // ---- 全局积分聚合端点(只读共享 BalanceCache,零上游)----
        .route("/api/admin/credits/global", get(handler::global_credits))
        .route("/api/admin/usage/daily", get(handler::daily_usage_summary))
        .route("/api/admin/usage/summary", get(handler::usage_summary))
        .route(
            "/api/admin/usage/daily/{date}/records",
            get(handler::daily_records),
        )
        // ---- Phase 2 API-KEY 管理端点(/api/admin/api-keys*)----
        // `/api-keys/usage` 为静态段,优先于 `/api-keys/{id}`(axum 0.8 静态优先);
        // 顺序无关,两者不冲突。
        .route(
            "/api/admin/api-keys",
            get(handler::list_api_keys).post(handler::create_api_key),
        )
        .route("/api/admin/api-keys/usage", get(handler::all_api_key_usage))
        .route(
            "/api/admin/api-keys/{id}",
            put(handler::update_api_key).delete(handler::delete_api_key),
        )
        .route(
            "/api/admin/api-keys/{id}/usage",
            get(handler::api_key_usage).delete(handler::reset_api_key_usage),
        )
        .route(
            "/api/admin/api-keys/{id}/usage/records",
            get(handler::api_key_usage_records),
        )
        // ---- 旧端点保留(向后兼容)----
        .route("/admin/api/stats", get(handler::stats))
        .route("/admin/api/config", get(handler::config_view))
        .route("/admin/api/accounts/{id}/enable", post(handler::enable))
        .route("/admin/api/accounts/{id}/disable", post(handler::disable))
        .with_state(state)
}
