//! 活跃账号的令牌提前续期。
//!
//! # 为什么只续"活跃"账号
//!
//! 令牌刷新本身是懒的:账号被选中、令牌快过期了才刷。这对长期不用的账号是最优的 ——
//! 一次多余的上游调用都不产生。代价是**活跃账号的请求偶尔要等一次刷新**。
//!
//! 直觉的解法是"定时把全池令牌都续上",但那会给每个账号造出一条**永不停歇的后台心跳**:
//! 253 个账号、令牌约 1 小时到期,就是每小时 253 次上游调用,24 小时不停,背后**没有任何
//! 用户请求**。而本项目的账号已经被上游以 `security precaution` 封过(写这段时池里躺着
//! 24 个),"无人使用却持续产生规律流量"恰恰是风控最容易盯上的形状。用户此前为「全局
//! 积分」定方案时,也正是出于同一顾虑明确要求**不加定时刷新**(见共享余额缓存那套规则)。
//!
//! 故这里把提前续期**与真实使用量挂钩**:只有近 [`ACTIVE_WINDOW_SECS`] 内被选中过的账号
//! 才享受提前续期。于是上游流量正比于实际用量,长期闲置的账号一次多余调用都不会产生 ——
//! 既拿到"活跃账号不必等刷新"的好处,又不制造那条心跳。
//!
//! # 纪律
//!
//! - **不自己实现刷新**:一律走 [`ensure_fresh`](crate::kiro::ensure_fresh::ensure_fresh),
//!   与数据面共用同一把 per-credential 单飞锁、同一套落盘与失败处置。另写一份必然漂移。
//! - **失败不额外处置**:`ensure_fresh` 内部已按分类反馈账号池(冷却/禁用/结论)。这里
//!   只记一条 debug,绝不重复记账 —— 重复反馈会让同一次失败被记两次。
//! - **每轮有上限**:一轮最多续 [`MAX_PER_TICK`] 个,避免刚导入一大批账号时突发扇出。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::kiro::ensure_fresh::{RefreshCtx, ensure_fresh};
use crate::kiro::pool::Pool;

/// 扫描间隔。扫描本身只读内存、不发网络,故可以密一些;真正的上游调用由
/// "是否活跃 + 是否临近过期"两道闸决定。
const TICK_SECS: u64 = 10;

/// 「活跃」的定义:近 24 小时内被选中过。
///
/// 取一天而非更短,是因为账号池按优先级/权重轮转,单个账号可能隔很久才轮到一次;
/// 窗口太短会把仍在服役的账号判成闲置,提前续期就形同虚设。
const ACTIVE_WINDOW_SECS: u64 = 24 * 60 * 60;

/// 提前量:令牌剩余不足这个时长就续。
///
/// 比数据面的 300 秒略长 —— 后台续期的意义正是**赶在**数据面判定"即将过期"之前把它办掉,
/// 两者取同一个值的话,后台永远只能和请求抢同一个瞬间,提前也就无从谈起。
const REFRESH_MARGIN_SECS: u64 = 600;

/// 单轮最多续期的账号数。刚批量导入时大量账号的令牌会在相近时刻到期,
/// 不设上限会在同一拍打出几百个刷新请求。
const MAX_PER_TICK: usize = 5;

/// 当前 Unix 秒。注入点集中在此,与本模块其余部分一致地不散读系统时钟。
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 启动后台续期循环。进程存活期间常驻。
pub fn spawn(pool: Arc<Mutex<Pool>>, client: reqwest::Client, ctx: RefreshCtx) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(TICK_SECS));
        // 漏过的拍不补:落后时一次性追平即可,补拍只会在恢复瞬间打出一串刷新。
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let now = now_unix();
            let due = due_for_refresh(&pool, now).await;
            for id in due {
                // 失败由 ensure_fresh 内部反馈池;这里只记录,不重复处置。
                if let Err(e) =
                    ensure_fresh(&pool, &id, &client, now, REFRESH_MARGIN_SECS, Some(&ctx)).await
                {
                    tracing::debug!(
                        event = "keepalive_refresh_failed",
                        credential_id = %id,
                        error = ?e,
                        "活跃账号提前续期失败(已由 ensure_fresh 反馈池,此处仅记录)"
                    );
                }
            }
        }
    });
}

