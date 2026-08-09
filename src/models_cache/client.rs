//! 上游 `ListAvailableModels` 客户端。
//!
//! 请求形态(照观测的 wire 事实):
//!   `POST https://codewhisperer.{region}.amazonaws.com/`
//!   头:`Content-Type: application/x-amz-json-1.0`、
//!       `X-Amz-Target: AmazonCodeWhispererService.ListAvailableModels`、
//!       `Authorization: Bearer <access_token>`、`amz-sdk-invocation-id`、
//!       `amz-sdk-request: attempt=1; max=1`、`x-amz-user-agent`、`user-agent`。
//!   体:`{"origin":"AI_EDITOR"[,"profileArn":"<arn>"]}`(amz-json)。
//! 伪装身份(machine_id/kiro_version 等)复用与数据面一致的口径。
//! URL 组装、header 构造、请求体拼装、响应解析、错误分类均为本项目自写。

use crate::kiro::credential::Credential;
use crate::kiro::provider::Impersonation;
use crate::models_cache::model::AvailableModelsResponse;

/// 上游错误体里回带的说明文字截断上限(字符数),避免超长响应刷爆日志/响应。
const UPSTREAM_MSG_MAX: usize = 300;

/// 拉取模型清单时的失败原因。
#[derive(Debug)]
pub enum ModelsError {
    /// 网络/传输错误。
    Http,
    /// 上游非 2xx:携带状态码 + 从响应体解析出的说明文字(供管理员看清真因,
    /// 如 AWS "Your User ID ... temporarily is suspended")。
    Upstream { status: u16, message: String },
    /// 响应体解析失败。
    Decode,
}

impl std::fmt::Display for ModelsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelsError::Http => write!(f, "models http error"),
            ModelsError::Upstream { status, message } => {
                write!(f, "models upstream HTTP {status}: {message}")
            }
            ModelsError::Decode => write!(f, "models decode error"),
        }
    }
}
impl std::error::Error for ModelsError {}

impl ModelsError {
    /// 是否为上游鉴权失败(401/403)——供"强制刷新令牌 + 重试一次"判定。
    /// 只看状态码,与错误体文字无关。
    pub fn is_auth(&self) -> bool {
        matches!(
            self,
            ModelsError::Upstream {
                status: 401 | 403,
                ..
            }
        )
    }
}

/// 从上游 amz-json 错误体里抽 `message` 字段;拿不到则回落到原始文本(去空白)。
/// 结果按 [`UPSTREAM_MSG_MAX`] 字符截断,绝不含任何令牌(错误体本身不含令牌)。
fn extract_amz_message(body: &str) -> String {
    let raw = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            // amz-json 错误体常见形态:{"__type":"...","message":"..."} 或 {"Message":"..."}。
            v.get("message")
                .or_else(|| v.get("Message"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string());
    truncate_chars(&raw, UPSTREAM_MSG_MAX)
}

/// 按字符(非字节)截断,避免切断多字节 UTF-8;超长时以 `…` 收尾。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// `X-Amz-Target`(照观测):`{service}.{operation}`。
const AMZ_TARGET: &str = "AmazonCodeWhispererService.ListAvailableModels";

/// 生产用 base:`https://codewhisperer.{region}.amazonaws.com`(控制面/元数据同 host)。
fn region_base(region: &str) -> String {
    format!("https://codewhisperer.{region}.amazonaws.com")
}

/// 由凭据 + 配置构造伪装身份(machine_id 优先显式,否则由 refresh_token 派生)。
fn impersonation_for(cred: &Credential, cfg: &crate::config::Config) -> Impersonation {
    // 收口到唯一入口(理由同 balance::client):此前这份不认配置级 machineId。
    Impersonation::for_credential(cred, cfg)
}

/// 组装请求体:`{"origin":"AI_EDITOR"[,"profileArn":"<arn>"]}`。
fn build_body(profile_arn: Option<&str>) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "origin".to_string(),
        serde_json::Value::String("AI_EDITOR".to_string()),
    );
    if let Some(arn) = profile_arn
        && !arn.is_empty()
    {
        obj.insert(
            "profileArn".to_string(),
            serde_json::Value::String(arn.to_string()),
        );
    }
    serde_json::to_vec(&serde_json::Value::Object(obj)).unwrap_or_else(|_| b"{}".to_vec())
}

