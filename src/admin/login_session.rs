//! 交互式登录会话的内存中转态存储(Builder-ID 设备码 / IAM SSO 授权码)。
//!
//! 与 `kiro::login::SessionStore`(一次性 `take` 消费)不同,本存储面向**可重复轮询**的
//! Builder-ID 设备码流:`/poll` 需在 `authorization_pending` 期间反复读取同一会话而不消费,
//! 只在终态(completed/denied/expired)才移除。故这里提供**非消费性 `get`(克隆返回)**、
//! 消费性 `take`,以及基于**注入时钟**的 TTL 过期(默认 ~600s,契合设备码有效期)。
//!
//! 时钟经 `now_unix: u64` 注入(与全仓时钟注入纪律一致),便于测试确定性地推进过期,
//! 不依赖真实挂钟。存储用 `parking_lot::Mutex`(同步、无 await 跨锁)。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

/// 默认会话 TTL(秒):设备码/授权码有效期一般 600s。
pub const DEFAULT_TTL_SECS: u64 = 600;

/// 一条会话记录:落入时刻(unix 秒,注入时钟)+ 负载。
struct Entry<T> {
    created_unix: u64,
    val: T,
}

/// TTL 登录会话存储。`T` 为流特定的中转态(Builder-ID 的 `BuilderIdSession`、
/// IAM SSO 的 `IamSsoSession`)。
///
/// 线程安全、可 `Clone`(内部 `Arc`),直接放进 `MessagesState` 共享。
pub struct LoginSessions<T> {
    ttl_secs: u64,
    map: Arc<Mutex<HashMap<String, Entry<T>>>>,
}

impl<T> Clone for LoginSessions<T> {
    fn clone(&self) -> Self {
        Self {
            ttl_secs: self.ttl_secs,
            map: self.map.clone(),
        }
    }
}

impl<T: Clone> LoginSessions<T> {
    /// 以指定 TTL 新建。
    pub fn new(ttl_secs: u64) -> Self
    where
        T: Send + 'static,
    {
        let map = Arc::new(Mutex::new(HashMap::new()));
        // #11 兜底:除 put/get/take 惰性全表 GC 外,再挂一个后台定时清扫,
        // 使"服务器完全无登录流量"时残留的过期会话也能被回收。
        // 仅在存在 tokio 运行时时启动(生产 serve 内);同步测试构建 router 时
        // 无运行时则跳过(不 spawn、不 panic)。任务持 Weak,存储被 Drop 即自然退出。
        if tokio::runtime::Handle::try_current().is_ok() {
            let weak = Arc::downgrade(&map);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                    let Some(strong) = weak.upgrade() else { break };
                    let now = wall_now_unix();
                    strong.lock().retain(|_, e: &mut Entry<T>| {
                        now.saturating_sub(e.created_unix) <= ttl_secs
                    });
                }
            });
        }
        Self { ttl_secs, map }
    }

    /// 以默认 TTL(600s)新建。
    pub fn with_default_ttl() -> Self
    where
        T: Send + 'static,
    {
        Self::new(DEFAULT_TTL_SECS)
    }

    /// 在持有的锁内清扫所有已按 `now_unix` 过期的会话(惰性全表 GC)。
    ///
    /// #11:此前仅 `put()` 做全表清扫,`get()` 只删自己查的那一条。若长期无新 `put()`
    /// 且客户端放弃轮询(never-completed OIDC 会话),这些会话会永久滞留内存(泄漏)。
    /// 现改为 `put()/get()/take()` 每次进锁都顺带全表清扫,保证即便没有新 put 也能回收。
    /// 调用方须已持锁(`&mut HashMap`),清扫与本次读写共用同一临界区,并发下始终一致。
    fn sweep_expired(&self, m: &mut HashMap<String, Entry<T>>, now_unix: u64) {
        m.retain(|_, e| now_unix.saturating_sub(e.created_unix) <= self.ttl_secs);
    }

    /// 放入一条会话并返回随机 session id(32 字节 CSPRNG、base64url-nopad)。
    /// 顺带按 `now_unix` 清扫过期项(惰性 GC,避免存储无界增长)。
    pub fn put(&self, val: T, now_unix: u64) -> String {
        let id = new_session_id();
        let mut m = self.map.lock();
        self.sweep_expired(&mut m, now_unix);
        m.insert(
            id.clone(),
            Entry {
                created_unix: now_unix,
                val,
            },
        );
        id
    }

    /// 非消费性读取(克隆返回),供 Builder-ID `/poll` 反复轮询。
    /// 未命中 / 已按 `now_unix` 过期 → None。
    ///
    /// #11:进锁即做全表过期清扫(不止查的那一条),回收被放弃的、永不完成的会话,
    /// 使过期项即便在没有新 `put()` 的情况下也能被 `/poll` 流量驱动着回收。
    pub fn get(&self, id: &str, now_unix: u64) -> Option<T> {
        let mut m = self.map.lock();
        self.sweep_expired(&mut m, now_unix);
        // 清扫后仍在表中的必然未过期,直接克隆返回。
        m.get(id).map(|e| e.val.clone())
    }

    /// 消费性取出(移除),供 IAM SSO `/complete` 与终态清理。
    /// 未命中 / 已过期 → None。
    ///
    /// #11:进锁即做全表过期清扫,回收其它被放弃的过期会话。
    pub fn take(&self, id: &str, now_unix: u64) -> Option<T> {
        let mut m = self.map.lock();
        self.sweep_expired(&mut m, now_unix);
        // 清扫后仍在表中的必然未过期;移除并返回(过期项已被清扫掉 → None)。
        m.remove(id).map(|e| e.val)
    }

    /// 显式移除(终态清理:completed/denied/expired 后丢弃会话)。
    pub fn remove(&self, id: &str) {
        self.map.lock().remove(id);
    }
}

