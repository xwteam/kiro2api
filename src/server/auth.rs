use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// 单次在途请求对消费上限的“预留额”估算(USD)。准入时无法预知本次真实 cost
/// (cost 依赖响应后的 token 计数),故用一个保守的名义单次上限做在途预留:
/// 把消费上限的并发 check-then-act 收敛为原子 reserve-then-reconcile,令越界
/// 被界定在“至多一次在途请求之内”。真实 cost 仍由既有 stats 路径在请求完成后记账,
/// 预留只在请求在途期间临时占额、完成即释放(RAII),不参与持久记账。
/// 取值口径:一次大请求的量级上限的保守估计;偏大只会让接近上限时更早 402(更保守),
/// 不会漏放导致无界超支。credits 单位下按 CREDITS_PER_USD_DIVISOR 同步换算。
/// 取值依据:单次请求实测成本约 $0.0003(输出千余 token 量级),取 $0.05 留约两个数量级
/// 余量以覆盖长上下文大请求。**必须 >= 单次真实花费**,这是 SpendCache 复用快照不漏放的
/// 前提(见 `spent_for_admission`);同时不能过大 —— 上限本身若小于一次预留,这把 key 从
/// 签发起就发不出任何请求。旧值 1.0 USD 是后者的反例(实测成本的三千多倍)。
const EST_COST_PER_REQUEST_USD: f64 = 0.05;

/// credits 单位下的单次在途预留(credits 原生,不由 USD 除算)。
///
/// 曾经写作 `EST_COST_PER_REQUEST_USD / 0.72` = 1.389 credits。那是 cost 反算时代的产物:
/// 当时"已花"也是反算的极小值(实测 0.0037),两个错配的量纲凑在一起看不出问题。改用真实
/// credits 后,已花是真实量级(实测约 0.137/次),再配 1.389 的预留就成了 10 倍超额预留 ——
/// 一个 2 credits 的上限会在真实只用掉 0.6 时就开始 402,用户看着还剩七成却发不出请求。
/// 取 1.0:Kiro 自身的名义口径(一次请求 ≈ 1 credit),仍比实测保守约 7 倍,
/// 偏大只会更早 402(保守方向),不会漏放。
/// 取值依据:单次请求实测约 0.137 credits,取 0.25 留约一倍余量。
///
/// 旧值 1.0 是 credits 由 cost 反算时代的产物,当时"已花"也是反算的极小值,两个错配的量纲
/// 凑在一起看不出问题。改用真实 credits 后,1.0 的预留会让一个 1 credit 的上限从第一发起
/// 就 `0.08 + 1.0 > 1.00` → 402:面板显示 0.08/1.00 还剩九成,请求却一个都发不出去(实际踩过)。
/// 与 USD 同理,此值**必须 >= 单次真实花费**(SpendCache 正确性前提),又不能吃掉整个上限。
const EST_CREDITS_PER_REQUEST: f64 = 0.25;

/// 鉴权闸解析出的 store-key id,经请求扩展下传给 relay 做用量归属。
/// `None` = 全局 key / 开放模式(无 store-key 归属,relay 记 id=0)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApiKeyId(pub Option<u32>);

/// 鉴权闸解析出的**账号绑定白名单**:本次请求只允许使用这些上游凭据 id。
/// 与 [`ApiKeyId`] 同规约,经请求扩展下传给 relay 的选号层。
///
/// 契约(选号层必须照此裁决):
/// - 扩展**缺席** = 不受限(全局 key / 开放模式 / 未绑定的 store key)——热路径原样选号,零开销;
/// - 扩展**在场** = 只准从 `.0` 列出的凭据 id 里选,成员判定一律走 [`BoundCredentialIds::allows`]。
///
/// 为什么要在鉴权闸解析而不是让选号层自己再查一遍 store:绑定值必须与本次鉴权命中的那条 key
/// **同一份快照**(闸内已持有 `validate` 的结果),否则请求在途时管理员改绑定会出现"按旧 key
/// 放行、按新绑定选号"的错配;且热路径可省掉一次 store 读锁。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundCredentialIds(pub Vec<u64>);

impl BoundCredentialIds {
    /// 池里的凭据 id(String)是否落在白名单内。
    ///
    /// 池 id 是任意字符串(见 `kiro::credential` 的 flexible 反序列化),而绑定列表是数值 id
    /// (管理面按账号数值 id 勾选),故解析不出数值的 id 一律**不放行**(fail-closed):
    /// 绑定的语义是"只准用这几个账号",宁可选不出账号(上游 503)也不能把请求漏给未授权账号。
    pub fn allows(&self, credential_id: &str) -> bool {
        match credential_id.parse::<u64>() {
            Ok(n) => self.0.contains(&n),
            Err(_) => false,
        }
    }
}