/// 向 `base` 发一次 ListAvailableModels 并解析。测试可注入 mock base;生产用 [`fetch_available_models`]。
pub async fn fetch_at(
    client: &reqwest::Client,
    base: &str,
    cred: &Credential,
    imp: &Impersonation,
) -> Result<AvailableModelsResponse, ModelsError> {
    let url = format!("{}/", base.trim_end_matches('/'));
    let body = build_body(cred.profile_arn.as_deref());
    let inv = new_invocation_id();
    let user_agent = format!(
        "aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}",
        imp.system_version, imp.node_version, imp.kiro_version, imp.machine_id
    );
    let amz_user_agent = format!(
        "aws-sdk-js/1.0.0 KiroIDE-{}-{}",
        imp.kiro_version, imp.machine_id
    );
    let req = client
        .post(&url)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", AMZ_TARGET)
        // bearer 走 `cred.bearer()` + 配套 tokentype:ksk 凭据的令牌在 `kiro_api_key` 里,
        // 读 `access_token` 会发出空 Bearer,模型清单必然 401/403(理由同 balance::client)。
        .header("authorization", format!("Bearer {}", cred.bearer()))
        .header("amz-sdk-invocation-id", inv)
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("x-amz-user-agent", amz_user_agent)
        .header(reqwest::header::USER_AGENT, user_agent)
        .header("connection", "close");
    let req = if cred.is_api_key() {
        req.header("tokentype", "API_KEY")
    } else {
        req
    };
    let resp = req.body(body).send().await.map_err(|_| ModelsError::Http)?;
    let status = resp.status();
    if !status.is_success() {
        // 读错误体(有界)并解析 amz-json 的 message,带进错误里供管理员看清真因。
        let body = resp.text().await.unwrap_or_default();
        let message = extract_amz_message(&body);
        return Err(ModelsError::Upstream {
            status: status.as_u16(),
            message,
        });
    }
    resp.json::<AvailableModelsResponse>()
        .await
        .map_err(|_| ModelsError::Decode)
}

/// 生产入口:按凭据 region 组 base + 伪装头,拉该账号可用模型清单。
pub async fn fetch_available_models(
    client: &reqwest::Client,
    cfg: &crate::config::Config,
    cred: &Credential,
) -> Result<AvailableModelsResponse, ModelsError> {
    let base = region_base(&cred.region);
    let imp = impersonation_for(cred, cfg);
    fetch_at(client, &base, cred, &imp).await
}

/// 集中保鲜版:调 ListAvailableModels 前先经 [`crate::kiro::ensure_fresh`] 确保 access_token
/// 新鲜(即将过期则刷新并写回活池,令牌与 relay/balance 共享),再用刷新后的凭据实拉。
///
/// 令牌过期是 models 独立 403 的根因:此前直接用池内(可能已过期)token 调上游。
/// 保鲜失败(池内已无该 id / 刷新失败)映射为 [`ModelsError::Http`]。不记录任何令牌明文。
pub async fn fetch_available_models_fresh(
    client: &reqwest::Client,
    cfg: &crate::config::Config,
    pool: &std::sync::Arc<tokio::sync::Mutex<crate::kiro::pool::Pool>>,
    cred_id: &str,
    now_unix: u64,
    ctx: Option<&crate::kiro::ensure_fresh::RefreshCtx>,
) -> Result<AvailableModelsResponse, ModelsError> {
    let cred = crate::kiro::ensure_fresh::ensure_fresh(pool, cred_id, client, now_unix, 300, ctx)
        .await
        .map_err(|_| ModelsError::Http)?;
    match fetch_available_models(client, cfg, &cred).await {
        // 401/403:该 access_token 可能已被服务端失效(哪怕尚未过期)。强制刷新令牌后重试一次。
        Err(e) if e.is_auth() => {
            // 传本次失败请求实际用的 access_token 作双检基线:池内若已换过新的,
            // 说明别人刚刷完,直接复用而不再轮换一次(轮换会作废他人在用的令牌)。
            let fresh = match crate::kiro::ensure_fresh::force_refresh(
                pool,
                cred_id,
                client,
                now_unix,
                &cred.access_token,
                ctx,
            )
            .await
            {
                Ok(c) => c,
                // 强制刷新失败(refresh_token 也失效/网络):回传原 401/403,不再重试。
                Err(_) => return Err(e),
            };
            fetch_available_models(client, cfg, &fresh).await
        }
        other => other,
    }
}

