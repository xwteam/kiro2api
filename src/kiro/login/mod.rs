//! 登录流共享原语。
//! PKCE 按 RFC 7636(S256),state/verifier 用 CSPRNG(getrandom),
//! AWS SSO-OIDC 端点基址按 oidc.{region}.amazonaws.com。
pub mod builderid;
pub mod iam_sso;
pub mod sso_token;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Instant;

/// CodeWhisperer 授权范围(AWS/Kiro 强制)。
pub const SCOPES: [&str; 5] = [
    "codewhisperer:completions",
    "codewhisperer:analysis",
    "codewhisperer:conversations",
    "codewhisperer:transformations",
    "codewhisperer:taskassist",
];

/// 登录成功产出的凭据。
///
/// `client_id`/`client_secret` = 该 OIDC 流程 `/client/register` 得到的公开客户端凭据。
/// 铸凭据时必须带出:这些账号 auth=Idc,首刷要走 AWS SSO-OIDC `/token`(refresh_token 授权)
/// 端点、须重放 clientId+clientSecret;丢掉它们首刷会打错端点而失败。admin 层据此设 auth=Idc。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in_secs: u64,
    pub region: String,
    pub client_id: String,
    pub client_secret: String,
}

/// 登录错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginError {
    /// 传输层错误(连接被拒/超时/DNS 失败等),无 HTTP 应答。
    Http,
    /// 上游语义错误码(回调 error 参数、设备流 error 字段、数据面短标签等)。
    Upstream(String),
    /// 上游返回了非 2xx HTTP 应答:携带状态码 + 应答体摘要(已提取 AWS 错误信息、截断)。
    UpstreamHttp {
        status: u16,
        body: String,
    },
    /// 瞬态故障:上游限流(429)/5xx,或非 2xx 却给不出可解析的错误体(代理塞回 HTML
    /// 错误页、应答被截断等)。此时**判不出终态**,调用方应退避重试而非销毁登录会话。
    /// `detail` 带失败环节与上游摘要;状态码在 `status` 里,渲染时二者都要带出。
    Transient {
        status: u16,
        detail: String,
    },
    Pending,
    SlowDown,
    Expired,
    Denied,
    BadCallback,
}

impl LoginError {
    /// 是否属于"再试一次仍可能成功"的非终态。设备码轮询/批量导入据此决定继续退避重试,
    /// 还是判定终态并销毁登录会话——把瞬态故障当终态会让用户已在浏览器点过的授权白费。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            LoginError::Http
                | LoginError::Pending
                | LoginError::SlowDown
                | LoginError::Transient { .. }
        )
    }
}

/// 应答体摘要上限(字符),防日志/对外串过长。
const BODY_SNIPPET_MAX: usize = 200;

/// 上游未回 `expiresIn` 时的保守回落有效期(秒)。不能取 0——0 会让凭据一落盘就被判为
/// 已过期,每个请求都触发一次刷新,形成刷新风暴;短有效期只是让首次使用前多刷一次。
pub(crate) const FALLBACK_EXPIRES_IN_SECS: u64 = 300;

/// `expiresIn` 缺失时 serde 默认得到 0,回落到保守短有效期。
pub(crate) fn effective_expires_in(raw: u64) -> u64 {
    if raw == 0 {
        FALLBACK_EXPIRES_IN_SECS
    } else {
        raw
    }
}

/// 从上游应答体提取简明错误信息:优先 JSON 的 `message`/`error`/`__type`/`error_description`
/// 字段,否则取原文前若干字符;并做单行化 + 截断。
pub fn extract_upstream_message(body: &str) -> String {
    let picked = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            for key in ["message", "error_description", "error", "__type", "Message"] {
                if let Some(s) = v.get(key).and_then(|x| x.as_str())
                    && !s.is_empty()
                {
                    return Some(s.to_string());
                }
            }
            None
        })
        .unwrap_or_else(|| body.to_string());
    let flat = picked.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(BODY_SNIPPET_MAX).collect()
}

/// 读取一个 reqwest 应答:非 2xx → 读体并归为 `UpstreamHttp`;2xx → 反序列化,失败归为
/// `UpstreamHttp`(带状态码 + 解析错误摘要),而不是笼统 `Http`,便于对外定位真因。
pub async fn read_upstream_json<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, LoginError> {
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(LoginError::UpstreamHttp {
            status,
            body: extract_upstream_message(&body),
        });
    }
    resp.json::<T>()
        .await
        .map_err(|e| LoginError::UpstreamHttp {
            status,
            body: extract_upstream_message(&e.to_string()),
        })
}