/// Builder-ID 设备码会话的存储值:待批准态 + region。
/// `Pending` 本身不含 region,而 `/poll` 需要 region 重建 oidc base,故一并保留。
#[derive(Clone)]
pub struct BuilderIdSession {
    pub pending: crate::kiro::login::builderid::Pending,
    pub region: String,
}

/// IAM SSO 授权码会话的存储值:授权态(含 PKCE verifier + state)+ region。
#[derive(Clone)]
pub struct IamSsoSession {
    pub auth: crate::kiro::login::iam_sso::AuthStart,
    pub region: String,
}

/// 32 字节 CSPRNG → base64url-nopad(43 字符),作会话标识(不可猜)。
fn new_session_id() -> String {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw).expect("CSPRNG");
    URL_SAFE_NO_PAD.encode(raw)
}

/// 后台清扫任务用的挂钟(unix 秒)。业务逻辑仍走注入的 `now_unix`(便于测试),
/// 仅这个后台定时器用真实时钟——它本就是无法注入时钟的独立 janitor。
fn wall_now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_roundtrip_and_id_is_unguessable() {
        let s: LoginSessions<u32> = LoginSessions::new(600);
        let id = s.put(42, 1000);
        assert_eq!(id.len(), 43); // 32 字节 base64url-nopad
        // get 非消费:可反复读
        assert_eq!(s.get(&id, 1000), Some(42));
        assert_eq!(s.get(&id, 1100), Some(42));
        // 两次 put 的 id 不同
        let id2 = s.put(7, 1000);
        assert_ne!(id, id2);
    }

    #[test]
    fn get_expires_after_ttl_and_removes() {
        let s: LoginSessions<u32> = LoginSessions::new(600);
        let id = s.put(42, 1000);
        assert_eq!(s.get(&id, 1600), Some(42)); // 边界内(<= ttl)
        assert_eq!(s.get(&id, 1601), None); // 越界 → 过期
        assert_eq!(s.get(&id, 1000), None); // 已被移除
    }

    #[test]
    fn take_consumes_the_session() {
        let s: LoginSessions<u32> = LoginSessions::new(600);
        let id = s.put(9, 1000);
        assert_eq!(s.take(&id, 1200), Some(9));
        assert_eq!(s.take(&id, 1200), None); // 已消费
        assert_eq!(s.get(&id, 1200), None);
    }

    #[test]
    fn take_expired_returns_none() {
        let s: LoginSessions<u32> = LoginSessions::new(600);
        let id = s.put(9, 1000);
        assert_eq!(s.take(&id, 1601), None);
    }

    #[test]
    fn remove_is_explicit_terminal_cleanup() {
        let s: LoginSessions<u32> = LoginSessions::new(600);
        let id = s.put(9, 1000);
        s.remove(&id);
        assert_eq!(s.get(&id, 1000), None);
    }

    #[test]
    fn put_sweeps_expired_entries() {
        let s: LoginSessions<u32> = LoginSessions::new(600);
        let old = s.put(1, 1000);
        // 远晚于 old 的 TTL 时再 put,触发惰性清扫,old 应被回收。
        let _fresh = s.put(2, 5000);
        assert_eq!(s.get(&old, 5000), None);
    }

    /// #11:被放弃的过期会话必须能被**针对其它 id 的 `get()`** 清扫掉(全表 GC),
    /// 而不只是被查到的那一条被删——证明没有新 `put()` 也不会无界泄漏。
    #[test]
    fn get_sweeps_all_expired_not_just_looked_up() {
        let s: LoginSessions<u32> = LoginSessions::new(600);
        let abandoned = s.put(1, 1000); // 一条永不完成的会话(created=1000)
        let live = s.put(2, 1100); // 稍后创建(created=1100)
        // now=1650:abandoned diff=650>600 已过期;live diff=550<=600 仍活。对 live 做 get。
        assert_eq!(s.get(&live, 1650), Some(2));
        // 全表清扫应已回收 abandoned(即便从没针对它做过 get/put)。
        assert_eq!(map_len(&s), 1);
        assert_eq!(s.get(&abandoned, 1650), None);
    }

    /// #11:`take()` 同样做全表清扫——取走一条时顺带回收其它放弃的过期会话。
    #[test]
    fn take_sweeps_all_expired() {
        let s: LoginSessions<u32> = LoginSessions::new(600);
        let abandoned = s.put(1, 1000); // created=1000
        let target = s.put(2, 1100); // created=1100
        // now=1650:target 未过期(diff=550)被取走;abandoned 过期(diff=650)被清扫。
        assert_eq!(s.take(&target, 1650), Some(2));
        let _ = abandoned;
        assert_eq!(map_len(&s), 0);
    }

    /// 测试用:读内部表长度(仅测试内可见)。
    fn map_len<T: Clone>(s: &LoginSessions<T>) -> usize {
        s.map.lock().len()
    }
}
