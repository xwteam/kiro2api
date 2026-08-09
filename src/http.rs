//! 共享 HTTP 客户端工厂。
//!
//! 全进程的出站 `reqwest::Client` 都应经此工厂构造,以保证每个生产客户端
//! 都带**有界超时**——避免上游卡死时连接无限期挂起(HTTP TIMEOUT 修复)。
//!
//! 两类客户端,按数据面/控制面区分:
//!
//! * [`streaming`] —— 中转数据面(relay `provider::call`)。上游是 SSE/事件流,
//!   合法长流可持续数分钟,故**不设整请求超时**(`.timeout()` 会误杀长流);
//!   改用 `connect_timeout` 界定建连阶段 + `read_timeout` 界定单次读取停顿
//!   ——`read_timeout` 每成功读一段即重置,只掐"卡死不再吐字节"的连接,
//!   不影响仍在持续产出的长流。
//!
//! * [`unary`] —— 控制面一问一答(登录/余额/模型清单/令牌刷新等)。响应短小,
//!   加 `connect_timeout` + 整请求 `timeout` 硬顶,任何环节卡住都在上限内失败。
//!
//! 二者共享同一组连接超时常量,数值集中在此便于调优。

use std::time::Duration;

/// 建连阶段超时(TCP + TLS 握手)。两类客户端通用。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 数据面单次读取停顿上限(每成功读一段即重置)。
/// 只掐"卡死不再吐字节"的流,不误杀仍在持续产出的合法长流。
const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// 控制面整请求硬顶(建连→发送→收全响应)。
const UNARY_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);

/// 空闲连接在池中的存活上限,避免复用早已被上游/中间设备静默关闭的死连接。
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// 每主机保留的空闲连接数:**0 = 不复用连接**。
///
/// 本中转的每个请求都携带**不同账号**的令牌,user-agent 里的 machineId 也随账号变化。
/// 若复用连接,同一条 TCP/TLS 上就会依次出现几十个不同身份 —— 真实的 Kiro 客户端不可能
/// 这样(一台机器、一个账号、一条连接),而「同一条连接上轮换多个身份」是账号共享最直接
/// 的证据,比「同 IP 多账号」强得多:后者还能用 NAT 解释,前者解释不了。
///
/// 线上症状与此吻合:账号**没经过中转时是活的**(直查上游余额正常),被中转用过就以
/// `security precaution` 封停;而长期稳定的 kiro.rs 每个请求都显式带 `Connection: close`。
///
/// 代价是每次请求多一次 TLS 握手。这个代价是值得的 —— 复用省下的那点延迟,换来的是把
/// 整池账号暴露在同一条连接上。
const POOL_MAX_IDLE_PER_HOST: usize = 0;

/// 构造中转数据面(流式)客户端。
///
/// 有 `connect_timeout` + `read_timeout`,**无**整请求超时——见模块文档。
/// 构造失败(TLS 后端初始化异常等)时回落到 `Client::new()`,保证不 panic、
/// 服务仍可起(只是该实例失去超时护栏,概率极低)。
pub fn streaming() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(STREAM_READ_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 构造控制面(一问一答)客户端。
///
/// 有 `connect_timeout` + 整请求 `timeout` 硬顶。构造失败时回落 `Client::new()`。
pub fn unary() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(UNARY_TOTAL_TIMEOUT)
        // 控制面同样不复用:令牌刷新与余额查询一样是**逐账号**的,复用会把多个账号的
        // 刷新请求串在同一条连接上,与数据面同一个问题。
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 工厂只需保证"能构造出客户端且不 panic";超时是否真生效由集成/真机验证覆盖。
    #[test]
    fn streaming_client_builds() {
        let _ = streaming();
    }

    #[test]
    fn unary_client_builds() {
        let _ = unary();
    }

    /// 连接复用上限必须是 0 —— 数据面与控制面都不复用。
    ///
    /// 复用会让同一条 TCP/TLS 上依次出现几十个不同账号的令牌,而每个账号还各自声称是
    /// 不同的机器(user-agent 里的 machineId 各不相同)。真实客户端不可能这样,而
    /// 「同一条连接上轮换多个身份」是账号共享最直接的证据:同 IP 还能用 NAT 解释,
    /// 同一条连接解释不了。线上症状吻合 —— 账号没经过中转时是活的(直查余额正常),
    /// 被中转用过就以 `security precaution` 封停;长期稳定的 kiro.rs 每请求都
    /// 显式带 `Connection: close`。
    ///
    /// 真正会再次出错的场景是有人为省一次 TLS 握手把复用加回来,故把 0 钉死在测试里。
    #[test]
    fn connections_are_never_reused() {
        assert_eq!(
            POOL_MAX_IDLE_PER_HOST, 0,
            "复用连接会把整池账号串在同一条连接上暴露给上游"
        );
    }
}
