//! Kiro token 刷新。Social 走 Kiro auth 端点;IdC 走 AWS SSO-OIDC refresh_token grant。
//! 端点为照观测的 wire 事实;请求/响应处理为本项目自写。
use crate::kiro::credential::{AuthMethod, Credential};
use crate::kiro::login::LoginError;
use serde::Deserialize;

/// 刷新/OIDC 端点使用的 region:与数据面保持一致地优先取 profileArn 里的 region,
/// 缺省(无 arn 或 arn 不含合法 region 段)才回落 `cred.region`。
///
/// #12:此前刷新端点直接用 `cred.region`,而数据面 region 来自 profileArn,二者可能
/// 不一致 → IdC 账号可能把 OIDC 刷新打到与数据面不同的 region。这里复用
/// [`crate::kiro::endpoint::region_from_profile_arn`] 统一来源。
fn refresh_region(cred: &Credential) -> String {
    cred.profile_arn
        .as_deref()
        .and_then(crate::kiro::endpoint::region_from_profile_arn)
        .unwrap_or_else(|| cred.region.clone())
}

/// 刷新端点(照观测)。region 与数据面一致地优先取 profileArn(#12)。
pub fn endpoint(cred: &Credential) -> String {
    let region = refresh_region(cred);
    match cred.auth {
        AuthMethod::Social => format!("https://prod.{region}.auth.desktop.kiro.dev/refreshToken"),
        AuthMethod::Idc => format!("https://oidc.{region}.amazonaws.com/token"),
        // API Key 没有刷新端点。调用方(ensure_fresh)在更上层就把这类凭据短路掉了,
        // 走到这里说明短路漏了;返回空串会让请求打到一个空 URL 上、错得莫名其妙,
        // 故给一个一眼能认出出处的哨兵值。
        AuthMethod::ApiKey => String::from("about:blank#api-key-never-refreshes"),
    }
}

#[derive(Deserialize)]
struct RefreshResp {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// AWS SSO-OIDC 的 refresh 响应带 idToken(JWT):IdC 数据面(Amazon Q
    /// generateAssistantResponse)要的是这个 JWT,而非 portal 会话型 accessToken。
    #[serde(rename = "idToken", default)]
    id_token: Option<String>,
    #[serde(rename = "refreshToken", default)]
    refresh_token: Option<String>,
    #[serde(rename = "expiresIn", default)]
    expires_in: u64,
    /// 若刷新响应回带 profileArn 则采纳(通常缺省)。
    #[serde(rename = "profileArn", default)]
    profile_arn: Option<String>,
}

