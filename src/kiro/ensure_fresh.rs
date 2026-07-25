//! 集中式令牌保鲜:一个凭据即将过期时刷新一次,并把新令牌写回活池,
//! 使 relay / balance / models 三个独立消费方共享同一份最新令牌。
//!
//! 背景(为何需要集中):Kiro 的 refresh_token 是**单次轮换**的——刷新响应可能带回
//! 新的 refresh_token 替换旧的。若三方各自独立刷新,后刷的会把先刷方持有的
//! refresh_token 作废,形成级联 401/403。集中到本助手后:任一方触发刷新即把新
//! access/refresh/过期时刻写回 `Arc<Mutex<Pool>>`,后续各方从池取到的都是最新令牌,
//! 不再各自反复轮换。
//!
//! 并发纪律:**池锁绝不跨网络 `.await` 持有**。取快照时短暂持锁 → 释放 → 网络刷新 →
//! 再短暂持锁写回。刷新走 [`crate::kiro::refresh::refresh`](上游端点由 auth 方式决定)。
//!
//! 不记录任何令牌明文。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::kiro::credential::Credential;
use crate::kiro::login::LoginError;
use crate::kiro::pool::Pool;

/// 单飞刷新协调器:按 `credential_id` 维护一把 per-credential 异步锁。
///
/// 目的(修 Bug A——无单飞锁的级联 401):`ensure_fresh`/`force_refresh` 取快照后释放池锁
/// 再走网络刷新。若 relay+balance+models 三方**并发**刷新同一凭据,会各自拿着**同一个旧
/// refresh_token** 发刷新请求——Kiro 的 refresh_token 单次轮换,先到者换走新 token,后到者
/// 的旧 token 已被作废 → 401 级联。
///
/// 本协调器保证:**同一 credential_id 同一时刻至多一个在飞刷新**;并发的其余调用先在该锁上
/// 等待,拿到锁后**双检**池内快照——若赢家已换出新 token(refresh_token 变了且不再即将过期),
/// 直接返回新令牌**不再重复刷新**,从而不会再拿旧 token 打第二枪。
///
/// 跨凭据无干扰:不同 credential_id 落在 HashMap 的不同 key,各自独立并发刷新。
/// per-credential 锁用 `Arc<Mutex<()>>` 承载,首次访问时惰性建、按 id 复用。
#[derive(Clone, Default)]
pub struct RefreshCoordinator {
    /// credential_id → 该凭据的刷新串行锁。外层 `Mutex` 只在"取/建 per-id 锁"这一极短临界区
    /// 持有,绝不跨网络 `.await`;真正的单飞临界区是取出的那把 `Arc<Mutex<()>>`。
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl RefreshCoordinator {
    /// 新建空协调器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取出 `credential_id` 对应的刷新锁(不存在则建)。返回 `Arc<Mutex<()>>`,调用方随后
    /// `.lock().await` 进入单飞临界区。外层 map 锁只在此短暂持有,不跨网络。
    ///
    /// #10:取锁前顺手清理 map 中**无人引用**的陈旧条目(`strong_count == 1`,即只有 map 自己持有,
    /// 没有任何任务正持有它或处于"已 lock_for 拿到 Arc、尚未 lock" 的窗口内)。全程持 map 锁做,故
    /// 不会误删另一个任务即将使用的锁:任何并发任务要么已在 map 锁外持有它自己的 Arc 克隆
    /// (strong_count≥2 → 保留),要么还没进 lock_for(此刻被删也无妨,它进来会重建)。
    /// 由此避免为已删除凭据留下的锁条目无限堆积。
    async fn lock_for(&self, credential_id: &str) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().await;
        // 顺手回收:移除所有当前无人引用的条目(不含即将返回的这一个)。
        map.retain(|k, v| k == credential_id || Arc::strong_count(v) > 1);
        map.entry(credential_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// #10 测试辅助:当前 map 中缓存的 per-credential 锁条目数(用于断言陈旧条目被回收)。
    #[cfg(test)]
    async fn cached_lock_count(&self) -> usize {
        self.locks.lock().await.len()
    }
}

/// 刷新上下文:把单飞协调器 + 凭据落盘路径 + 落盘串行锁打包一处,便于以单参数穿过三个调用方。
///
/// - `coordinator`:per-credential 单飞锁(修 Bug A)。
/// - `credentials_path`:kiro2api 自己的 credentials.json 路径,刷新成功后原子落盘(修 Bug B——
///   重启后加载到已轮换作废的旧 refresh_token → 401)。
/// - `persist_lock`:序列化并发落盘,防止两次写交错损坏文件。仅在"序列化 + 原子写"这一极短
///   临界区持有,绝不跨网络 `.await`。
#[derive(Clone)]
pub struct RefreshCtx {
    pub coordinator: RefreshCoordinator,
    pub credentials_path: String,
    pub persist_lock: Arc<Mutex<()>>,
}

impl RefreshCtx {
    /// 便捷构造:给定落盘路径,自建协调器与落盘锁。
    pub fn new(credentials_path: impl Into<String>) -> Self {
        Self {
            coordinator: RefreshCoordinator::new(),
            credentials_path: credentials_path.into(),
            persist_lock: Arc::new(Mutex::new(())),
        }
    }
}

/// 刷新成功后把活池当前快照原子落盘到 `credentials_path`(修 Bug B)。
///
/// 并发纪律:`persist_lock` 只在"取池快照 + 序列化 + 原子写"这段极短临界区持有,**不跨网络**。
/// `credential::save` 已做 tmp → rename 原子替换,故进程中途崩溃也不会留半写文件。
/// 落盘失败**不**使请求失败:内存池已更新,本次调用可用;仅记 warn(不含任何令牌明文)。
async fn persist_pool_credentials(pool: &Arc<Mutex<Pool>>, ctx: &RefreshCtx) {
    let _guard = ctx.persist_lock.lock().await;
    let snapshot = {
        let pool_lock = pool.lock().await;
        pool_lock.snapshot_credentials()
    };
    // 落盘失败重试至多 3 次(短暂退避),兜住并发 rename 竞争/瞬时 I/O 抖动;
    // 仍失败才 warn(内存池已更新,请求不受影响,下次刷新会再尝试)。
    let mut last_err = None;
    for attempt in 0..3 {
        match crate::kiro::credential::save(&ctx.credentials_path, &snapshot) {
            Ok(()) => return,
            Err(e) => {
                last_err = Some(e);
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }
    if let Some(e) = last_err {
        tracing::warn!(
            "刷新后持久化 credentials.json 失败(已重试3次,内存池已更新,下次请求会再刷新): {e}"
        );
    }
}

/// 保鲜失败原因。
#[derive(Debug)]
pub enum EnsureFreshError {
    /// 池中找不到该 id(已被删除等)。
    NotFound,
    /// 上游刷新失败(网络/非 2xx/解析)。
    Refresh(LoginError),
}

impl std::fmt::Display for EnsureFreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnsureFreshError::NotFound => write!(f, "credential not found in pool"),
            EnsureFreshError::Refresh(e) => write!(f, "refresh failed: {e:?}"),
        }
    }
}
impl std::error::Error for EnsureFreshError {}

/// 确保 `credential_id` 的 access_token 新鲜:
/// 1. 短暂持锁取该凭据快照(找不到 → `NotFound`)。
/// 2. 未在 `margin_secs` 内过期 → 原样返回(不发网络)。
/// 3. 取该凭据的**单飞锁**(同一 id 串行);拿到后**双检**——赢家已换新则直接返回,不重复刷新。
/// 4. 否则(池锁已释放)向上游刷新一次。
/// 5. 再短暂持锁把新 access/refresh/过期时刻写回池,并原子落盘 credentials.json。
/// 6. 返回刷新后的凭据供即时使用。
///
/// 池锁只在取快照/写回时短暂持有,网络 `.await` 期间不持锁;单飞锁(per-credential)可跨网络
/// 持有——那正是"同一凭据同一时刻至多一个在飞刷新"的临界区。
///
/// `ctx` 为 `Some` 时启用单飞 + 落盘(生产);为 `None` 时退化为原语义(测试/无协调器场景)。
pub async fn ensure_fresh(
    pool: &Arc<Mutex<Pool>>,
    credential_id: &str,
    client: &reqwest::Client,
    now_unix: u64,
    margin_secs: u64,
    ctx: Option<&RefreshCtx>,
) -> Result<Credential, EnsureFreshError> {
    // 1) 取快照(短暂持锁)。
    let cred_snapshot = {
        let pool_lock = pool.lock().await;
        pool_lock
            .snapshot_credentials()
            .into_iter()
            .find(|c| c.id == credential_id)
            .ok_or(EnsureFreshError::NotFound)?
    };

    // 2) 未即将过期 → 直接用(无网络,无需进单飞锁)。
    if !cred_snapshot.expires_soon(now_unix, margin_secs) {
        return Ok(cred_snapshot);
    }

    // 3) 进单飞锁(有 ctx 时)。拿到锁后双检:等待期间赢家可能已刷新过。
    let _flight = match ctx {
        Some(c) => Some(
            c.coordinator
                .lock_for(credential_id)
                .await
                .lock_owned()
                .await,
        ),
        None => None,
    };
    if ctx.is_some() {
        let after = {
            let pool_lock = pool.lock().await;
            pool_lock
                .snapshot_credentials()
                .into_iter()
                .find(|c| c.id == credential_id)
        };
        if let Some(after) = after {
            // #4:赢家刷新后,输家应直接返回赢家的新令牌**不再重复刷新**——无论 refresh_token 是否轮换。
            // 判据不能只看 refresh_token(IdC 的 refresh_token 不轮换,只看它会让每个输家都误判"没人刷过"
            // 而重复刷新,单飞对 IdC 失效)。
            //
            // #5:判"赢家刷过没"的**权威信号是 access_token 值变了**——只要令牌值不同于本任务进锁前的
            // 快照,就说明赢家已向上游换出新令牌并写回池,输家必须直接返回它、**不得**再刷。
            // 早先版本额外用 `!after.expires_soon(...)` 当门:若赢家换来的新令牌本就比 margin 短命
            //(即刷完仍落在 expiring-soon 窗口内),这个门会误判"还没刷好"→ 输家再刷一次,单飞被击穿
            //(多打一枪上游、且可能把赢家刚轮换出的 refresh_token 再次轮换作废)。现改为:
            //   access_token 值变了 ⇒ 无条件返回赢家令牌(不看新过期时刻 vs margin);
            //   仅当 access_token **未变**(赢家没换令牌值)但 expires_at 前进时,也算刷过一次(覆盖
            //     "IdC 同值仅推进过期"及未来同令牌续期语义),同样返回不再刷。
            // 只有令牌值真未变**且**仍即将过期时,才落到下方真正刷新。
            let token_changed = after.access_token != cred_snapshot.access_token;
            let expiry_advanced = after.expires_at_unix > cred_snapshot.expires_at_unix;
            if token_changed {
                // 赢家已换出新令牌值 → 直接返回,无论新过期时刻是否仍在 margin 内。
                return Ok(after);
            }
            if expiry_advanced && !after.expires_soon(now_unix, margin_secs) {
                // 令牌值未变但过期前进且已脱离 margin(同令牌续期)→ 也无需再刷。
                return Ok(after);
            }
        }
    }

    // 4) 刷新(仅持单飞锁,不持池锁)。
    let refreshed = crate::kiro::refresh::refresh(client, &cred_snapshot, now_unix)
        .await
        .map_err(EnsureFreshError::Refresh)?;

    // 5) 写回(短暂持锁)。凭据可能在刷新期间被删,写回未命中不报错——
    //    刷新结果仍可供本次调用即时使用。
    {
        let mut pool_lock = pool.lock().await;
        pool_lock.update_credential_tokens(
            credential_id,
            refreshed.access_token.clone(),
            refreshed.refresh_token.clone(),
            refreshed.expires_at_unix,
        );
    }

    // 6) 落盘(有 ctx 时;best-effort,失败不使请求失败)。
    if let Some(ctx) = ctx {
        persist_pool_credentials(pool, ctx).await;
    }

    Ok(refreshed)
}

/// 强制刷新 `credential_id` 的令牌:**无条件**向上游刷新一次(不看 expires_soon),
/// 再把新 access/refresh/过期时刻写回活池,返回刷新后的凭据。
///
/// 动机:上游可能对**尚未过期**的 access_token 返回 403 AccessDeniedException
/// (bearer token invalid)——因活主已在服务端轮换失效了该 token。这类"未过期但已失效"
/// 的 token 靠 `ensure_fresh`(仅在 expires_soon 时刷新)无法自愈,故三个消费方在收到
/// 上游 401/403 后调用本函数强制换新令牌再重试一次。
///
/// 并发纪律同 [`ensure_fresh`]:池锁只在取快照(步骤 1)与写回(步骤 3)时短暂持有,
/// 网络 `.await` 期间不持锁。不记录任何令牌明文。
pub async fn force_refresh(
    pool: &Arc<Mutex<Pool>>,
    credential_id: &str,
    client: &reqwest::Client,
    now_unix: u64,
    ctx: Option<&RefreshCtx>,
) -> Result<Credential, EnsureFreshError> {
    // 1) 取快照(短暂持锁)。
    let cred_snapshot = {
        let pool_lock = pool.lock().await;
        pool_lock
            .snapshot_credentials()
            .into_iter()
            .find(|c| c.id == credential_id)
            .ok_or(EnsureFreshError::NotFound)?
    };

    // 2) 进单飞锁(有 ctx 时)。拿到锁后双检:等待期间赢家可能已强制刷新过。
    //    #4:force 无 expires_soon 门,判"赢家刷过没"不能只看 refresh_token(IdC 不轮换 → 每个输家都
    //    误判没人刷过 → 重复刷新,单飞失效)。改看 access_token 变了 或 expires_at 前进——任一成立即证明
    //    赢家已把新令牌写回池,直接返回赢家的新令牌,**不再拿本快照里可能已失效的旧 token 打第二枪**。
    let _flight = match ctx {
        Some(c) => Some(
            c.coordinator
                .lock_for(credential_id)
                .await
                .lock_owned()
                .await,
        ),
        None => None,
    };
    if ctx.is_some() {
        let after = {
            let pool_lock = pool.lock().await;
            pool_lock
                .snapshot_credentials()
                .into_iter()
                .find(|c| c.id == credential_id)
        };
        if let Some(after) = after
            && (after.access_token != cred_snapshot.access_token
                || after.expires_at_unix > cred_snapshot.expires_at_unix)
        {
            return Ok(after);
        }
    }

    // 3) 无条件刷新(仅持单飞锁,不持池锁),不看 expires_soon。
    let refreshed = crate::kiro::refresh::refresh(client, &cred_snapshot, now_unix)
        .await
        .map_err(EnsureFreshError::Refresh)?;

    // 4) 写回(短暂持锁)。凭据可能在刷新期间被删,写回未命中不报错。
    {
        let mut pool_lock = pool.lock().await;
        pool_lock.update_credential_tokens(
            credential_id,
            refreshed.access_token.clone(),
            refreshed.refresh_token.clone(),
            refreshed.expires_at_unix,
        );
    }

    // 5) 落盘(有 ctx 时;best-effort)。
    if let Some(ctx) = ctx {
        persist_pool_credentials(pool, ctx).await;
    }

    Ok(refreshed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};
    use crate::kiro::pool::LbMode;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cred(id: &str, expires_at_unix: u64, region: &str) -> Credential {
        Credential {
            id: id.into(),
            access_token: "old-at".into(),
            refresh_token: "old-rt".into(),
            expires_at_unix,
            region: region.into(),
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

    /// 未过期 → 不刷新,原样返回(无网络)。
    #[tokio::test]
    async fn not_expiring_returns_as_is_without_network() {
        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("1", u64::MAX, "us-east-1")],
            LbMode::Priority,
        )));
        let client = reqwest::Client::new();
        let out = ensure_fresh(&pool, "1", &client, 1000, 300, None)
            .await
            .unwrap();
        assert_eq!(out.access_token, "old-at");
        assert_eq!(out.expires_at_unix, u64::MAX);
    }

    /// 未知 id → NotFound。
    #[tokio::test]
    async fn unknown_id_yields_not_found() {
        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("1", u64::MAX, "us-east-1")],
            LbMode::Priority,
        )));
        let client = reqwest::Client::new();
        let r = ensure_fresh(&pool, "nope", &client, 1000, 300, None).await;
        assert!(matches!(r, Err(EnsureFreshError::NotFound)));
    }

    /// 即将过期 → 刷新并把新令牌写回池;返回值与池内均更新。
    /// 通过给凭据一个能命中 mock 的 refresh 端点来验证写回。
    #[tokio::test]
    async fn expiring_refreshes_and_writes_back_to_pool() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "fresh-at", "refreshToken": "fresh-rt", "expiresIn": 3600
            })))
            .mount(&server)
            .await;

        // 生产端点由 auth 方式派生(无法直指 mock),故用 `ensure_fresh_with_base` 注入 mock
        // 刷新端点,验证等价逻辑:刷新 + 写回活池。生产 `ensure_fresh` 除端点来源外与其同构。
        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("1", 1000, "us-east-1")],
            LbMode::Priority,
        )));
        let client = reqwest::Client::new();
        let base = format!("{}/refreshToken", server.uri());
        let out = ensure_fresh_with_base(&pool, "1", &client, 1000, 300, Some(&base), None)
            .await
            .unwrap();
        assert_eq!(out.access_token, "fresh-at");
        assert_eq!(out.refresh_token, "fresh-rt");
        assert_eq!(out.expires_at_unix, 1000 + 3600);
        // 池内也已写回
        let snap = pool.lock().await.snapshot_credentials();
        assert_eq!(snap[0].access_token, "fresh-at");
        assert_eq!(snap[0].refresh_token, "fresh-rt");
        assert_eq!(snap[0].expires_at_unix, 1000 + 3600);
    }

    /// 强制刷新:即便**未即将过期**也刷新并写回池(模拟"未过期但被服务端失效"后的强制换新)。
    #[tokio::test]
    async fn force_refresh_refreshes_even_when_not_expiring() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "fresh-at", "refreshToken": "fresh-rt", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        // 凭据 expires_at = u64::MAX(远未过期),ensure_fresh 会原样返回;force 仍刷新。
        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("1", u64::MAX, "us-east-1")],
            LbMode::Priority,
        )));
        let client = reqwest::Client::new();
        let base = format!("{}/refreshToken", server.uri());
        let out = force_refresh_with_base(&pool, "1", &client, 1000, Some(&base), None)
            .await
            .unwrap();
        assert_eq!(out.access_token, "fresh-at");
        assert_eq!(out.refresh_token, "fresh-rt");
        assert_eq!(out.expires_at_unix, 1000 + 3600);
        let snap = pool.lock().await.snapshot_credentials();
        assert_eq!(snap[0].access_token, "fresh-at");
        assert_eq!(snap[0].refresh_token, "fresh-rt");
    }

    /// 强制刷新未知 id → NotFound。
    #[tokio::test]
    async fn force_refresh_unknown_id_yields_not_found() {
        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("1", u64::MAX, "us-east-1")],
            LbMode::Priority,
        )));
        let client = reqwest::Client::new();
        let r = force_refresh_with_base(&pool, "nope", &client, 1000, None, None).await;
        assert!(matches!(r, Err(EnsureFreshError::NotFound)));
    }

    /// 测试专用:强制刷新版,允许注入刷新 base(mock 端点)。生产 [`force_refresh`] 用凭据自身端点。
    /// 与生产 [`force_refresh`] 同构:含 per-credential 单飞锁 + 双检 + 落盘(仅当 `ctx` 为 Some)。
    async fn force_refresh_with_base(
        pool: &Arc<Mutex<Pool>>,
        credential_id: &str,
        client: &reqwest::Client,
        now_unix: u64,
        base_override: Option<&str>,
        ctx: Option<&RefreshCtx>,
    ) -> Result<Credential, EnsureFreshError> {
        let cred_snapshot = {
            let pool_lock = pool.lock().await;
            pool_lock
                .snapshot_credentials()
                .into_iter()
                .find(|c| c.id == credential_id)
                .ok_or(EnsureFreshError::NotFound)?
        };
        let _flight = match ctx {
            Some(c) => Some(
                c.coordinator
                    .lock_for(credential_id)
                    .await
                    .lock_owned()
                    .await,
            ),
            None => None,
        };
        if ctx.is_some() {
            let after = {
                let pool_lock = pool.lock().await;
                pool_lock
                    .snapshot_credentials()
                    .into_iter()
                    .find(|c| c.id == credential_id)
            };
            if let Some(after) = after
                && (after.access_token != cred_snapshot.access_token
                    || after.expires_at_unix > cred_snapshot.expires_at_unix)
            {
                return Ok(after);
            }
        }
        let refreshed = match base_override {
            Some(base) => crate::kiro::refresh::refresh_at(client, base, &cred_snapshot, now_unix)
                .await
                .map_err(EnsureFreshError::Refresh)?,
            None => crate::kiro::refresh::refresh(client, &cred_snapshot, now_unix)
                .await
                .map_err(EnsureFreshError::Refresh)?,
        };
        {
            let mut pool_lock = pool.lock().await;
            pool_lock.update_credential_tokens(
                credential_id,
                refreshed.access_token.clone(),
                refreshed.refresh_token.clone(),
                refreshed.expires_at_unix,
            );
        }
        if let Some(ctx) = ctx {
            persist_pool_credentials(pool, ctx).await;
        }
        Ok(refreshed)
    }

    /// 测试专用:允许注入刷新 base(mock 端点),生产 [`ensure_fresh`] 用凭据自身端点。
    /// 与生产 [`ensure_fresh`] 同构:含 per-credential 单飞锁 + 双检 + 落盘(仅当 `ctx` 为 Some)。
    async fn ensure_fresh_with_base(
        pool: &Arc<Mutex<Pool>>,
        credential_id: &str,
        client: &reqwest::Client,
        now_unix: u64,
        margin_secs: u64,
        base_override: Option<&str>,
        ctx: Option<&RefreshCtx>,
    ) -> Result<Credential, EnsureFreshError> {
        let cred_snapshot = {
            let pool_lock = pool.lock().await;
            pool_lock
                .snapshot_credentials()
                .into_iter()
                .find(|c| c.id == credential_id)
                .ok_or(EnsureFreshError::NotFound)?
        };
        if !cred_snapshot.expires_soon(now_unix, margin_secs) {
            return Ok(cred_snapshot);
        }
        let _flight = match ctx {
            Some(c) => Some(
                c.coordinator
                    .lock_for(credential_id)
                    .await
                    .lock_owned()
                    .await,
            ),
            None => None,
        };
        if ctx.is_some() {
            let after = {
                let pool_lock = pool.lock().await;
                pool_lock
                    .snapshot_credentials()
                    .into_iter()
                    .find(|c| c.id == credential_id)
            };
            // #5:与生产 `ensure_fresh` 同构的双检门——access_token 值变了即无条件返回赢家令牌
            // (不因新令牌短命而误判需再刷);仅令牌值未变但过期前进且脱离 margin 时也返回不再刷。
            if let Some(after) = after {
                let token_changed = after.access_token != cred_snapshot.access_token;
                let expiry_advanced = after.expires_at_unix > cred_snapshot.expires_at_unix;
                if token_changed {
                    return Ok(after);
                }
                if expiry_advanced && !after.expires_soon(now_unix, margin_secs) {
                    return Ok(after);
                }
            }
        }
        let refreshed = match base_override {
            Some(base) => crate::kiro::refresh::refresh_at(client, base, &cred_snapshot, now_unix)
                .await
                .map_err(EnsureFreshError::Refresh)?,
            None => crate::kiro::refresh::refresh(client, &cred_snapshot, now_unix)
                .await
                .map_err(EnsureFreshError::Refresh)?,
        };
        {
            let mut pool_lock = pool.lock().await;
            pool_lock.update_credential_tokens(
                credential_id,
                refreshed.access_token.clone(),
                refreshed.refresh_token.clone(),
                refreshed.expires_at_unix,
            );
        }
        if let Some(ctx) = ctx {
            persist_pool_credentials(pool, ctx).await;
        }
        Ok(refreshed)
    }

    // ==================== 单飞 + 双检并发测试(修 Bug A)====================

    /// (A) CONCURRENCY:对**同一凭据**并发发起 N 次 ensure_fresh_with_base,mock 刷新端点计数命中。
    /// 断言:上游刷新恰好被调用 **1 次**,且 N 个调用方都拿到了 fresh token(证明单飞 + 双检:
    /// 赢家刷一次,其余等待后从池读到新 token 直接返回,不再拿旧 refresh_token 打第二枪)。
    #[tokio::test]
    async fn concurrent_ensure_fresh_refreshes_upstream_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_mock = hits.clone();
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(move |_req: &wiremock::Request| {
                hits_mock.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "accessToken": "fresh-at", "refreshToken": "fresh-rt", "expiresIn": 3600
                }))
            })
            .mount(&server)
            .await;

        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("1", 1000, "us-east-1")],
            LbMode::Priority,
        )));
        let client = reqwest::Client::new();
        let base = Arc::new(format!("{}/refreshToken", server.uri()));
        let ctx = Arc::new(RefreshCtx::new(
            "/nonexistent/kiro2api_concurrency_test.json",
        ));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let pool = pool.clone();
            let client = client.clone();
            let base = base.clone();
            let ctx = ctx.clone();
            handles.push(tokio::spawn(async move {
                ensure_fresh_with_base(&pool, "1", &client, 1000, 300, Some(&base), Some(&ctx))
                    .await
                    .unwrap()
            }));
        }
        for h in handles {
            let out = h.await.unwrap();
            assert_eq!(out.access_token, "fresh-at");
            assert_eq!(out.refresh_token, "fresh-rt");
        }
        // 单飞 + 双检 → 上游刷新恰好 1 次。
        assert_eq!(hits.load(Ordering::SeqCst), 1, "上游刷新应恰好被调用一次");
    }

    /// (A) 同上,但针对 force_refresh_with_base(无 expires_soon 门,单飞据 refresh_token 变化双检)。
    #[tokio::test]
    async fn concurrent_force_refresh_refreshes_upstream_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_mock = hits.clone();
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(move |_req: &wiremock::Request| {
                hits_mock.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "accessToken": "fresh-at", "refreshToken": "fresh-rt", "expiresIn": 3600
                }))
            })
            .mount(&server)
            .await;

        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("1", u64::MAX, "us-east-1")],
            LbMode::Priority,
        )));
        let client = reqwest::Client::new();
        let base = Arc::new(format!("{}/refreshToken", server.uri()));
        let ctx = Arc::new(RefreshCtx::new(
            "/nonexistent/kiro2api_concurrency_test2.json",
        ));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let pool = pool.clone();
            let client = client.clone();
            let base = base.clone();
            let ctx = ctx.clone();
            handles.push(tokio::spawn(async move {
                force_refresh_with_base(&pool, "1", &client, 1000, Some(&base), Some(&ctx))
                    .await
                    .unwrap()
            }));
        }
        for h in handles {
            let out = h.await.unwrap();
            assert_eq!(out.refresh_token, "fresh-rt");
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "force_refresh 上游刷新应恰好一次"
        );
    }

    /// (#4)IdC(refresh_token **不轮换**)的并发 ensure_fresh 也必须恰好一次上游刷新。
    /// 用 IdC 端点(/token)且 mock 响应**不带 refreshToken**(模拟 IdC 非轮换),只换 accessToken +
    /// 推进 expiresIn。若双检仍只看 refresh_token,输家会因 refresh_token 未变而每个都重复刷新 → hits>1。
    /// 修复后双检看 access_token 变 / expires_at 前进 → 输家直接返回赢家令牌,hits==1。
    #[tokio::test]
    async fn concurrent_ensure_fresh_idc_non_rotating_refreshes_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_mock = hits.clone();
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(move |_req: &wiremock::Request| {
                hits_mock.fetch_add(1, Ordering::SeqCst);
                // 注意:无 refreshToken 字段 → IdC 保留旧 refresh_token(不轮换)。
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "accessToken": "fresh-at-idc", "expiresIn": 3600
                }))
            })
            .mount(&server)
            .await;

        let mut c = cred("1", 1000, "us-east-1");
        c.auth = AuthMethod::Idc;
        c.client_id = Some("cid".into());
        c.client_secret = Some("csec".into());
        let pool = Arc::new(Mutex::new(Pool::new(vec![c], LbMode::Priority)));
        let client = reqwest::Client::new();
        let base = Arc::new(format!("{}/token", server.uri()));
        let ctx = Arc::new(RefreshCtx::new(
            "/nonexistent/kiro2api_idc_concurrency_test.json",
        ));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let pool = pool.clone();
            let client = client.clone();
            let base = base.clone();
            let ctx = ctx.clone();
            handles.push(tokio::spawn(async move {
                ensure_fresh_with_base(&pool, "1", &client, 1000, 300, Some(&base), Some(&ctx))
                    .await
                    .unwrap()
            }));
        }
        for h in handles {
            let out = h.await.unwrap();
            assert_eq!(out.access_token, "fresh-at-idc");
            assert_eq!(out.refresh_token, "old-rt"); // 非轮换:refresh_token 未变
        }
        // 关键:IdC 非轮换下,单飞 + 新判据 → 上游刷新仍恰好 1 次(修复前会是 8 次)。
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "IdC 非轮换上游刷新应恰好一次"
        );
    }

    /// (#5)赢家换来的**新令牌本身短命**(刷完仍落在 margin 内 → expires_soon 仍为 true)时,
    /// 输家的双检门**不得**因"新令牌仍即将过期"而再刷一次。判据应看 access_token 值是否变了:
    /// 令牌值变了即证明赢家已刷,直接返回,无论新过期时刻是否仍在 margin 内。
    ///
    /// 构造:now=1000、margin=300,mock 刷新只把 expiresIn 设为 100(新过期时刻 1100,仍 < 1000+300
    /// → expires_soon 仍 true)。修复前的门 `winner_refreshed && !expires_soon` 会因后半为 false 而
    /// 放行输家再刷 → hits==8;修复后 access_token 变了即返回 → hits==1。
    #[tokio::test]
    async fn concurrent_ensure_fresh_shortlived_winner_token_still_refreshes_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_mock = hits.clone();
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(move |_req: &wiremock::Request| {
                hits_mock.fetch_add(1, Ordering::SeqCst);
                // expiresIn=100 → 新过期时刻 1100,仍在 now(1000)+margin(300) 内 → 刷完仍 expires_soon。
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "accessToken": "fresh-at", "refreshToken": "fresh-rt", "expiresIn": 100
                }))
            })
            .mount(&server)
            .await;

        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("1", 1000, "us-east-1")],
            LbMode::Priority,
        )));
        let client = reqwest::Client::new();
        let base = Arc::new(format!("{}/refreshToken", server.uri()));
        let ctx = Arc::new(RefreshCtx::new(
            "/nonexistent/kiro2api_shortlived_winner_test.json",
        ));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let pool = pool.clone();
            let client = client.clone();
            let base = base.clone();
            let ctx = ctx.clone();
            handles.push(tokio::spawn(async move {
                ensure_fresh_with_base(&pool, "1", &client, 1000, 300, Some(&base), Some(&ctx))
                    .await
                    .unwrap()
            }));
        }
        for h in handles {
            let out = h.await.unwrap();
            // 所有输家都拿到赢家换出的短命新令牌(而非各自再刷一枪)。
            assert_eq!(out.access_token, "fresh-at");
            assert_eq!(out.refresh_token, "fresh-rt");
            assert_eq!(out.expires_at_unix, 1100);
        }
        // 关键:即便新令牌刷完仍 expires_soon,单飞仍恰好 1 次上游刷新(修复前会是 8 次)。
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "短命新令牌下,输家应据 access_token 值变化直接返回赢家令牌,上游刷新仍恰好一次"
        );
    }

    // ==================== #10:锁 map 不无限增长 ====================

    /// (#10)完成一次单飞后其锁条目无人引用;再对**另一凭据** lock_for 时应把陈旧条目清掉,
    /// map 不随已用过/已删除的凭据无限堆积。
    #[tokio::test]
    async fn coordinator_prunes_idle_lock_entries() {
        let coord = RefreshCoordinator::new();
        // 对 "a" 完整走一次单飞:拿锁 → 进临界区 → 释放(guard+Arc 全部 drop)。
        {
            let arc = coord.lock_for("a").await;
            let _g = arc.lock_owned().await;
            // 临界区内:map 里有 "a"(此刻 strong_count≥2:map + arc/guard 持有)。
            assert_eq!(coord.cached_lock_count().await, 1);
        } // _g(含内部 Arc)在此 drop → "a" 的锁此后仅 map 持有(strong_count==1)。

        // 对另一凭据 "b" 取锁:lock_for 的 retain 顺手清掉无人引用的 "a",只保留 "b"。
        let arc_b = coord.lock_for("b").await;
        let _gb = arc_b.lock_owned().await;
        assert_eq!(
            coord.cached_lock_count().await,
            1,
            "陈旧的 a 条目应被回收,map 只剩正在使用的 b"
        );
    }

    /// (#10)竞争中的条目**不会**被误删:两个任务并发拿同一凭据的锁时,map 里该条目 strong_count≥2,
    /// retain 必须保留它(否则会破坏单飞正确性)。
    #[tokio::test]
    async fn coordinator_keeps_contended_lock_entry() {
        let coord = RefreshCoordinator::new();
        let held = coord.lock_for("a").await; // 任务1 已持有 Arc 克隆
        let _g = held.lock_owned().await;
        // 任务2 再次 lock_for 同一凭据:此时 map 里 "a" 的 strong_count≥2,不能被清掉。
        let again = coord.lock_for("a").await;
        assert!(Arc::strong_count(&again) >= 2);
        assert_eq!(coord.cached_lock_count().await, 1, "竞争中的条目必须保留");
    }

    // ==================== 落盘测试(修 Bug B)====================

    /// (B) PERSISTENCE:刷新成功后,从磁盘重新加载 credentials.json,断言其中是**新** refresh_token
    /// (而非已被轮换作废的旧 refresh_token)——防止重启后加载旧 token 触发 401。
    #[tokio::test]
    async fn refresh_persists_new_refresh_token_to_disk() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "fresh-at", "refreshToken": "fresh-rt", "expiresIn": 3600
            })))
            .mount(&server)
            .await;

        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "kiro2api_persist_test_{}_{}.json",
            std::process::id(),
            now_nanos()
        ));
        let p = path.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&p);

        // 先把初始凭据落盘一份(模拟启动状态:磁盘上是旧 refresh_token)。
        crate::kiro::credential::save(&p, &[cred("1", 1000, "us-east-1")]).unwrap();
        assert_eq!(
            crate::kiro::credential::load(&p).unwrap()[0].refresh_token,
            "old-rt"
        );

        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("1", 1000, "us-east-1")],
            LbMode::Priority,
        )));
        let client = reqwest::Client::new();
        let base = format!("{}/refreshToken", server.uri());
        let ctx = RefreshCtx::new(p.clone());

        let out = ensure_fresh_with_base(&pool, "1", &client, 1000, 300, Some(&base), Some(&ctx))
            .await
            .unwrap();
        assert_eq!(out.refresh_token, "fresh-rt");

        // 关键断言:磁盘上已是新 refresh_token(而非旧 old-rt)。
        let on_disk = crate::kiro::credential::load(&p).unwrap();
        assert_eq!(on_disk.len(), 1);
        assert_eq!(on_disk[0].refresh_token, "fresh-rt");
        assert_eq!(on_disk[0].access_token, "fresh-at");
        assert_eq!(on_disk[0].expires_at_unix, 1000 + 3600);

        let _ = std::fs::remove_file(&p);
    }

    /// 单调纳秒,给测试临时文件名去重。
    fn now_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }
}
