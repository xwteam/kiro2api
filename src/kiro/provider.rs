//! Kiro 数据面 provider:构造伪装头、发请求、端点 429 回退、把成败反馈账号池。
//! 头/URL/UA 形态为照观测的 wire 事实(真机核对);编排与代码为本项目自写。
use crate::kiro::credential::Credential;
use crate::kiro::endpoint::{Endpoint, sorted};
use crate::kiro::login::LoginError;
use crate::kiro::pool::{FailureKind, Pool, classify, classify_with_body};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// 伪装身份(版本号自定,从配置注入)。
#[derive(Debug, Clone)]
pub struct Impersonation {
    pub machine_id: String,
    pub kiro_version: String,
    pub agent_mode: String,
    /// user-agent 里的 `os/{system_version}`(如 `win32#10.0.22631`)。
    pub system_version: String,
    /// user-agent 里的 `md/nodejs#{node_version}`(如 `22.22.0`)。
    pub node_version: String,
}

/// 伪装用的进程级静态配置(kiro/系统/node 版本 + 全局 machineId)。
///
/// 为什么要有这么一份:令牌刷新发生在 `ensure_fresh` 里,而那条路径上没有 `Config`——
/// 它被 keepalive、余额查询、模型清单、数据面共用,每个调用方都塞一份 config 进去会把
/// 签名污染一大片。而这几个值本就是**进程启动后不再变**的静态配置,放一份全局的正合适。
///
/// 没初始化时回落 [`crate::config::Config::default`] 的取值(测试即走这条),
/// 于是任何路径都不会因为"忘了初始化"而发出空的版本号。
#[derive(Debug, Clone)]
pub struct ImpersonationDefaults {
    pub kiro_version: String,
    pub system_version: String,
    pub node_version: String,
    pub machine_id: Option<String>,
    /// 全局出站代理(凭据级未设时的兜底)。
    pub proxy_url: Option<String>,
}

static IMP_DEFAULTS: std::sync::OnceLock<ImpersonationDefaults> = std::sync::OnceLock::new();

/// 进程启动时灌入一次。重复调用无效(OnceLock 语义),不会中途改变身份。
pub fn init_impersonation_defaults(cfg: &crate::config::Config) {
    let _ = IMP_DEFAULTS.set(ImpersonationDefaults {
        kiro_version: cfg.kiro_version.clone(),
        system_version: cfg.system_version.clone(),
        node_version: cfg.node_version.clone(),
        machine_id: cfg.machine_id.clone(),
        proxy_url: cfg.proxy_url.clone(),
    });
}

/// 进程级配置里的全局 machineId(未初始化时取 `Config::default`)。
pub fn config_machine_id() -> Option<String> {
    imp_defaults().machine_id
}

/// 进程级配置里的全局出站代理。
pub fn config_proxy_url() -> Option<String> {
    imp_defaults().proxy_url
}

fn imp_defaults() -> ImpersonationDefaults {
    IMP_DEFAULTS.get().cloned().unwrap_or_else(|| {
        let d = crate::config::Config::default();
        ImpersonationDefaults {
            kiro_version: d.kiro_version,
            system_version: d.system_version,
            node_version: d.node_version,
            machine_id: d.machine_id,
            proxy_url: d.proxy_url,
        }
    })
}

impl Impersonation {
    /// 同 [`Impersonation::for_credential`],但从进程级默认值取静态配置。
    /// 供拿不到 `Config` 的路径(令牌刷新)使用。
    pub fn for_credential_global(cred: &Credential) -> Self {
        let d = imp_defaults();
        let machine_id = crate::kiro::machine_id::for_credential(
            cred.machine_id.as_deref(),
            d.machine_id.as_deref(),
            cred.is_api_key(),
            cred.kiro_api_key.as_deref(),
            &cred.refresh_token,
        )
        .unwrap_or_else(|| crate::kiro::machine_id::fallback_for(&cred.id));
        Self {
            machine_id,
            kiro_version: d.kiro_version,
            agent_mode: "vibe".to_string(),
            system_version: d.system_version,
            node_version: d.node_version,
        }
    }

    /// 为一条凭据定出完整伪装身份。**全项目唯一入口** —— 数据面、令牌刷新、余额查询、
    /// 模型清单都必须走它。
    ///
    /// 此前 handler / balance / models_cache 各写了一份 `impersonation_for`,而且三份并不
    /// 一致:只有数据面那份认配置级 machineId,另两份不认。于是配置里设了 machineId 时,
    /// 同一个账号在数据面报一个机器、在余额查询报另一个 —— 同一时刻同一个 Bearer 来自
    /// 两台机器,这恰恰是最该避免的形状。刷新链路更是**一份都没有**。
    pub fn for_credential(cred: &Credential, cfg: &crate::config::Config) -> Self {
        let machine_id = crate::kiro::machine_id::for_credential(
            cred.machine_id.as_deref(),
            cfg.machine_id.as_deref(),
            cred.is_api_key(),
            cred.kiro_api_key.as_deref(),
            &cred.refresh_token,
        )
        .unwrap_or_else(|| crate::kiro::machine_id::fallback_for(&cred.id));
        Self {
            machine_id,
            kiro_version: cfg.kiro_version.clone(),
            agent_mode: "vibe".to_string(),
            system_version: cfg.system_version.clone(),
            node_version: cfg.node_version.clone(),
        }
    }
}

