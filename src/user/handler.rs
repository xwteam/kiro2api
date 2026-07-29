//! 用户面处理器:登录校验 + 该 key 的用量汇总/记录。
//!
//! 鉴权:每个 handler 自行从请求提取调用方 key（登录优先 body 的 `apiKey`，
//! 回退 header；其余端点用 header/`?token=`），经 `Arc<ApiKeyStore>` 校验；
//! 未命中/停用/过期 → 401（`{ "error": "<msg>" }`，前端读 `data.error`）。
//! 校验成功后按解析出的 api_key id 过滤 `StatsManager` 用量。
//!
//! 输出 camelCase，逐字对齐 user-ui `src/types/api.ts`。

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::apikey::{ApiKey, ApiKeyAuthResult};
use crate::protocol::anthropic::handler::MessagesState;
use crate::server::auth::extract_key;
use crate::stats::model::UsageRecord as StoredUsageRecord;
use crate::stats::usage::{ApiKeyUsageSummary, ModelUsageAgg, Page};

/// 把累计花费按 key 的计量单位归一（与 auth 闸的 `current_spent` 同一套换算）。
/// 已花额归一到上限单位。credits 用**上游回报的真实值**,不由 cost 反算 ——
/// 必须与 `server::auth::current_spent` 逐字同口径,否则面板与闸门会各说各话。
fn spent_in_unit(total_cost: f64, total_credits: f64, limit_unit: &str) -> f64 {
    if limit_unit.eq_ignore_ascii_case("credits") {
        total_credits
    } else {
        total_cost
    }
}

/// 这把 key 的**下一发请求**会不会被鉴权闸以 402 拒掉。
///
/// 判据必须与 `server::auth` 的准入闸逐字相同：闸上放行的条件是
/// `已花 + 在途预留 + 单次预留估算 <= 上限`（见 `apikey::ApiKeyStore::try_reserve_spend`），
/// 而面板过去只比 `已花 >= 上限`。两者整整差一个单次预留额：在 `上限 - 预留 < 已花 < 上限`
/// 这一段里，中继对每一发请求都回 402，面板却还画着绿色的「正常」和一条没走完的额度条——
/// 用户看着有额度、却一个字都发不出去，且面板上找不到任何线索。极端情形是上限本身小于一次
/// 预留（如 1.38 credits）：这把 key 从签发起就发不出任何请求，面板却始终显示 0 / 1.38 正常。
/// 故这里直接引用闸上的 `est_cost_in_unit`，单位归一也照同一套，让展示与执行必然同口径。
///
/// 在途预留（并发请求临时占额）不计入：它随请求结束即释放，不是用户能看懂的稳定状态；
/// 少算它只会让面板比闸更宽松一格，不会出现「面板说用完、实际还能发」这种反向错报。
fn spending_exhausted(
    spending_limit: Option<f64>,
    limit_unit: &str,
    total_cost: f64,
    total_credits: f64,
) -> bool {
    let Some(limit) = spending_limit else {
        return false;
    };
    let spent = spent_in_unit(total_cost, total_credits, limit_unit);
    // 取「闸上的放行条件」再取反，而不是直接写 `已花 + 预留 > 上限`：NaN 参与的比较恒为假，
    // 而闸上对非有限的上限/已花额一律保守拒绝（`try_reserve_spend` 直接 Err）。按放行条件
    // 取反，NaN 会落到「已用完」这一侧、与闸一致；直接写 `>` 则会得出「正常」，正好相反。
    let gate_admits = spent + crate::server::auth::est_cost_in_unit(limit_unit) <= limit;
    !gate_admits
}

/// 当前 UTC 时刻（校验过期用；注入点集中于此，与 store/auth 的注入时钟规约对齐）。
fn now_utc() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

