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

/// 模型清单的**备用**主机。
///
/// `codewhisperer.{region}` 只在部分 region 存在 —— 实测 `codewhisperer.eu-central-1`
/// 连 DNS 都解析不了,于是非 us-east-1 的账号"刷新模型"必然失败,而且失败形态是**传输层
/// 错误**,面板上只显示一句 `models http error`,完全看不出是主机不存在。
///
/// `q.{region}` 在这些 region 是存在的,故作备用:至少能拿到一个**真实的 HTTP 应答**,
/// 把上游的原话(例如"该订阅不支持此应用")带给运营者,而不是一个语焉不详的传输失败。
fn region_base_fallback(region: &str) -> String {
    format!("https://q.{region}.amazonaws.com")
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
    // **链上的次序即线上顺序**,故下面每一行的位置都不是风格问题:头次序是与 TLS 指纹
    // 并列的识别维度,同一个客户端库发出的次序是固定的。这条链路曾经自成一派 ——
    // `authorization` 排在 UA 系列**之前**、`tokentype` 落在 `connection` **之后**,
    // 而同一个账号在数据面、余额、MCP 上都是另一种排法,自己跟自己对不上。现按
    // `provider::build_headers` 逐项对齐(那里是本项目头构造的唯一范本)。
    let mut req = client
        .post(&url)
        .header("content-type", "application/x-amz-json-1.0")
        // 控制面同样每请求一条新连接,理由同数据面:一条连接上轮换多个账号的令牌,
        // 而每个账号还各自声称是不同的机器,这在 wire 上解释不了。
        .header("connection", "close")
        .header("x-amz-user-agent", amz_user_agent)
        .header(reqwest::header::USER_AGENT, user_agent);
    // 显式设 host。不设的话底层 HTTP 库会在序列化时把它补到头列**末尾** —— 取值一样,
    // 但落位不同;控制面客户端锁死 HTTP/1.1,Host 是以明文头真实上线的,位置可观测。
    // 此前这里是全项目唯一不设 host 的 AWS 出站点。
    if let Some(host) = crate::kiro::provider::host_of(&url) {
        req = req.header("host", host);
    }
    // bearer 走 `cred.bearer()` + 配套 tokentype:ksk 凭据的令牌在 `kiro_api_key` 里,
    // 读 `access_token` 会发出空 Bearer,模型清单必然 401/403(理由同 balance::client)。
    req = req
        .header("amz-sdk-invocation-id", inv)
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("authorization", format!("Bearer {}", cred.bearer()));
    if cred.is_api_key() {
        req = req.header("tokentype", "API_KEY");
    }
    // amz-json 的 target 头压在末尾,与数据面一致。
    req = req.header("x-amz-target", AMZ_TARGET);
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
    let imp = impersonation_for(cred, cfg);
    // region 只算一次并贯穿"主机 / 备用主机 / 日志"三处。此前日志打的是裸 `cred.region`,
    // 而两个 base 都由 `effective_region` 算(优先取 profileArn 里编码的 region)——
    // ARN 写 eu-central-1、导入时 region 填了 us-east-1 的账号,请求实打 `q.eu-central-1`
    // 日志却说 us-east-1,排障的人照着日志去查另一个 region 只会白跑。
    let region = crate::kiro::endpoint::effective_region(cred);
    match fetch_at(client, &region_base(&region), cred, &imp).await {
        // 传输层失败(最常见的是主机在该 region 根本不存在)→ 换备用主机再试一次。
        // 只对传输失败回落:拿到了 HTTP 应答就说明主机是对的,那种失败换主机也没用。
        Err(ModelsError::Http) => {
            let fb = region_base_fallback(&region);
            tracing::debug!(
                region = %region,
                fallback = %fb,
                "模型清单主机不可达,改用备用主机"
            );
            fetch_at(client, &fb, cred, &imp).await
        }
        other => other,
    }
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
    // 出口按该账号自己的代理取:它的数据面走哪个出口,余额/模型清单就得走哪个。
    // 只有配了代理才覆盖调用方传入的客户端,没配时行为与此前完全一致。
    let per_cred = {
        let g = pool.lock().await;
        g.snapshot_credentials()
            .into_iter()
            .find(|c| c.id == cred_id)
    }
    .filter(|c| {
        c.effective_proxy(crate::kiro::provider::config_proxy_url().as_deref())
            .is_some()
    })
    .map(|c| crate::http::unary_for(&c));
    let client = per_cred.as_ref().unwrap_or(client);
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
    // 收口到唯一实现。此前这里各自写了一份 `hex(16 字节)` —— 32 位无连字符十六进制,
    // 而 aws-sdk-js 这个头**永远**是 UUID v4。数据面那份在 v0.10.0 已改对,这两条控制面
    // 链路却各留一份旧的,于是同一个账号在数据面发 UUID、在余额/模型清单发裸 hex,
    // 自己跟自己对不上——比统一发错还刺眼。
    crate::kiro::provider::new_invocation_id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cred() -> Credential {
        Credential {
            quota_reset_unix: None,
            priority: 999,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
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

    /// 采集一次请求里**线上真实的头名次序**,只保留 `want` 里点名的头 —— 过滤掉
    /// reqwest 自动补的 `content-length`/`accept-encoding` 等不由本模块决定的项。
    fn ordered(req: &wiremock::Request, want: &[&str]) -> Vec<String> {
        req.headers
            .iter()
            .map(|(k, _)| k.as_str().to_ascii_lowercase())
            .filter(|k| want.contains(&k.as_str()))
            .collect()
    }

    fn want_vec(want: &[&str]) -> Vec<String> {
        want.iter().map(|s| s.to_string()).collect()
    }

    /// 模型清单的头必须与数据面同一范本:次序一致,且 `host` **显式**设置。
    ///
    /// 回归:此前这条链路是全项目唯一不设 host 的 AWS 出站点 —— 不显式设的话底层 HTTP 库
    /// 会在序列化时把 Host 补到头列**末尾**,而这两个客户端都锁死 HTTP/1.1、Host 是以明文
    /// 头上线的,它在头序列里的落位一样可观测。次序本身也是独一份:`authorization` 排在
    /// UA 系列**之前**、`tokentype` 落在 `connection` **之后**,而同一个账号在数据面、余额、
    /// MCP 上都是另一种排法。同一个客户端库发出的头次序是固定的,自己跟自己对不上,
    /// 比统一排错更刺眼。
    #[tokio::test]
    async fn models_headers_follow_the_data_plane_order_with_explicit_host() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "models": [] })),
            )
            .mount(&server)
            .await;
        fetch_at(&reqwest::Client::new(), &server.uri(), &cred(), &imp())
            .await
            .unwrap();
        let reqs = server.received_requests().await.unwrap();
        let want = [
            "content-type",
            "connection",
            "x-amz-user-agent",
            "user-agent",
            "host",
            "amz-sdk-invocation-id",
            "amz-sdk-request",
            "authorization",
            "x-amz-target",
        ];
        assert_eq!(
            ordered(&reqs[0], &want),
            want_vec(&want),
            "模型清单头序须与 provider::build_headers 逐项对齐"
        );
    }

    /// ksk 凭据的 `tokentype` 落位同样照数据面:紧跟 `authorization`,在 `x-amz-target` 之前。
    #[tokio::test]
    async fn models_tokentype_sits_right_after_authorization_for_api_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "models": [] })),
            )
            .mount(&server)
            .await;
        let mut c = cred();
        c.auth = AuthMethod::ApiKey;
        c.kiro_api_key = Some("ksk_TESTKEY".into());
        c.access_token = String::new();
        fetch_at(&reqwest::Client::new(), &server.uri(), &c, &imp())
            .await
            .unwrap();
        let reqs = server.received_requests().await.unwrap();
        let want = [
            "content-type",
            "connection",
            "x-amz-user-agent",
            "user-agent",
            "host",
            "amz-sdk-invocation-id",
            "amz-sdk-request",
            "authorization",
            "tokentype",
            "x-amz-target",
        ];
        assert_eq!(
            ordered(&reqs[0], &want),
            want_vec(&want),
            "ksk 的 tokentype 须紧跟 authorization"
        );
    }

    /// 采日志用的写入端:把 fmt 层的输出攒在内存里供断言。
    #[derive(Clone, Default)]
    struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
        type Writer = LogBuf;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// 回落备用主机时,日志里的 region 必须是**这次请求实际用的** region。
    ///
    /// 回归:此前日志打的是裸 `cred.region`,而两个 base 都由
    /// [`crate::kiro::endpoint::effective_region`] 算(优先取 profileArn 里编码的 region)。
    /// 于是 ARN 写 eu-central-1、导入时 region 填了 us-east-1 的账号,请求实打
    /// `q.eu-central-1`、日志却说 us-east-1 —— 排障的人照着日志去查 us-east-1 只会白跑。
    #[test]
    fn fallback_log_reports_the_region_actually_used() {
        let mut c = cred();
        c.region = "us-east-1".into();
        c.profile_arn = Some("arn:aws:codewhisperer:eu-central-1:111:profile/ABC".into());
        // 出口指向一个必然拒连的端口:两次请求都在传输层失败,既不做真实 DNS 解析也不出网,
        // 但回落前那条日志照样会打 —— 这里要看的正是它。
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
            .build()
            .unwrap();
        let buf = LogBuf::default();
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(buf.clone())
            .finish();
        tracing::subscriber::with_default(sub, || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _ = rt.block_on(fetch_available_models(
                &client,
                &crate::config::Config::default(),
                &c,
            ));
        });
        let logs = String::from_utf8_lossy(&buf.0.lock().unwrap()).to_string();
        assert!(logs.contains("备用主机"), "应打出回落日志: {logs}");
        assert!(
            logs.contains("region=eu-central-1"),
            "日志须打实际用的 region: {logs}"
        );
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

    /// ksk(API Key)凭据刷新模型清单时必须带 **ksk 本身**作 bearer,并配套 `tokentype: API_KEY`。
    ///
    /// 回归:此前这里直接读 `access_token`,而 ksk 的令牌在 `kiroApiKey` 里 —— 发出去的是
    /// 空 Bearer,于是 ksk 账号"刷新模型"必然报错。用户报过这个现象。
    #[tokio::test]
    async fn api_key_credentials_refresh_models_with_the_ksk_itself() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("authorization", "Bearer ksk_TESTKEY"))
            .and(header("tokentype", "API_KEY"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [{ "modelId": "claude-sonnet-4.5", "modelName": "Claude Sonnet 4.5" }]
            })))
            .mount(&server)
            .await;
        let mut c = cred();
        c.auth = AuthMethod::ApiKey;
        c.kiro_api_key = Some("ksk_TESTKEY".into());
        c.access_token = String::new();
        let r = fetch_at(&reqwest::Client::new(), &server.uri(), &c, &imp())
            .await
            .expect("ksk 必须能刷新模型清单");
        assert!(!r.models.is_empty());
    }

    /// 非 ksk 凭据不得带 `tokentype`。
    #[tokio::test]
    async fn oauth_credentials_do_not_send_tokentype_on_models() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": []
            })))
            .mount(&server)
            .await;
        let _ = fetch_at(&reqwest::Client::new(), &server.uri(), &cred(), &imp()).await;
        let reqs = server.received_requests().await.unwrap();
        assert!(!reqs[0].headers.contains_key("tokentype"));
    }

    /// 主主机不可达时回落备用主机。
    ///
    /// 回归:`codewhisperer.{region}` 只在部分 region 存在(实测 `codewhisperer.eu-central-1`
    /// 连 DNS 都解析不了),于是非 us-east-1 的账号刷新模型必然失败,面板只显示一句
    /// `models http error` —— 完全看不出是主机不存在。用户报过 ksk 刷新模型报错。
    #[tokio::test]
    async fn unreachable_primary_host_falls_back() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [{ "modelId": "m1", "modelName": "M1" }]
            })))
            .mount(&server)
            .await;
        // 主主机指向一个必然连不上的端口 → 传输失败 → 回落到 mock
        let r = match fetch_at(
            &reqwest::Client::new(),
            "http://127.0.0.1:1",
            &cred(),
            &imp(),
        )
        .await
        {
            Err(ModelsError::Http) => {
                fetch_at(&reqwest::Client::new(), &server.uri(), &cred(), &imp()).await
            }
            other => other,
        };
        assert!(r.is_ok(), "回落后应成功");
        assert_eq!(r.unwrap().models.len(), 1);
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
