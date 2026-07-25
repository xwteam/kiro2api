//! 用户面 REST API(user-ui 的后端,挂 `/api/user/*`)。
//!
//! 与 admin 面不同:**这些端点不走 admin 鉴权闸**——它们由调用方随请求携带的
//! **API-KEY 本身**做鉴权(见 user-ui `src/api/user.ts`:axios 请求拦截器把
//! key 塞进 `x-api-key` 头;登录另在 body 里带 `{apiKey}`)。故 handler 自行从
//! 请求提取 key、经 `Arc<ApiKeyStore>` 校验,解析出 api_key id 后再按该 id 过滤
//! `StatsManager` 的用量。校验失败一律 401,响应体 `{ "error": "<msg>" }`
//! (前端读 `response.data.error` 作 toast 文案)。
//!
//! 输出一律 camelCase(与 user-ui `src/types/api.ts` 的 TS 接口逐字对齐);
//! 内部 `DateTime<Utc>` → RFC3339(秒精度、Z 后缀);credits = cost / 0.72
//! (与前端 detail 页、admin 面同规约)。
//!
//! 本模块**只读**用量、**不写**任何状态(登录不下发 cookie/session,前端自持 key)。

pub mod handler;

use axum::Router;
use axum::routing::{get, post};

use crate::protocol::anthropic::handler::MessagesState;

/// 用户面子路由,`.with_state(state)` 后可直接 `.merge()` 进顶层 Router。
///
/// 三个端点全部走 API-KEY 自鉴权(handler 内解析),**不叠加**任何 admin/global
/// 鉴权 `.layer()`——挂载点(server::build_router)保证这一点。
///
/// - `POST /api/user/login`     校验 key,返回该 key 的档案 + 累计用量(或 401)。
/// - `GET  /api/user/usage`     该 key 的用量汇总(含 byModel)。
/// - `GET  /api/user/usage/records?page=&page_size=`  该 key 的用量记录分页(降序)。
pub fn user_api_router(state: MessagesState) -> Router {
    Router::new()
        .route("/api/user/login", post(handler::login))
        .route("/api/user/usage", get(handler::usage))
        .route("/api/user/usage/records", get(handler::usage_records))
        .with_state(state)
}