/// RFC3339（UTC、秒精度、Z 后缀）字符串化。
fn dt_to_rfc3339(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 从请求里取调用方 key：`Authorization: Bearer` > `x-api-key` > `?token=`。
/// 登录额外接受 body 的 `apiKey`（见 `login`），此处不含 query（登录无 query 语义）。
fn key_from_headers(headers: &HeaderMap) -> Option<String> {
    extract_key(headers, None)
}

/// 401 响应：`{ "error": "<msg>" }`（前端 login-page 读 `response.data.error`）。
fn unauthorized(msg: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": msg }))).into_response()
}

/// 把 key 明文解析成有效 `ApiKey`；任何非 Valid 情形返回对应 401 响应（Err 分支）。
/// 成功返回完整 `ApiKey`（用于回填档案字段）。
fn resolve_key(state: &MessagesState, key: Option<&str>) -> Result<ApiKey, Response> {
    let Some(key) = key.filter(|k| !k.is_empty()) else {
        return Err(unauthorized("缺少 API Key"));
    };
    let now = now_utc();
    match state.api_keys.validate(key, now) {
        ApiKeyAuthResult::Valid { id, .. } => state
            .api_keys
            .get_by_id(id)
            // validate 命中后 get_by_id 理论必有；竞态删除极罕见 → 401 兜底。
            .ok_or_else(|| unauthorized("无效的 API Key")),
        ApiKeyAuthResult::Disabled => Err(unauthorized("API Key 已停用")),
        ApiKeyAuthResult::Expired => Err(unauthorized("API Key 已过期")),
        ApiKeyAuthResult::NotFound => Err(unauthorized("无效的 API Key")),
    }
}

// ==================== 登录 ====================

/// `POST /api/user/login` 请求体，对齐前端 `LoginRequest`。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    #[serde(default)]
    pub api_key: Option<String>,
}

/// 登录成功响应，camelCase 逐字对齐前端 `LoginResponse`。
/// `spendingLimit/expiresAt/durationDays/activatedAt` 为 nullable（`null` 显式出现），
/// 故不做 `skip_serializing_if`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    pub id: u32,
    pub name: String,
    pub spending_limit: Option<f64>,
    pub limit_unit: String,
    pub total_cost: f64,
    pub total_credits: f64,
    pub expires_at: Option<String>,
    pub duration_days: Option<f64>,
    pub activated_at: Option<String>,
    /// 额度是否已用完（口径 = 鉴权闸的准入算式，见 [`spending_exhausted`]）。
    /// 前端据此显示「额度已用完」，不再自己按 `已花 >= 上限` 推断。
    pub exhausted: bool,
}

/// `POST /api/user/login`：校验 key（body `apiKey` 优先，回退 header），返回该 key
/// 的档案 + 累计消费（totalCost/totalCredits）。未命中/停用/过期 → 401。
pub async fn login(
    State(state): State<MessagesState>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Response {
    // body 的 apiKey 优先（登录首次调用前端尚未走拦截器可能无 header），回退 header。
    let provided = req
        .api_key
        .filter(|k| !k.is_empty())
        .or_else(|| key_from_headers(&headers));
    let key = match resolve_key(&state, provided.as_deref()) {
        Ok(k) => k,
        Err(resp) => return resp,
    };

    let summary = state.stats.get_summary_by_api_key(key.id).await;
    let exhausted = spending_exhausted(
        key.spending_limit,
        &key.limit_unit,
        summary.total_cost,
        summary.total_credits,
    );
    Json(LoginResponse {
        id: key.id,
        name: key.name,
        spending_limit: key.spending_limit,
        limit_unit: key.limit_unit,
        total_cost: summary.total_cost,
        total_credits: summary.total_credits,
        expires_at: key.expires_at.map(dt_to_rfc3339),
        duration_days: key.duration_days,
        activated_at: key.activated_at.map(dt_to_rfc3339),
        exhausted,
    })
    .into_response()
}

// ==================== 用量汇总 ====================

/// 单模型用量明细，camelCase 对齐前端 `ModelUsage`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageView {
    pub model: String,
    pub requests: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
}

impl From<ModelUsageAgg> for ModelUsageView {
    fn from(m: ModelUsageAgg) -> Self {
        ModelUsageView {
            model: m.model,
            requests: m.requests,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            cost: m.cost,
        }
    }
}