/// 令牌刷新请求的伪装头。
///
/// 此前这条链路**一个头都不带** —— 连 User-Agent 都没有。而 Social 的刷新端点
/// `prod.{region}.auth.desktop.kiro.dev` 是 Kiro **自家**的服务,日志完全在他们手里,
/// 一个没有 UA 的请求几乎等于自报家门;更要命的是刷新是**每个账号都必然走**的路径,
/// 数据面伪装得再像,这里先把身份交代了。
///
/// 两条链路在真实客户端里由**不同的库**发出,故形态完全不同,不能合并:
/// - Social:桌面端用 axios 打自家 auth 服务。`Accept: application/json, text/plain, */*`
///   与 `Accept-Encoding: gzip, compress, deflate, br` 都是 axios 的默认值,UA 是
///   `KiroIDE-{版本}-{machineId}` 这种自定义短串,**不是** aws-sdk 那套。
/// - IdC:走 AWS SSO-OIDC,是标准 aws-sdk-js 请求,故带 `x-amz-user-agent` /
///   `amz-sdk-invocation-id` / `amz-sdk-request`,且 SDK 版本是 sso-oidc 自己的 3.980.0
///   (不是数据面那个 1.0.34),尾部也**不带** machineId。
///
/// 注意 `Accept-Encoding`:声明了就必须真解得开,故 reqwest 必须开着 gzip/brotli/deflate
/// 特性(见 Cargo.toml)。声明了却解不开,每次刷新都会拿到一堆压缩字节。
fn refresh_headers(
    cred: &Credential,
    imp: &crate::kiro::provider::Impersonation,
    base: &str,
) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let hv = |s: &str| HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static(""));
    // 插入顺序即线上顺序,照真实客户端排。
    let mut h = HeaderMap::new();
    match cred.auth {
        AuthMethod::Social => {
            h.insert(
                "accept",
                HeaderValue::from_static("application/json, text/plain, */*"),
            );
            h.insert("content-type", HeaderValue::from_static("application/json"));
            h.insert(
                reqwest::header::USER_AGENT,
                hv(&format!("KiroIDE-{}-{}", imp.kiro_version, imp.machine_id)),
            );
            h.insert(
                "accept-encoding",
                HeaderValue::from_static("gzip, compress, deflate, br"),
            );
        }
        _ => {
            h.insert("content-type", HeaderValue::from_static("application/json"));
            if let Ok(name) = reqwest::header::HeaderName::from_bytes(b"x-amz-user-agent") {
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
    }
    if let Some(host) = crate::kiro::provider::host_of(base) {
        h.insert("host", hv(&host));
    }
    if cred.auth != AuthMethod::Social {
        h.insert(
            "amz-sdk-invocation-id",
            hv(&crate::kiro::provider::new_invocation_id()),
        );
        // 刷新链路的重试上限与数据面不同(真实客户端如此):数据面 max=3,刷新 max=4。
        h.insert(
            "amz-sdk-request",
            HeaderValue::from_static("attempt=1; max=4"),
        );
    }
    h.insert("connection", HeaderValue::from_static("close"));
    h
}

/// 向 `base` 发刷新请求,返回更新后的 Credential。
pub async fn refresh_at(
    client: &reqwest::Client,
    base: &str,
    cred: &Credential,
    imp: &crate::kiro::provider::Impersonation,
    now_unix: u64,
) -> Result<Credential, LoginError> {
    let body = match cred.auth {
        AuthMethod::Social => serde_json::json!({ "refreshToken": cred.refresh_token }),
        AuthMethod::Idc => serde_json::json!({
            "clientId": cred.client_id, "clientSecret": cred.client_secret,
            "refreshToken": cred.refresh_token, "grantType": "refresh_token",
        }),
        // ksk 本身就是数据面 bearer,没有可换的东西。走到这里是上层短路失效,
        // 明确报错而不是拿着空 body 去打上游——后者会得到一个与真实病因无关的 4xx。
        AuthMethod::ApiKey => return Err(LoginError::Http),
    };
    // Social 走 axios 形态、要声明 Accept-Encoding,故必须用开着解压的那个客户端;
    // 其余(IdC)沿用调用方传入的控制面客户端(不带该头,与真实 aws-sdk-js 一致)。
    // 两者都按该账号自己的代理取,出口与它的数据面一致 —— 数据面走代理而刷新走主 IP,
    // 比不配代理更糟。
    let axios;
    let client = match cred.auth {
        AuthMethod::Social => {
            axios = crate::http::axios_for(cred);
            &axios
        }
        _ => client,
    };
    let resp = client
        .post(base)
        .headers(refresh_headers(cred, imp, base))
        .json(&body)
        .send()
        .await
        .map_err(|_| LoginError::Http)?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        // 非 2xx 必须把**响应体**一并带出:"该 refresh_token 已作废"的确证(invalid_grant /
        // InvalidGrantException)只活在体里。丢掉体,账号池就只剩一个状态码可看,无从把
        // "凭据已吊销(应禁用)"与"上游 5xx 抖动(应冷却)"区分开。
        // 体按数据面既有约定有界读取(≤ERROR_BODY_CAP),避免超大/恶意错误体被无界缓冲。
        let body = crate::kiro::provider::read_body_capped(resp).await;
        return Err(LoginError::UpstreamHttp { status, body });
    }
    let r: RefreshResp = resp.json().await.map_err(|_| LoginError::Http)?;
    let mut out = cred.clone();
    // IdC 数据面认 idToken(JWT);缺省才回落 accessToken。Social 一律用 accessToken。
    // 用 portal 型 accessToken 打 IdC 数据面会 403,进而账号被判死,故必须区分。
    out.access_token = match cred.auth {
        AuthMethod::Idc => r.id_token.unwrap_or(r.access_token),
        AuthMethod::Social => r.access_token,
        // 不可达:ApiKey 在函数开头就已 return。留此臂只为穷尽,不做静默回落。
        AuthMethod::ApiKey => return Err(LoginError::Http),
    };
    if let Some(rt) = r.refresh_token {
        out.refresh_token = rt;
    }
    // 刷新响应若回带 profileArn 且与当前不同,则采纳(通常缺省,保留旧值)。
    if let Some(pa) = r.profile_arn
        && out.profile_arn.as_deref() != Some(pa.as_str())
    {
        out.profile_arn = Some(pa);
    }
    out.expires_at_unix = now_unix.saturating_add(r.expires_in);
    Ok(out)
}

