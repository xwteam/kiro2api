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
/// `security precaution` 封停。
///
/// 代价是每次请求多一次 TLS 握手。这个代价是值得的 —— 复用省下的那点延迟,换来的是把
/// 整池账号暴露在同一条连接上。
const POOL_MAX_IDLE_PER_HOST: usize = 0;

/// 按编译特性选择 TLS 后端。
///
/// 默认 **native-tls(vendored OpenSSL)**:真实 Kiro 客户端是 Electron/Node,TLS 握手走
/// OpenSSL,其 ClientHello(JA3)与 rustls 截然不同。TLS 指纹在任何 HTTP 内容发出**之前**
/// 就暴露,是最标准的机器人识别手段之一;而我们此前只有 rustls。
///
/// 关掉 `native-tls-backend` 特性则回落 rustls(编译快、不需要 OpenSSL 工具链)。
fn with_tls(b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    #[cfg(feature = "native-tls-backend")]
    {
        b.use_native_tls()
    }
    #[cfg(not(feature = "native-tls-backend"))]
    {
        b.use_rustls_tls()
    }
}

/// 构造中转数据面(流式)客户端。
///
/// 有 `connect_timeout` + `read_timeout`,**无**整请求超时——见模块文档。
/// 构造失败(TLS 后端初始化异常等)时回落到 `Client::new()`,保证不 panic、
/// 服务仍可起(只是该实例失去超时护栏,概率极低)。
pub fn streaming() -> reqwest::Client {
    streaming_builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// streaming 客户端的构造配置。抽出来是为了让"按代理分桶"的那份复用同一套设置——
/// 两处各写一遍必然漂移,而这里的每一项(协议、连接池、解压)都是 wire 可观测的。
fn streaming_builder() -> reqwest::ClientBuilder {
    with_tls(reqwest::Client::builder())
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(STREAM_READ_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        // 强制 HTTP/1.1。真实客户端是 aws-sdk-js on Node,其 HTTP handler 默认走 1.1,
        // 不主动升 h2 —— 而我们此前经 ALPN 协商到了 **h2**(实测),协议版本本身就是差异。
        // 另外 `Connection: close` 是 HTTP/1.1 的头,h2 明确禁止它:不锁到 1.1,那个头
        // 只是摆设(此前版本正是如此)。
        .http1_only()
        // **不要**自动补 accept-encoding。开了 gzip/brotli/deflate 特性后 reqwest 会给每个
        // 没显式带该头的请求自动加一个,而真实客户端(aws-sdk-js on Node)数据面**不发**
        // 这个头。特性是为 Social 刷新那条 axios 链路开的(见 `axios`),数据面必须关掉。
        .no_gzip()
        .no_brotli()
        .no_deflate()
}

/// 构造控制面(一问一答)客户端。
///
/// 有 `connect_timeout` + 整请求 `timeout` 硬顶。构造失败时回落 `Client::new()`。
pub fn unary() -> reqwest::Client {
    unary_builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// unary 客户端的构造配置。抽出来是为了让"按代理分桶"的那份复用同一套设置——
/// 两处各写一遍必然漂移,而这里的每一项(协议、连接池、解压)都是 wire 可观测的。
fn unary_builder() -> reqwest::ClientBuilder {
    with_tls(reqwest::Client::builder())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(UNARY_TOTAL_TIMEOUT)
        // 控制面同样锁 1.1:令牌刷新/余额查询与数据面来自同一个出口,协议版本不一致
        // 本身就是可识别的差异。
        .http1_only()
        // 控制面同样不复用:令牌刷新与余额查询一样是**逐账号**的,复用会把多个账号的
        // 刷新请求串在同一条连接上,与数据面同一个问题。
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        // 同数据面:余额查询、模型清单、IdC 刷新在真实客户端里都是 aws-sdk-js 请求,
        // 一律不带 accept-encoding。
        .no_gzip()
        .no_brotli()
        .no_deflate()
}

/// Social 令牌刷新专用客户端:**开着解压**。
///
/// 桌面端用 axios 打自家 auth 服务,axios 固定声明 `Accept-Encoding: gzip, compress,
/// deflate, br`。要对齐这个声明就必须真解得开上游据此压缩的响应,故只有这一条链路开解压;
/// 其余链路都关着,以免 reqwest 给它们自动补上一个真实客户端不发的头。
pub fn axios() -> reqwest::Client {
    axios_builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// axios 客户端的构造配置。抽出来是为了让"按代理分桶"的那份复用同一套设置——
/// 两处各写一遍必然漂移,而这里的每一项(协议、连接池、解压)都是 wire 可观测的。
fn axios_builder() -> reqwest::ClientBuilder {
    with_tls(reqwest::Client::builder())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(UNARY_TOTAL_TIMEOUT)
        .http1_only()
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
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
    /// 被中转用过就以 `security precaution` 封停。
    ///
    /// 真正会再次出错的场景是有人为省一次 TLS 握手把复用加回来,故把 0 钉死在测试里。
    #[test]
    fn connections_are_never_reused() {
        assert_eq!(
            POOL_MAX_IDLE_PER_HOST, 0,
            "复用连接会把整池账号串在同一条连接上暴露给上游"
        );
    }

    /// 数据面与控制面都必须锁 HTTP/1.1。
    ///
    /// 真实客户端是 aws-sdk-js on Node,其 HTTP handler 默认 1.1、不主动升 h2;而我们此前
    /// 经 ALPN 协商到了 **h2**(实测),协议版本本身就是可识别的差异。另外
    /// `Connection: close` 是 HTTP/1.1 的头、h2 明确禁止它 —— 不锁到 1.1,那个头就只是
    /// 摆设(v0.9.0 正是如此,当时误以为它在起作用)。
    ///
    /// reqwest 不暴露该设置的读取接口,故断言构造成功即可:真正会再次出错的是有人为了
    /// 复用/性能把 `.http1_only()` 去掉。
    #[test]
    fn both_clients_build_with_http1_locked() {
        // 构造不 panic 即说明 builder 链(含 http1_only 与 TLS 后端)有效。
        let _ = streaming();
        let _ = unary();
    }

    /// TLS 后端默认必须是 native-tls(OpenSSL),与真实 Electron/Node 客户端一致。
    ///
    /// TLS 指纹(ClientHello/JA3)在任何 HTTP 内容发出**之前**就暴露,是最标准的机器人
    /// 识别手段之一;而我们此前只有 rustls,与真实客户端对不上。
    /// 断言写成"只在未启用该特性时失败":clippy 会把 `assert!(cfg!(..))` 判成常量断言,
    /// 而它要守的东西是真的 —— 默认构建必须带 native-tls。
    #[test]
    #[cfg(not(feature = "native-tls-backend"))]
    fn native_tls_is_the_default_backend() {
        panic!("默认构建必须启用 native-tls:rustls 的 ClientHello 与真实客户端对不上");
    }

    /// 两个客户端都必须锁在 HTTP/1.1。
    ///
    /// 真实客户端是 aws-sdk-js on Node,其 HTTP handler 默认走 1.1、不主动升 h2;而我们
    /// 此前经 ALPN 协商到了 **h2**(实测),协议版本本身就是可识别的差异。另外
    /// `Connection: close` 是 HTTP/1.1 的头,h2 明确禁止它 —— 不锁到 1.1,那个头只是摆设。
    #[test]
    fn all_clients_are_pinned_to_http1() {
        let src = include_str!("http.rs");
        // 只数真正的 builder 调用行(去掉缩进后以 `.http1_only()` 开头),
        // 不数文档注释里提到它的地方 —— 否则加一句注释就会把测试弄红。
        let calls = src
            .lines()
            .filter(|l| l.trim_start().starts_with(concat!(".http1", "_only()")))
            .count();
        assert_eq!(
            calls, 3,
            "数据面、控制面、axios 刷新三个客户端都必须锁 HTTP/1.1"
        );
    }

    /// TLS 后端默认必须是 native-tls(OpenSSL)。
    ///
    /// 真实客户端是 Electron/Node,TLS 握手走 OpenSSL,其 ClientHello(JA3)与 rustls
    /// 截然不同。TLS 指纹在任何 HTTP 内容发出**之前**就暴露,是最标准的机器人识别手段之一。
    #[test]
    fn native_tls_is_the_default_backend() {
        // 读 Cargo.toml 断言默认特性,而不是 `cfg!(...)` —— 后者在本次构建里是编译期常量,
        // 断言恒真、什么也守不住(clippy 会直接指出)。真正要守的是"默认构建带这个特性"。
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains(concat!("default = [\"native-tls", "-backend\"]")),
            "默认构建必须启用 native-tls-backend,否则 TLS 指纹与真实客户端对不上"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 按代理分桶的客户端缓存
// ─────────────────────────────────────────────────────────────────────────────

/// 客户端种类。三种在超时/协议/解压上的取值不同,不能混用同一个实例。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Streaming,
    Unary,
    Axios,
}

/// 缓存键:种类 + 代理(地址/用户名/密码)。同一个地址配不同凭证算作不同出口。
type CacheKey = (Kind, Option<(String, Option<String>, Option<String>)>);

static CLIENTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<CacheKey, reqwest::Client>>,
> = std::sync::OnceLock::new();

fn build_kind(
    kind: Kind,
    proxy: Option<&(String, Option<String>, Option<String>)>,
) -> reqwest::Client {
    let base = match kind {
        Kind::Streaming => return with_proxy(streaming_builder(), proxy),
        Kind::Unary => unary_builder(),
        Kind::Axios => axios_builder(),
    };
    with_proxy(base, proxy)
}

fn with_proxy(
    b: reqwest::ClientBuilder,
    proxy: Option<&(String, Option<String>, Option<String>)>,
) -> reqwest::Client {
    let built = match proxy {
        None => b.build(),
        Some((url, user, pass)) => match reqwest::Proxy::all(url) {
            Ok(mut p) => {
                if let (Some(u), Some(pw)) = (user, pass) {
                    p = p.basic_auth(u, pw);
                }
                b.proxy(p).build()
            }
            Err(_) => {
                // 代理地址不合法时**不静默直连**:那会让这个账号在运营者以为走代理的情况下
                // 从主 IP 发出去,是最糟的一种"看起来配好了"。这里大声报错并仍然直连,
                // 因为拒绝构造客户端会让整个账号不可用——两害相权,记日志更可诊断。
                tracing::error!(
                    proxy = %url,
                    "代理地址无法解析,该客户端将直连——请检查 proxyUrl(支持 http/https/socks5)"
                );
                b.build()
            }
        },
    };
    built.unwrap_or_else(|_| reqwest::Client::new())
}

/// 取(或构造并缓存)一个客户端。
fn client_of(
    kind: Kind,
    proxy: Option<(String, Option<String>, Option<String>)>,
) -> reqwest::Client {
    let cache = CLIENTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let key = (kind, proxy.clone());
    if let Ok(map) = cache.lock()
        && let Some(c) = map.get(&key)
    {
        return c.clone();
    }
    let client = build_kind(kind, proxy.as_ref());
    if let Ok(mut map) = cache.lock() {
        map.insert(key, client.clone());
    }
    client
}

/// 该凭据的数据面客户端(按其代理分桶)。
pub fn streaming_for(cred: &crate::kiro::credential::Credential) -> reqwest::Client {
    client_of(Kind::Streaming, effective_proxy_of(cred))
}

/// 该凭据的控制面客户端(令牌刷新、余额、模型清单)。
///
/// **必须与数据面走同一个出口** —— 一个账号的数据面从代理出、刷新却从主 IP 出,
/// 比不配代理更糟:那等于主动告诉上游这两条流量属于同一个中转。
pub fn unary_for(cred: &crate::kiro::credential::Credential) -> reqwest::Client {
    client_of(Kind::Unary, effective_proxy_of(cred))
}

/// 该凭据的 Social 刷新客户端(axios 形态,开着解压)。出口同上。
pub fn axios_for(cred: &crate::kiro::credential::Credential) -> reqwest::Client {
    client_of(Kind::Axios, effective_proxy_of(cred))
}

fn effective_proxy_of(
    cred: &crate::kiro::credential::Credential,
) -> Option<(String, Option<String>, Option<String>)> {
    cred.effective_proxy(crate::kiro::provider::config_proxy_url().as_deref())
}