/// `GET /api/user/usage` 响应，camelCase 逐字对齐前端 `UsageResponse`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageResponse {
    pub id: u32,
    pub name: String,
    pub spending_limit: Option<f64>,
    pub limit_unit: String,
    pub expires_at: Option<String>,
    pub duration_days: Option<f64>,
    pub activated_at: Option<String>,
    pub total_requests: u64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    pub total_credits: f64,
    pub by_model: Vec<ModelUsageView>,
    /// 额度是否已用完（口径 = 鉴权闸的准入算式，见 [`spending_exhausted`]）。
    pub exhausted: bool,
}

fn build_usage_response(key: ApiKey, summary: ApiKeyUsageSummary) -> UsageResponse {
    let exhausted = spending_exhausted(
        key.spending_limit,
        &key.limit_unit,
        summary.total_cost,
        summary.total_credits,
    );
    UsageResponse {
        id: key.id,
        name: key.name,
        spending_limit: key.spending_limit,
        limit_unit: key.limit_unit,
        expires_at: key.expires_at.map(dt_to_rfc3339),
        duration_days: key.duration_days,
        activated_at: key.activated_at.map(dt_to_rfc3339),
        total_requests: summary.total_requests,
        total_input_tokens: summary.total_input_tokens,
        total_output_tokens: summary.total_output_tokens,
        total_cost: summary.total_cost,
        total_credits: summary.total_credits,
        by_model: summary
            .by_model
            .into_iter()
            .map(ModelUsageView::from)
            .collect(),
        exhausted,
    }
}

/// `GET /api/user/usage`：该 key 的用量汇总（含 byModel）。key 校验失败 → 401。
/// key 有效但无记录 → 全零汇总（不 500）。
pub async fn usage(State(state): State<MessagesState>, headers: HeaderMap) -> Response {
    let key = match resolve_key(&state, key_from_headers(&headers).as_deref()) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let summary = state.stats.get_summary_by_api_key(key.id).await;
    Json(build_usage_response(key, summary)).into_response()
}

// ==================== 用量记录分页 ====================

/// 分页查询参数：`page`/`page_size` 均可选；缺省 page=1、page_size=50
/// （与 user-ui `getUsageRecords` 默认 pageSize=50 对齐）。越界由存储层钳制。
#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}

impl PageQuery {
    fn page(&self) -> usize {
        self.page.unwrap_or(1)
    }
    fn page_size(&self) -> usize {
        self.page_size.unwrap_or(50)
    }
}

/// 单条用量记录，camelCase 对齐前端 `UsageRecordItem`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordView {
    pub model: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub estimated_cost: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits_used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits_saved: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<i32>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_label: Option<String>,
}

impl From<StoredUsageRecord> for UsageRecordView {
    fn from(r: StoredUsageRecord) -> Self {
        UsageRecordView {
            model: r.model,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            estimated_cost: r.estimated_cost,
            credits_used: r.credits_used,
            // 无 creditsSaved 数据源（前端会以 estimatedCost/0.72 兜底）。
            credits_saved: None,
            cache_read_input_tokens: r.cache_read_input_tokens,
            cache_creation_input_tokens: r.cache_creation_input_tokens,
            created_at: crate::stats::model::unix_to_rfc3339(r.created_at_unix),
            client_ip: r.client_ip,
            // 凭据标签暂不解析（前端可选字段）。
            credential_label: None,
        }
    }
}

/// 用量记录分页响应，camelCase 逐字对齐前端 `UsageRecordsPage`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsPage {
    pub records: Vec<UsageRecordView>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub total_pages: usize,
}

impl From<Page<StoredUsageRecord>> for UsageRecordsPage {
    fn from(p: Page<StoredUsageRecord>) -> Self {
        UsageRecordsPage {
            records: p.items.into_iter().map(UsageRecordView::from).collect(),
            total: p.total,
            page: p.page,
            page_size: p.page_size,
            total_pages: p.total_pages,
        }
    }
}