/// 挑出本轮该续期的账号 id:**近期用过** && **令牌临近过期** && 当前可用。
///
/// 只读一次池锁后立即释放 —— 刷新是网络操作,绝不能持池锁跨 await。
async fn due_for_refresh(pool: &Arc<Mutex<Pool>>, now: u64) -> Vec<String> {
    let stats = {
        let guard = pool.lock().await;
        guard.stats(now)
    };
    stats
        .into_iter()
        .filter(|a| {
            // 闲置账号不续:上游流量必须正比于真实使用量。
            let active =
                a.last_used_unix > 0 && now.saturating_sub(a.last_used_unix) <= ACTIVE_WINDOW_SECS;
            // 已禁用/冷却中/被封禁的不续:它们本就不该被选中,续了也用不上,
            // 反而是在给一个上游已经拒绝的账号继续发请求。
            let usable = !a.disabled && !a.in_cooldown && a.status_reason != "banned";
            // 没有到期时刻的凭据不需要续(也无从判断该何时续)。
            let has_expiry = a.expires_at_unix > 0;
            let expiring = a.expires_at_unix <= now.saturating_add(REFRESH_MARGIN_SECS);
            active && usable && has_expiry && expiring
        })
        .map(|a| a.id)
        .take(MAX_PER_TICK)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};
    use crate::kiro::pool::LbMode;

    fn cred(id: &str, expires_at: u64) -> Credential {
        Credential {
            id: id.into(),
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            kiro_api_key: None,
            expires_at_unix: expires_at,
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
            status_reason: None,
        }
    }

    /// 闲置账号绝不续期 —— 这是整个设计的要害:上游流量必须正比于真实使用量,
    /// 否则 253 个账号就变成一条永不停歇的后台心跳,而本项目的账号已被上游以
    /// `security precaution` 封过。
    #[tokio::test]
    async fn idle_accounts_are_never_refreshed() {
        let now = 10_000_000u64;
        // 令牌马上过期,但从没被用过(last_used = 0)
        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("idle", now + 60)],
            LbMode::Priority,
        )));
        assert!(due_for_refresh(&pool, now).await.is_empty());
    }

    /// 用过但已超出活跃窗口的账号同样不续。
    #[tokio::test]
    async fn accounts_idle_longer_than_the_window_are_not_refreshed() {
        let now = 10_000_000u64;
        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("stale", now + 60)],
            LbMode::Priority,
        )));
        {
            // 池内没有公共的"标记已用",用 select() 走真实路径打上使用时刻。
            let mut g = pool.lock().await;
            g.select(now - ACTIVE_WINDOW_SECS - 1).expect("应选出账号");
        }
        assert!(due_for_refresh(&pool, now).await.is_empty());
    }

    /// 近期用过 + 临近过期 → 该续。
    #[tokio::test]
    async fn recently_used_and_expiring_is_due() {
        let now = 10_000_000u64;
        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("hot", now + 60)],
            LbMode::Priority,
        )));
        {
            let mut g = pool.lock().await;
            g.select(now - 60).expect("应选出账号");
        }
        assert_eq!(due_for_refresh(&pool, now).await, vec!["hot".to_string()]);
    }

    /// 令牌还早得很 → 不续。提前量只覆盖临近过期的那一段。
    #[tokio::test]
    async fn a_token_far_from_expiry_is_not_refreshed() {
        let now = 10_000_000u64;
        let pool = Arc::new(Mutex::new(Pool::new(
            vec![cred("fresh", now + REFRESH_MARGIN_SECS * 10)],
            LbMode::Priority,
        )));
        {
            let mut g = pool.lock().await;
            g.select(now - 60).expect("应选出账号");
        }
        assert!(due_for_refresh(&pool, now).await.is_empty());
    }

    /// 单轮有上限:刚批量导入时大量账号的令牌在相近时刻到期,不设上限会在同一拍
    /// 打出几百个刷新请求。
    #[tokio::test]
    async fn a_single_tick_is_capped() {
        let now = 10_000_000u64;
        let creds: Vec<_> = (0..MAX_PER_TICK * 4)
            .map(|i| cred(&format!("c{i}"), now + 60))
            .collect();
        // 这里用 Balanced 而不是 Priority:Priority 现在是**粘滞**的(一个账号一直用到
        // 它不可用),连选 20 次只会把 1 个账号标成"用过",凑不出"一堆账号同时临近过期"
        // 这个被测场景。Balanced 会在账号间铺开,正好造出该场景。
        let pool = Arc::new(Mutex::new(Pool::new(creds, LbMode::Balanced)));
        {
            let mut g = pool.lock().await;
            for _ in 0..MAX_PER_TICK * 4 {
                g.select(now - 60).expect("应选出账号");
            }
        }
        assert_eq!(due_for_refresh(&pool, now).await.len(), MAX_PER_TICK);
    }
}
