//! 代理是否**真的生效**的端到端验证。
//!
//! 单元测试只能证明"该用哪个代理算得对";算得对而客户端没真走代理,是这类功能最典型的
//! 失败方式 —— 而且它在生产上表现为"面板显示配好了、流量照旧从主 IP 出",最难发现。
//!
//! 这里起一个真的 HTTP 代理(只认 absolute-form 请求行并原样应答),让凭据指向它,
//! 看请求是否落到代理上。

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use kiro2api::kiro::credential::{AuthMethod, Credential};

fn cred_with_proxy(proxy: Option<String>) -> Credential {
    Credential {
        id: "p1".into(),
        access_token: "AT".into(),
        refresh_token: "RT".into(),
        kiro_api_key: None,
        expires_at_unix: 4_000_000_000,
        region: "us-east-1".into(),
        auth: AuthMethod::Social,
        client_id: None,
        client_secret: None,
        profile_arn: None,
        machine_id: None,
        email: None,
        nickname: None,
        weight: 1,
        label: None,
        disabled: false,
        status_reason: None,
        proxy_url: proxy,
        proxy_username: None,
        proxy_password: None,
    }
}

/// 极简 HTTP 代理:收下一个请求,把请求行发回测试线程,再回一个 200。
fn spawn_proxy() -> (u16, mpsc::Receiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let mut s = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut line = String::new();
            let _ = BufReader::new(s.try_clone().unwrap()).read_line(&mut line);
            let _ = tx.send(line);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            let _ = s.flush();
        }
    });
    (port, rx)
}

/// 配了代理的凭据,其请求必须落到代理上,且请求行是 absolute-form(代理协议的标志)。
#[tokio::test]
async fn a_credential_with_a_proxy_actually_goes_through_it() {
    let (port, rx) = spawn_proxy();
    let cred = cred_with_proxy(Some(format!("http://127.0.0.1:{port}")));

    let client = kiro2api::http::streaming_for(&cred);
    let _ = client.get("http://example.invalid/probe").send().await;

    let line = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("请求没有落到代理上——代理没生效");
    assert!(
        line.contains("http://example.invalid/probe"),
        "应是 absolute-form 的代理请求行,实得: {line}"
    );
}

/// 同一个账号的**控制面**出口必须与数据面一致。
///
/// 数据面走代理、令牌刷新却从主 IP 出,比不配代理更糟:那等于主动告诉上游这两条流量
/// 属于同一个中转。
#[tokio::test]
async fn control_plane_uses_the_same_exit_as_the_data_plane() {
    let (port, rx) = spawn_proxy();
    let cred = cred_with_proxy(Some(format!("http://127.0.0.1:{port}")));

    for c in [
        kiro2api::http::unary_for(&cred),
        kiro2api::http::axios_for(&cred),
    ] {
        let _ = c.get("http://example.invalid/ctrl").send().await;
        let line = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("控制面请求没有走代理");
        assert!(line.contains("http://example.invalid/ctrl"), "{line}");
    }
}

/// `direct` 显式直连:即便配了全局代理也不得走代理。
#[tokio::test]
async fn direct_bypasses_the_proxy() {
    let (port, rx) = spawn_proxy();
    // 这里不设全局代理(进程级默认值在测试里不易注入),只验凭据级 "direct" 不指向代理:
    // 客户端不应把请求发到代理端口上。
    let cred = cred_with_proxy(Some("direct".into()));
    let client = kiro2api::http::streaming_for(&cred);
    let _ = client
        .get(format!("http://127.0.0.1:{port}/direct-probe"))
        .send()
        .await;
    // 直连时这条请求会以 origin-form 直接打到该端口(它此刻扮演普通服务端),
    // 请求行**不**含绝对 URL —— 这正是"没走代理"的判据。
    let line = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("请求应直接到达该端口");
    assert!(
        !line.contains("http://127.0.0.1"),
        "direct 不该产生代理形式的请求行,实得: {line}"
    );
    assert!(line.starts_with("GET /direct-probe"), "{line}");
}