/// 随机请求关联 id(16 字节 CSPRNG 十六进制),供 amz-sdk-invocation-id 使用。
fn new_invocation_id() -> String {
    let mut raw = [0u8; 16];
    getrandom::getrandom(&mut raw).expect("CSPRNG");
    hex::encode(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cred() -> Credential {
        Credential {
            id: "7".into(),
            access_token: "AT".into(),
            refresh_token: "rt".into(),
            kiro_api_key: None,
            expires_at_unix: u64::MAX,
            region: "us-east-1".into(),
            auth: AuthMethod::Social,
            client_id: None,
            client_secret: None,
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:111:profile/ABC".into()),
            machine_id: Some("mid".into()),
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
    fn body_carries_origin_and_profile_arn() {
        let b = build_body(Some("arn:aws:x/y"));
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["origin"], "AI_EDITOR");
        assert_eq!(v["profileArn"], "arn:aws:x/y");
    }

    #[test]
    fn body_omits_profile_arn_when_absent() {
        let b = build_body(None);
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["origin"], "AI_EDITOR");
        assert!(v.get("profileArn").is_none());
    }

    #[tokio::test]
    async fn fetch_at_parses_response_and_sends_target_and_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(header("x-amz-target", AMZ_TARGET))
            .and(header("content-type", "application/x-amz-json-1.0"))
            .and(header("authorization", "Bearer AT"))
            .and(body_string_contains("AI_EDITOR"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "defaultModel": { "modelId": "auto" },
                "models": [
                    { "modelId": "claude-sonnet-5", "modelName": "Claude Sonnet 5",
                      "rateMultiplier": 1.3,
                      "tokenLimits": { "maxOutputTokens": 64000 } }
                ]
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = fetch_at(&client, &server.uri(), &cred(), &imp())
            .await
            .unwrap();
        let infos = r.to_model_infos();
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].id, "auto");
        assert_eq!(infos[1].id, "claude-sonnet-5");
    }

    #[tokio::test]
    async fn fetch_at_maps_non_success_to_upstream() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = fetch_at(&client, &server.uri(), &cred(), &imp()).await;
        assert!(matches!(r, Err(ModelsError::Upstream { status: 403, .. })));
    }

    #[tokio::test]
    async fn fetch_at_carries_amz_message_in_error_display() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "__type": "AccessDeniedException",
                "message": "Your User ID abc temporarily is suspended"
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = fetch_at(&client, &server.uri(), &cred(), &imp()).await;
        let e = r.unwrap_err();
        assert!(e.is_auth());
        let d = e.to_string();
        assert!(d.contains("403"), "display should carry status: {d}");
        assert!(
            d.contains("temporarily is suspended"),
            "display should carry message: {d}"
        );
    }

    #[test]
    fn extract_amz_message_prefers_message_field_and_falls_back_to_raw() {
        assert_eq!(
            extract_amz_message(r#"{"__type":"X","message":"boom"}"#),
            "boom"
        );
        assert_eq!(extract_amz_message("plain text error"), "plain text error");
    }

    #[test]
    fn extract_amz_message_truncates_long_body() {
        let long = "a".repeat(1000);
        let out = extract_amz_message(&long);
        assert_eq!(out.chars().count(), UPSTREAM_MSG_MAX + 1); // 300 chars + '…'
        assert!(out.ends_with('…'));
    }

    #[tokio::test]
    async fn fetch_at_maps_bad_json_to_decode() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = fetch_at(&client, &server.uri(), &cred(), &imp()).await;
        assert!(matches!(r, Err(ModelsError::Decode)));
    }
}
