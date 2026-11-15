//! 缺失 profileArn 的惰性解析:按账号单飞,分页完成后才写回活池与磁盘。
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::kiro::credential::{Credential, fill_offline_profile};
use crate::kiro::ensure_fresh::RefreshCtx;
use crate::kiro::pool::{FailureKind, Pool, classify_with_body};
use crate::kiro::provider::Impersonation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileError {
    pub kind: FailureKind,
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ProfileError {}

impl ProfileError {
    fn transport() -> Self {
        Self {
            kind: FailureKind::Transient,
            status: 0,
            message: "ListAvailableProfiles request or response failed; retry the request.".into(),
        }
    }
}

type Flight = Mutex<Option<Result<(), ProfileError>>>;

/// 「查不到可用 profile」这类确定性结论记多久:期间同一账号直接复用结论,不再打上游。
const NO_PROFILE_BACKOFF_SECS: u64 = 600;
/// 网络错误、5xx 这类瞬时失败的退避:请求量大时别每个请求都去重查,把上游抖动放大。
const TRANSIENT_BACKOFF_SECS: u64 = 30;

/// 一条负缓存:到期时刻、结论,以及得出结论时的账号快照(身份变了就作废)。
struct Negative {
    until: u64,
    error: ProfileError,
    cred: Credential,
}

/// 单飞表的 Weak 条目只存活于本批并发调用;另有一份有界负缓存,见 [`Negative`]。
#[derive(Clone, Default)]
pub(crate) struct ProfileCoordinator(
    Arc<Mutex<HashMap<String, Weak<Flight>>>>,
    Arc<Mutex<HashMap<String, Negative>>>,
);

impl ProfileCoordinator {
    /// 未过期、且账号身份未变的负缓存结论。
    async fn cached_failure(&self, cred: &Credential, now: u64) -> Option<ProfileError> {
        let mut map = self.1.lock().await;
        map.retain(|_, n| n.until > now);
        map.get(&cred.id)
            .filter(|n| Pool::identity_matches(cred, &n.cred))
            .map(|n| n.error.clone())
    }

    async fn remember_failure(&self, cred: &Credential, error: &ProfileError, now: u64) {
        let ttl = match error.kind {
            FailureKind::AccountConfig => NO_PROFILE_BACKOFF_SECS,
            FailureKind::Transient => TRANSIENT_BACKOFF_SECS,
            // 鉴权、配额类失败已反馈账号池(冷却/停用会把它排除出选号),不另做缓存。
            _ => return,
        };
        self.1.lock().await.insert(
            cred.id.clone(),
            Negative {
                until: now.saturating_add(ttl),
                error: error.clone(),
                cred: cred.clone(),
            },
        );
    }

    async fn forget(&self, id: &str) {
        self.1.lock().await.remove(id);
    }

    async fn flight(&self, id: &str) -> Arc<Flight> {
        let mut map = self.0.lock().await;
        map.retain(|_, f| f.strong_count() > 0);
        if let Some(flight) = map.get(id).and_then(Weak::upgrade) {
            return flight;
        }
        let flight = Arc::new(Mutex::new(None));
        map.insert(id.into(), Arc::downgrade(&flight));
        flight
    }
}

/// Profile discovery 属于既有 bearer 控制面,不是新的管理服务。
pub fn base(cred: &Credential) -> String {
    format!(
        "https://q.{}.amazonaws.com",
        crate::kiro::endpoint::effective_region(cred)
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    profiles: Vec<Profile>,
    next_token: Option<String>,
}
#[derive(Deserialize)]
struct Profile {
    arn: String,
}

async fn query(
    client: &reqwest::Client,
    base: &str,
    cred: &Credential,
    imp: &Impersonation,
) -> Result<Vec<String>, ProfileError> {
    let url = format!("{}/", base.trim_end_matches('/'));
    let mut body = serde_json::json!({"maxResults":100});
    let mut arns = Vec::new();
    let mut tokens = HashSet::new();
    loop {
        let headers = if imp.endpoint == "cli" {
            crate::kiro::provider::cli_headers(
                cred,
                imp,
                &url,
                "AmazonCodeWhispererService.ListAvailableProfiles",
                "codewhispererruntime/0.1.19636",
                false,
            )
        } else {
            let mut h = reqwest::header::HeaderMap::new();
            let hv = |v: &str| {
                reqwest::header::HeaderValue::from_str(v)
                    .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static(""))
            };
            h.insert("content-type", hv("application/x-amz-json-1.0"));
            h.insert("connection", hv("close"));
            h.insert(
                "x-amz-user-agent",
                hv(&format!(
                    "aws-sdk-js/1.0.0 KiroIDE-{}-{}",
                    imp.kiro_version, imp.machine_id
                )),
            );
            h.insert("user-agent", hv(&format!("aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}", imp.system_version, imp.node_version, imp.kiro_version, imp.machine_id)));
            if let Some(host) = crate::kiro::provider::host_of(&url) {
                h.insert("host", hv(&host));
            }
            h.insert("amz-sdk-invocation-id", hv(&crate::kiro::uuid_v4()));
            h.insert("amz-sdk-request", hv("attempt=1; max=1"));
            h.insert("authorization", hv(&format!("Bearer {}", cred.bearer())));
            h.insert(
                "x-amz-target",
                hv("AmazonCodeWhispererService.ListAvailableProfiles"),
            );
            h
        };
        let resp = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|_| ProfileError::transport())?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = crate::kiro::provider::read_body_capped(resp).await;
            return Err(ProfileError {
                kind: classify_with_body(status, &text),
                status,
                message: format!(
                    "ListAvailableProfiles HTTP {status}: {}",
                    crate::kiro::login::extract_upstream_message(&text)
                ),
            });
        }
        let page: Page = resp.json().await.map_err(|_| ProfileError::transport())?;
        for profile in page.profiles {
            if profile.arn.trim().is_empty() {
                return Err(ProfileError::transport());
            }
            if !arns.contains(&profile.arn) {
                arns.push(profile.arn);
            }
        }
        match page.next_token.filter(|s| !s.is_empty()) {
            None => return Ok(arns),
            Some(token) if tokens.insert(token.clone()) => body["nextToken"] = token.into(),
            Some(_) => return Err(ProfileError::transport()),
        }
    }
}