/// AWS SSO-OIDC 端点基址。
pub fn oidc_base(region: &str) -> String {
    format!("https://oidc.{region}.amazonaws.com")
}

/// base64url-nopad(SHA256(verifier))。
pub fn s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// 生成一对 PKCE (verifier, challenge)。verifier = 32 CSPRNG 字节的 base64url-nopad。
pub fn pkce_pair() -> (String, String) {
    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw).expect("CSPRNG");
    let verifier = URL_SAFE_NO_PAD.encode(raw);
    let challenge = s256_challenge(&verifier);
    (verifier, challenge)
}

/// CSPRNG state(防 CSRF),32 字节 base64url-nopad。
pub fn new_state() -> String {
    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw).expect("CSPRNG");
    URL_SAFE_NO_PAD.encode(raw)
}

/// 带 TTL 的一次性会话缓存(登录中转态)。
pub struct SessionStore<T> {
    ttl: std::time::Duration,
    map: Mutex<HashMap<String, (Instant, T)>>,
}

impl<T> SessionStore<T> {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            ttl: std::time::Duration::from_secs(ttl_secs),
            map: Mutex::new(HashMap::new()),
        }
    }
    pub fn put(&self, key: String, val: T) {
        self.map.lock().insert(key, (Instant::now(), val));
    }
    /// 取出并消费;过期返回 None。
    pub fn take(&self, key: &str) -> Option<T> {
        let (t, v) = self.map.lock().remove(key)?;
        if t.elapsed() <= self.ttl {
            Some(v)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_matches_rfc7636_appendix_b() {
        // RFC 7636 Appendix B 已知答案向量(公开):
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(s256_challenge(verifier), challenge);
    }

    #[test]
    fn pkce_pair_roundtrips() {
        let (v, c) = pkce_pair();
        assert_eq!(v.len(), 43); // 32 字节 base64url-nopad
        assert_eq!(s256_challenge(&v), c);
        let (v2, _) = pkce_pair();
        assert_ne!(v, v2); // 随机
    }

    #[test]
    fn state_is_unique_and_sized() {
        let a = new_state();
        let b = new_state();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
    }

    #[test]
    fn oidc_base_regionalized() {
        assert_eq!(
            oidc_base("us-east-1"),
            "https://oidc.us-east-1.amazonaws.com"
        );
        assert_eq!(
            oidc_base("eu-west-1"),
            "https://oidc.eu-west-1.amazonaws.com"
        );
    }

    #[test]
    fn extract_upstream_message_prefers_json_fields_and_truncates() {
        assert_eq!(
            extract_upstream_message(r#"{"message":"issuerUrl is invalid"}"#),
            "issuerUrl is invalid"
        );
        assert_eq!(
            extract_upstream_message(r#"{"__type":"ValidationException"}"#),
            "ValidationException"
        );
        // 非 JSON → 原文单行化。
        assert_eq!(extract_upstream_message("boom\n  next"), "boom next");
        // 超长截断到上限。
        let long = "x".repeat(500);
        assert_eq!(
            extract_upstream_message(&long).chars().count(),
            BODY_SNIPPET_MAX
        );
    }

    #[test]
    fn retryable_covers_transient_states_only() {
        assert!(LoginError::Pending.is_retryable());
        assert!(LoginError::SlowDown.is_retryable());
        assert!(LoginError::Http.is_retryable());
        assert!(
            LoginError::Transient {
                status: 502,
                detail: "bad gateway".into()
            }
            .is_retryable()
        );
        // 终态:重试也不会变好,须销毁会话/丢弃该行。
        assert!(!LoginError::Expired.is_retryable());
        assert!(!LoginError::Denied.is_retryable());
        assert!(!LoginError::BadCallback.is_retryable());
        assert!(!LoginError::Upstream("invalid_grant".into()).is_retryable());
        assert!(
            !LoginError::UpstreamHttp {
                status: 404,
                body: "not found".into()
            }
            .is_retryable()
        );
    }

    #[test]
    fn session_store_ttl() {
        let s: SessionStore<u32> = SessionStore::new(3600);
        s.put("k".into(), 7);
        assert_eq!(s.take("k"), Some(7));
        assert_eq!(s.take("k"), None); // take 后即消费
        let expired: SessionStore<u32> = SessionStore::new(0);
        expired.put("k".into(), 9);
        assert_eq!(expired.take("k"), None); // ttl=0 立即过期
    }
}

/// 给一个 AWS SSO-OIDC 请求补齐伪装头(照真实客户端形态)。
///
/// 登录流(`/client/register`、`/device_authorization`、`/token`)与令牌刷新打的是**同一台
/// 主机** `oidc.{region}.amazonaws.com`,理应长得一样。此前只有刷新补齐了,登录流
/// **一个头都不带、连 User-Agent 都没有** —— 而登录恰恰是账号刚被创建、上游最会看指纹的
/// 时刻。更糟的是前后不一致:注册时裸奔,几分钟后同一个 clientId 又用完美的 aws-sdk-js 头
/// 去刷新,这种矛盾比单纯的裸请求更容易被关联。
///
/// SDK 版本是 sso-oidc 自己的 3.980.0(不是数据面那个 1.0.34),尾部**不带** machineId。
pub fn apply_sso_oidc_headers(
    h: &mut reqwest::header::HeaderMap,
    imp: &crate::kiro::provider::Impersonation,
) {
    use reqwest::header::{HeaderName, HeaderValue};
    let hv = |s: &str| HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static(""));
    h.insert("content-type", HeaderValue::from_static("application/json"));
    if let Ok(name) = HeaderName::from_bytes(b"x-amz-user-agent") {
        h.insert(name, HeaderValue::from_static("aws-sdk-js/3.980.0 KiroIDE"));
    }
    h.insert(
        reqwest::header::USER_AGENT,
        hv(&format!(
            "aws-sdk-js/3.980.0 ua/2.1 os/{} lang/js md/nodejs#{} api/sso-oidc#3.980.0 m/E KiroIDE",
            imp.system_version, imp.node_version
        )),
    );
}

/// portal 批准链用的浏览器形态请求头。
///
/// 这条链(`/token/whoAmI`、`/session/device`、`/device_authorization/accept_user_code`、
/// `/device_authorization/associate_token`)打的不是 `oidc.{region}.amazonaws.com`,而是
/// **portal 主机**;而 portal 只见得到浏览器 —— 那四个端点本就是 SSO 起始页里的 JS 发出的
/// 同源 XHR。此前这四步一个 header 都不设:reqwest 不配 `user_agent` 就连 `User-Agent`
/// 都不发,于是四个打向 AWS portal 的请求全部零 UA;而且只有第三步带 `Referer`,浏览器
/// 对同一个页面发出的连续同源 XHR 不会时有时无地带它。这是 sso-oidc 那条路修好之后**剩下
/// 的第三条路径**。
///
/// UA 里的操作系统跟着配置的 `system_version` 走 —— 同一次登录里 sso-oidc 的 UA 说
/// `os/win32#10.0.22631`、portal 的 UA 却说 Macintosh,这种自相矛盾比裸奔更显眼。
pub fn apply_portal_headers(
    h: &mut reqwest::header::HeaderMap,
    imp: &crate::kiro::provider::Impersonation,
    portal: &str,
) {
    use reqwest::header::{HeaderName, HeaderValue};
    let hv = |s: &str| HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static(""));
    h.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("application/json, text/plain, */*"),
    );
    h.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9"),
    );
    h.insert(
        reqwest::header::USER_AGENT,
        hv(&browser_user_agent(&imp.system_version)),
    );
    if let Some(origin) = crate::kiro::provider::origin_of(portal) {
        h.insert(reqwest::header::ORIGIN, hv(&origin));
    }
    h.insert(reqwest::header::REFERER, hv(portal));
    if let Ok(name) = HeaderName::from_bytes(b"sec-fetch-site") {
        h.insert(name, HeaderValue::from_static("same-origin"));
    }
    if let Ok(name) = HeaderName::from_bytes(b"sec-fetch-mode") {
        h.insert(name, HeaderValue::from_static("cors"));
    }
    if let Ok(name) = HeaderName::from_bytes(b"sec-fetch-dest") {
        h.insert(name, HeaderValue::from_static("empty"));
    }
}