/// 把 store 里的绑定字段收敛成"受限白名单":`None` 与**空列表**都表示不受限,返回 `None`。
///
/// 空列表按"不受限"处理是为了与管理面显示保持一致:面板把 `boundCredentialIds` 为空的 key
/// 归进"全局策略(未绑定)"那一组(admin-ui-v2/js/sec-apikeys.js 的分组判据就是 `length > 0`)。
/// 若这里把空列表当成"一个账号都不准用",面板上显示为未绑定的 key 会在数据面被判死(选不出
/// 账号),显示与行为对不上——绑定这类只在管理面配置的策略,最忌看到的和执行的两回事。
fn restricted_binding(bound: Option<Vec<u64>>) -> Option<BoundCredentialIds> {
    bound.filter(|ids| !ids.is_empty()).map(BoundCredentialIds)
}

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
    /// 消费上限准入用的"已花额"快照缓存(见 [`SpendCache`])。
    /// `AuthState` 每请求被 axum 克隆一次,故这里必须是 `Arc`——缓存要在整个路由生命周期里共享,
    /// 克隆一份新的等于每请求都从空缓存开始,又退回到每请求全量扫描账本。
    pub spend_cache: Arc<SpendCache>,
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
            spend_cache: Arc::new(SpendCache::default()),
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
            spend_cache: Arc::new(SpendCache::default()),
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
            spend_cache: Arc::new(SpendCache::default()),
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
///    - Valid 且未超消费上限 → 惰性激活 + 把 `ApiKeyId(Some(id))`(以及该 key 若设了账号绑定,
///      再加一个 [`BoundCredentialIds`])塞进请求扩展后放行;
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
                bound_credential_ids,
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
                    let est = est_cost_in_unit(&limit_unit);
                    // `spent` 走快照缓存:直接调 current_spent 等于**每个请求**都持读锁线性扫描
                    // 整个用量账本(生产上千万级条目),记账写入还得排在它后面。快照只在
                    // "连本次在内都够不到上限"时复用,故不会漏放,见 SpendCache。
                    match spent_for_admission(&auth.spend_cache, stats, id, &limit_unit, limit, est)
                        .await
                    {
                        // 快照已证明"再放一次的预留都放不下":直接 402。不必再扫账本
                        // (扫完必然还是这个结论),也不必进预留锁。
                        Admission::Exhausted => {
                            return payment_required("api key spending limit exceeded");
                        }
                        Admission::Spent(spent) => {
                            match store.try_reserve_spend(id, spent, limit, est) {
                                Ok(guard) => Some(guard),
                                Err(()) => {
                                    return payment_required("api key spending limit exceeded");
                                }
                            }
                        }
                    }
                } else {
                    None
                };
                // 惰性激活(首次使用才据 duration_days 定 expires_at;已激活幂等)。
                let _ = store.activate_key(id, now);
                request.extensions_mut().insert(ApiKeyId(Some(id)));
                // 账号绑定随本次鉴权结果一起下传:管理面把 key 绑到某几个账号后,这份白名单
                // 必须真正抵达选号层,否则绑定只停留在存储与面板展示上,任何 store key 仍能
                // 消费池里任意账号(绑定形同虚设)。只有受限时才插扩展:未绑定的 key 与全局
                // key 走原路径、扩展缺席 = 不限,热路径无任何额外开销与语义变化。
                if let Some(bound) = restricted_binding(bound_credential_ids) {
                    request.extensions_mut().insert(bound);
                }
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
///
/// **代价**:委托到 `usage` 层的 `summary_for_api_key`,那是持读锁**线性扫描整个用量账本**
/// (记录上限是 每凭据 10_000 × 账号数,生产上约千个账号 = 千万级条目),并为每条命中记录克隆一次
/// model 串。故不能每个请求都调 —— 调用方一律走 [`SpendCache`] 的快照路径,只在快照不再安全时
/// 才落到这里重算。
async fn current_spent(stats: &StatsManager, api_key_id: u32, limit_unit: &str) -> f64 {
    let summary = stats.get_summary_by_api_key(api_key_id).await;
    if limit_unit.eq_ignore_ascii_case("credits") {
        // 真实 credits,不由 cost 反算。闸门比错了数比面板显示错更糟:面板错你看得见,
        // 闸门错的表现是「限额设了却永远不触发」——以为有保护,其实没有。
        summary.total_credits
    } else {
        summary.total_cost
    }
}

