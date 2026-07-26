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
