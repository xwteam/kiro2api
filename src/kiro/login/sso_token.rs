//! sso_token 登录:用一个已有 SSO bearer,程序化走完设备批准链换取 CodeWhisperer 凭据。
//! 注册/设备授权/换 token 复用 AWS SSO-OIDC 公开流程(见 `builderid`);中间的 portal 批准
//! 四步端点系照观测 portal.sso 流量自行设计的最小实现(非公开 API)。
use super::{LoginError, MintedCredential, builderid};

/// 批量导入上限(自设)。
const MAX_BULK: usize = 200;

/// 批准后换 token 的轮询上限(自设):次数与总等待秒数双限,任一到顶即以最后一次瞬态
/// 错误返回。批量导入是串行的,单行的等待预算必须够小,否则一行卡住会拖垮整批。
const REDEEM_MAX_POLLS: u32 = 6;
const REDEEM_MAX_WAIT_SECS: u64 = 15;
/// 上游回 `slow_down` 时每次追加的退避秒数(RFC 8628 §3.5 要求加大轮询间隔)。
const SLOW_DOWN_STEP_SECS: u64 = 5;

/// 用一个 bearer 兑换一份凭据:注册+设备授权 → bearer 自动批准 → 换 token。
pub async fn redeem(
    client: &reqwest::Client,
    oidc: &str,
    portal: &str,
    region: &str,
    bearer: &str,
) -> Result<MintedCredential, LoginError> {
    // 1) 注册 + 发起设备授权(公开 SSO-OIDC 流程,复用 builderid::start)。
    let pending = builderid::start(client, oidc).await?;

    // 2) 携带 bearer 程序化批准(自设四步,见 approve_with_bearer)。
    approve_with_bearer(client, portal, bearer, &pending.user_code).await?;

    // 3) 批准后换 token(设备码授权,公开流程,复用 builderid::poll)。
    poll_until_minted(client, oidc, region, &pending).await
}

/// 批准落地到设备码可换 token 之间有传播延迟:`authorization_pending`/`slow_down` 与瞬态
/// 故障都要退避重试,只有终态(过期/拒绝/明确上游错误)才立刻返回。只轮询一次会让批量
/// 导入随机少掉几个账号(重导一次又能成功)。
async fn poll_until_minted(
    client: &reqwest::Client,
    oidc: &str,
    region: &str,
    pending: &builderid::Pending,
) -> Result<MintedCredential, LoginError> {
    let mut backoff = pending.interval_secs.min(REDEEM_MAX_WAIT_SECS);
    let mut waited = 0u64;
    // 一次都没轮到终态时对外报的错;首轮必被覆盖。
    let mut last = LoginError::Pending;
    for attempt in 0..REDEEM_MAX_POLLS {
        match builderid::poll(client, oidc, region, pending).await {
            Ok(cred) => return Ok(cred),
            Err(e) if e.is_retryable() => {
                if matches!(e, LoginError::SlowDown) {
                    backoff = backoff.saturating_add(SLOW_DOWN_STEP_SECS);
                }
                last = e;
            }
            Err(e) => return Err(e),
        }
        let remaining = REDEEM_MAX_WAIT_SECS.saturating_sub(waited);
        if attempt + 1 == REDEEM_MAX_POLLS || remaining == 0 {
            break;
        }
        let nap = backoff.min(remaining);
        tokio::time::sleep(std::time::Duration::from_secs(nap)).await;
        waited += nap;
    }
    Err(last)
}

/// 携带 bearer 走完 portal 的四步设备批准(照观测自行设计):
/// 确认身份 → 建设备会话 → 接受 userCode → 关联 token。
async fn approve_with_bearer(
    client: &reqwest::Client,
    portal: &str,
    bearer: &str,
    user_code: &str,
) -> Result<(), LoginError> {
    // 四步共用**同一份**浏览器形态的头(含 Referer/Origin/UA)。portal 只见得到浏览器,
    // 而且同一个页面里的连续同源 XHR 不会时有时无地带 Referer —— 此前四步里只有第三步带,
    // 其余三步连 User-Agent 都没有。
    let mut headers = reqwest::header::HeaderMap::new();
    super::apply_portal_headers(&mut headers, &super::login_impersonation(), portal);
    let auth = |req: reqwest::RequestBuilder| req.headers(headers.clone()).bearer_auth(bearer);

    // 确认身份。
    approve_step("whoAmI", auth(client.get(format!("{portal}/token/whoAmI")))).await?;

    // 建设备会话。
    approve_step(
        "session/device",
        auth(client.post(format!("{portal}/session/device"))),
    )
    .await?;

    // 接受 userCode。
    approve_step(
        "accept_user_code",
        auth(client.post(format!("{portal}/device_authorization/accept_user_code")))
            .json(&serde_json::json!({ "userCode": user_code })),
    )
    .await?;

    // 关联 token。
    approve_step(
        "associate_token",
        auth(client.post(format!("{portal}/device_authorization/associate_token"))),
    )
    .await?;

    Ok(())
}