/// 已花额快照的最长有效期。
///
/// 快照本身的"不漏放"由下面的名义上界判据保证,与时间无关;TTL 只负责**向下修正**:
/// 管理员清空某 key 的用量、或账本按每凭据上限淘汰旧记录时,真实已花额会变小,快照偏高会误报 402。
/// 取 3s:足够短到运营者点完"重置用量"几乎立刻见效,又足够长到把热路径的全量扫描收敛成每秒至多一次。
const SPEND_CACHE_TTL: Duration = Duration::from_secs(3);

/// 快照表的容量上限(按 store key 计)。超出即先清过期项、仍超则整表清空(下一请求自然重建)。
/// 纯防御:正常情况下条目数 = 设了消费上限的 key 数(数千量级),这里只保证它不会无界增长。
const SPEND_CACHE_MAX_ENTRIES: usize = 4096;

/// 某 store key 的"已花额"快照,以及快照之后本闸放行过多少次请求。
struct SpendSnapshot {
    /// 取快照时全量聚合出的已花额(按 `unit` 归一)。
    spent: f64,
    /// 快照所用的计量单位(key 的 limit_unit 被改过就必须重算:credits 与 usd 差 0.72 倍)。
    unit: String,
    /// 取快照的时刻(单调钟,不受系统时钟回拨影响)。
    taken: Instant,
    /// 快照之后经本闸放行的请求数。这些请求的真实花费可能尚未进账本,故按名义预估上界计入。
    admitted_since: u32,
}

/// 消费上限准入用的"已花额"快照缓存。
///
/// 为什么需要它:准入判据要拿"该 key 已花多少",而这个数只能靠扫描整个用量账本聚合出来
/// (见 [`current_spent`])。账本可以长到千万条,而这段扫描落在**每一个**带消费上限的请求的
/// 关键路径上,并且持的是账本读锁 —— 记账写入(`record_usage`)得排在它后面。上量之后这会
/// 从"慢"直接变成"互相拖死"。
///
/// 为什么复用快照仍然不会漏放(关键不变量):设快照时刻已花 `S`,此后经本闸放行 `n` 次请求。
/// 一条用量记录只可能由**经本闸放行的请求**产生,而单次请求的花费按既有预留口径不超过名义预估
/// `est`([`EST_COST_PER_REQUEST_USD`]),故此刻真实已花 `S_true <= S + n * est`。
/// 复用的判据取 `S + (n+1) * est <= limit`(把本次也算上),它蕴含 `S_true + est <= limit` ——
/// 而后者正是精确路径的准入口径本身。也就是说:复用快照放行的请求,拿精确值算同样会放行,
/// 二者一致而非放宽。一旦这个上界够到上限就落回全量重算 —— 于是"逼近上限"的那一小撮 key 仍
/// 逐请求按精确值裁决(与修复前完全一致),而绝大多数远离上限的请求不再扫账本。
///
/// **拒的方向同样走快照**,否则"额度已用完"的 key 就是一台免费的扫描发生器:消费上限是
/// 终身总额,用完即**永久**状态,而这些请求横竖都要被 402 拒掉 —— 每拒一次却要先全量扫一遍
/// 账本(快照的名义上界必然够到上限,只能落回精确重算),纯属浪费,且客户端一个重试循环就能
/// 让服务端持着账本读锁反复扫、把记账写入全堵在后面。判据取 `S + est > limit`:账本只增不减
/// (记录只追加),故真实已花 `S_true >= S`,精确路径同样会拒(`try_reserve_spend` 还要再加上
/// 在途预留,只会更严),二者一致而非放宽。
///
/// 反过来的方向(快照偏高导致误报 402)由 [`SPEND_CACHE_TTL`] 兜住:能让 `S_true < S` 的只有
/// "管理员清用量"与"账本按每凭据上限淘汰旧记录"两种情形,至多 3s 后必然重算。上限本身**每请求**
/// 从 store 现读,不进快照,故管理员调高上限对已判定用完的 key 也是立即生效。
#[derive(Default)]
pub struct SpendCache {
    entries: parking_lot::Mutex<std::collections::HashMap<u32, SpendSnapshot>>,
    /// 累计的全量账本扫描次数。用于回归测试钉住"多次请求只扫一次账本";正常运行也可作观测量。
    full_scans: std::sync::atomic::AtomicU64,
}

