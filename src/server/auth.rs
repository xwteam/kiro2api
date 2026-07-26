use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, extract::Request};
use chrono::{DateTime, Utc};
use serde_json::json;
use subtle::ConstantTimeEq;

use crate::apikey::{ApiKeyAuthResult, ApiKeyStore};
use crate::stats::StatsManager;

/// USD→credits 换算基:credits = cost / 0.72(与 admin/user 展示层同规约)。
const CREDITS_PER_USD_DIVISOR: f64 = 0.72;

/// 单次在途请求对消费上限的“预留额”估算(USD)。准入时无法预知本次真实 cost
/// (cost 依赖响应后的 token 计数),故用一个保守的名义单次上限做在途预留:
/// 把消费上限的并发 check-then-act 收敛为原子 reserve-then-reconcile,令越界
/// 被界定在“至多一次在途请求之内”。真实 cost 仍由既有 stats 路径在请求完成后记账,
/// 预留只在请求在途期间临时占额、完成即释放(RAII),不参与持久记账。
/// 取值口径:一次大请求的量级上限的保守估计;偏大只会让接近上限时更早 402(更保守),
/// 不会漏放导致无界超支。credits 单位下按 CREDITS_PER_USD_DIVISOR 同步换算。
const EST_COST_PER_REQUEST_USD: f64 = 1.0;

/// 鉴权闸解析出的 store-key id,经请求扩展下传给 relay 做用量归属。
/// `None` = 全局 key / 开放模式(无 store-key 归属,relay 记 id=0)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApiKeyId(pub Option<u32>);

/// 鉴权中间件所需状态:
/// - `api_key`:全局 API key(`None`/空串 = 该来源不设全局 key)。
/// - `api_keys`:store-backed 每用户 key。协议闸据此放行 store key,并据“store 里是否有 key”
///   判定协议面是否已启用鉴权;admin 闸虽接同一份 store,但**既不放行也不据它判定**(见 `AuthRole`)。
/// - `stats`:用量统计(仅协议闸用于消费上限求和;admin 闸传 `None`)。
///
/// 放行规则按闸分开(见 `AuthRole`):请求必须命中本闸接受的凭据,否则 401;本闸判定
/// “尚未配置鉴权”时才维持首次运行的开放模式。
/// 鉴权闸的角色:决定运行期可变配置里“期望的全局 key”取哪一个字段、是否接受 store key,
/// 以及**据什么判定“已配置鉴权”**(判定为已配置后,未命中凭据的请求一律 401)。
/// - `Protocol`:协议闸,期望 = 主 `api_key`;另接受有效 store key。
///   判据 = 主 `api_key` 或 store 里任意一条 key(数据面只要发出过 key 就不再开放)。
/// - `Admin`:管理闸,期望 = `admin_api_key`(非空)否则回退主 `api_key`;
///   **不接受 store key**——数据面每用户 key 不得取得管理员权限。
///   判据 = **只看管理员级凭据**(admin key / 主 key),store key 不参与,理由见 [`require_api_key`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthRole {
    Protocol,
    Admin,
}

#[derive(Clone)]
pub struct AuthState {
    /// 静态全局 key(仅在未接 `runtime_cfg` 时使用:既有 `global_only` 及单测直连场景)。
    pub api_key: Option<String>,
    /// 运行期可变配置句柄;`Some` 时期望 key 每请求从此实时读取(轮换即时生效)。
    /// `None` 时退回静态 `api_key`,保持既有行为与测试语义不变。
    pub runtime_cfg: Option<crate::config::SharedRuntimeConfig>,
    /// 角色:决定从 runtime 取哪个字段作为期望全局 key。
    pub role: AuthRole,
    pub api_keys: Option<Arc<ApiKeyStore>>,
    pub stats: Option<Arc<StatsManager>>,
}

impl AuthState {
    /// 仅全局 key 的鉴权状态(admin 闸用:不接 store、不查消费上限)。
    /// 静态 key 版本:不随运行期轮换,保留给测试/内部直连场景。
    pub fn global_only(api_key: Option<String>) -> Self {
        Self {
            api_key,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: None,
            stats: None,
        }
    }

    /// 协议闸:期望 = 运行期主 `api_key`(实时读取),并接入 store + stats。
    pub fn protocol(
        runtime_cfg: crate::config::SharedRuntimeConfig,
        api_keys: Arc<ApiKeyStore>,
        stats: Arc<StatsManager>,
    ) -> Self {
        Self {
            api_key: None,
            runtime_cfg: Some(runtime_cfg),
            role: AuthRole::Protocol,
            api_keys: Some(api_keys),
            stats: Some(stats),
        }
    }