/// 便捷刷新:用 `endpoint(cred)` 作 base。
pub async fn refresh(
    client: &reqwest::Client,
    cred: &Credential,
    now_unix: u64,
) -> Result<Credential, LoginError> {
    let base = endpoint(cred);
    let imp = crate::kiro::provider::Impersonation::for_credential_global(cred);
    refresh_at(client, &base, cred, &imp, now_unix).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, Credential};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Social 刷新必须带齐 axios 那套头。
    ///
    /// 回归:这条链路曾经**一个头都不带**——连 User-Agent 都没有。而它打的
    /// `auth.desktop.kiro.dev` 是 Kiro 自家服务,且每个账号都必然走这条路。
    #[tokio::test]
    async fn social_refresh_sends_the_axios_headers() {
        let server = MockServer::start().await;
        let c = cred(AuthMethod::Social);
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let out = refresh_at(
            &reqwest::Client::new(),
            &format!("{}/refreshToken", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await
        .expect("请求应成功");
        assert_eq!(out.access_token, "new");

        // 逐条核实际发出的头。
        //
        // 不用 wiremock 的 `header()` 匹配器:它把逗号分隔的取值拆成多个值再比,而
        // axios 那两个头(`Accept`、`Accept-Encoding`)本来就是**一行里带逗号**的单值,
        // 匹配器永远对不上 —— 那是匹配器的语义问题,不是我们发错了。
        let reqs = server.received_requests().await.unwrap();
        let h = &reqs[0].headers;
        let get = |k: &str| h[k].to_str().unwrap().to_string();
        assert_eq!(get("accept"), "application/json, text/plain, */*");
        assert_eq!(get("accept-encoding"), "gzip, compress, deflate, br");
        assert_eq!(get("content-type"), "application/json");
        assert_eq!(get("connection"), "close");
        assert!(h.contains_key("host"), "必须显式带 host");

        // UA 从实际请求里核。**不**拿 imp_for 现算的值去比:那份读的是进程级 OnceLock,
        // 而并行跑的 server 测试可能在两次读之间把它灌上,比对就会随执行顺序飘。
        // 这里只钉住形状 —— 必须是 `KiroIDE-<版本>-<64 位十六进制 machineId>`,
        // 且**绝不能**是 aws-sdk 那套(刷新链路由 axios 发出,不是 SDK)。
        let reqs = server.received_requests().await.unwrap();
        let ua = reqs[0].headers["user-agent"].to_str().unwrap();
        assert!(ua.starts_with("KiroIDE-"), "UA 形态不对: {ua}");
        assert!(
            !ua.contains("aws-sdk-js"),
            "Social 刷新不该用 aws-sdk 形态: {ua}"
        );
        let mid = ua.rsplit('-').next().unwrap();
        assert_eq!(mid.len(), 64, "UA 尾部应是 64 位 machineId: {ua}");
        assert!(mid.chars().all(|c| c.is_ascii_hexdigit()), "{ua}");
    }

    /// 声明了 `Accept-Encoding` 就必须真解得开。
    ///
    /// 这条不是形式主义:我们照真实客户端声明了 `gzip, compress, deflate, br`,上游据此
    /// 压缩响应是完全合法的。若 reqwest 没开解压特性,拿到的就是一堆压缩字节,
    /// **每一次刷新都会失败**——而刷新失败会被记成账号故障。
    #[tokio::test]
    async fn social_refresh_decodes_a_gzipped_response() {
        // {"accessToken":"AT2","expiresIn":3600} 的 gzip 字节(固定,便于确定性重放)
        const GZ: &[u8] = &[
            31, 139, 8, 0, 0, 0, 0, 0, 2, 3, 171, 86, 74, 76, 78, 78, 45, 46, 14, 201, 207, 78,
            205, 83, 178, 82, 114, 12, 49, 82, 210, 81, 74, 173, 40, 200, 44, 74, 45, 246, 4, 138,
            24, 155, 25, 24, 212, 2, 0, 63, 51, 10, 91, 38, 0, 0, 0,
        ];
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(GZ)
                    .insert_header("content-encoding", "gzip")
                    .insert_header("content-type", "application/json"),
            )
            .mount(&server)
            .await;
        let c = cred(AuthMethod::Social);
        let out = refresh_at(
            &reqwest::Client::new(),
            &format!("{}/refreshToken", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await
        .expect("压缩响应必须能解开");
        assert_eq!(out.access_token, "AT2");
        assert_eq!(out.expires_at_unix, 1000 + 3600);
    }

    /// 测试夹具:按被测凭据取伪装身份(生产同一入口)。
    fn imp_for(c: &Credential) -> crate::kiro::provider::Impersonation {
        crate::kiro::provider::Impersonation::for_credential_global(c)
    }

    fn cred(auth: AuthMethod) -> Credential {
        Credential {
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            id: "a".into(),
            access_token: "old".into(),
            refresh_token: "rt".into(),
            kiro_api_key: None,
            expires_at_unix: 0,
            region: "us-east-1".into(),
            auth,
            client_id: Some("cid".into()),
            client_secret: Some("csec".into()),
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

    #[tokio::test]
    async fn social_refresh_updates_token_and_expiry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new", "refreshToken": "rt2", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Social);
        let out = refresh_at(
            &client,
            &format!("{}/refreshToken", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(out.access_token, "new");
        assert_eq!(out.refresh_token, "rt2");
        assert_eq!(out.expires_at_unix, 1000 + 3600);
    }

    #[tokio::test]
    async fn idc_refresh_keeps_old_refresh_when_absent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new", "expiresIn": 1800
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Idc);
        let out = refresh_at(
            &client,
            &format!("{}/token", server.uri()),
            &c,
            &imp_for(&c),
            500,
        )
        .await
        .unwrap();
        assert_eq!(out.access_token, "new");
        assert_eq!(out.refresh_token, "rt"); // 响应未带则保留旧的
        assert_eq!(out.expires_at_unix, 500 + 1800);
    }

    #[tokio::test]
    async fn idc_refresh_prefers_id_token_over_portal_access_token() {
        // AWS SSO-OIDC refresh 回 {accessToken: portal 会话型, idToken: 数据面 JWT}。
        // IdC 数据面(Amazon Q)要 idToken;用 portal accessToken 会 403 → 账号被判死。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "portal-session-token",
                "idToken": "jwt-data-plane",
                "refreshToken": "rt2", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Idc);
        let out = refresh_at(
            &client,
            &format!("{}/token", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(out.access_token, "jwt-data-plane"); // 采 idToken,不是 portal token
        assert_eq!(out.refresh_token, "rt2");
    }

    #[tokio::test]
    async fn idc_refresh_falls_back_to_access_token_when_no_id_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "only-access", "expiresIn": 1800
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Idc);
        let out = refresh_at(
            &client,
            &format!("{}/token", server.uri()),
            &c,
            &imp_for(&c),
            500,
        )
        .await
        .unwrap();
        assert_eq!(out.access_token, "only-access"); // 无 idToken → 回落 accessToken
    }

    #[tokio::test]
    async fn social_refresh_ignores_id_token_and_uses_access_token() {
        // Social 数据面用 accessToken;即便响应误带 idToken 也不采纳。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "social-at", "idToken": "should-be-ignored",
                "refreshToken": "rt2", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Social);
        let out = refresh_at(
            &client,
            &format!("{}/refreshToken", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(out.access_token, "social-at");
    }

    #[tokio::test]
    async fn refresh_adopts_profile_arn_when_returned() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new",
                "profileArn": "arn:aws:codewhisperer:us-east-1:222222222222:profile/NEW",
                "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Social); // profile_arn 初始 None
        let out = refresh_at(
            &client,
            &format!("{}/refreshToken", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(
            out.profile_arn.as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:222222222222:profile/NEW")
        );
    }

    #[tokio::test]
    async fn refresh_keeps_profile_arn_when_absent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new", "expiresIn": 3600
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let mut c = cred(AuthMethod::Social);
        c.profile_arn = Some("arn:existing".into());
        let out = refresh_at(
            &client,
            &format!("{}/refreshToken", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(out.profile_arn.as_deref(), Some("arn:existing")); // 响应未带则保留旧值
    }

    /// 非 2xx 必须带出状态码 + 响应体:invalid_grant 这类"refresh_token 已作废"的确证只在体里,
    /// 丢掉它账号池就无从把永久失效与瞬时故障区分开(会把已吊销账号一直当健康账号选)。
    #[tokio::test]
    async fn refresh_non_2xx_carries_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_string(r#"{"error":"invalid_grant","error_description":"revoked"}"#),
            )
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Social);
        let r = refresh_at(
            &client,
            &format!("{}/refreshToken", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await;
        match r {
            Err(LoginError::UpstreamHttp { status, body }) => {
                assert_eq!(status, 400);
                assert!(body.contains("invalid_grant"), "原始体必须保留: {body}");
                // 调用方契约:status + body 一并可用 → 刷新端点分类得 AuthInvalid(永久禁用)。
                assert_eq!(
                    crate::kiro::pool::classify_refresh_failure(status, &body),
                    crate::kiro::pool::FailureKind::AuthInvalid
                );
            }
            other => panic!("expected UpstreamHttp with body, got {other:?}"),
        }
    }

    /// 5xx 无确证 → 分类落瞬时类(记 strike / 冷却),不禁用账号。
    #[tokio::test]
    async fn refresh_5xx_classifies_as_transient() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Social);
        match refresh_at(
            &client,
            &format!("{}/refreshToken", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await
        {
            Err(LoginError::UpstreamHttp { status, body }) => {
                assert_eq!(status, 503);
                assert_eq!(
                    crate::kiro::pool::classify_refresh_failure(status, &body),
                    crate::kiro::pool::FailureKind::Transient
                );
            }
            other => panic!("expected UpstreamHttp, got {other:?}"),
        }
    }

    /// 错误体读取有上限(与数据面同一约定),超大错误体不会被无界缓冲。
    #[tokio::test]
    async fn refresh_error_body_is_capped() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/refreshToken"))
            .respond_with(ResponseTemplate::new(500).set_body_string("A".repeat(64 * 1024)))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let c = cred(AuthMethod::Social);
        match refresh_at(
            &client,
            &format!("{}/refreshToken", server.uri()),
            &c,
            &imp_for(&c),
            1000,
        )
        .await
        {
            Err(LoginError::UpstreamHttp { body, .. }) => {
                assert!(
                    body.len() <= 8 * 1024,
                    "体必须被截断,实际 {} 字节",
                    body.len()
                );
            }
            other => panic!("expected UpstreamHttp, got {other:?}"),
        }
    }

    #[test]
    fn endpoint_by_auth() {
        assert_eq!(
            endpoint(&cred(AuthMethod::Social)),
            "https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken"
        );
        assert_eq!(
            endpoint(&cred(AuthMethod::Idc)),
            "https://oidc.us-east-1.amazonaws.com/token"
        );
    }

    /// #12:有 profileArn 时刷新端点 region 取自 arn(与数据面一致),而非 cred.region。
    #[test]
    fn endpoint_region_prefers_profile_arn() {
        let mut c = cred(AuthMethod::Idc);
        c.region = "us-east-1".into(); // cred.region 与 arn 不同
        c.profile_arn = Some("arn:aws:codewhisperer:ap-northeast-1:123:profile/X".into());
        assert_eq!(
            endpoint(&c),
            "https://oidc.ap-northeast-1.amazonaws.com/token"
        );

        let mut s = cred(AuthMethod::Social);
        s.region = "us-east-1".into();
        s.profile_arn = Some("arn:aws:codewhisperer:eu-west-1:123:profile/X".into());
        assert_eq!(
            endpoint(&s),
            "https://prod.eu-west-1.auth.desktop.kiro.dev/refreshToken"
        );
    }

    /// #12:profileArn 缺省或不含合法 region 段时回落 cred.region。
    #[test]
    fn endpoint_region_falls_back_to_cred_region() {
        let mut c = cred(AuthMethod::Idc);
        c.region = "eu-central-1".into();
        c.profile_arn = None;
        assert_eq!(
            endpoint(&c),
            "https://oidc.eu-central-1.amazonaws.com/token"
        );

        // arn 的第 4 段不像 region → 回落 cred.region
        c.profile_arn = Some("arn:aws:codewhisperer::123:profile/X".into());
        assert_eq!(
            endpoint(&c),
            "https://oidc.eu-central-1.amazonaws.com/token"
        );
    }
}