impl SpendCache {
    /// 迄今为止的全量账本扫描次数。
    pub fn full_scans(&self) -> u64 {
        self.full_scans.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 据快照裁决本次请求:放行(带已花额)/ 直接拒 / 无法裁决(调用方全量重算)。
    ///
    /// 判据见 [`SpendCache`] 的不变量说明。放行判据里 `upper_bound` 的 `+1` 是把**本次**请求
    /// 也算进去,保证复用发生在"连本次都还够不到上限"时;拒的判据是精确路径的准入口径本身
    /// (`已花 + 单次预留 > 上限`),两者不重叠且拒优先。
    fn decide(&self, id: u32, unit: &str, limit: f64, est: f64, now: Instant) -> SpendVerdict {
        // 非有限的上限/预估一律走慢路径,由 `try_reserve_spend` 按同一口径保守拒绝:
        // NaN 参与的比较恒为 false,`upper_bound > limit` 会被误判成"没超",从而漏放(与 apikey
        // 层 `try_reserve_spend` 里"非有限即保守处理"同规约)。
        if !limit.is_finite() || !est.is_finite() {
            return SpendVerdict::Recompute;
        }
        let mut map = self.entries.lock();
        let Some(snap) = map.get_mut(&id) else {
            return SpendVerdict::Recompute;
        };
        if snap.unit != unit || now.duration_since(snap.taken) >= SPEND_CACHE_TTL {
            return SpendVerdict::Recompute;
        }
        if !snap.spent.is_finite() {
            return SpendVerdict::Recompute;
        }
        // 拒的方向:快照已花额连一次预留都放不下 → 精确值只会更大,重算改变不了结论。
        if snap.spent + est > limit {
            return SpendVerdict::Exhausted;
        }
        let upper_bound = snap.spent + f64::from(snap.admitted_since.saturating_add(1)) * est;
        if upper_bound > limit {
            return SpendVerdict::Recompute;
        }
        snap.admitted_since = snap.admitted_since.saturating_add(1);
        SpendVerdict::Admit(snap.spent)
    }

    /// 记下一次全量重算的结果。`admitted_since` 从 1 起(本次请求即算一次放行)。
    fn store(&self, id: u32, unit: &str, spent: f64, now: Instant) {
        let mut map = self.entries.lock();
        if map.len() >= SPEND_CACHE_MAX_ENTRIES && !map.contains_key(&id) {
            map.retain(|_, s| now.duration_since(s.taken) < SPEND_CACHE_TTL);
            if map.len() >= SPEND_CACHE_MAX_ENTRIES {
                map.clear();
            }
        }
        map.insert(
            id,
            SpendSnapshot {
                spent,
                unit: unit.to_string(),
                taken: now,
                admitted_since: 1,
            },
        );
    }
}

/// [`SpendCache::decide`] 的裁决结果。
#[derive(Debug, PartialEq)]
enum SpendVerdict {
    /// 快照证明"连本次在内都够不到上限"→ 按快照的已花额放行,不扫账本。
    Admit(f64),
    /// 快照证明"连一次预留都放不下"→ 直接 402,不扫账本(见 [`SpendCache`] 的"拒的方向")。
    Exhausted,
    /// 快照不足以裁决(缺失/过期/换了计量单位/名义上界够到上限)→ 全量重算。
    Recompute,
}

/// 准入闸对本次请求的判断。
enum Admission {
    /// 可以继续走预留:附带本次判据所用的已花额(快照或全量重算所得)。
    Spent(f64),
    /// 已花额加上一次预留就越界 → 本次必被 402,无需再走预留锁。
    Exhausted,
}

/// 准入判据用的"已花额":能安全复用快照就复用;快照已证明必被拒则直接返回
/// [`Admission::Exhausted`](省掉一次结论注定不变的全量扫描);否则全量重算并刷新快照。
/// 语义与直接调 [`current_spent`] 一致(见 [`SpendCache`] 的不变量),只是省掉绝大多数账本扫描。
async fn spent_for_admission(
    cache: &SpendCache,
    stats: &StatsManager,
    id: u32,
    limit_unit: &str,
    limit: f64,
    est: f64,
) -> Admission {
    match cache.decide(id, limit_unit, limit, est, Instant::now()) {
        SpendVerdict::Admit(spent) => return Admission::Spent(spent),
        SpendVerdict::Exhausted => return Admission::Exhausted,
        SpendVerdict::Recompute => {}
    }
    cache
        .full_scans
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let spent = current_spent(stats, id, limit_unit).await;
    cache.store(id, limit_unit, spent, Instant::now());
    // 重算后仍由 `try_reserve_spend` 统一裁决(它还要叠加在途预留),保持单一判定点。
    Admission::Spent(spent)
}

/// 单次在途请求的预留额,按 limit_unit 归一(与 `current_spent`/上限同单位)。
/// credits 单位下把 USD 名义预估同步换算成 credits;其余按 USD。
///
/// 对外 `pub`:用户面板要按**与本闸完全相同**的算式判断"这把 key 还发不发得出请求"
/// (见 `crate::user::handler::spending_exhausted`)。面板若自己另算一套(例如只比
/// `已花 >= 上限`),就会在"还差不到一次预留"的那段区间里显示绿色的正常,而中继对同一把
/// key 每一发请求都回 402 —— 展示与执行两回事,用户无从判断自己到底还能不能用。
pub fn est_cost_in_unit(limit_unit: &str) -> f64 {
    if limit_unit.eq_ignore_ascii_case("credits") {
        EST_CREDITS_PER_REQUEST
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
            spend_cache: Arc::new(SpendCache::default()),
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
        crate::test_tmp::stable_file(&format!("authtest_{tag}"), "store.json")
    }

    /// 每个用例独占的统计目录:`temp_dir()` 是全局共享的,用量断言不能被别的用例
    /// 落在同一份 usage_records.json 里的记录串扰。
    fn tmp_stats_dir(tag: &str) -> std::path::PathBuf {
        crate::test_tmp::dir(&format!("authstats_{tag}"))
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
            spend_cache: Arc::new(SpendCache::default()),
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

    /// 回显解析出的账号绑定白名单:扩展缺席 → "unbound",在场 → "3,7" 形式的 id 列表。
    async fn echo_binding(ext: Option<Extension<BoundCredentialIds>>) -> String {
        match ext {
            Some(Extension(BoundCredentialIds(ids))) => ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(","),
            None => "unbound".to_string(),
        }
    }

    fn binding_router(auth: AuthState) -> Router {
        Router::new()
            .route("/protected", get(echo_binding))
            .layer(middleware::from_fn_with_state(auth, require_api_key))
    }

    fn store_auth(store: Arc<ApiKeyStore>) -> AuthState {
        AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(StatsManager::load_from_dir(&std::env::temp_dir())),
            spend_cache: Arc::new(SpendCache::default()),
        }
    }

    /// 回归(账号绑定被丢弃):管理面给 key 绑定了账号 [3,7],鉴权闸必须把这份白名单
    /// 经请求扩展下传给选号层。修复前闸内 `..` 直接丢掉 `bound_credential_ids`,
    /// 扩展永不出现 → 绑定只存在于存储与面板展示里,数据面任意账号照用不误。
    #[tokio::test]
    async fn store_key_binding_reaches_request_extensions() {
        let path = tmp_store_path("bound");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let k = store.create(
            "u1".into(),
            None,
            None,
            None,
            None,
            Some(vec![3, 7]),
            Utc::now(),
        );
        let app = binding_router(store_auth(store));
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
        assert_eq!(
            body_text(resp).await,
            "3,7",
            "绑定的凭据白名单必须原样抵达选号层"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 未绑定的 store key:扩展缺席 = 不受限(既有 happy path 不得被收紧)。
    #[tokio::test]
    async fn store_key_without_binding_is_unrestricted() {
        let path = tmp_store_path("unbound");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let k = store.create("u1".into(), None, None, None, None, None, Utc::now());
        let app = binding_router(store_auth(store));
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
        assert_eq!(body_text(resp).await, "unbound");
        let _ = std::fs::remove_file(&path);
    }

    /// 空绑定列表按"不受限"处理:面板把它显示成"全局策略(未绑定)",数据面不得把这类 key
    /// 判死(否则显示与行为不一致,且线上会突然一个账号都选不出)。
    #[tokio::test]
    async fn empty_binding_list_is_treated_as_unrestricted() {
        let path = tmp_store_path("emptybound");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let k = store.create(
            "u1".into(),
            None,
            None,
            None,
            None,
            Some(Vec::new()),
            Utc::now(),
        );
        let app = binding_router(store_auth(store));
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
        assert_eq!(body_text(resp).await, "unbound");
        let _ = std::fs::remove_file(&path);
    }

    /// 走全局 key 放行时不带任何绑定:全局 key 不属于任何 store key,自然不受其绑定约束。
    #[tokio::test]
    async fn global_key_path_carries_no_binding() {
        let path = tmp_store_path("globalnobind");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let _ = store.create(
            "u1".into(),
            None,
            None,
            None,
            None,
            Some(vec![3]),
            Utc::now(),
        );
        let auth = AuthState {
            api_key: Some("secret".into()),
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(StatsManager::load_from_dir(&std::env::temp_dir())),
            spend_cache: Arc::new(SpendCache::default()),
        };
        let app = binding_router(auth);
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
        assert_eq!(body_text(resp).await, "unbound");
        let _ = std::fs::remove_file(&path);
    }

    /// 白名单成员判定:数值 id 命中才放行;池里非数值 id 一律拒(fail-closed)。
    #[test]
    fn binding_allows_only_listed_numeric_ids() {
        let b = BoundCredentialIds(vec![3, 7]);
        assert!(b.allows("3"));
        assert!(b.allows("7"));
        assert!(!b.allows("4"));
        // 池 id 可以是任意字符串;解析不出数值 → 绝不可能在白名单里 → 不放行。
        assert!(!b.allows("a1"));
        assert!(!b.allows(""));
        assert!(!b.allows("03x"));
    }

    /// 收敛规则:None / 空列表 → 不受限;非空 → 受限白名单原样保留。
    #[test]
    fn restricted_binding_normalizes_empty_to_unrestricted() {
        assert_eq!(restricted_binding(None), None);
        assert_eq!(restricted_binding(Some(Vec::new())), None);
        assert_eq!(
            restricted_binding(Some(vec![5, 9])),
            Some(BoundCredentialIds(vec![5, 9]))
        );
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
            spend_cache: Arc::new(SpendCache::default()),
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
            spend_cache: Arc::new(SpendCache::default()),
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
            spend_cache: Arc::new(SpendCache::default()),
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
            spend_cache: Arc::new(SpendCache::default()),
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
            spend_cache: Arc::new(SpendCache::default()),
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
            spend_cache: Arc::new(SpendCache::default()),
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

    /// 关键回归(热路径):带消费上限的 key 连续请求时,用量账本只应被**全量扫描一次**。
    ///
    /// 修复前每个请求都调 `current_spent` → `summary_for_api_key`,那是持读锁线性扫描整个
    /// `records`(每凭据上限 10_000 × 约千个账号 = 千万级条目)并为每条命中记录克隆 model 串;
    /// 而记账写入还得排在这把读锁后面。上量之后这不是"慢一点",是把数据面和记账互相拖死。
    #[tokio::test]
    async fn spending_limited_key_scans_usage_ledger_once_for_many_requests() {
        let path = tmp_store_path("cache_hit");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        // 上限 100 USD,远离上限 → 每次都该走快照。
        let k = store.create(
            "u1".into(),
            None,
            Some(100.0),
            Some("usd".into()),
            None,
            None,
            Utc::now(),
        );
        let cache = Arc::new(SpendCache::default());
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(StatsManager::load_from_dir(&tmp_stats_dir("cache_hit"))),
            spend_cache: cache.clone(),
        };
        let app = store_router(auth);

        for i in 0..5 {
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
            assert_eq!(resp.status(), StatusCode::OK, "第 {i} 次请求应放行");
            // 读完 body 释放在途预留(预留 guard 绑在 body 生命周期上)。
            let _ = body_text(resp).await;
        }
        assert_eq!(
            cache.full_scans(),
            1,
            "5 次请求只该扫一次账本,实际扫了 {} 次",
            cache.full_scans()
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 缓存不得削弱消费上限:快照的名义上界一够到上限就必须落回精确重算,
    /// 并按重算出的真实已花额把超限请求拒成 402。
    #[tokio::test]
    async fn spend_cache_rescans_and_still_enforces_limit_near_the_cap() {
        let path = tmp_store_path("cache_cap");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        // 上限 3.0 USD,单次名义预估 est = 1.0 USD:第 4 次请求时上界 0+4*1 > 3 → 必须重算。
        let k = store.create(
            "u1".into(),
            None,
            // 上限取 3 倍单次预留,且**用同一个乘法表达式**写出来:字面量 0.15 与
            // `3.0 * 0.05` 在浮点下并非同一个数,写死字面量会让"快照上界恰好够到上限"
            // 这一拍错位、重扫时机随常量取值漂移。
            Some(3.0 * EST_COST_PER_REQUEST_USD),
            Some("usd".into()),
            None,
            None,
            Utc::now(),
        );
        let stats = StatsManager::load_from_dir(&tmp_stats_dir("cache_cap"));
        let cache = Arc::new(SpendCache::default());
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(stats.clone()),
            spend_cache: cache.clone(),
        };
        let app = store_router(auth);
        let send = |app: Router, key: String| async move {
            app.oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        };

        // 前三次:真实花费每次 0.9 倍 est(不超过单次名义预估,满足快照复用的前提)。
        // 全部按 est 的倍数表达,常量改值时比例自动跟随,不必回来重算夹具。
        for i in 0..3 {
            let resp = send(app.clone(), k.key.clone()).await;
            assert_eq!(resp.status(), StatusCode::OK, "第 {i} 次请求应放行");
            let _ = body_text(resp).await;
            stats
                .usage
                .record_usage_with_api_key(
                    1,
                    k.id,
                    "m".into(),
                    10,
                    20,
                    0.04,
                    None,
                    None,
                    None,
                    1000,
                )
                .await;
        }
        // 此刻真实已花 2.7 倍 est:第 4 次的快照上界(4×est)越过上限 → 重算 → 2.7+1 > 3.5 → 402。
        let resp = send(app.clone(), k.key.clone()).await;
        assert_eq!(
            resp.status(),
            StatusCode::PAYMENT_REQUIRED,
            "逼近上限时必须按精确值裁决,缓存不得放过超限请求"
        );
        assert_eq!(cache.full_scans(), 2, "只该在快照上界够到上限时才重扫账本");
        let _ = std::fs::remove_file(&path);
    }

    /// 关键回归:**已用完额度**的 key 被反复重试时,不得每次都全量扫描一遍账本。
    ///
    /// 消费上限是终身总额,用完即永久状态;这些请求横竖都会被 402 拒掉,却在修复前每拒一次
    /// 就先持账本读锁线性扫一遍全部记录(快照的名义上界必然够到上限 → 只能落回精确重算)。
    /// 客户端一个重试循环就能把服务端按在这条路径上反复扫,记账写入(需写锁)全被堵在后面 ——
    /// 纯浪费且可被无限撞。此处钉住:连撞 5 次只该扫一次账本,且 5 次都必须仍是 402。
    #[tokio::test]
    async fn exhausted_key_keeps_402_without_rescanning_ledger_every_request() {
        let path = tmp_store_path("exhausted_rescan");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        // usd 上限 1.0。
        let k = store.create(
            "u1".into(),
            None,
            Some(1.0),
            Some("usd".into()),
            None,
            None,
            Utc::now(),
        );
        let stats = StatsManager::load_from_dir(&tmp_stats_dir("exhausted_rescan"));
        // 已消费 1.5 USD:超过上限,之后每一发请求都注定 402。
        stats
            .usage
            .record_usage_with_api_key(1, k.id, "m".into(), 10, 20, 1.5, None, None, None, 1000)
            .await;
        let cache = Arc::new(SpendCache::default());
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(stats),
            spend_cache: cache.clone(),
        };
        let app = store_router(auth);

        for i in 0..5 {
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
            assert_eq!(
                resp.status(),
                StatusCode::PAYMENT_REQUIRED,
                "第 {i} 次重试仍须 402(上限是终身总额)"
            );
            let _ = body_text(resp).await;
        }
        assert_eq!(
            cache.full_scans(),
            1,
            "已用完的 key 连撞 5 次只该扫一次账本,实际扫了 {} 次",
            cache.full_scans()
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 上面那条快路径不得变成"粘住的判决":管理员把消费上限调高后,下一发请求必须立刻放行。
    /// (上限每请求从 store 现读,快照只提供已花额;若把"已用完"这个结论本身缓存起来,
    /// 运营者充值/提额后还要干等 TTL,面板上看着额度充足却继续 402。)
    #[tokio::test]
    async fn raising_the_limit_immediately_unblocks_an_exhausted_key() {
        let path = tmp_store_path("limit_raised");
        let _ = std::fs::remove_file(&path);
        let store = ApiKeyStore::load(&path);
        let k = store.create(
            "u1".into(),
            None,
            Some(1.0),
            Some("usd".into()),
            None,
            None,
            Utc::now(),
        );
        let stats = StatsManager::load_from_dir(&tmp_stats_dir("limit_raised"));
        stats
            .usage
            .record_usage_with_api_key(1, k.id, "m".into(), 10, 20, 1.5, None, None, None, 1000)
            .await;
        let store_for_update = store.clone();
        let auth = AuthState {
            api_key: None,
            runtime_cfg: None,
            role: AuthRole::Protocol,
            api_keys: Some(store),
            stats: Some(stats),
            spend_cache: Arc::new(SpendCache::default()),
        };
        let app = store_router(auth);
        let send = |app: Router, key: String| async move {
            app.oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        };

        // 两发:第一发全量重算后判拒,第二发走"已用完"快路径判拒。
        for i in 0..2 {
            let resp = send(app.clone(), k.key.clone()).await;
            assert_eq!(
                resp.status(),
                StatusCode::PAYMENT_REQUIRED,
                "提额前第 {i} 发应 402"
            );
            let _ = body_text(resp).await;
        }
        // 管理员提额到 100 USD。
        store_for_update.update(k.id, None, None, None, Some(Some(100.0)), None, None, None);
        let resp = send(app.clone(), k.key.clone()).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "提额后必须立刻放行,不能等快照过期"
        );
        let _ = body_text(resp).await;
        let _ = std::fs::remove_file(&path);
    }

    /// 快照裁决的各条边界(不经 HTTP,直接钉住判据):过期、单位改动、上界够到上限、
    /// 以及"已用完直接拒"这条快路径本身的边界。
    #[test]
    fn spend_cache_reuse_boundaries() {
        let cache = SpendCache::default();
        let t0 = Instant::now();
        cache.store(1, "usd", 0.0, t0);

        // 未过期 + 单位一致 + 上界远低于上限 → 复用。
        assert_eq!(
            cache.decide(1, "usd", 100.0, 1.0, t0),
            SpendVerdict::Admit(0.0)
        );
        // TTL 到点 → 不复用(用量被重置/淘汰时靠它向下修正)。
        assert_eq!(
            cache.decide(1, "usd", 100.0, 1.0, t0 + SPEND_CACHE_TTL),
            SpendVerdict::Recompute,
            "过期快照必须重算"
        );
        // 计量单位被改过(credits 与 usd 差 0.72 倍)→ 不复用。
        assert_eq!(
            cache.decide(1, "credits", 100.0, 1.4, t0),
            SpendVerdict::Recompute,
            "换了计量单位的快照不可比,必须重算"
        );
        // 未知 key → 不复用。
        assert_eq!(
            cache.decide(2, "usd", 100.0, 1.0, t0),
            SpendVerdict::Recompute
        );

        // 上界够到上限、但已花额本身还放得下一次预留 → 落回精确重算(不能直接拒:
        // 这段区间里真实已花可能远低于名义上界,拒了就是误杀)。
        let near = SpendCache::default();
        near.store(3, "usd", 2.4, t0);
        assert_eq!(
            near.decide(3, "usd", 4.0, 1.0, t0),
            SpendVerdict::Recompute,
            "2.4 + 2*1.0 > 4.0 但 2.4 + 1.0 <= 4.0,必须落回精确重算"
        );

        // 已花额连一次预留都放不下 → 直接拒,不再扫账本(精确重算只会得出同样结论)。
        let done = SpendCache::default();
        done.store(5, "usd", 2.4, t0);
        assert_eq!(
            done.decide(5, "usd", 3.0, 1.0, t0),
            SpendVerdict::Exhausted,
            "2.4 + 1.0 > 3.0,精确路径必拒,快照可直接给结论"
        );
        // 拒的快照同样受 TTL 约束:清用量/记录淘汰后至多 3s 就必须重算,不会永久误拒。
        assert_eq!(
            done.decide(5, "usd", 3.0, 1.0, t0 + SPEND_CACHE_TTL),
            SpendVerdict::Recompute,
            "过期的拒决必须重算,否则清空用量后 key 会被永久判死"
        );
        // 上限被现场调高 → 立即改判(上限每请求现读,不进快照)。
        assert_eq!(
            done.decide(5, "usd", 100.0, 1.0, t0),
            SpendVerdict::Admit(2.4),
            "上限调高后快照必须立刻改判放行"
        );

        // 上限被写成 NaN 之类的非法值 → 一律走慢路径,不因比较恒假而误放。
        let nan = SpendCache::default();
        nan.store(4, "usd", 0.0, t0);
        assert_eq!(
            nan.decide(4, "usd", f64::NAN, 1.0, t0),
            SpendVerdict::Recompute
        );
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
            spend_cache: Arc::new(SpendCache::default()),
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
            EST_COST_PER_REQUEST_USD,
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