    /// 管理闸:期望 = 运行期 `admin_api_key`(非空)否则回退主 `api_key`。
    ///
    /// 同时接入 store,但 `AuthRole::Admin` 下 store **完全不参与本闸的裁决**:既不放行
    /// store key(数据面每用户 key 不得取得管理员权限),也不据“store 里有 key”把管理闸
    /// 判成已配置鉴权(否则首次运行会自锁,见 [`require_api_key`])。入参保留是为了不动
    /// 调用方接线、并给日后 admin 侧用得上 store 的场景留口。
    pub fn admin(
        runtime_cfg: crate::config::SharedRuntimeConfig,
        api_keys: Arc<ApiKeyStore>,
    ) -> Self {
        Self {
            api_key: None,
            runtime_cfg: Some(runtime_cfg),
            role: AuthRole::Admin,
            api_keys: Some(api_keys),
            stats: None,
        }
    }

    /// 解析本次请求的“期望全局 key”:优先运行期可变配置(实时,支持轮换),
    /// 否则回退静态 `api_key`。admin 角色下取 admin key 非空否则回退主 key。
    fn expected_key(&self) -> Option<String> {
        if let Some(rc) = &self.runtime_cfg {
            let g = rc.read();
            return match self.role {
                AuthRole::Protocol => g.api_key.clone(),
                AuthRole::Admin => g
                    .admin_api_key
                    .clone()
                    .filter(|s| !s.is_empty())
                    .or_else(|| g.api_key.clone()),
            };
        }
        self.api_key.clone()
    }
}

/// 401 认证错误响应(不泄露期望 key)。
fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "type": "error",
            "error": { "type": "authentication_error", "message": message },
        })),
    )
        .into_response()
}

/// 402 消费上限错误响应(store key 用量已达/超上限)。
fn payment_required(message: &str) -> Response {
    (
        StatusCode::PAYMENT_REQUIRED,
        Json(json!({
            "type": "error",
            "error": { "type": "billing_error", "message": message },
        })),
    )
        .into_response()
}

/// 把在途消费预留 guard(`SpendReservation`)绑定到 `response` 的 body 生命周期,
/// 使其在 body **彻底发完**(缓冲响应写完 / 流式 SSE 最后一帧发完)时才 Drop 释放预留。
///
/// 背景(finding #3):鉴权中间件返回 `Response` 后即离开 `require_api_key` 作用域,若 guard
/// 只随该作用域 drop,则对流式响应而言预留在 body 一个字节都没发出去前就释放了,并发上限失效。
/// axum 的 body 是惰性的:`Response` 返回时 body 尚未被 poll。把 guard 移入一个包裹 body 的
/// 数据流里(guard 作为该 async 流的局部量),body 全部读完、流 async 块结束时 guard 才 Drop——
/// 从而“预留活到整条响应真正结束”。
///
/// `reservation = None`(无消费上限)时原样返回,不做任何包裹,零额外开销。
///
/// 说明:重建 body 走 `into_data_stream()` + `Body::from_stream()`,保留全部**数据帧**;
/// trailer 帧不透传——本代理的响应(SSE 流 / JSON)不使用 HTTP trailers,故无影响。
/// 响应头(含 content-length/​content-type)不动,只替换 body 包装。
fn attach_reservation_to_response(
    response: Response,
    reservation: Option<crate::apikey::SpendReservation>,
) -> Response {
    let Some(reservation) = reservation else {
        // 无预留(未设消费上限):原样返回,不包裹 body。
        return response;
    };
    let (parts, body) = response.into_parts();
    let data = body.into_data_stream();
    // guard 被 move 进 async 流:流被完整消费(最后一帧读完,async 块自然结束)或被提前
    // drop(客户端断连 → 下游 body drop → 本流 drop)时,`_reservation` 随之 Drop → 释放预留。
    // 二者都在“响应不再产出字节”之后,故预留严格活到整条响应(含流式 body)真正结束。
    // 逐帧 `yield` 原样透传数据帧,不改变响应字节内容。
    let guarded = async_stream::stream! {
        // move 进来:本地量,流生命周期结束即 Drop。
        let _reservation = reservation;
        for await frame in data {
            yield frame;
        }
    };
    let new_body = axum::body::Body::from_stream(guarded);
    Response::from_parts(parts, new_body)
}