fn hv(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// 错误响应体读取上限(字节)。#3:`resp.text()` 会**无界**缓冲整个错误体,
/// 而分类只用到 ≤200 字符的标记片段,故读到该上限即停,避免被超大/恶意错误体撑爆内存。
const ERROR_BODY_CAP: usize = 8 * 1024; // 8 KiB

/// 有界读取一个(非 2xx)应答体:按 chunk 累积,达到 [`ERROR_BODY_CAP`] 即停并丢弃其余,
/// 返回已读片段的 UTF-8 有损解码。用于错误分类——只需前若干字节的标记片段,无需整体缓冲。
///
/// 传输中断/读取错误时返回已累积的部分(尽力而为),不 panic:分类对残缺片段仍安全
/// (命中即升级、不命中即保守)。
///
/// 控制面(`kiro::refresh`)读刷新端点的错误体时复用同一上限与同一读法,故对 crate 内可见。
pub(crate) async fn read_body_capped(mut resp: reqwest::Response) -> String {
    let mut buf: Vec<u8> = Vec::new();
    while buf.len() < ERROR_BODY_CAP {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = ERROR_BODY_CAP - buf.len();
                let take = remaining.min(chunk.len());
                buf.extend_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    break; // 达上限,丢弃其余并停止拉流
                }
            }
            Ok(None) => break, // 体读完
            Err(_) => break,   // 传输错误 → 用已累积的部分尽力而为
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// 生成 `amz-sdk-invocation-id` 的取值:**UUID v4 的标准写法**(8-4-4-4-12,带连字符)。
///
/// 此前发的是 32 位无连字符十六进制。aws-sdk-js 这个头**永远**是 UUID v4,格式不对是个
/// 零成本、100% 命中的判别器 —— 每一个请求都带着它。这里手工按 RFC 4122 置好 version
/// 与 variant 位并加连字符,不引第三方依赖。
pub fn new_invocation_id() -> String {
    crate::kiro::uuid_v4()
}

/// 从 `https://host/path` 里取出 host(不含端口以外的任何修饰)。
/// 取不出就返回 None —— 那种情况下交给 hyper 自己补,总好过发一个错的 host。
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| host.to_string())
}

/// 构造一次数据面请求的头(照观测,契约 §3)。
pub fn build_headers(
    cred: &Credential,
    imp: &Impersonation,
    ep: &Endpoint,
    invocation_id: &str,
) -> HeaderMap {
    // **插入顺序即线上顺序**(HeaderMap 按插入序迭代),故这里的次序不是风格问题。
    //
    // HTTP 头顺序是与 TLS 指纹并列的一个标准识别维度:同一个客户端库发出的头次序是固定的。
    // 此前我们的次序与真实客户端不同,且 `host` 因为没有显式设置、被 hyper 追加到了倒数
    // 第二位(真实客户端把它排在中间)。本函数的次序照真实客户端逐项对齐,改动前请先用
    // 探针抓一遍真实字节再动。
    let mut h = HeaderMap::new();
    h.insert("content-type", HeaderValue::from_static("application/json"));
    // 每请求一条新连接,**绝不复用**。
    //
    // 我们本来走 reqwest 连接池(idle 90s),于是同一条 TCP/TLS 连接上会依次发出不同账号的
    // 令牌,而每个账号还各自声称是不同的机器(user-agent 里的 machineId 各不相同)。
    // 真实的 Kiro 客户端不可能这样 —— 一台机器、一个账号、一条连接。「同一条连接上轮换
    // 多个身份」是账号共享/盗用最直接的证据,比「同 IP 多账号」强得多:后者还能用 NAT 或
    // 公司网络解释,前者解释不了。
    //
    // 线上症状与此吻合:账号在**没经过中转时是活的**(直查上游余额正常),一旦被中转用过就
    // 被以 `security precaution` 封停。真实客户端每个请求都显式
    // 带 `Connection: close`。这是两边在 wire 上最实质的差异,故对齐。
    h.insert("connection", HeaderValue::from_static("close"));
    h.insert(
        "x-amzn-codewhisperer-optout",
        HeaderValue::from_static("true"),
    );
    h.insert("x-amzn-kiro-agent-mode", hv(&imp.agent_mode));
    if let Ok(name) = HeaderName::from_bytes(b"x-amz-user-agent") {
        h.insert(
            name,
            hv(&format!(
                "aws-sdk-js/1.0.34 KiroIDE-{}-{}",
                imp.kiro_version, imp.machine_id
            )),
        );
    }
    h.insert(
        reqwest::header::USER_AGENT,
        hv(&format!(
            "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererstreaming#1.0.34 m/E KiroIDE-{}-{}",
            imp.system_version, imp.node_version, imp.kiro_version, imp.machine_id
        )),
    );
    // 显式设 host。不设的话 hyper 会在序列化时把它补到末尾,位置与真实客户端不同;
    // 取值本身一样,但"在头序列里的落位"同样是可观测的。
    if let Some(host) = host_of(&ep.url) {
        h.insert("host", hv(&host));
    }
    h.insert("amz-sdk-invocation-id", hv(invocation_id));
    h.insert(
        "amz-sdk-request",
        HeaderValue::from_static("attempt=1; max=3"),
    );
    h.insert("authorization", hv(&format!("Bearer {}", cred.bearer())));
    // API Key 凭据必须显式声明令牌类型,否则上游按 OAuth 令牌解析这枚 ksk 并拒绝。
    // 该头为功能必需的 wire 事实(照观测,无公开文档),与 bearer 取值必须同时成立。
    if cred.is_api_key() {
        h.insert("tokentype", HeaderValue::from_static("API_KEY"));
    }
    if let Some(t) = ep.target
        && let Ok(name) = HeaderName::from_bytes(b"x-amz-target")
    {
        h.insert(name, hv(t));
    }
    h
}