/// 执行批准链的一步,按状态码分类失败:
/// - 401/403:bearer 无效或无权,才是真正的 `Denied`(该行应丢弃,重试无意义);
/// - 429 与 5xx:上游限流/故障,`Transient`(可重试);
/// - 其余(404 端点变更、400 参数不符等):`UpstreamHttp` 明确上游错误。
///
/// 失败信息一律带上步骤名——四步塌缩成同一句"授权被拒绝"时,管理员既判不出该重试还是
/// 该丢弃,也定位不到是哪一步坏了。状态码在错误变体的 `status` 字段里,不在文案中重复。
async fn approve_step(step: &str, req: reqwest::RequestBuilder) -> Result<(), LoginError> {
    let resp = req.send().await.map_err(|_| LoginError::Http)?;
    let st = resp.status();
    if st.is_success() {
        return Ok(());
    }
    let status = st.as_u16();
    let summary = super::extract_upstream_message(&resp.text().await.unwrap_or_default());
    let detail = if summary.is_empty() {
        format!("portal 批准步骤 {step} 失败")
    } else {
        format!("portal 批准步骤 {step} 失败: {summary}")
    };
    match status {
        401 | 403 => {
            // Denied 是无载荷的终态变体,步骤名与状态码只能落到日志,否则排查无据。
            tracing::warn!(
                event = "sso_token_approve_denied",
                step = %step,
                status = status,
                detail = %detail,
                "portal 批准被拒(bearer 无效或无权),该行应丢弃而非重试"
            );
            Err(LoginError::Denied)
        }
        429 | 500..=599 => Err(LoginError::Transient { status, detail }),
        _ => Err(LoginError::UpstreamHttp {
            status,
            body: detail,
        }),
    }
}