/// 统一鉴权闸:提取 key(Authorization: Bearer > x-api-key > x-goog-api-key > query)。
///
/// 判定顺序:
/// 1. 配置了非空全局 key 且常量时间匹配 → 放行(全局模式,不归属 store key)。
/// 2. 否则,协议闸按 store 的 `validate` 裁决:
///    - Valid 且未超消费上限 → 惰性激活 + 把 `ApiKeyId(Some(id))` 塞进请求扩展后放行;
///    - Valid 但已达/超上限 → 402;
///    - Disabled / Expired / NotFound → 401(不泄露具体命中与否细节,统一措辞)。
///    admin 闸跳过本步:store key 不得取得管理员权限。
/// 3. 本闸判定“已配置鉴权”却未命中凭据 → 401。判据按闸不同:
///    - 协议闸:主 `api_key` 非空,或 store 里有任意一条 key;
///    - admin 闸:**只看管理员级凭据**(admin key,回退主 key)。
/// 4. 本闸判定“尚未配置鉴权” → 开放模式放行(首次运行体验)。
pub async fn require_api_key(
    State(auth): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    // 查询串鉴权:原生 EventSource(SSE)无法设自定义头,只能把 key 放进 URL query。
    // 前端日志流用 `?api_key=`,历史契约用 `?token=`,Gemini SDK 用 `?key=`——都接受,
    // 优先级见 [`query_param_key`]。
    let query_key = request.uri().query().and_then(query_param_key);
    let key = extract_key(request.headers(), query_key.as_deref());

    // 期望全局 key 每请求实时解析:接了 runtime_cfg 时读运行期值(auth key 轮换即时生效),
    // 否则退回静态 api_key。快照到局部,后续常量时间比较与开放模式判定都基于同一份。
    let expected_global = auth.expected_key();
    let global_configured = expected_global.as_deref().is_some_and(|k| !k.is_empty());

    // 1) 全局 key 优先:命中即放行(不做 store key 归属)。
    if let Some(expected) = &expected_global
        && !expected.is_empty()
        && verify(key.as_deref(), expected)
    {
        return next.run(request).await;
    }

    // 本闸是否已配置鉴权(已配置 → 未命中凭据的请求必须 401;未配置 → 开放模式)。
    // 判据按闸分开,**不能共用一套**:
    // - 协议闸:期望主 key 非空,或 store 里有任意一条 key。数据面一旦发出过 key 就说明
    //   鉴权已启用,裸请求必须拒。
    // - admin 闸:只认管理员级凭据(admin key,回退主 key),store key 一概不算。
    //   若让 store key 把管理闸判成"已配置",首次运行会自锁:全新部署没有任何全局 key
    //   时管理面按设计是开放的(供操作者做初始配置),操作者在这个开放的管理面里建出
    //   第一条 API-KEY 的瞬间,管理闸就变成"已配置"→ 下一个 /api/admin/* 请求 401,而
    //   store key 在本闸永不放行,产品内再没有任何补救入口,操作者被永久锁在门外。
    //   故管理闸的开闭只能由管理员级凭据决定:设了才关,没设就一直开着——开放期间操作者
    //   可经 PUT /api/admin/config/auth-keys 设 admin key,设完立即收口(即时生效+落盘)。
    //   "没设 admin/主 key 时管理面开放"这一暴露面由启动告警显式点名,见 `auth_startup_warnings`。
    let auth_configured = match auth.role {
        AuthRole::Protocol => {
            global_configured || auth.api_keys.as_ref().is_some_and(|s| !s.is_empty())
        }
        AuthRole::Admin => global_configured,
    };

    // 2) store key 路径:仅协议闸把 store key 当作凭据;admin 闸不在此放行,
    //    store key 不得取得管理员权限(它在 admin 闸也不参与上面的已配置判定)。
    if auth.role == AuthRole::Protocol
        && let Some(store) = &auth.api_keys
        && let Some(provided) = key.as_deref()
        && auth_configured
    {
        let now = now_utc();
        match store.validate(provided, now) {
            ApiKeyAuthResult::Valid {
                id,
                spending_limit,
                limit_unit,
                ..
            } => {
                // 消费上限:原子 reserve-then-reconcile(闭合 check-then-act 的并发窗口)。
                // 读当前已花用量后,在 store 的预留锁内**单一临界区**原子判
                // `已花 + 在途预留 + 本次预估 <= 上限`,通过即累加预留并拿到 RAII guard。
                //
                // finding #3:guard 必须活到**整条响应(含流式 body)真正发完**为止,而非
                // 只活到 `next.run` 返回。axum 的 `next.run(request).await` 返回的是一个 body
                // 尚未被 poll 的 `Response`——对流式(SSE)响应,此刻 body 一个字节都还没发出去。
                // 若 guard 在此作用域结束(即 `require_api_key` return 处)就 drop,则预留在流式
                // 消费**开始前**已释放,并发上限形同虚设。故对准入放行的响应,把 guard **移入
                // response body**,让 Drop 在 body 流(缓冲或流式一视同仁)彻底读完时才触发。
                //
                // finding #7:`spent` 快照先读好(stats 是 async 锁,无法与 sync 预留锁合并),
                // 随后 `try_reserve_spend` 在同一把 sync 锁内原子完成“读 reserved→判越界→写 reserved”,
                // 二者之间无 await;reserved 锁内累加,故并发请求不会据同一 spent 各自越界放行。
                let reservation = if let Some(limit) = spending_limit
                    && let Some(stats) = &auth.stats
                {
                    let spent = current_spent(stats, id, &limit_unit).await;
                    let est = est_cost_in_unit(&limit_unit);
                    match store.try_reserve_spend(id, spent, limit, est) {
                        Ok(guard) => Some(guard),
                        Err(()) => {
                            return payment_required("api key spending limit exceeded");
                        }
                    }
                } else {
                    None
                };
                // 惰性激活(首次使用才据 duration_days 定 expires_at;已激活幂等)。
                let _ = store.activate_key(id, now);
                request.extensions_mut().insert(ApiKeyId(Some(id)));
                let response = next.run(request).await;
                // 把预留 guard 绑定到响应 body 的生命周期:body 全部发完(流式亦然)才释放预留。
                return attach_reservation_to_response(response, reservation);
            }
            ApiKeyAuthResult::Disabled => return unauthorized("api key disabled"),
            ApiKeyAuthResult::Expired => return unauthorized("api key expired"),
            ApiKeyAuthResult::NotFound => return unauthorized("invalid api key"),
        }
    }

    // 3) 未命中本闸接受的任何凭据:本闸已配置鉴权就必须拒。
    if auth_configured {
        return unauthorized("invalid api key");
    }
    // 4) 本闸尚未配置鉴权(协议闸:主 key 与 store 全空;admin 闸:admin/主 key 全空)→ 开放模式。
    next.run(request).await
}