/// 向单个端点发一次请求。成功返回原始 Response(流由上层解析);非 2xx → Err。
pub async fn call(
    client: &reqwest::Client,
    ep: &Endpoint,
    cred: &Credential,
    imp: &Impersonation,
    body: &[u8],
) -> Result<reqwest::Response, LoginError> {
    let inv = new_invocation_id();
    let resp = client
        .post(&ep.url)
        .headers(build_headers(cred, imp, ep, &inv))
        .body(body.to_vec())
        .send()
        .await
        .map_err(|_| LoginError::Http)?;
    let status = resp.status().as_u16();
    if resp.status().is_success() {
        Ok(resp)
    } else {
        // #2 body capture:非 2xx 必须**先读响应体**再返回,否则真 AWS/OIDC 错误信息
        //(如 "The security token included in the request is invalid.")被丢弃,body-aware
        // classify_with_body 永远看不到失效信号 → AuthInvalid(永久禁用)不可达。
        // #3:有界读取(≤ERROR_BODY_CAP),避免无界缓冲超大/恶意错误体。
        // #6:此 body 会被调用方(handler 的 override 路径)直接喂给 classify_with_body,
        // 故这里携带**完整原始体**(仅做有界截断),**不**再走 extract_upstream_message 的
        // 单字段有损摘要——否则活在 `__type` 的机器稳定异常名(ExpiredTokenException/
        // InvalidGrantException)会被丢,分类退化。`call` 的唯一生产调用点即刻分类、不作展示。
        let body = read_body_capped(resp).await;
        Err(LoginError::UpstreamHttp { status, body })
    }
}

/// 失败分类的"信号强度":数字越大,证据越硬。
///
/// - `Transient`:无 HTTP 应答的传输层错误,证据最弱;
/// - `AuthAmbiguous`:401/403 但体里没有确证;
/// - `Quota`:上游明确的 429/402(该账号本轮已撞配额墙,应吃长冷却);
/// - `AuthInvalid`/`InvalidRequest`:上游明确回了作废/确定性拒绝的机器稳定信号。
fn signal_strength(kind: FailureKind) -> u8 {
    match kind {
        FailureKind::Transient => 0,
        FailureKind::AuthAmbiguous => 1,
        FailureKind::Quota => 2,
        // 模型不可用与确定性请求错误同为"机器稳定信号":都不该被后续端点的传输错误盖掉。
        FailureKind::AuthInvalid | FailureKind::InvalidRequest | FailureKind::ModelUnavailable => 3,
    }
}

/// 多端点回退时合并两次失败的分类:保留**信号更强**的那个。
///
/// 先收到的 429(配额,应触发长冷却)若被后一个端点的传输层错误覆盖成 `Transient`,账号就会
/// 错过长冷却、继续去撞配额墙;故回退过程中只允许分类向上升级,不允许被更弱的信号冲掉。
fn strongest(a: FailureKind, b: FailureKind) -> FailureKind {
    if signal_strength(b) > signal_strength(a) {
        b
    } else {
        a
    }
}

/// 数据面失败:分类 + **上游原始响应体**(可能为空:传输层失败或 429 端点回退时没读体)。
///
/// 之前只回 `FailureKind`,响应体分类完就丢,于是失败日志里的"详情"列在生产恒为空 ——
/// 运维点开只看到一个 `—`,真正的上游错误信息(权限不足?模型不可用?令牌失效?)只存在于
/// 进程日志里,面板上查不到。这里把它一并带上来,落进失败/限流日志。
#[derive(Debug, Clone)]
pub struct DataPlaneFailure {
    pub kind: FailureKind,
    /// 上游原始响应体(已按 `ERROR_BODY_CAP` 截断);无体时为空串。
    pub body: String,
}

impl DataPlaneFailure {
    fn bare(kind: FailureKind) -> Self {
        Self {
            kind,
            body: String::new(),
        }
    }
}