/// 批量兑换,每行一个 bearer,超上限截断;每行独立报告成功/失败。
pub async fn redeem_bulk(
    client: &reqwest::Client,
    oidc: &str,
    portal: &str,
    region: &str,
    lines: &[&str],
) -> Vec<Result<MintedCredential, LoginError>> {
    let mut out = Vec::new();
    for bearer in lines.iter().take(MAX_BULK) {
        out.push(redeem(client, oidc, portal, region, bearer).await);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// portal 批准链的四步(方法, 路径),顺序即调用顺序。
    const APPROVE_STEPS: [(&str, &str); 4] = [
        ("GET", "/token/whoAmI"),
        ("POST", "/session/device"),
        ("POST", "/device_authorization/accept_user_code"),
        ("POST", "/device_authorization/associate_token"),
    ];

    /// 注册 + 设备授权(SSO-OIDC 公开流程)。`interval` 直接决定轮询退避,测试取 0 以免真等。
    async fn mount_oidc_start(s: &MockServer, interval: u64) {
        Mock::given(method("POST"))
            .and(path("/client/register"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"clientId":"cid","clientSecret":"csec"})),
            )
            .mount(s)
            .await;
        Mock::given(method("POST"))
            .and(path("/device_authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceCode":"dev","userCode":"UC-1","verificationUri":"https://x/",
                "interval": interval,
                "verificationUriComplete":"https://x/?user_code=UC-1"})))
            .mount(s)
            .await;
    }

    /// portal 自动批准链(端点为观测所得):`failing` 命中的那一步回 `status`,其余回 200。
    async fn mount_portal_approval(s: &MockServer, failing: Option<(&str, u16)>) {
        for (m, p) in APPROVE_STEPS {
            let code = match failing {
                Some((f, status)) if f == p => status,
                _ => 200,
            };
            Mock::given(method(m))
                .and(path(p))
                .respond_with(
                    ResponseTemplate::new(code)
                        .set_body_json(serde_json::json!({"ok": code == 200})),
                )
                .mount(s)
                .await;
        }
    }

    async fn mount_token_success(s: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"accessToken":"atok","refreshToken":"rtok","expiresIn":3600}),
            ))
            .mount(s)
            .await;
    }

    async fn mock_full_chain() -> MockServer {
        let s = MockServer::start().await;
        mount_oidc_start(&s, 0).await;
        mount_portal_approval(&s, None).await;
        mount_token_success(&s).await;
        s
    }

    /// 让批准链的 `failing` 步以 `status` 失败,返回 redeem 的错误。
    async fn redeem_with_failing_step(failing: &str, status: u16) -> LoginError {
        let s = MockServer::start().await;
        mount_oidc_start(&s, 0).await;
        mount_portal_approval(&s, Some((failing, status))).await;
        mount_token_success(&s).await;
        let client = reqwest::Client::new();
        redeem(&client, &s.uri(), &s.uri(), "us-east-1", "bearer-xyz")
            .await
            .unwrap_err()
    }

    /// portal 批准链的四步必须**头一致、且都带 User-Agent**。
    ///
    /// 回归:这条链是我们自有的功能(登录/刷新那两条已在 v0.16.0 补齐,portal 是**第三条
    /// 路径**,当时漏了)。它此前一个 header 都不设 —— reqwest 不配就连 `User-Agent` 都
    /// 不发。四个打向 AWS portal 主机的请求,零 UA,而且四步里只有第三步带 `Referer`:
    /// 浏览器对同一个页面发出的同源 XHR 不会时有时无地带 Referer。这两件事都是自动化的
    /// 形状,不是浏览器的形状,而且发生在账号刚被创建、上游最会看指纹的时刻。
    #[tokio::test]
    async fn portal_approval_steps_share_one_browser_shaped_header_set() {
        let s = mock_full_chain().await;
        let client = reqwest::Client::new();
        redeem(&client, &s.uri(), &s.uri(), "us-east-1", "bearer-xyz")
            .await
            .unwrap();

        let reqs = s.received_requests().await.unwrap();
        let portal: Vec<_> = reqs
            .iter()
            .filter(|r| {
                let p = r.url.path();
                p.contains("whoAmI")
                    || p.contains("session/device")
                    || p.contains("accept_user_code")
                    || p.contains("associate_token")
            })
            .collect();
        assert_eq!(portal.len(), 4, "四步都该发出");

        for r in &portal {
            let p = r.url.path();
            let ua = r
                .headers
                .get("user-agent")
                .unwrap_or_else(|| panic!("{p}: 一个打向 portal 的请求不能不带 User-Agent"))
                .to_str()
                .unwrap();
            assert!(ua.contains("Mozilla/5.0"), "{p}: portal 只见得到浏览器: {ua}");
            // 同源 XHR:Origin 与 Referer 每一步都在,不能时有时无。
            assert!(r.headers.contains_key("referer"), "{p}: 缺 Referer");
            assert!(r.headers.contains_key("origin"), "{p}: 缺 Origin");
            assert!(r.headers.contains_key("accept"), "{p}: 缺 Accept");
            assert!(
                r.headers["authorization"]
                    .to_str()
                    .unwrap()
                    .starts_with("Bearer "),
                "{p}: bearer 不能丢"
            );
        }

        // 四步的 UA 必须**是同一个**(同一个页面里的连续 XHR)。
        let uas: std::collections::BTreeSet<_> = portal
            .iter()
            .map(|r| r.headers["user-agent"].to_str().unwrap())
            .collect();
        assert_eq!(uas.len(), 1, "四步的 UA 必须一致: {uas:?}");
    }

    /// portal 那四步的浏览器 UA 必须跟着**进程级**配置的操作系统走。
    ///
    /// 回归:登录流的伪装身份曾经取编译期默认值。于是把 `systemVersion` 配成 macOS 的部署,
    /// sso-oidc 那条链照配置报 macOS,portal 这条链却仍按编译期默认的 Windows 报 —— 同一次
    /// 登录里两台主机看到同一个人来自两个操作系统,这比整条链都用默认值更显眼。
    #[tokio::test]
    async fn portal_browser_ua_follows_the_process_level_os() {
        let effective = crate::kiro::login::install_test_process_config();
        assert_ne!(
            effective.system_version,
            crate::config::Config::default().system_version,
            "夹具没能把进程级配置灌进去,本用例失去区分力"
        );
        let want = crate::kiro::login::browser_user_agent(&effective.system_version);
        assert_ne!(
            want,
            crate::kiro::login::browser_user_agent(
                &crate::config::Config::default().system_version
            ),
            "夹具的 system_version 必须落在与默认值不同的平台上,否则本用例分不出对错"
        );

        let s = mock_full_chain().await;
        let client = reqwest::Client::new();
        redeem(&client, &s.uri(), &s.uri(), "us-east-1", "bearer-xyz")
            .await
            .unwrap();

        let reqs = s.received_requests().await.unwrap();
        let portal: Vec<_> = reqs
            .iter()
            .filter(|r| APPROVE_STEPS.iter().any(|(_, p)| r.url.path() == *p))
            .collect();
        assert_eq!(portal.len(), 4, "四步都该发出");
        for r in &portal {
            assert_eq!(
                r.headers["user-agent"].to_str().unwrap(),
                want,
                "{}: portal 的浏览器 UA 没跟着进程级 system_version 走",
                r.url.path()
            );
        }
    }

    #[tokio::test]
    async fn redeem_walks_chain_to_credential() {
        let s = mock_full_chain().await;
        let client = reqwest::Client::new();
        let cred = redeem(&client, &s.uri(), &s.uri(), "us-east-1", "bearer-xyz")
            .await
            .unwrap();
        assert_eq!(cred.access_token, "atok");
        assert_eq!(cred.region, "us-east-1");
        // 客户端凭据经 builderid::poll 一路带出(auth=Idc 首刷依赖)。
        assert_eq!(cred.client_id, "cid");
        assert_eq!(cred.client_secret, "csec");
        assert!(!cred.client_id.is_empty() && !cred.client_secret.is_empty());
    }

    #[tokio::test]
    async fn bulk_caps_and_reports_each() {
        let s = mock_full_chain().await;
        let client = reqwest::Client::new();
        let lines = vec!["b1", "b2"];
        let out = redeem_bulk(&client, &s.uri(), &s.uri(), "us-east-1", &lines).await;
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.is_ok()));
    }

    #[tokio::test]
    async fn redeem_retries_pending_until_token_ready() {
        // 批准落地到设备码可换 token 有传播延迟:只轮询一次会让批量导入随机少几个账号。
        let s = MockServer::start().await;
        mount_oidc_start(&s, 0).await;
        mount_portal_approval(&s, None).await;
        // 头一次 /token 说"还在等",第二次是代理塞回的纯文本 429(判不出终态的瞬态),
        // 第三次才给 token。优先级由高到低保证按此顺序命中。
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error":"authorization_pending"})),
            )
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(429).set_body_string("Too Many Requests"))
            .up_to_n_times(1)
            .with_priority(2)
            .mount(&s)
            .await;
        mount_token_success(&s).await; // 默认优先级 5,兜底

        let client = reqwest::Client::new();
        let cred = redeem(&client, &s.uri(), &s.uri(), "us-east-1", "bearer-xyz")
            .await
            .unwrap();
        assert_eq!(cred.access_token, "atok");
        assert_eq!(cred.refresh_token, "rtok");
    }

    #[tokio::test]
    async fn redeem_stops_at_terminal_token_error() {
        // 设备码过期是终态,不该继续轮询浪费整批导入的时间预算。
        let s = MockServer::start().await;
        mount_oidc_start(&s, 0).await;
        mount_portal_approval(&s, None).await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error":"expired_token"})),
            )
            .expect(1) // 只打一次;多打即说明终态被当成瞬态重试了
            .mount(&s)
            .await;
        let client = reqwest::Client::new();
        let err = redeem(&client, &s.uri(), &s.uri(), "us-east-1", "bearer-xyz")
            .await
            .unwrap_err();
        assert_eq!(err, LoginError::Expired);
    }

    #[tokio::test]
    async fn approve_chain_classifies_status_by_step() {
        // 401 = bearer 真失效/无权 → Denied(该行丢弃)。
        assert_eq!(
            redeem_with_failing_step("/token/whoAmI", 401).await,
            LoginError::Denied
        );
        assert_eq!(
            redeem_with_failing_step("/session/device", 403).await,
            LoginError::Denied
        );
        // 429 限流 → 瞬态可重试,且能看出是哪一步。
        match redeem_with_failing_step("/session/device", 429).await {
            LoginError::Transient { status, detail } => {
                assert_eq!(status, 429);
                assert!(detail.contains("session/device"), "detail={detail}");
            }
            other => panic!("expected Transient, got {other:?}"),
        }
        // 上游 5xx → 瞬态可重试。
        match redeem_with_failing_step("/device_authorization/associate_token", 503).await {
            LoginError::Transient { status, detail } => {
                assert_eq!(status, 503);
                assert!(detail.contains("associate_token"), "detail={detail}");
            }
            other => panic!("expected Transient, got {other:?}"),
        }
        // 404 端点变更 → 明确上游错误,而不是笼统的"授权被拒绝"。
        match redeem_with_failing_step("/device_authorization/accept_user_code", 404).await {
            LoginError::UpstreamHttp { status, body } => {
                assert_eq!(status, 404);
                assert!(body.contains("accept_user_code"), "body={body}");
            }
            other => panic!("expected UpstreamHttp, got {other:?}"),
        }
    }
}