/// 该 store key 当前已花用量,按 limit_unit 归一(与消费上限同单位以便直接算术比较)。
/// "credits" 单位下 credits = cost / 0.72;其余(含 "usd")直接取 cost。
async fn current_spent(stats: &StatsManager, api_key_id: u32, limit_unit: &str) -> f64 {
    let summary = stats.get_summary_by_api_key(api_key_id).await;
    if limit_unit.eq_ignore_ascii_case("credits") {
        summary.total_cost / CREDITS_PER_USD_DIVISOR
    } else {
        summary.total_cost
    }
}

/// 单次在途请求的预留额,按 limit_unit 归一(与 `current_spent`/上限同单位)。
/// credits 单位下把 USD 名义预估同步换算成 credits;其余按 USD。
fn est_cost_in_unit(limit_unit: &str) -> f64 {
    if limit_unit.eq_ignore_ascii_case("credits") {
        EST_COST_PER_REQUEST_USD / CREDITS_PER_USD_DIVISOR
    } else {
        EST_COST_PER_REQUEST_USD
    }
}

/// 当前 UTC 时刻(注入点集中在此,便于与 store 的注入时钟规约对齐)。
fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

/// 从 URL query 串提取用于鉴权的 key:优先 `api_key`(SSE EventSource 用),
/// 回退 `token`(历史契约),再回退 `key`(Gemini 生态标准的 `?key=<API key>`)。
/// 原生 EventSource 不能设自定义头,故 query 参数是浏览器端 SSE 唯一的携带 key 通道;
/// 官方 Gemini SDK 则默认把 key 放在 `?key=`。
pub fn query_param_key(query: &str) -> Option<String> {
    let pairs: Vec<(std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>)> =
        url::form_urlencoded::parse(query.as_bytes()).collect();
    for name in ["api_key", "token", "key"] {
        if let Some((_, v)) = pairs.iter().find(|(k, _)| k == name) {
            return Some(v.clone().into_owned());
        }
    }
    None
}

/// 从请求提取调用方 key:优先级
/// Authorization: Bearer > x-api-key > x-goog-api-key > query(api_key/token/key)。
///
/// `x-goog-api-key` 与 `?key=` 是 Gemini 生态的标准凭据通道:官方 SDK 只会走这两条,
/// 不带 `Authorization`/`x-api-key`,故 Gemini 兼容路由必须认它们,否则携带正确 key 也 401。
pub fn extract_key(headers: &HeaderMap, query_key: Option<&str>) -> Option<String> {
    if let Some(v) = headers.get("authorization").and_then(|h| h.to_str().ok())
        && let Some(rest) = v.strip_prefix("Bearer ")
    {
        return Some(rest.trim().to_string());
    }
    if let Some(v) = headers.get("x-api-key").and_then(|h| h.to_str().ok()) {
        return Some(v.trim().to_string());
    }
    if let Some(v) = headers.get("x-goog-api-key").and_then(|h| h.to_str().ok()) {
        return Some(v.trim().to_string());
    }
    query_key.map(|s| s.to_string())
}

