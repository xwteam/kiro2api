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
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
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
}