/// `GET /api/user/usage/records?page=&page_size=`：该 key 的用量记录分页（降序，
/// 最新在前）。key 校验失败 → 401。key 有效但无记录 → 空页（total=0，不 500）。
pub async fn usage_records(
    State(state): State<MessagesState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Response {
    let key = match resolve_key(&state, key_from_headers(&headers).as_deref()) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let page = state
        .stats
        .get_records_by_api_key(key.id, q.page(), q.page_size())
        .await;
    Json(UsageRecordsPage::from(page)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::kiro::pool::{LbMode, Pool};
    use crate::stats::StatsManager;
    use crate::user::user_api_router;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode as HttpStatusCode};
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    fn unique_tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "kiro2api_user_{}_{}_{}.json",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    fn empty_stats(tag: &str) -> Arc<StatsManager> {
        let dir = std::env::temp_dir().join(format!(
            "kiro2api_user_stats_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        StatsManager::load_from_dir(&dir)
    }

    /// 建一个带独立 api_keys 存储 + stats 的 MessagesState。返回 (state, api_keys)。
    fn make_state(
        tag: &str,
        stats: Arc<StatsManager>,
    ) -> (MessagesState, Arc<crate::apikey::ApiKeyStore>) {
        let api_keys = crate::apikey::ApiKeyStore::load(unique_tmp(tag));
        let state = MessagesState {
            pool: Arc::new(Mutex::new(Pool::new(vec![], LbMode::Priority))),
            client: reqwest::Client::new(),
            control_client: reqwest::Client::new(),
            cfg: Arc::new(Config::default()),
            runtime_cfg: crate::config::shared_runtime_config(&crate::config::Config::default()),
            endpoint_override: None,
            stats,
            api_keys: api_keys.clone(),
            balance: crate::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
            models_cache: crate::models_cache::ModelsCache::new(),
            builderid_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            iam_sso_sessions: crate::admin::login_session::LoginSessions::with_default_ttl(),
            log_capture: None,
            refresh_ctx: crate::kiro::ensure_fresh::RefreshCtx::new(
                std::env::temp_dir()
                    .join(format!(
                        "kiro2api_refreshctx_src_user_handler_rs_{}.json",
                        std::process::id()
                    ))
                    .to_string_lossy()
                    .to_string(),
            ),
        };
        (state, api_keys)
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn login_valid_returns_profile_camelcase() {
        let stats = empty_stats("login_ok");
        let (state, api_keys) = make_state("login_ok", stats.clone());
        let now = chrono::Utc::now();
        let k = api_keys.create(
            "0007".into(),
            None,
            Some(50.0),
            Some("credits".into()),
            None,
            None,
            now,
        );
        // 播一条该 key 的用量：cost=0.72 → credits=1.0
        stats
            .usage
            .record_usage_with_api_key(
                1,
                k.id,
                "claude-opus-4.6".into(),
                100,
                200,
                0.72,
                None,
                None,
                None,
                1000,
            )
            .await;
        let app = user_api_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/user/login")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"apiKey":"{}"}}"#, k.key)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["id"], k.id);
        assert_eq!(v["name"], "0007");
        assert_eq!(v["spendingLimit"], 50.0);
        assert_eq!(v["limitUnit"], "credits");
        assert!((v["totalCost"].as_f64().unwrap() - 0.72).abs() < 1e-9);
        // credits 取上游回报的真实值(本夹具未填 credits_used → 0),**不由 cost 反算**。
        // 旧断言 1.0 正是 0.72/0.72 的反算结果,与账单无关。
        assert!((v["totalCredits"].as_f64().unwrap() - 0.0).abs() < 1e-9);
        // nullable 字段显式出现为 null
        assert!(v["expiresAt"].is_null());
        assert!(v["durationDays"].is_null());
        assert!(v["activatedAt"].is_null());
    }

    #[tokio::test]
    async fn login_via_header_when_body_missing_key() {
        let stats = empty_stats("login_hdr");
        let (state, api_keys) = make_state("login_hdr", stats);
        let k = api_keys.create("h".into(), None, None, None, None, None, chrono::Utc::now());
        let app = user_api_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/user/login")
                    .header("content-type", "application/json")
                    .header("x-api-key", &k.key)
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["id"], k.id);
    }

    #[tokio::test]
    async fn login_unknown_key_401_with_error_field() {
        let stats = empty_stats("login_bad");
        let (state, api_keys) = make_state("login_bad", stats);
        let _ = api_keys.create("x".into(), None, None, None, None, None, chrono::Utc::now());
        let app = user_api_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/user/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"apiKey":"sk-nope"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::UNAUTHORIZED);
        let v = body_json(resp).await;
        assert!(v["error"].is_string());
    }

    #[tokio::test]
    async fn login_disabled_and_expired_401() {
        let stats = empty_stats("login_de");
        let (state, api_keys) = make_state("login_de", stats);
        let now = chrono::Utc::now();
        let disabled = api_keys.create("d".into(), None, None, None, None, None, now);
        api_keys.disable(disabled.id);
        let past = now - chrono::Duration::days(2);
        let expired = api_keys.create(
            "e".into(),
            Some(now - chrono::Duration::days(1)),
            None,
            None,
            None,
            None,
            past,
        );
        let app = user_api_router(state);

        for key in [&disabled.key, &expired.key] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/user/login")
                        .header("content-type", "application/json")
                        .body(Body::from(format!(r#"{{"apiKey":"{key}"}}"#)))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), HttpStatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn usage_scoped_to_key_and_bymodel_shape() {
        let stats = empty_stats("usage");
        let (state, api_keys) = make_state("usage", stats.clone());
        let now = chrono::Utc::now();
        let mine = api_keys.create("mine".into(), None, None, None, None, None, now);
        let other = api_keys.create("other".into(), None, None, None, None, None, now);
        // 我的 key：两条不同模型
        stats
            .usage
            .record_usage_with_api_key(
                1,
                mine.id,
                "claude-opus-4.6".into(),
                100,
                200,
                1.0,
                None,
                None,
                None,
                1000,
            )
            .await;
        stats
            .usage
            .record_usage_with_api_key(
                2,
                mine.id,
                "claude-sonnet-4.5".into(),
                50,
                60,
                0.5,
                None,
                None,
                None,
                1001,
            )
            .await;
        // 别的 key：不应计入我的汇总
        stats
            .usage
            .record_usage_with_api_key(
                1,
                other.id,
                "claude-opus-4.6".into(),
                999,
                999,
                9.9,
                None,
                None,
                None,
                1002,
            )
            .await;
        let app = user_api_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/user/usage")
                    .header("x-api-key", &mine.key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["id"], mine.id);
        assert_eq!(v["name"], "mine");
        assert_eq!(v["totalRequests"], 2);
        assert_eq!(v["totalInputTokens"], 150);
        assert_eq!(v["totalOutputTokens"], 260);
        assert!((v["totalCost"].as_f64().unwrap() - 1.5).abs() < 1e-9);
        let by_model = v["byModel"].as_array().unwrap();
        assert_eq!(by_model.len(), 2);
        // 升序：opus 在 sonnet 前（字母序 'o' < 's'）
        assert_eq!(by_model[0]["model"], "claude-opus-4.6");
        assert!(by_model[0]["cost"].is_number());
        assert!(by_model[0]["requests"].is_number());
    }

    #[tokio::test]
    async fn usage_no_records_returns_zeroed_not_500() {
        let stats = empty_stats("usage_zero");
        let (state, api_keys) = make_state("usage_zero", stats);
        let k = api_keys.create("z".into(), None, None, None, None, None, chrono::Utc::now());
        let app = user_api_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/user/usage")
                    .header("x-api-key", &k.key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["totalRequests"], 0);
        assert!(v["byModel"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn usage_without_key_401() {
        let stats = empty_stats("usage_noauth");
        let (state, api_keys) = make_state("usage_noauth", stats);
        let _ = api_keys.create("k".into(), None, None, None, None, None, chrono::Utc::now());
        let app = user_api_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/user/usage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn usage_records_scoped_paginated_camelcase() {
        let stats = empty_stats("records");
        let (state, api_keys) = make_state("records", stats.clone());
        let now = chrono::Utc::now();
        let mine = api_keys.create("mine".into(), None, None, None, None, None, now);
        let other = api_keys.create("other".into(), None, None, None, None, None, now);
        // 我的 5 条（created_at 递增），别人的 1 条
        for i in 1..=5 {
            stats
                .usage
                .record_usage_with_api_key(
                    3,
                    mine.id,
                    "claude-sonnet-4.5".into(),
                    100,
                    200,
                    0.01 * i as f64,
                    Some("9.9.9.9".into()),
                    Some(10),
                    Some(20),
                    1000 + i,
                )
                .await;
        }
        stats
            .usage
            .record_usage_with_api_key(3, other.id, "m".into(), 1, 1, 1.0, None, None, None, 2000)
            .await;
        let app = user_api_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/user/usage/records?page=1&page_size=2")
                    .header("x-api-key", &mine.key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v = body_json(resp).await;
        // 仅我的 5 条计入
        assert_eq!(v["total"], 5);
        assert_eq!(v["page"], 1);
        assert_eq!(v["pageSize"], 2);
        assert_eq!(v["totalPages"], 3);
        let recs = v["records"].as_array().unwrap();
        assert_eq!(recs.len(), 2);
        let r0 = &recs[0];
        assert_eq!(r0["model"], "claude-sonnet-4.5");
        assert_eq!(r0["inputTokens"], 100);
        assert_eq!(r0["outputTokens"], 200);
        assert!(r0["estimatedCost"].is_number());
        assert_eq!(r0["cacheReadInputTokens"], 10);
        assert_eq!(r0["cacheCreationInputTokens"], 20);
        assert_eq!(r0["clientIp"], "9.9.9.9");
        assert!(r0["createdAt"].as_str().unwrap().ends_with('Z'));
        // 降序：最新 created_at=1005 在首
        assert!(r0["createdAt"].as_str().unwrap() > recs[1]["createdAt"].as_str().unwrap());
    }

    #[tokio::test]
    async fn usage_records_default_page_size_is_50() {
        let stats = empty_stats("records_dflt");
        let (state, api_keys) = make_state("records_dflt", stats.clone());
        let k = api_keys.create("k".into(), None, None, None, None, None, chrono::Utc::now());
        stats
            .usage
            .record_usage_with_api_key(1, k.id, "m".into(), 1, 1, 0.1, None, None, None, 1000)
            .await;
        let app = user_api_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/user/usage/records")
                    .header("x-api-key", &k.key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["pageSize"], 50);
        assert_eq!(v["total"], 1);
        assert_eq!(v["totalPages"], 1);
    }

    /// 把鉴权闸原样搭成一条最小路由，用来问「这把 key 现在还发不发得出请求」。
    /// 面板的额度状态只有和它逐发对齐才算数，故回归测试必须同时问两边。
    fn gate_router(
        api_keys: Arc<crate::apikey::ApiKeyStore>,
        stats: Arc<StatsManager>,
    ) -> axum::Router {
        use crate::server::auth::{AuthRole, AuthState, SpendCache, require_api_key};
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(api_keys),
            stats: Some(stats),
            spend_cache: Arc::new(SpendCache::default()),
        };
        axum::Router::new()
            .route("/v1/messages", axum::routing::post(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(auth, require_api_key))
    }

    async fn gate_status(
        api_keys: Arc<crate::apikey::ApiKeyStore>,
        stats: Arc<StatsManager>,
        key: &str,
    ) -> HttpStatusCode {
        let resp = gate_router(api_keys, stats)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/messages")
                    .header("x-api-key", key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        // 放行的话预留 guard 绑在 body 上，读完才释放（对 402 无影响）。
        let _ = axum::body::to_bytes(resp.into_body(), 65536).await;
        status
    }

    /// 关键回归：面板的「额度状态」必须与鉴权闸同口径。
    ///
    /// 闸上放行的条件是 `已花 + 单次预留 <= 上限`，面板过去只比 `已花 >= 上限`，两者差着
    /// 一整个单次预留额。取一把 1.0 credits 的 key（单次预留 = 1/0.72 ≈ 1.39 credits，比上限
    /// 还大）：中继对它每一发请求都 402，而按旧判据 `0 >= 1.0` 为假，面板会显示绿色「正常」
    /// 和一条空着的额度条——用户看着有额度，却一个字也发不出去，面板上还找不到任何线索。
    /// 这里同时问闸和面板：闸拒 402，面板就必须回 exhausted=true。
    #[tokio::test]
    async fn usage_reports_exhausted_exactly_when_the_gate_rejects() {
        let stats = empty_stats("exhausted_band");
        let (state, api_keys) = make_state("exhausted_band", stats.clone());
        // 上限须**小于一次在途预留**(EST_CREDITS_PER_REQUEST = 0.25),测试意图才成立:
        // 这把 key 连一次请求都买不起,闸必拒,面板就必须同步报 exhausted。
        let k = api_keys.create(
            "band".into(),
            None,
            Some(0.2),
            Some("credits".into()),
            None,
            None,
            chrono::Utc::now(),
        );

        // 闸:一发都发不出去。
        assert_eq!(
            gate_status(api_keys.clone(), stats.clone(), &k.key).await,
            HttpStatusCode::PAYMENT_REQUIRED,
            "上限比一次预留还小，中继必然 402"
        );

        let app = user_api_router(state);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/user/usage")
                    .header("x-api-key", &k.key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        let v = body_json(resp).await;
        // 旧判据(已花 >= 上限)在这里为假 —— 正是它让面板显示「正常」。
        assert!(
            v["totalCredits"].as_f64().unwrap() < v["spendingLimit"].as_f64().unwrap(),
            "本用例必须落在「已花 < 上限」区间，否则钉不住这个缺陷"
        );
        assert_eq!(
            v["exhausted"], true,
            "闸已 402，面板必须报「额度已用完」而不是绿色正常"
        );

        // 登录响应同样要带这个判断:只登录不刷新用量的客户端也不能被误导。
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/user/login")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"apiKey":"{}"}}"#, k.key)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::OK);
        assert_eq!(body_json(resp).await["exhausted"], true);
    }

    /// 反向:额度真的还够用时不得误报「已用完」——否则用户会以为卡废了。
    #[tokio::test]
    async fn usage_not_exhausted_while_the_gate_still_admits() {
        let stats = empty_stats("healthy_limit");
        let (state, api_keys) = make_state("healthy_limit", stats.clone());
        let k = api_keys.create(
            "healthy".into(),
            None,
            Some(100.0),
            Some("credits".into()),
            None,
            None,
            chrono::Utc::now(),
        );
        stats
            .usage
            .record_usage_with_api_key(1, k.id, "m".into(), 10, 20, 0.72, None, None, None, 1000)
            .await;

        assert_eq!(
            gate_status(api_keys.clone(), stats.clone(), &k.key).await,
            HttpStatusCode::OK,
            "额度充足时闸必须放行"
        );

        let resp = user_api_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/user/usage")
                    .header("x-api-key", &k.key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(resp).await;
        assert_eq!(v["exhausted"], false, "闸还在放行，面板不得报已用完");
    }

    /// 无消费上限的 key 永远不算「已用完」（面板据此完全不画额度条）。
    #[tokio::test]
    async fn usage_without_spending_limit_is_never_exhausted() {
        let stats = empty_stats("nolimit");
        let (state, api_keys) = make_state("nolimit", stats.clone());
        let k = api_keys.create(
            "free".into(),
            None,
            None,
            None,
            None,
            None,
            chrono::Utc::now(),
        );
        stats
            .usage
            .record_usage_with_api_key(1, k.id, "m".into(), 10, 20, 99.0, None, None, None, 1000)
            .await;
        let resp = user_api_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/user/usage")
                    .header("x-api-key", &k.key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(resp).await["exhausted"], false);
    }

    #[tokio::test]
    async fn usage_records_without_key_401() {
        let stats = empty_stats("records_noauth");
        let (state, api_keys) = make_state("records_noauth", stats);
        let _ = api_keys.create("k".into(), None, None, None, None, None, chrono::Utc::now());
        let app = user_api_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/user/usage/records")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatusCode::UNAUTHORIZED);
    }
}
