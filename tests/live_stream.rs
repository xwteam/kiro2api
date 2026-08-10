//! 真机流式端到端中转测试(`#[ignore]`,默认不跑,须手动 `--ignored`)。
//!
//! 安全约束(严格遵守,与 `tests/live_relay.rs` 同法):
//! - 只读凭据文件,绝不写回、绝不调用任何持久化保存、绝不写 `.tmp` 兄弟文件。
//! - 绝不 `println!`/日志任何 token/refreshToken/accessToken 值。
//! - 只发一次真实调用(tiny prompt "ping"),失败即报错、不对真实后端重试。
//!
//! 凭据文件路径来源:环境变量 `KIRO2API_LIVE_CREDS`(必须显式设置,无默认路径)。
//! 未设置时本测试直接跳过(打印提示后 return),不 panic、不失败。

use axum::response::IntoResponse;
use kiro2api::config::Config;
use kiro2api::kiro::credential::{self, AuthMethod};
use kiro2api::kiro::pool::{LbMode, Pool};
use kiro2api::protocol::anthropic::handler::{MessagesState, relay_stream};
use kiro2api::protocol::anthropic::types::{ContentIn, InMsg, MessagesRequest};
use std::sync::Arc;
use tokio::sync::Mutex;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 真机一次性流式中转:选 social 账号(契约验证通路)、走生产端点回退、
/// 断言 SSE 全文含 message_start/message_stop 与至少一处 content_block_delta。
#[tokio::test]
#[ignore = "hits real production Kiro backend; run manually with --ignored"]
async fn live_stream_one_ping() {
    let Ok(path) = std::env::var("KIRO2API_LIVE_CREDS") else {
        eprintln!(
            "live_stream: set KIRO2API_LIVE_CREDS=/path/to/credentials.json to run this #[ignore] test; skipping"
        );
        return;
    };
    let creds = credential::load(&path).expect("加载凭据文件失败");
    assert!(!creds.is_empty(), "凭据文件为空,无法进行真机测试");

    // 优先选唯一的 social 账号(带 profileArn,契约验证的工作通路)。
    // 只把该账号放进池,确保 select 必命中它;其余账号一律不入池。
    let social: Vec<_> = creds
        .into_iter()
        .filter(|c| c.auth == AuthMethod::Social && !c.disabled)
        .collect();
    assert!(!social.is_empty(), "未找到可用的 social 账号");

    let pool = Arc::new(Mutex::new(Pool::new(social, LbMode::Priority)));
    let state = MessagesState {
        pool,
        client: reqwest::Client::new(),
        control_client: reqwest::Client::new(),
        cfg: Arc::new(Config::default()),
        runtime_cfg: kiro2api::config::shared_runtime_config(&Config::default()),
        endpoint_override: None, // 生产路径:真实端点回退
        stats: kiro2api::stats::StatsManager::load_from_dir(&std::env::temp_dir()),
        api_keys: kiro2api::apikey::ApiKeyStore::load(
            std::env::temp_dir().join("kiro2api_live_stream_apikeys.json"),
        ),
        balance: kiro2api::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
        models_cache: kiro2api::models_cache::ModelsCache::new(),
        builderid_sessions: kiro2api::admin::login_session::LoginSessions::with_default_ttl(),
        iam_sso_sessions: kiro2api::admin::login_session::LoginSessions::with_default_ttl(),
        log_capture: None,
        refresh_ctx: kiro2api::kiro::ensure_fresh::RefreshCtx::new(
            std::env::temp_dir()
                .join(format!(
                    "kiro2api_refreshctx_tests_live_stream_rs_{}.json",
                    std::process::id()
                ))
                .to_string_lossy()
                .to_string(),
        ),
    };

    let req = MessagesRequest {
        thinking: None,
        metadata: None,
        model: "sonnet".to_string(),
        system: None,
        messages: vec![InMsg {
            role: "user".to_string(),
            content: ContentIn::Text("ping".to_string()),
        }],
        max_tokens: Some(16),
        stream: Some(true),
        tools: None,
        tool_choice: None,
    };

    // select→expires_soon?refresh:use 的分支都由 relay_stream 内部处理(内存刷新不写盘)。
    match relay_stream(&state, req, now_unix()).await {
        Ok(sse) => {
            let resp = sse.into_response();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .expect("读取 SSE 响应体失败");
            let s = String::from_utf8_lossy(&bytes);

            let delta_count = s.matches("content_block_delta").count();

            // 不打印令牌;只报告字节长度与 delta 计数。
            assert!(
                s.contains("event: message_start"),
                "SSE 全文缺少 message_start 事件"
            );
            assert!(
                s.contains("event: message_stop"),
                "SSE 全文缺少 message_stop 事件"
            );
            assert!(
                delta_count > 0,
                "SSE 全文未见任何 content_block_delta(文本增量)"
            );

            eprintln!(
                "[live] 流式中转成功:SSE {} 字节,{} 处 content_block_delta",
                bytes.len(),
                delta_count
            );
        }
        Err(e) => {
            // 只报告粗粒度错误枚举(不含令牌);不重试。
            panic!("真机流式中转失败:{e:?}");
        }
    }
}
