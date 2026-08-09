//! Kiro/CodeWhisperer 后端相关模块。
pub mod convert;
pub mod credential;
pub mod endpoint;
pub mod ensure_fresh;
pub mod eventstream;
pub mod keepalive;
pub mod login;
pub mod machine_id;
pub mod pool;
pub mod provider;
pub mod refresh;
pub mod wire;

/// 生成一个 RFC 4122 UUID v4 的标准写法(8-4-4-4-12,带连字符)。
///
/// 上游请求里有好几个字段本就是 UUID:`amz-sdk-invocation-id`、`conversationId`、
/// `agentContinuationId`。此前它们发的是 32 位无连字符十六进制 —— 形状不对,而这些字段
/// 出现在**每一个**请求上,是零成本、100% 命中的判别器。故统一收口到这里。
pub fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("CSPRNG");
    b[6] = (b[6] & 0x0f) | 0x40; // version = 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant = RFC 4122
    let h = hex::encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// 判断一个字符串是否是标准写法的 UUID(8-4-4-4-12 十六进制)。
pub fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12] == parts.iter().map(|p| p.len()).collect::<Vec<_>>()[..]
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}