/// 内部:按给定端点列表尝试,429 回退下一个;返回 Response 或分类失败(带响应体)。
async fn try_endpoints(
    client: &reqwest::Client,
    eps: &[Endpoint],
    cred: &Credential,
    imp: &Impersonation,
    body: &[u8],
) -> Result<reqwest::Response, DataPlaneFailure> {
    let mut last = FailureKind::Transient;
    for ep in eps {
        let inv = new_invocation_id();
        match client
            .post(&ep.url)
            .headers(build_headers(cred, imp, ep, &inv))
            .body(body.to_vec())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => return Ok(r),
            Ok(r) => {
                let status = r.status().as_u16();
                // #2 body capture(端点回退路径):429 是配额、直接换下个端点、无需读体;
                // 其余终态(尤其 401/403)必须**读响应体**再分类,否则真凭据失效信号被丢弃、
                // AuthInvalid(永久禁用)在生产回退路径同样不可达。读体做单行化 + 截断。
                if classify(status) == FailureKind::Quota {
                    last = strongest(last, FailureKind::Quota);
                    continue; // 429 → 试下一个端点
                }
                // #6:分类必须喂**完整原始体**,而非 extract_upstream_message 的有损摘要
                //(摘要只挑一个字段,会丢掉活在 `__type` 里的机器稳定异常名,例如
                // ExpiredTokenException/InvalidGrantException → 分类退化)。
                // #3:有界读取(≤ERROR_BODY_CAP),避免无界缓冲超大/恶意错误体。
                let raw = read_body_capped(r).await;
                // 记录真实上游状态码与响应体摘要——否则分类后二者被丢弃,上层只见合成的
                // status=0,无从判断是模型不支持/权限不足/上游异常。响应体截断避免日志过长。
                tracing::warn!(
                    event = "upstream_http_error",
                    endpoint = %ep.url,
                    status = status,
                    body = %raw.chars().take(400).collect::<String>(),
                    "上游返回非 2xx,按响应体分类后重试下一个端点/账号"
                );
                // 其余终态错误按原始响应体分类后返回,并把响应体一并交给上层落库。
                return Err(DataPlaneFailure {
                    kind: classify_with_body(status, &raw),
                    body: raw,
                });
            }
            Err(e) => {
                // 传输层错误(连接被重置/超时/请求或解码失败等):无 HTTP 状态可依,归
                // Transient。记录底层错误串——否则上层只看到合成的 status=0,无从区分是
                // 网络抖动,还是上游对该请求(如未知 modelId)在字节层直接断连。
                tracing::warn!(
                    event = "upstream_transport_error",
                    endpoint = %ep.url,
                    error = %e,
                    is_timeout = e.is_timeout(),
                    is_connect = e.is_connect(),
                    is_request = e.is_request(),
                    is_body = e.is_body(),
                    "上游传输层失败,尝试下一个端点"
                );
                last = strongest(last, FailureKind::Transient);
                continue; // 网络错误 → 试下一个端点
            }
        }
    }
    Err(DataPlaneFailure::bare(last))
}

/// 选出的账号 + 端点回退 + 反馈池。成功返回 Response;失败已 report 并返回 `(LoginError, FailureKind)`。
///
/// 错误里附带 [`FailureKind`],使上层(统计接线)无需重新解析状态码即可把失败归类到
/// 失败日志(Auth=401/403)/限流日志(Quota=429)/瞬时,而不改变既有反馈池语义。
// 参数个数由任务约定的公开接口形状决定(client/pool/region/preferred/fallback/cred/imp/body/now_unix),
// 拆分成 struct 会偏离既定接口签名,故按需放行该 lint。
#[allow(clippy::too_many_arguments)]
pub async fn call_with_fallback(
    client: &reqwest::Client,
    pool: &mut Pool,
    region: &str,
    preferred: &str,
    fallback: bool,
    cred: &Credential,
    imp: &Impersonation,
    body: &[u8],
    now_unix: u64,
) -> Result<reqwest::Response, (LoginError, FailureKind)> {
    let eps = sorted(region, preferred, fallback);
    match try_endpoints(client, &eps, cred, imp, body).await {
        Ok(r) => {
            pool.report_success(&cred.id);
            Ok(r)
        }
        Err(f) => {
            pool.report_failure(&cred.id, f.kind, now_unix);
            let kind = f.kind;
            Err((
                LoginError::Upstream(format!("data-plane failed: {kind:?}")),
                kind,
            ))
        }
    }
}

/// 与 [`call_with_fallback`] 同构,但**不反馈池**:端点回退后把结果原样返回给调用方,
/// 由调用方决定是否 report_success / report_failure。
///
/// 动机:relay 需在数据面 401/403(Auth)时**先强制刷新令牌重试一次**,只有重试仍失败才
/// 反馈池(禁用账号)。若沿用会内部 report_failure 的 [`call_with_fallback`],首个 Auth
/// 失败就会立即禁用一个其实可刷新自愈的好账号。成功返回 Response;失败返回 [`FailureKind`]。
#[allow(clippy::too_many_arguments)]
pub async fn call_with_fallback_no_report(
    client: &reqwest::Client,
    region: &str,
    preferred: &str,
    fallback: bool,
    cred: &Credential,
    imp: &Impersonation,
    body: &[u8],
) -> Result<reqwest::Response, DataPlaneFailure> {
    let eps = sorted(region, preferred, fallback);
    try_endpoints(client, &eps, cred, imp, body).await
}