/// 常量时间比较,避免时序侧信道。
pub fn verify(provided: Option<&str>, expected: &str) -> bool {
    let Some(p) = provided else { return false };
    let pb = p.as_bytes();
    let eb = expected.as_bytes();
    if pb.len() != eb.len() {
        return false;
    }
    pb.ct_eq(eb).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use axum::{Router, middleware};
    use tower::ServiceExt;

    #[test]
    fn verify_is_constant_time_equal() {
        assert!(verify(Some("sk-abc"), "sk-abc"));
        assert!(!verify(Some("sk-abc"), "sk-xyz"));
        assert!(!verify(None, "sk-abc"));
        assert!(!verify(Some("sk-ab"), "sk-abc")); // 长度不等
    }

    async fn ok_handler() -> &'static str {
        "ok"
    }

    fn guarded_router(api_key: Option<String>) -> Router {
        Router::new()
            .route("/protected", get(ok_handler))
            .layer(middleware::from_fn_with_state(
                AuthState::global_only(api_key),
                require_api_key,
            ))
    }

    /// runtime_cfg 接入的协议闸:改写运行期 api_key 后,新 key 放行、旧 key 401——
    /// 即 auth key 轮换无需重建 router / 重启即时生效。
    #[tokio::test]
    async fn runtime_key_rotation_takes_effect_live() {
        use crate::config::{Config, shared_runtime_config};
        let cfg = Config {
            api_key: Some("old-key".into()),
            ..Config::default()
        };
        let rc = shared_runtime_config(&cfg);
        let auth = AuthState {
            api_key: None,
            runtime_cfg: Some(rc.clone()),
            role: AuthRole::Protocol,
            api_keys: None,
            stats: None,
        };
        let app = Router::new()
            .route("/protected", get(ok_handler))
            .layer(middleware::from_fn_with_state(auth, require_api_key));

        // 旧 key 起初有效。
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer old-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 运行期轮换 key。
        rc.write().api_key = Some("new-key".into());

        // 旧 key 现在 401。
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer old-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 新 key 放行。
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer new-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// admin 角色:admin_api_key 非空时用它;为空/None 回退主 api_key。运行期改写即时生效。
    #[tokio::test]
    async fn admin_role_prefers_admin_key_then_falls_back_live() {
        use crate::config::{Config, shared_runtime_config};
        let cfg = Config {
            api_key: Some("main".into()),
            admin_api_key: None,
            ..Config::default()
        };
        let rc = shared_runtime_config(&cfg);
        let path = tmp_store_path("adminrole");
        let _ = std::fs::remove_file(&path);
        let auth = AuthState::admin(rc.clone(), ApiKeyStore::load(&path));
        let app = Router::new()
            .route("/protected", get(ok_handler))
            .layer(middleware::from_fn_with_state(auth, require_api_key));

        // admin key 未设 → 回退主 key "main"。
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer main")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 运行期设置 admin key → 此后仅 admin key 放行,主 key 不再作为 admin 通行证。
        rc.write().admin_api_key = Some("adm".into());
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer main")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer adm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let _ = std::fs::remove_file(&path);
    }

    /// 首次运行自锁回归:全新部署(无全局 api_key / admin_api_key)管理面按设计开放,
    /// 操作者在开放的管理面里建出第一条 store key 后,管理面**必须仍然开放**。
    ///
    /// 若 store key 能把管理闸判成"已配置鉴权",建 key 的下一个 /api/admin/* 就 401,而
    /// store key 在 admin 闸永不放行 → 操作者被永久锁死、产品内无补救入口。管理闸的开闭
    /// 只由管理员级凭据决定:本测试同时钉住"设了 admin key 立刻收口"的补救路径。
    #[tokio::test]
    async fn admin_gate_stays_open_when_only_store_keys_exist() {
        use crate::config::{Config, shared_runtime_config};
        let path = tmp_store_path("adminonlystore");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        // 全局 key 与 admin key 均未配置,只有操作者刚建出来的 store key。
        let k = store.create("u1".into(), None, None, None, None, None, Utc::now());
        assert!(!store.is_empty());
        let rc = shared_runtime_config(&Config::default());
        let auth = AuthState::admin(rc.clone(), store);
        let app = Router::new()
            .route("/protected", get(ok_handler))
            .layer(middleware::from_fn_with_state(auth, require_api_key));

        // 核心断言:store 里已有 key,但没有任何管理员级凭据 → 管理面仍开放(不自锁)。
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "首次运行的管理面不得被 store key 锁死"
        );

        // 带 store key 同样通过——走的是开放模式,而非"store key 被当作管理员凭据"
        // (下面配上 admin key 后同一条 store key 立即 401,即可证明这一点)。
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {}", k.key))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 补救路径:操作者经开放的管理面设 admin key(PUT /config/auth-keys 写的就是这个
        // 运行期字段)→ 管理闸即时收口,store key 与裸请求一律 401,只认 admin key。
        rc.write().admin_api_key = Some("adm".into());
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {}", k.key))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer adm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let _ = std::fs::remove_file(&path);
    }

    /// 协议闸不受上面的放宽影响:store 里有 key = 数据面已启用鉴权,不带凭据的请求仍 401
    /// (管理闸放开的只是管理闸自己,协议端点不得跟着裸奔)。
    #[tokio::test]
    async fn protocol_gate_still_requires_key_when_store_has_keys() {
        use crate::config::{Config, shared_runtime_config};
        let path = tmp_store_path("protoonlystore");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let _ = store.create("u1".into(), None, None, None, None, None, Utc::now());
        // 全局 api_key 未配置,只有 store key。
        let auth = AuthState::protocol(
            shared_runtime_config(&Config::default()),
            store,
            StatsManager::load_from_dir(&std::env::temp_dir()),
        );
        let app = store_router(auth);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_file(&path);
    }

    /// 配了 admin key 时管理闸照常收口:不带凭据 → 401(开放模式只属于"一个管理员级凭据都没配")。
    #[tokio::test]
    async fn admin_gate_requires_admin_key_when_configured() {
        use crate::config::{Config, shared_runtime_config};
        let path = tmp_store_path("adminkeyset");
        let _ = std::fs::remove_file(&path);
        let cfg = Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        };
        let auth = AuthState::admin(shared_runtime_config(&cfg), ApiKeyStore::load(&path));
        let app = Router::new()
            .route("/protected", get(ok_handler))
            .layer(middleware::from_fn_with_state(auth, require_api_key));
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 正确 admin key → 放行。
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer adm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let _ = std::fs::remove_file(&path);
    }

    /// 配了全局 key 且 store 里也有 key 时:admin 闸只认全局 key,store key 仍 401。
    #[tokio::test]
    async fn admin_gate_accepts_global_key_but_never_store_key() {
        use crate::config::{Config, shared_runtime_config};
        let path = tmp_store_path("adminglobalonly");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let k = store.create("u1".into(), None, None, None, None, None, Utc::now());
        let cfg = Config {
            admin_api_key: Some("adm".into()),
            ..Config::default()
        };
        let auth = AuthState::admin(shared_runtime_config(&cfg), store);
        let app = Router::new()
            .route("/protected", get(ok_handler))
            .layer(middleware::from_fn_with_state(auth, require_api_key));

        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer adm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {}", k.key))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_file(&path);
    }

    /// 一条鉴权材料都没有(无全局 key、无 admin key、store 为空)时,admin 闸维持首次运行的开放行为。
    #[tokio::test]
    async fn admin_gate_open_when_no_auth_material_at_all() {
        use crate::config::{Config, shared_runtime_config};
        let path = tmp_store_path("adminnomaterial");
        let _ = std::fs::remove_file(&path);
        let auth = AuthState::admin(
            shared_runtime_config(&Config::default()),
            ApiKeyStore::load(&path),
        );
        let app = Router::new()
            .route("/protected", get(ok_handler))
            .layer(middleware::from_fn_with_state(auth, require_api_key));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn rejects_missing_key_with_401_and_no_leak() {
        let app = guarded_router(Some("secret".into()));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("authentication_error"));
        assert!(!text.contains("secret"));
    }

    #[tokio::test]
    async fn accepts_correct_bearer_key() {
        let app = guarded_router(Some("secret".into()));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn accepts_correct_query_token() {
        let app = guarded_router(Some("secret".into()));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected?token=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// SSE EventSource 契约:`?api_key=<正确>` 放行(原生 EventSource 只能走 query 携带 key)。
    #[tokio::test]
    async fn accepts_correct_query_api_key() {
        let app = guarded_router(Some("secret".into()));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected?api_key=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// `?api_key=<错误>` → 401(query 鉴权拒绝路径)。
    #[tokio::test]
    async fn rejects_wrong_query_api_key() {
        let app = guarded_router(Some("secret".into()));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected?api_key=wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// 同时给 api_key 与 token 时 api_key 优先:api_key 正确即放行(即便 token 错误)。
    #[tokio::test]
    async fn query_api_key_takes_priority_over_token() {
        let app = guarded_router(Some("secret".into()));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected?token=wrong&api_key=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Gemini 生态标准通道之一:`x-goog-api-key` 请求头(官方 SDK 默认携带方式)。
    #[tokio::test]
    async fn accepts_correct_x_goog_api_key_header() {
        let app = guarded_router(Some("secret".into()));
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("x-goog-api-key", "secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 错误 key 仍 401。
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("x-goog-api-key", "wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// Gemini 生态标准通道之二:`?key=<API key>` 查询参数。
    #[tokio::test]
    async fn accepts_correct_query_key_param() {
        let app = guarded_router(Some("secret".into()));
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected?key=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected?key=wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn query_param_key_prefers_api_key_then_token_then_key() {
        assert_eq!(
            query_param_key("api_key=aaa&token=bbb").as_deref(),
            Some("aaa")
        );
        assert_eq!(query_param_key("token=bbb").as_deref(), Some("bbb"));
        // Gemini SDK 的 `?key=`;优先级低于既有两条,不改变既有契约。
        assert_eq!(query_param_key("key=ccc").as_deref(), Some("ccc"));
        assert_eq!(
            query_param_key("key=ccc&api_key=aaa").as_deref(),
            Some("aaa")
        );
        assert_eq!(query_param_key("key=ccc&token=bbb").as_deref(), Some("bbb"));
        assert_eq!(query_param_key("foo=bar").as_deref(), None);
        assert_eq!(query_param_key("").as_deref(), None);
    }

    #[test]
    fn extract_key_header_priority_covers_gemini_channels() {
        use axum::http::HeaderValue;
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-api-key", HeaderValue::from_static("goog"));
        // 只有 x-goog-api-key → 用它。
        assert_eq!(extract_key(&headers, None).as_deref(), Some("goog"));
        // x-api-key 优先于 x-goog-api-key。
        headers.insert("x-api-key", HeaderValue::from_static("xapi"));
        assert_eq!(extract_key(&headers, None).as_deref(), Some("xapi"));
        // Authorization: Bearer 最优先。
        headers.insert("authorization", HeaderValue::from_static("Bearer bearer"));
        assert_eq!(extract_key(&headers, None).as_deref(), Some("bearer"));
        // 头都没有时才回落 query。
        assert_eq!(
            extract_key(&HeaderMap::new(), Some("fromquery")).as_deref(),
            Some("fromquery")
        );
    }

    #[tokio::test]
    async fn open_mode_allows_any_request_when_key_is_none() {
        let app = guarded_router(None);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn open_mode_allows_any_request_when_key_is_empty_string() {
        let app = guarded_router(Some(String::new()));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ---- store-backed key path ----------------------------------------

    use axum::Extension;
    use chrono::Utc;

    fn tmp_store_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "kiro2api_authtest_{}_{}.json",
            tag,
            std::process::id()
        ))
    }

    /// 回显解析出的 ApiKeyId 扩展(None → "none",Some(id) → id 字符串)。
    async fn echo_key_id(ext: Option<Extension<ApiKeyId>>) -> String {
        match ext {
            Some(Extension(ApiKeyId(Some(id)))) => id.to_string(),
            _ => "none".to_string(),
        }
    }

    fn store_router(auth: AuthState) -> Router {
        Router::new()
            .route("/protected", get(echo_key_id))
            .layer(middleware::from_fn_with_state(auth, require_api_key))
    }

    async fn body_text(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[tokio::test]
    async fn store_key_valid_passes_and_inserts_extension() {
        let path = tmp_store_path("valid");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let k = store.create("u1".into(), None, None, None, None, None, Utc::now());
        let stats = StatsManager::load_from_dir(&std::env::temp_dir());
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(stats),
        };
        let app = store_router(auth);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {}", k.key))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_text(resp).await, k.id.to_string());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn global_key_wins_and_does_not_attribute_store_key() {
        let path = tmp_store_path("globalwins");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let _ = store.create("u1".into(), None, None, None, None, None, Utc::now());
        let auth = AuthState {
            api_key: Some("secret".into()),
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(StatsManager::load_from_dir(&std::env::temp_dir())),
        };
        let app = store_router(auth);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // 走全局 key → 不归属任何 store key。
        assert_eq!(body_text(resp).await, "none");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn store_key_disabled_rejected_401() {
        let path = tmp_store_path("disabled");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let k = store.create("u1".into(), None, None, None, None, None, Utc::now());
        store.disable(k.id);
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(StatsManager::load_from_dir(&std::env::temp_dir())),
        };
        let app = store_router(auth);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {}", k.key))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn store_key_expired_rejected_401() {
        let path = tmp_store_path("expired");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let past = Utc::now() - chrono::Duration::days(1);
        let k = store.create(
            "u1".into(),
            Some(past),
            None,
            None,
            None,
            None,
            past - chrono::Duration::days(1),
        );
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(StatsManager::load_from_dir(&std::env::temp_dir())),
        };
        let app = store_router(auth);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {}", k.key))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn store_key_unknown_rejected_401_when_store_has_keys() {
        let path = tmp_store_path("unknown");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let _ = store.create("u1".into(), None, None, None, None, None, Utc::now());
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(StatsManager::load_from_dir(&std::env::temp_dir())),
        };
        let app = store_router(auth);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", "Bearer sk-nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn empty_store_no_global_key_is_open_mode() {
        let path = tmp_store_path("emptyopen");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path); // 空 store
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(StatsManager::load_from_dir(&std::env::temp_dir())),
        };
        let app = store_router(auth);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_text(resp).await, "none"); // 开放模式无归属
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn store_key_over_spending_limit_rejected_402() {
        let path = tmp_store_path("overlimit");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        // usd 上限 1.0
        let k = store.create(
            "u1".into(),
            None,
            Some(1.0),
            Some("usd".into()),
            None,
            None,
            Utc::now(),
        );
        let stats = StatsManager::load_from_dir(&std::env::temp_dir());
        // 该 key 已消费 1.5 USD(达/超 1.0 上限)。
        stats
            .usage
            .record_usage_with_api_key(1, k.id, "m".into(), 10, 20, 1.5, None, None, None, 1000)
            .await;
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(stats),
        };
        let app = store_router(auth);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {}", k.key))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
        let _ = std::fs::remove_file(&path);
    }

    /// finding #3:预留必须活到**流式响应 body 发完**为止,而非只活到中间件返回。
    /// 构造一个多帧流式响应:body 还没被消费时预留应仍占额(reserved>0);
    /// 只有把 body 完整读完后预留才归零并从 map 移除。
    #[tokio::test]
    async fn reservation_held_until_streaming_body_fully_consumed() {
        use futures_core::Stream;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        let path = tmp_store_path("stream_hold");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        // usd 上限 10.0(足够放行一次 est=1.0 的预留)。
        let k = store.create(
            "u1".into(),
            None,
            Some(10.0),
            Some("usd".into()),
            None,
            None,
            Utc::now(),
        );
        let store_probe = store.clone();
        let key_id = k.id;
        let stats = StatsManager::load_from_dir(&std::env::temp_dir());
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(stats),
        };

        // handler 返回一个 3 帧的流式 body(每帧一小段 bytes)。
        async fn streaming_handler() -> Response {
            let s = async_stream::stream! {
                for i in 0..3u8 {
                    yield Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![b'a' + i]));
                }
            };
            Response::new(axum::body::Body::from_stream(s))
        }

        let app = Router::new()
            .route("/protected", get(streaming_handler))
            .layer(middleware::from_fn_with_state(auth, require_api_key));

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {}", k.key))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 拿到 Response 但 body 尚未消费:预留必须仍占额(finding #3 的核心断言)。
        assert_eq!(
            store_probe.reserved_amount(key_id),
            1.0,
            "body 未消费前预留必须仍被持有"
        );

        // 手动逐帧 poll body,确认全程消费完毕前预留一直在,消费完才释放。
        let mut stream = resp.into_body().into_data_stream();
        let mut collected = Vec::new();
        {
            let mut pinned = Pin::new(&mut stream);
            let mut cx = Context::from_waker(futures_util_noop_waker());
            // 第一帧后预留仍应在。
            loop {
                match pinned.as_mut().poll_next(&mut cx) {
                    Poll::Ready(Some(Ok(b))) => {
                        collected.extend_from_slice(&b);
                        // 每读到一帧、流尚未结束时,预留仍应 >0。
                        assert!(
                            store_probe.reserved_amount(key_id) > 0.0,
                            "流未结束前预留必须仍被持有"
                        );
                    }
                    Poll::Ready(Some(Err(e))) => panic!("body error: {e}"),
                    Poll::Ready(None) => break,
                    Poll::Pending => {
                        // 同步流不应 pending;真 pending 说明测试构造有误。
                        panic!("unexpected pending on synchronous stream");
                    }
                }
            }
        }
        assert_eq!(collected, b"abc");
        // 流已彻底读完 → guard 随 stream 结束 Drop → 预留归零并从 map 移除。
        drop(stream);
        assert_eq!(
            store_probe.reserved_amount(key_id),
            0.0,
            "body 读完后预留必须已释放"
        );
        assert_eq!(
            store_probe.reserved_amount(key_id),
            0.0,
            "释放后 reserved 归零"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 极简 no-op waker,供上面手动 poll 同步流用(无需引入 futures-util)。
    fn futures_util_noop_waker() -> &'static std::task::Waker {
        use std::task::{RawWaker, RawWakerVTable, Waker};
        fn no_op(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
        static WAKER: std::sync::OnceLock<Waker> = std::sync::OnceLock::new();
        WAKER.get_or_init(|| unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) })
    }
}