/// 由伪装用的 `system_version`(如 `win32#10.0.22631` / `darwin#23.5.0`)推出配套的浏览器
/// UA。两者必须指向同一个操作系统 —— 见 [`apply_portal_headers`]。
pub fn browser_user_agent(system_version: &str) -> String {
    let platform = match system_version.split('#').next().unwrap_or("") {
        "darwin" => "Macintosh; Intel Mac OS X 10_15_7",
        "linux" => "X11; Linux x86_64",
        // win32 及未知一律按 Windows(与默认 system_version 一致)。
        _ => "Windows NT 10.0; Win64; x64",
    };
    format!(
        "Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    )
}

/// 登录流用的伪装身份。登录时还没有凭据,故 machineId 不参与(sso-oidc 的 UA 本就不带它)。
///
/// 版本三元组取**进程级**配置,与令牌刷新、数据面同源。这里曾经写的是
/// `Config::default()`:用户在配置里改了 kiroVersion / systemVersion / nodeVersion 之后,
/// 登录流仍报编译期默认值,而同一台 `oidc.{region}.amazonaws.com` 上的令牌刷新报的是配置
/// 里的真值 —— 同一个 clientId,注册时说自己是一台 Windows 机器,几分钟后刷新时说自己是
/// 另一台机器、另一个 node 版本。这种前后矛盾比两边都用默认值更容易被关联出来,而且它只
/// 在"配置被改过"的部署上发作,默认部署跑测试是看不出来的。
pub fn login_impersonation() -> crate::kiro::provider::Impersonation {
    // 进程级配置只经 `for_credential_global` 一个口子对外(见 `placeholder_cred`)。
    let mut imp = crate::kiro::provider::Impersonation::for_credential_global(&placeholder_cred());
    imp.machine_id = String::new();
    imp
}

/// 取进程级伪装配置用的占位凭据。
///
/// 进程级配置(kiro/系统/node 版本)只经
/// [`crate::kiro::provider::Impersonation::for_credential_global`] 一个口子对外,而它以凭据
/// 为参数 —— 登录时还没有凭据,故传这一枚全空的占位件。由它算出的 machineId 随即被丢弃
/// (sso-oidc 的 UA 本就不带 machineId)。
///
/// 绕这一下,是为了不在本文件里再写一份"版本号从哪来"的实现:版本值只能有一个来源,
/// 否则改了配置之后总有一条链路还在报旧值。
fn placeholder_cred() -> crate::kiro::credential::Credential {
    crate::kiro::credential::Credential {
        id: String::new(),
        access_token: String::new(),
        refresh_token: String::new(),
        expires_at_unix: 0,
        region: String::new(),
        auth: crate::kiro::credential::AuthMethod::Social,
        client_id: None,
        client_secret: None,
        profile_arn: None,
        machine_id: None,
        email: None,
        nickname: None,
        weight: 0,
        priority: crate::kiro::credential::DEFAULT_PRIORITY,
        label: None,
        disabled: false,
        kiro_api_key: None,
        status_reason: None,
        proxy_url: None,
        proxy_username: None,
        proxy_password: None,
        quota_reset_unix: None,
    }
}

/// 测试夹具:把一份**区别于编译期默认值**的伪装配置灌进进程级槽位,再按生产的唯一入口
/// 读回**实际生效**的那份。
///
/// 全 crate 只此一处灌、取值固定:那个槽位只认第一次写入,多处灌不同值会让用例随执行
/// 顺序飘。返回的是读回来的值而不是想灌的值 —— 万一这一处不是第一个写进去的,用例仍按
/// 实际生效的去断言,不会假绿。
#[cfg(test)]
pub(crate) fn install_test_process_config() -> crate::kiro::provider::Impersonation {
    let cfg = crate::config::Config {
        kiro_version: "9.9.99".into(),
        system_version: "darwin#23.5.0".into(),
        node_version: "99.9.9".into(),
        ..crate::config::Config::default()
    };
    crate::kiro::provider::init_impersonation_defaults(&cfg);
    crate::kiro::provider::Impersonation::for_credential_global(&placeholder_cred())
}

#[cfg(test)]
mod sso_header_tests {
    use super::*;

    /// portal 的浏览器 UA 必须与 sso-oidc 的 `os/` 指向**同一个操作系统**。
    ///
    /// 同一次登录里 sso-oidc 说 `os/win32#10.0.22631`、portal 却说 Macintosh —— 这种
    /// 自相矛盾比裸奔更显眼,因为它证明两边是拼出来的而不是同一台机器发的。
    #[test]
    fn portal_browser_ua_agrees_with_the_configured_os() {
        assert!(browser_user_agent("win32#10.0.22631").contains("Windows NT 10.0"));
        assert!(browser_user_agent("darwin#23.5.0").contains("Macintosh"));
        assert!(browser_user_agent("linux#6.1.0").contains("X11; Linux"));
        // 未知/空一律回落 Windows(与默认 system_version 同侧),绝不产出没有平台段的 UA。
        assert!(browser_user_agent("").contains("Windows NT 10.0"));
        assert!(browser_user_agent("plan9#1").contains("Windows NT 10.0"));

        // 两条链自洽:portal 的浏览器 UA 必须跟着**当前生效**的 system_version 走。
        // 这里不钉死 win32 —— system_version 是可配置项,钉死等于只测了编译期默认值那一种
        // 情形,而配置被改过才是真正会出岔子的情形。
        let imp = login_impersonation();
        let mut h = reqwest::header::HeaderMap::new();
        apply_portal_headers(&mut h, &imp, "https://x.awsapps.com/start");
        let portal_ua = h[reqwest::header::USER_AGENT].to_str().unwrap();
        assert_eq!(portal_ua, browser_user_agent(&imp.system_version));

        // Origin 只到主机,不带路径 —— Origin 的定义就是这一段。
        assert_eq!(
            h[reqwest::header::ORIGIN].to_str().unwrap(),
            "https://x.awsapps.com"
        );
        assert_eq!(
            h[reqwest::header::REFERER].to_str().unwrap(),
            "https://x.awsapps.com/start"
        );
    }

    /// SSO-OIDC 请求必须带齐伪装头,**登录流与刷新用同一份**。
    ///
    /// 回归:登录流(`/client/register`、`/device_authorization`、`/token`)打的是与刷新
    /// **同一台主机** `oidc.{region}.amazonaws.com`,却一个头都不带、连 User-Agent 都没有。
    /// 而登录恰恰是账号刚被创建、上游最会看指纹的时刻;更糟的是前后不一致 —— 注册时裸奔,
    /// 几分钟后同一个 clientId 又用完美的 aws-sdk-js 头去刷新。
    #[test]
    fn sso_oidc_headers_are_complete_and_shared() {
        let mut h = reqwest::header::HeaderMap::new();
        apply_sso_oidc_headers(&mut h, &login_impersonation());

        let ua = h[reqwest::header::USER_AGENT].to_str().unwrap();
        assert!(
            ua.starts_with("aws-sdk-js/3.980.0"),
            "SDK 版本应是 sso-oidc 自己的: {ua}"
        );
        assert!(ua.contains("api/sso-oidc#3.980.0"), "{ua}");
        assert!(ua.ends_with("KiroIDE"), "尾部不带 machineId: {ua}");
        assert_eq!(
            h["x-amz-user-agent"].to_str().unwrap(),
            "aws-sdk-js/3.980.0 KiroIDE"
        );
        assert_eq!(h["content-type"].to_str().unwrap(), "application/json");
    }

    /// 登录流的伪装身份必须取**进程级**配置,与刷新/数据面同源。
    ///
    /// 回归:这里曾经写死 `Config::default()`。于是用户在配置里改了
    /// kiroVersion/systemVersion/nodeVersion 之后,登录流仍报编译期默认值,而同一台
    /// `oidc.{region}.amazonaws.com` 上的令牌刷新报的是配置里的真值 —— 同一个 clientId
    /// 注册时说自己是一台 Windows、几分钟后刷新时说自己是另一台机器另一个 node 版本。
    /// 这种前后矛盾比两边都用默认值更容易被关联出来。
    #[test]
    fn login_identity_is_read_from_the_process_level_config() {
        let effective = install_test_process_config();
        assert_ne!(
            effective.system_version,
            crate::config::Config::default().system_version,
            "夹具没能把进程级配置灌进去,本用例失去区分力"
        );

        let imp = login_impersonation();
        assert_eq!(imp.kiro_version, effective.kiro_version);
        assert_eq!(imp.system_version, effective.system_version);
        assert_eq!(imp.node_version, effective.node_version);
        // 登录时还没有凭据,machineId 不参与(sso-oidc 的 UA 本就不带它)。
        assert!(imp.machine_id.is_empty(), "登录流不该带 machineId");
    }
}
