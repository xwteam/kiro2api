//! 真机工具往返测试(`#[ignore]`,默认不跑,须手动 `--ignored`)。
//!
//! 安全约束(严格遵守,镜像 `live_relay.rs`):
//! - 只读凭据文件,绝不写回、绝不调用任何持久化保存、绝不写 `.tmp` 兄弟文件。
//! - 绝不 `println!`/日志任何 token/refreshToken/accessToken 值。
//! - 只发一次真实调用,失败即报错、不对真实后端重试。
//!
//! 凭据文件路径来源:环境变量 `KIRO2API_LIVE_CREDS`(必须显式设置,无默认路径)。
//! 未设置时本测试直接跳过(打印提示后 return),不 panic、不失败。

use kiro2api::config::Config;
use kiro2api::kiro::credential::{self, AuthMethod};
use kiro2api::kiro::pool::{LbMode, Pool};
use kiro2api::protocol::anthropic::handler::{MessagesState, relay_core};
use kiro2api::protocol::anthropic::types::{ContentIn, InMsg, MessagesRequest, OutBlock, ToolDef};
use std::sync::Arc;
use tokio::sync::Mutex;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 真机一次性工具往返:声明 `get_weather` 工具、断言响应含 `tool_use` 块且
/// `stop_reason == "tool_use"`(契约验证通路,证明工具调用可端到端透传)。
#[tokio::test]
#[ignore = "hits real production Kiro backend; run manually with --ignored"]
async fn live_tools_get_weather_round_trip() {
    let Ok(path) = std::env::var("KIRO2API_LIVE_CREDS") else {
        eprintln!("live_tools: set KIRO2API_LIVE_CREDS=... to run this #[ignore] test; skipping");
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
            std::env::temp_dir().join("kiro2api_live_tools_apikeys.json"),
        ),
        balance: kiro2api::balance::BalanceCache::load_from_dir(&std::env::temp_dir()),
        models_cache: kiro2api::models_cache::ModelsCache::new(),
        builderid_sessions: kiro2api::admin::login_session::LoginSessions::with_default_ttl(),
        iam_sso_sessions: kiro2api::admin::login_session::LoginSessions::with_default_ttl(),
        log_capture: None,
        refresh_ctx: kiro2api::kiro::ensure_fresh::RefreshCtx::new(
            std::env::temp_dir()
                .join(format!(
                    "kiro2api_refreshctx_tests_live_tools_rs_{}.json",
                    std::process::id()
                ))
                .to_string_lossy()
                .to_string(),
        ),
    };

    let req = MessagesRequest {
        metadata: None,
        model: "sonnet".to_string(),
        system: None,
        messages: vec![InMsg {
            role: "user".to_string(),
            content: ContentIn::Text(
                "What is the weather in Paris right now? Call the get_weather tool.".to_string(),
            ),
        }],
        max_tokens: Some(64),
        stream: Some(false),
        tools: Some(vec![ToolDef {
            tool_type: None,
            name: "get_weather".to_string(),
            description: Some("Get the current weather for a city.".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string", "description": "City name" }
                },
                "required": ["city"]
            }),
        }]),
        tool_choice: None,
    };

    // select→expires_soon?refresh:use 的分支都由 relay_core 内部处理(内存刷新不写盘)。
    match relay_core(&state, req, now_unix()).await {
        Ok(resp) => {
            let tool_use = resp.content.iter().find_map(|b| match b {
                OutBlock::ToolUse { name, input, .. } if name == "get_weather" => {
                    Some((name.clone(), input.clone()))
                }
                _ => None,
            });
            let (name, input) = tool_use.expect("响应中未找到 get_weather 的 tool_use 块");
            let city_ok = input
                .get("city")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty());

            // 不打印令牌;只报告工具名与参数是否解析出非空 city。
            eprintln!(
                "[live] 工具往返成功:tool={name} city_parsed={city_ok} stop_reason={:?}",
                resp.stop_reason
            );

            assert!(city_ok, "get_weather 的 input.city 未解析为非空字符串");
            assert_eq!(
                resp.stop_reason.as_deref(),
                Some("tool_use"),
                "stop_reason 应为 tool_use"
            );
        }
        Err(e) => {
            // 只报告粗粒度错误枚举(不含令牌);不重试。
            panic!("真机工具往返失败:{e:?}");
        }
    }
}