#[cfg(test)]
mod tests {
    /// `amz-sdk-invocation-id` 必须是 UUID v4 的标准写法。
    ///
    /// 回归:此前发的是 32 位无连字符十六进制。aws-sdk-js 这个头**永远**是 UUID v4,
    /// 而它出现在每一个请求上 —— 格式不对就是个零成本、100% 命中的判别器。
    #[test]
    fn invocation_id_is_a_uuid_v4() {
        let id = super::new_invocation_id();
        assert_eq!(id.len(), 36, "带连字符的 UUID 长 36:{id}");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "分段应为 8-4-4-4-12:{id}"
        );
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "只能是十六进制与连字符:{id}"
        );
        // version nibble = 4,variant 高两位 = 10
        assert_eq!(
            parts[2].chars().next().unwrap(),
            '4',
            "version 必须是 4:{id}"
        );
        assert!(
            matches!(parts[3].chars().next().unwrap(), '8' | '9' | 'a' | 'b'),
            "variant 必须是 RFC 4122:{id}"
        );
        assert_ne!(super::new_invocation_id(), id, "每次必须不同");
    }

    /// 从 URL 取 host。
    #[test]
    fn host_of_extracts_the_authority() {
        assert_eq!(
            super::host_of("https://q.us-east-1.amazonaws.com/generateAssistantResponse"),
            Some("q.us-east-1.amazonaws.com".into())
        );
        assert_eq!(
            super::host_of("http://127.0.0.1:8080/x?y=1"),
            Some("127.0.0.1:8080".into())
        );
        assert_eq!(super::host_of("not-a-url"), None);
    }

    /// ksk 凭据的两件事必须同时成立:bearer 是 ksk 本身,且带 `tokentype: API_KEY`。
    /// 只做前者会让上游按 OAuth 令牌去解析这枚 key 并拒绝——而错误信息不会提到令牌类型,
    /// 排查时看起来就像"这个 key 是坏的"。
    #[test]
    fn api_key_credential_sends_ksk_and_declares_token_type() {
        let ep = Endpoint {
            url: "http://x/".into(),
            origin: "AI_EDITOR",
            target: None,
        };
        let mut c = cred();
        c.kiro_api_key = Some("ksk_live_123".into());
        c.auth = AuthMethod::ApiKey;
        let h = build_headers(&c, &imp(), &ep, "inv");
        assert_eq!(h.get("authorization").unwrap(), "Bearer ksk_live_123");
        assert_eq!(h.get("tokentype").unwrap(), "API_KEY");
    }

    /// OAuth 凭据不得带 `tokentype`:多一个上游没预期的头,风险不对称(有害无益)。
    #[test]
    fn oauth_credential_does_not_declare_token_type() {
        let ep = Endpoint {
            url: "http://x/".into(),
            origin: "AI_EDITOR",
            target: None,
        };
        let h = build_headers(&cred(), &imp(), &ep, "inv");
        assert_eq!(h.get("authorization").unwrap(), "Bearer AT");
        assert!(h.get("tokentype").is_none());
    }
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};
    use crate::kiro::endpoint::Endpoint;
    use crate::kiro::pool::{LbMode, Pool};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cred() -> Credential {
        Credential {
            quota_reset_unix: None,
            priority: 999,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            id: "a".into(),
            access_token: "AT".into(),
            refresh_token: "rt".into(),
            kiro_api_key: None,
            expires_at_unix: u64::MAX,
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
    fn imp() -> Impersonation {
        Impersonation {
            machine_id: "mid".into(),
            kiro_version: "0.0.1".into(),
            agent_mode: "vibe".into(),
            system_version: "win32#10.0.22631".into(),
            node_version: "22.22.0".into(),
        }
    }

    #[test]
    fn headers_carry_bearer_and_impersonation() {
        let ep = Endpoint {
            url: "http://x/".into(),
            origin: "AI_EDITOR",
            target: Some("AmazonCodeWhispererStreamingService.GenerateAssistantResponse"),
        };
        let h = build_headers(&cred(), &imp(), &ep, "inv-1");
        assert_eq!(h.get("authorization").unwrap(), "Bearer AT");
        assert_eq!(h.get("x-amzn-codewhisperer-optout").unwrap(), "true");
        assert!(
            h.get("user-agent")
                .unwrap()
                .to_str()
                .unwrap()
                .contains("KiroIDE-0.0.1-mid")
        );
        assert_eq!(h.get("x-amz-target").unwrap(), &ep.target.unwrap());
    }

    #[test]
    fn headers_carry_x_amz_user_agent_and_sdk_request() {
        let ep = Endpoint {
            url: "http://x/".into(),
            origin: "AI_EDITOR",
            target: None,
        };
        let h = build_headers(&cred(), &imp(), &ep, "inv-1");
        let xamz_ua = h.get("x-amz-user-agent").unwrap().to_str().unwrap();
        assert_eq!(xamz_ua, "aws-sdk-js/1.0.34 KiroIDE-0.0.1-mid");
        let sdk_req = h.get("amz-sdk-request").unwrap().to_str().unwrap();
        assert_eq!(sdk_req, "attempt=1; max=3");
    }

    #[test]
    fn user_agent_matches_full_contract_format() {
        let ep = Endpoint {
            url: "http://x/".into(),
            origin: "AI_EDITOR",
            target: None,
        };
        let h = build_headers(&cred(), &imp(), &ep, "inv-1");
        let ua = h.get("user-agent").unwrap().to_str().unwrap();
        assert!(ua.starts_with("aws-sdk-js/1.0.34 ua/2.1 os/win32#10.0.22631 lang/js"));
        assert!(ua.contains("md/nodejs#22.22.0"));
        assert!(ua.contains("api/codewhispererstreaming#1.0.34"));
        assert!(ua.contains("m/E KiroIDE-0.0.1-mid"));
    }

    #[tokio::test]
    async fn call_with_fallback_succeeds_and_reports() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/generateAssistantResponse"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let mut pool = Pool::new(vec![cred()], LbMode::Priority);
        // preferred="kiro" 且 base 指向 wiremock:用 Endpoint 直改 url 的方式,通过 sorted 拿到再改 base
        // 简化:直接用 call() 测成功;call_with_fallback 的端点 URL 用 server.uri() 注入见下
        let ep = Endpoint {
            url: format!("{}/generateAssistantResponse", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        };
        let resp = call(&client, &ep, &cred(), &imp(), b"body").await.unwrap();
        assert_eq!(resp.status(), 200);
        // 成功反馈
        pool.report_success("a");
        assert_eq!(pool.active_count(0), 1);
    }

    #[tokio::test]
    async fn call_maps_non_success_to_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let ep = Endpoint {
            url: format!("{}/x", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        };
        let r = call(&client, &ep, &cred(), &imp(), b"body").await;
        // #2:非 2xx 现在归为 UpstreamHttp{status, body}(携带数字状态码),而非笼统 Upstream。
        assert!(matches!(
            r,
            Err(LoginError::UpstreamHttp { status: 403, .. })
        ));
    }

    #[tokio::test]
    async fn call_captures_real_body_for_auth_invalid() {
        // #2 回归:非 2xx 必须先读响应体再返回,真 AWS 失效信息随 UpstreamHttp.body 传出,
        // 使调用方喂给 classify_with_body 能命中 AuthInvalid(否则永久禁用不可达)。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string("The security token included in the request is invalid."),
            )
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let ep = Endpoint {
            url: format!("{}/x", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        };
        let r = call(&client, &ep, &cred(), &imp(), b"body").await;
        match r {
            Err(LoginError::UpstreamHttp { status, body }) => {
                assert_eq!(status, 403);
                assert!(body.contains("security token included in the request is invalid"));
                // 调用方契约:status + body 一并可用 → classify_with_body 得 AuthInvalid。
                assert_eq!(
                    crate::kiro::pool::classify_with_body(status, &body),
                    FailureKind::AuthInvalid
                );
            }
            other => panic!("expected UpstreamHttp with real body, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn try_endpoints_403_with_invalid_token_body_is_auth_invalid() {
        // #2 端点回退路径:403 带真失效信号 → 读体后 classify_with_body 升级为 AuthInvalid。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(403).set_body_string(r#"{"error":"invalid_grant"}"#),
            )
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let eps = vec![Endpoint {
            url: format!("{}/x", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        }];
        let r = try_endpoints(&client, &eps, &cred(), &imp(), b"body").await;
        assert!(matches!(&r, Err(f) if f.kind == FailureKind::AuthInvalid));
    }

    #[tokio::test]
    async fn try_endpoints_does_not_replay_429_onto_another_endpoint() {
        let bad = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&bad)
            .await;
        let good = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .mount(&good)
            .await;
        let client = reqwest::Client::new();
        let eps = vec![
            Endpoint {
                url: format!("{}/a", bad.uri()),
                origin: "AI_EDITOR",
                target: None,
            },
            Endpoint {
                url: format!("{}/b", good.uri()),
                origin: "AI_EDITOR",
                target: None,
            },
        ];
        let r = try_endpoints(&client, &eps, &cred(), &imp(), b"body").await;
        // **不再**因为 429 就换端点重放。此前一次限流会被放大成对多个入口的连续请求,
        // 而那两个入口真实客户端根本不用——"被限流的账号紧接着去探别的服务入口"是探测
        // 行为的形状。现在限流就只是限流,原地返回、由上层退避。
        assert!(
            matches!(&r, Err(f) if f.kind == FailureKind::Transient),
            "429 不该被回退到下一个端点,got {r:?}"
        );
        assert_eq!(
            good.received_requests().await.unwrap().len(),
            0,
            "第二个端点不该被打到"
        );
    }

    #[tokio::test]
    async fn try_endpoints_treats_429_as_transient_not_quota() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let eps = vec![
            Endpoint {
                url: format!("{}/a", server.uri()),
                origin: "AI_EDITOR",
                target: None,
            },
            Endpoint {
                url: format!("{}/b", server.uri()),
                origin: "AI_EDITOR",
                target: None,
            },
        ];
        let r = try_endpoints(&client, &eps, &cred(), &imp(), b"body").await;
        // 429 是限流,不是配额:归 Transient,由上层退避重试,账号不吃长罚。
        assert!(
            matches!(&r, Err(f) if f.kind == FailureKind::Transient),
            "got {r:?}"
        );
    }

    #[tokio::test]
    async fn try_endpoints_keeps_quota_over_later_transport_error() {
        // 先收到 402(额度真的耗尽),再在下一个端点上撞传输层错误:分类必须仍是 Quota。
        // 若被最后一个错误覆盖成 Transient,这个已经没额度的号只记 1 个 strike,
        // 会立刻被重新选中继续撞同一堵墙。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(402))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let eps = vec![
            Endpoint {
                url: format!("{}/a", server.uri()),
                origin: "AI_EDITOR",
                target: None,
            },
            // 不可达端口 → 传输层错误(Transient),不得把先到的 Quota 冲掉。
            Endpoint {
                url: "http://127.0.0.1:1/unreachable".into(),
                origin: "AI_EDITOR",
                target: None,
            },
        ];
        let r = try_endpoints(&client, &eps, &cred(), &imp(), b"body").await;
        assert!(
            matches!(&r, Err(f) if f.kind == FailureKind::Quota),
            "402 的配额信号不得被后续端点的传输错误降级,got {r:?}"
        );
    }

    #[test]
    fn strongest_only_escalates_never_downgrades() {
        assert_eq!(
            strongest(FailureKind::Quota, FailureKind::Transient),
            FailureKind::Quota
        );
        assert_eq!(
            strongest(FailureKind::Transient, FailureKind::Quota),
            FailureKind::Quota
        );
        assert_eq!(
            strongest(FailureKind::Quota, FailureKind::AuthInvalid),
            FailureKind::AuthInvalid
        );
        assert_eq!(
            strongest(FailureKind::AuthAmbiguous, FailureKind::Transient),
            FailureKind::AuthAmbiguous
        );
        // 同强度保留先到的那个(回退过程中不做无谓翻转)。
        assert_eq!(
            strongest(FailureKind::Quota, FailureKind::Quota),
            FailureKind::Quota
        );
    }

    #[tokio::test]
    async fn try_endpoints_403_returns_auth_immediately() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let eps = vec![
            Endpoint {
                url: format!("{}/a", server.uri()),
                origin: "AI_EDITOR",
                target: None,
            },
            // 第二个端点若被调用会因为没挂载 mock 而网络错误;403 应立即返回、不应尝试它
            Endpoint {
                url: "http://127.0.0.1:1/unreachable".into(),
                origin: "AI_EDITOR",
                target: None,
            },
        ];
        let r = try_endpoints(&client, &eps, &cred(), &imp(), b"body").await;
        // 数据面拿不到响应体,裸 403 保守归类为 AuthAmbiguous(冷却,不永久禁用)。
        assert!(matches!(&r, Err(f) if f.kind == FailureKind::AuthAmbiguous));
    }

    #[tokio::test]
    async fn call_carries_raw_type_field_so_classifier_sees_machine_stable_name() {
        // #6 回归:__type 里是 InvalidGrantException 但 message 只是含糊的 "forbidden"。
        // 若只传 extract_upstream_message 的单字段摘要(会挑 message),__type 丢失 → 分类退化成
        // AuthAmbiguous。传**完整原始体**后 classify_with_body 能看到 __type → AuthInvalid。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"__type":"InvalidGrantException","message":"forbidden"}"#),
            )
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let ep = Endpoint {
            url: format!("{}/x", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        };
        match call(&client, &ep, &cred(), &imp(), b"body").await {
            Err(LoginError::UpstreamHttp { status, body }) => {
                assert_eq!(status, 403);
                assert!(
                    body.contains("InvalidGrantException"),
                    "raw __type must survive, got: {body}"
                );
                assert_eq!(
                    crate::kiro::pool::classify_with_body(status, &body),
                    FailureKind::AuthInvalid
                );
            }
            other => panic!("expected UpstreamHttp with raw body, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn expired_token_exception_via_call_is_ambiguous_not_permanent_disable() {
        // #1+#6 端到端:AWS ExpiredTokenException 真实体经 call → 完整体 → classify_with_body。
        // "expired" 触发 body_signals_recoverable 短路 → AuthAmbiguous(冷却,可刷新自愈),
        // 绝不永久禁用一个 refreshable 令牌。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403).set_body_string(
                r#"{"__type":"ExpiredTokenException","message":"The security token included in the request is expired."}"#,
            ))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let ep = Endpoint {
            url: format!("{}/x", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        };
        match call(&client, &ep, &cred(), &imp(), b"body").await {
            Err(LoginError::UpstreamHttp { status, body }) => {
                assert_eq!(
                    crate::kiro::pool::classify_with_body(status, &body),
                    FailureKind::AuthAmbiguous
                );
            }
            other => panic!("expected UpstreamHttp, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn error_body_read_is_capped() {
        // #3 回归:超大错误体只读到 ERROR_BODY_CAP(8 KiB)上限,不无界缓冲。
        let huge = "A".repeat(64 * 1024); // 64 KiB
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string(huge))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let ep = Endpoint {
            url: format!("{}/x", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        };
        match call(&client, &ep, &cred(), &imp(), b"body").await {
            Err(LoginError::UpstreamHttp { body, .. }) => {
                assert!(
                    body.len() <= 8 * 1024,
                    "body must be capped, got {} bytes",
                    body.len()
                );
            }
            other => panic!("expected UpstreamHttp, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn try_endpoints_403_expired_token_is_ambiguous_not_invalid() {
        // #1+#6 端点回退路径:ExpiredTokenException(__type)→ 完整体分类 → AuthAmbiguous(冷却)。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403).set_body_string(
                r#"{"__type":"ExpiredTokenException","message":"The security token included in the request is expired."}"#,
            ))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let eps = vec![Endpoint {
            url: format!("{}/x", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        }];
        let r = try_endpoints(&client, &eps, &cred(), &imp(), b"body").await;
        assert!(
            matches!(&r, Err(f) if f.kind == FailureKind::AuthAmbiguous),
            "expired → ambiguous, got {r:?}"
        );
    }

    #[tokio::test]
    async fn try_endpoints_403_invalid_grant_type_is_auth_invalid() {
        // #6 端点回退路径:__type=InvalidGrantException 但 message 含糊 → 完整体分类命中 → AuthInvalid。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"__type":"InvalidGrantException","message":"forbidden"}"#),
            )
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let eps = vec![Endpoint {
            url: format!("{}/x", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        }];
        let r = try_endpoints(&client, &eps, &cred(), &imp(), b"body").await;
        assert!(
            matches!(&r, Err(f) if f.kind == FailureKind::AuthInvalid),
            "invalid_grant __type → invalid, got {r:?}"
        );
    }

    #[tokio::test]
    async fn call_with_fallback_reports_failure_on_terminal_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let mut pool = Pool::new(vec![cred()], LbMode::Priority);
        let eps = vec![Endpoint {
            url: format!("{}/x", server.uri()),
            origin: "AI_EDITOR",
            target: None,
        }];
        let r = try_endpoints(&client, &eps, &cred(), &imp(), b"body").await;
        // 裸 403 → AuthAmbiguous:上报一次只记 1 个 strike(未达 AUTH_AMBIGUOUS_STRIKES=2),
        // 不进冷却、不永久禁用,账号仍然可用(与旧的"Auth 一次即永久禁用"契约相区分)。
        assert!(matches!(&r, Err(f) if f.kind == FailureKind::AuthAmbiguous));
        if let Err(f) = r {
            pool.report_failure("a", f.kind, 0);
        }
        assert_eq!(pool.active_count(0), 1); // AuthAmbiguous 单次不禁用,仍可用
    }

    /// 每请求一条新连接:数据面头必须带 `Connection: close`。
    ///
    /// 复用连接会让同一条 TCP/TLS 上依次出现几十个不同账号的令牌,而每个账号还各自声称
    /// 是不同的机器(user-agent 里的 machineId 各不相同)。真实客户端不可能这样,
    /// 而「同一条连接上轮换多个身份」是账号共享最直接的证据 —— 同 IP 还能用 NAT 解释,
    /// 同一条连接解释不了。线上症状吻合:账号没经过中转时是活的,用过就被 security
    /// precaution 封停;真实客户端每请求都带这个头。
    #[test]
    fn every_request_asks_the_connection_to_close() {
        let ep = Endpoint {
            url: "http://x/".into(),
            origin: "AI_EDITOR",
            target: None,
        };
        let h = build_headers(&cred(), &imp(), &ep, "inv");
        assert_eq!(
            h.get("connection").unwrap(),
            "close",
            "缺这个头连接就会被复用,整池账号暴露在同一条连接上"
        );
    }

    /// user-agent 里的 SDK 版本与被观测的真实客户端对齐。
    /// 版本号本身就是指纹的一部分,长期停在旧版是可识别的差异。
    #[test]
    fn sdk_version_matches_the_observed_client() {
        let ep = Endpoint {
            url: "http://x/".into(),
            origin: "AI_EDITOR",
            target: None,
        };
        let h = build_headers(&cred(), &imp(), &ep, "inv");
        let ua = h.get("user-agent").unwrap().to_str().unwrap();
        assert!(ua.contains("aws-sdk-js/1.0.34"), "UA 版本未对齐: {ua}");
        assert!(
            ua.contains("codewhispererstreaming#1.0.34"),
            "API 版本未对齐: {ua}"
        );
        assert!(!ua.contains("1.0.27"), "仍残留旧版本号: {ua}");
    }
}