fn snapshot(pool: &Pool, id: &str) -> Result<Credential, ProfileError> {
    pool.credential(id).ok_or_else(ProfileError::transport)
}

fn complete(cred: &Credential) -> bool {
    cred.is_api_key()
        || cred
            .profile_arn
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
}

pub async fn ensure_profile(
    pool: &Arc<Mutex<Pool>>,
    id: &str,
    client: &reqwest::Client,
    cfg: &crate::config::Config,
    now: u64,
    ctx: Option<&RefreshCtx>,
) -> Result<Credential, ProfileError> {
    ensure_at(pool, id, client, cfg, now, ctx, None).await
}

#[allow(clippy::too_many_arguments)] // 测试注入 base,其余参数与生产入口相同。
async fn ensure_at(
    pool: &Arc<Mutex<Pool>>,
    id: &str,
    client: &reqwest::Client,
    cfg: &crate::config::Config,
    now: u64,
    ctx: Option<&RefreshCtx>,
    override_base: Option<&str>,
) -> Result<Credential, ProfileError> {
    let coordinator = {
        let pool = pool.lock().await;
        let cred = snapshot(&pool, id)?;
        if complete(&cred) {
            return Ok(cred);
        }
        let coordinator = pool.profile_coordinator.clone();
        drop(pool);
        // 近期已得出「查不到」或刚遇到瞬时失败:直接复用结论,别每个请求都去重查上游。
        if let Some(error) = coordinator.cached_failure(&cred, now).await {
            return Err(error);
        }
        coordinator
    };
    let flight = coordinator.flight(id).await;
    let mut result = flight.lock().await;
    if let Some(outcome) = result.as_ref() {
        outcome.clone()?;
        return snapshot(&*pool.lock().await, id);
    }
    let mut cred = snapshot(&*pool.lock().await, id)?;
    if complete(&cred) {
        return Ok(cred);
    }
    let mut expected = cred.clone();
    let outcome = if fill_offline_profile(&mut cred) {
        Ok(())
    } else {
        let mut imp = Impersonation::for_credential(&cred, cfg);
        let mut queried = query(client, override_base.unwrap_or(&base(&cred)), &cred, &imp).await;
        // 401/403:先像数据面那样强制刷新令牌、用新令牌重查一次,只对最终结果下结论。
        // 令牌被上游提前作废时这样能自愈,而不是把一个可恢复的账号直接判死。
        // (测试注入 base 时不走真实刷新端点。)
        if let Err(e) = &queried
            && matches!(
                e.kind,
                FailureKind::AuthInvalid | FailureKind::AuthAmbiguous
            )
            && override_base.is_none()
            && let Some(ctx) = ctx
        {
            match crate::kiro::ensure_fresh::force_refresh(
                pool,
                id,
                client,
                now,
                cred.bearer(),
                Some(ctx),
            )
            .await
            {
                Ok(fresh) => {
                    cred = fresh;
                    expected = cred.clone();
                    imp = Impersonation::for_credential(&cred, cfg);
                    queried = query(client, &base(&cred), &cred, &imp).await;
                }
                // 刷新失败已由 force_refresh 按刷新结果反馈账号池,这里不再重复处罚。
                Err(_) => {
                    let error = e.clone();
                    *result = Some(Err(error.clone()));
                    return Err(error);
                }
            }
        }
        match queried {
            Ok(arns) if !arns.is_empty() => {
                if arns.len() > 1 {
                    tracing::warn!(account_id = %id, profiles = arns.len(), "多个 profile 可用,采用第一个;管理员可修改 profileArn");
                }
                cred.profile_arn = arns.into_iter().next();
                Ok(())
            }
            // 唯一可靠依据是登录流程写入的来源;旧 authMethod=idc、clientId 和 token 形状
            // 均不能证明 Builder ID。显式 Builder ID 已在离线分支处理,不发接口请求。
            // 账号级结论:不罚号,由上层换个账号再试(见 FailureKind::AccountConfig)。
            Ok(_) => Err(ProfileError {
                kind: FailureKind::AccountConfig,
                status: 400,
                message: "No available profileArn for this account. Configure profileArn using the account settings or grant access to a profile.".into(),
            }),
            Err(e) => Err(e),
        }
    };
    if let Err(e) = &outcome {
        if matches!(
            e.kind,
            FailureKind::AccountConfig | FailureKind::InvalidRequest
        ) {
            tracing::error!(account_id = %id, event = "account_profile_unavailable", "{}", e);
        } else {
            // 查询途中管理员可能改了这个账号:旧身份上的失败不能记到新凭据头上。
            let reason = crate::kiro::pool::status_reason_from_body(&e.message);
            let reported = pool
                .lock()
                .await
                .report_failure_if_unchanged(&expected, e.kind, reason, now);
            if reported {
                crate::kiro::ensure_fresh::persist_failure_conclusion(pool, ctx, e.kind).await;
            }
        }
        coordinator.remember_failure(&expected, e, now).await;
    }
    if outcome.is_ok() {
        let written = pool
            .lock()
            .await
            .set_profile_if_unchanged(&expected, cred.profile_arn.unwrap());
        if !written {
            let error = ProfileError::transport();
            *result = Some(Err(error.clone()));
            return Err(error);
        }
        coordinator.forget(id).await;
        if let Some(ctx) = ctx {
            crate::kiro::ensure_fresh::persist_pool_credentials(pool, ctx).await;
        }
    }
    *result = Some(outcome.clone());
    outcome?;
    snapshot(&*pool.lock().await, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::credential::{AuthMethod, BUILDER_ID_DEFAULT_PROFILE, Provider};
    use crate::kiro::pool::LbMode;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn pool() -> Arc<Mutex<Pool>> {
        let mut c = crate::kiro::credential::tests_support_cred();
        c.auth = AuthMethod::Idc;
        c.expires_at_unix = 4_000_000_000;
        Arc::new(Mutex::new(Pool::new(vec![c], LbMode::Priority)))
    }

    async fn run(
        pool: &Arc<Mutex<Pool>>,
        server: &MockServer,
        ctx: Option<&RefreshCtx>,
    ) -> Result<Credential, ProfileError> {
        run_at(pool, server, ctx, 1000).await
    }

    async fn run_at(
        pool: &Arc<Mutex<Pool>>,
        server: &MockServer,
        ctx: Option<&RefreshCtx>,
        now: u64,
    ) -> Result<Credential, ProfileError> {
        ensure_at(
            pool,
            "1",
            &reqwest::Client::new(),
            &crate::config::Config::default(),
            now,
            ctx,
            Some(&server.uri()),
        )
        .await
    }

    async fn page(server: &MockServer, body: serde_json::Value, response: serde_json::Value) {
        Mock::given(method("POST"))
            .and(path("/"))
            .and(header("content-type", "application/x-amz-json-1.0"))
            .and(header(
                "x-amz-target",
                "AmazonCodeWhispererService.ListAvailableProfiles",
            ))
            .and(header("authorization", "Bearer AT"))
            .and(body_json(body))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn one_profile_is_atomically_persisted_with_other_fields() {
        let s = MockServer::start().await;
        page(
            &s,
            serde_json::json!({"maxResults":100}),
            serde_json::json!({"profiles":[{"arn":"arn:one"}]}),
        )
        .await;
        let pool = pool();
        let path = crate::test_tmp::file("profile_one", "credentials.json");
        let ctx = RefreshCtx::new(path.to_string_lossy().to_string());
        assert_eq!(
            run(&pool, &s, Some(&ctx))
                .await
                .unwrap()
                .profile_arn
                .as_deref(),
            Some("arn:one")
        );
        let disk = crate::kiro::credential::load(&ctx.credentials_path).unwrap();
        assert_eq!(disk, pool.lock().await.snapshot_credentials());
        assert_eq!(disk[0].access_token, "AT");
        assert_eq!(run(&pool, &s, Some(&ctx)).await.unwrap(), disk[0]);
    }

    #[tokio::test]
    async fn multiple_profiles_across_all_pages_select_first() {
        let s = MockServer::start().await;
        page(
            &s,
            serde_json::json!({"maxResults":100}),
            serde_json::json!({"profiles":[{"arn":"first"}],"nextToken":"two"}),
        )
        .await;
        page(
            &s,
            serde_json::json!({"maxResults":100,"nextToken":"two"}),
            serde_json::json!({"profiles":[{"arn":"second"}],"nextToken":"three"}),
        )
        .await;
        page(
            &s,
            serde_json::json!({"maxResults":100,"nextToken":"three"}),
            serde_json::json!({"profiles":[]}),
        )
        .await;
        assert_eq!(
            run(&pool(), &s, None).await.unwrap().profile_arn.as_deref(),
            Some("first")
        );
    }

    #[tokio::test]
    async fn zero_profiles_without_reliable_builder_source_is_config_error_not_penalty() {
        let s = MockServer::start().await;
        page(
            &s,
            serde_json::json!({"maxResults":100}),
            serde_json::json!({"profiles":[]}),
        )
        .await;
        let pool = pool();
        let err = run(&pool, &s, None).await.unwrap_err();
        // 账号级:换号再试、不罚号,而不是请求级直接回 400。
        assert_eq!(err.kind, FailureKind::AccountConfig);
        assert_eq!(err.status, 400);
        assert!(err.message.contains("profileArn"));
        let p = pool.lock().await;
        assert!(p.snapshot_credentials()[0].profile_arn.is_none());
        assert_eq!(p.active_count(1000), 1);
        assert_eq!(p.stats(1000)[0].failures, 0);
    }

    /// 标记 builder_id 才是可靠依据。即使接口会回空也不探测,直接离线补默认值。
    #[tokio::test]
    async fn builder_source_uses_default_without_query_even_when_profiles_are_empty() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"profiles":[]})),
            )
            .expect(0)
            .mount(&s)
            .await;
        let pool = pool();
        pool.lock().await.update_credential(
            "1",
            crate::kiro::pool::CredentialUpdate {
                provider: Some(Provider::BuilderId),
                ..Default::default()
            },
        );
        assert_eq!(
            run(&pool, &s, None).await.unwrap().profile_arn.as_deref(),
            Some(BUILDER_ID_DEFAULT_PROFILE)
        );
    }

    #[tokio::test]
    async fn query_failure_does_not_write_profile_and_next_request_retries() {
        for status in [401, 403, 500] {
            let s = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .set_body_json(serde_json::json!({"message":"temporary failure"})),
                )
                .expect(2)
                .mount(&s)
                .await;
            let pool = pool();
            let path = crate::test_tmp::file("profile_failure", "credentials.json");
            let ctx = RefreshCtx::new(path.to_string_lossy().to_string());
            // 5xx 有 30 秒退避,第二次请求放到退避期之后。
            for now in [1000, 1000 + TRANSIENT_BACKOFF_SECS + 1] {
                let e = run_at(&pool, &s, Some(&ctx), now).await.unwrap_err();
                assert_eq!(e.kind, classify_with_body(status, "temporary failure"));
                assert!(
                    pool.lock().await.snapshot_credentials()[0]
                        .profile_arn
                        .is_none()
                );
            }
            assert!(!path.exists(), "瞬时结论不应写盘");
        }
    }

    #[tokio::test]
    async fn partial_pagination_failure_never_writes_first_page() {
        let s = MockServer::start().await;
        page(
            &s,
            serde_json::json!({"maxResults":100}),
            serde_json::json!({"profiles":[{"arn":"first"}],"nextToken":"two"}),
        )
        .await;
        Mock::given(body_json(
            serde_json::json!({"maxResults":100,"nextToken":"two"}),
        ))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&s)
        .await;
        let pool = pool();
        assert!(run(&pool, &s, None).await.is_err());
        assert!(
            pool.lock().await.snapshot_credentials()[0]
                .profile_arn
                .is_none()
        );
    }

    #[tokio::test]
    async fn concurrent_requests_share_one_query_including_failure() {
        for status in [200, 500] {
            let s = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .set_body_json(serde_json::json!({"profiles":[{"arn":"one"}]}))
                        .set_delay(std::time::Duration::from_millis(100)),
                )
                .expect(1)
                .mount(&s)
                .await;
            let pool = pool();
            let mut tasks = Vec::new();
            for _ in 0..12 {
                let pool = pool.clone();
                let base = s.uri();
                tasks.push(tokio::spawn(async move {
                    ensure_at(
                        &pool,
                        "1",
                        &reqwest::Client::new(),
                        &crate::config::Config::default(),
                        1000,
                        None,
                        Some(&base),
                    )
                    .await
                }));
            }
            for t in tasks {
                assert_eq!(t.await.unwrap().is_ok(), status == 200);
            }
            assert_eq!(
                pool.lock().await.stats(1000)[0].failures,
                u64::from(status != 200)
            );
        }
    }

    #[tokio::test]
    async fn cli_profile_query_uses_existing_bearer_host_and_cli_identity() {
        let s = MockServer::start().await;
        page(
            &s,
            serde_json::json!({"maxResults":100}),
            serde_json::json!({"profiles":[{"arn":"one"}]}),
        )
        .await;
        let pool = pool();
        let cfg = crate::config::Config {
            upstream_endpoint: "cli".into(),
            ..Default::default()
        };
        let c = ensure_at(
            &pool,
            "1",
            &reqwest::Client::new(),
            &cfg,
            1000,
            None,
            Some(&s.uri()),
        )
        .await
        .unwrap();
        assert_eq!(base(&c), "https://q.us-east-1.amazonaws.com");
        let requests = s.received_requests().await.unwrap();
        assert!(
            requests[0].headers["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("aws-sdk-rust/")
        );
        // 官方 CLI 抓包:x-amz-user-agent 与 user-agent 同值。
        assert_eq!(
            requests[0].headers["x-amz-user-agent"],
            requests[0].headers["user-agent"]
        );
    }

    #[tokio::test]
    async fn network_failure_and_repeated_page_token_do_not_write_profile() {
        let pool = pool();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(
            ensure_at(
                &pool,
                "1",
                &reqwest::Client::new(),
                &Default::default(),
                1000,
                None,
                Some(&base)
            )
            .await
            .is_err()
        );
        assert!(
            pool.lock().await.snapshot_credentials()[0]
                .profile_arn
                .is_none()
        );
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"profiles":[{"arn":"first"}],"nextToken":"loop"}),
            ))
            .expect(2)
            .mount(&s)
            .await;
        // 上一次网络失败留了 30 秒退避,这次放到退避期之后。
        assert!(
            run_at(&pool, &s, None, 1000 + TRANSIENT_BACKOFF_SECS + 1)
                .await
                .is_err()
        );
        assert!(
            pool.lock().await.snapshot_credentials()[0]
                .profile_arn
                .is_none()
        );
    }

    /// 「查不到可用 profile」记 10 分钟:期间不再打上游;管理员补上 profileArn 立即生效;
    /// 过期后才重查。
    #[tokio::test]
    async fn no_profile_conclusion_is_cached_and_bypassed_by_admin_edit() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"profiles":[]})),
            )
            .expect(2)
            .mount(&s)
            .await;
        let pool = pool();
        for now in [1000, 1000 + NO_PROFILE_BACKOFF_SECS - 1] {
            let e = run_at(&pool, &s, None, now).await.unwrap_err();
            assert_eq!(e.kind, FailureKind::AccountConfig);
        }
        // 过期后重查一次(第二次真实请求)。
        assert!(
            run_at(&pool, &s, None, 1000 + NO_PROFILE_BACKOFF_SECS + 1)
                .await
                .is_err()
        );
        pool.lock().await.update_credential(
            "1",
            crate::kiro::pool::CredentialUpdate {
                profile_arn: Some("admin".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            run_at(&pool, &s, None, 1000 + NO_PROFILE_BACKOFF_SECS + 2)
                .await
                .unwrap()
                .profile_arn
                .as_deref(),
            Some("admin")
        );
    }

    /// 查询发出后账号被管理员改了(令牌换了):旧查询的失败不能记到新凭据头上。
    #[tokio::test]
    async fn stale_failure_is_not_charged_to_replaced_credential() {
        let pool = pool();
        let before = pool.lock().await.snapshot_credentials()[0].clone();
        let mut p = pool.lock().await;
        p.update_credential(
            "1",
            crate::kiro::pool::CredentialUpdate {
                refresh_token: Some("replaced-by-admin".into()),
                ..Default::default()
            },
        );
        assert!(!p.report_failure_if_unchanged(
            &before,
            FailureKind::AuthInvalid,
            crate::kiro::pool::StatusReason::None,
            1000
        ));
        assert_eq!(p.stats(1000)[0].failures, 0);
        assert_eq!(p.active_count(1000), 1);
    }

    #[tokio::test]
    async fn administrator_profile_edit_wins_over_in_flight_discovery() {
        let pool = pool();
        let before = pool.lock().await.snapshot_credentials()[0].clone();
        let mut p = pool.lock().await;
        p.update_credential(
            "1",
            crate::kiro::pool::CredentialUpdate {
                profile_arn: Some("admin".into()),
                ..Default::default()
            },
        );
        assert!(p.set_profile_if_unchanged(&before, "discovered".into()));
        assert_eq!(
            p.snapshot_credentials()[0].profile_arn.as_deref(),
            Some("admin")
        );
    }
}
