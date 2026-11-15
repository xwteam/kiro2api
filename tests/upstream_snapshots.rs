//! 完整 HTTP/1.1 字节快照(不是 HeaderMap):覆盖两种身份的四条调用。
//! 与 0.19.0 的 IDE 差异仅为:版本 1.4.178、Node 24.22.1、对话/MCP SDK
//! 从 1.0.34 到 1.0.44,以及缺失时补 social profileArn。控制面 SDK 仍为 1.0.0;
//! 其它头的名称/值/顺序和正文结构不改。CLI 是新增身份,没有 0.19.0 基线。
use kiro2api::kiro::{convert, credential, endpoint, mcp, provider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn capture() -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let end = loop {
            let mut buf = [0; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "request truncated");
            raw.extend_from_slice(&buf[..n]);
            if let Some(i) = raw.windows(4).position(|b| b == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let headers = std::str::from_utf8(&raw[..end]).unwrap();
        let len = headers
            .split("\r\n")
            .find_map(|line| line.strip_prefix("content-length: "))
            .map(|v| v.parse::<usize>().unwrap())
            .unwrap_or(0);
        while raw.len() < end + len {
            let mut buf = [0; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "body truncated");
            raw.extend_from_slice(&buf[..n]);
        }
        assert_eq!(raw.len(), end + len);
        let body = b"{\"models\":[],\"result\":{\"content\":[]}}";
        stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        raw
    });
    (base, task)
}

fn credential() -> credential::Credential {
    serde_json::from_value(serde_json::json!({
        "id":1,"accessToken":"snapshot-access-token","refreshToken":"snapshot-refresh-token",
        "expiresAt":"2099-01-01T00:00:00Z","authMethod":"social",
        "machineId":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "proxyUrl":"direct"
    }))
    .unwrap()
}

/// UUID 替换保持原长度,故 content-length 仍逐字节校验;只把本地随机端口归一。
fn normalize(raw: Vec<u8>, base: &str) -> Vec<u8> {
    let text = String::from_utf8(raw).unwrap();
    let mut out = String::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 36 <= bytes.len() {
        if bytes[i..i + 36].is_ascii() && kiro2api::kiro::is_uuid(&text[i..i + 36]) {
            out.push_str(&text[start..i]);
            out.push_str("00000000-0000-4000-8000-000000000000");
            i += 36;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[start..]);
    out.replace(base.strip_prefix("http://").unwrap(), "upstream.test")
        .into_bytes()
}

async fn snapshot(identity: &str, operation: &str) {
    let (base, task) = capture().await;
    let mut c = credential();
    credential::fill_offline_profile(&mut c);
    let cfg = kiro2api::config::Config {
        kiro_version: "1.4.178".into(),
        node_version: "24.22.1".into(),
        system_version: "win32#10.0.22631".into(),
        upstream_endpoint: identity.into(),
        kiro_cli_version: "3.3.0".into(),
        ..Default::default()
    };
    let imp = provider::Impersonation::for_credential(&c, &cfg);
    let unary = kiro2api::http::unary_for(&c);
    match operation {
        "conversation" => {
            let request = serde_json::from_value(serde_json::json!({
                "model":"sonnet","max_tokens":128,"messages":[{"role":"user","content":"Hello snapshot"}]
            })).unwrap();
            let converted =
                convert::anthropic_to_kiro_full(&request, c.profile_arn.as_deref()).unwrap();
            let mut ep = match identity {
                "cli" => endpoint::cli("us-east-1"),
                "ide-runtime" => endpoint::ide_runtime("us-east-1"),
                _ => endpoint::all("us-east-1").remove(0),
            };
            ep.url = format!(
                "{}{}",
                base,
                if identity == "cli" {
                    "/"
                } else {
                    "/generateAssistantResponse"
                }
            );
            provider::call(
                &kiro2api::http::streaming_for(&c),
                &ep,
                &c,
                &imp,
                &serde_json::to_vec(&converted.request).unwrap(),
            )
            .await
            .unwrap();
        }
        "mcp" => {
            let request =
                mcp::tools_call("web_search", serde_json::json!({"query":"snapshot query"}));
            mcp::call_at(
                &unary,
                &format!("{}{}", base, if identity == "cli" { "/" } else { "/mcp" }),
                &c,
                &imp,
                &request,
            )
            .await
            .unwrap();
        }
        "models" => {
            kiro2api::models_cache::client::fetch_at(&unary, &base, &c, &imp)
                .await
                .unwrap();
        }
        "balance" => {
            kiro2api::balance::client::fetch_at(&unary, &base, &c, &imp)
                .await
                .unwrap();
        }
        _ => unreachable!(),
    }
    let raw = normalize(
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap(),
        &base,
    );
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/snapshots/{identity}-{operation}.http"));
    if std::env::var_os("UPDATE_UPSTREAM_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &raw).unwrap();
    }
    let expected = std::fs::read(&file).expect("snapshot must exist");
    assert_eq!(
        String::from_utf8(raw).unwrap(),
        String::from_utf8(expected).unwrap(),
        "{identity}/{operation}"
    );
}

#[tokio::test]
async fn ide_complete_raw_requests_match_snapshots() {
    for operation in ["conversation", "mcp", "models", "balance"] {
        snapshot("ide", operation).await;
    }
}

/// IDE 新服务身份(官方 IDE 1.4.178 默认形态):对话与 MCP 打 runtime 主机(REST 路径、无 target),
/// 模型列表 GET `/List-Available-Models`,余额沿用 IDE 的 `GET /getUsageLimits`(主机为 management)。
#[tokio::test]
async fn ide_runtime_complete_raw_requests_match_snapshots() {
    for operation in ["conversation", "mcp", "models", "balance"] {
        snapshot("ide-runtime", operation).await;
    }
}

#[tokio::test]
async fn cli_complete_raw_requests_match_snapshots() {
    for operation in ["conversation", "mcp", "models", "balance"] {
        snapshot("cli", operation).await;
    }
}
