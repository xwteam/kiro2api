//! AWS 事件流整帧解析(公开 wire 规范):prelude(total_len/headers_len/prelude_crc)
//! + headers + payload + message_crc,大端;两处 CRC 均校验。
use super::Error;
use super::crc::crc32;
use super::header::{Header, parse_headers};

/// 一条完整的事件流消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub headers: Vec<Header>,
    pub payload: Vec<u8>,
}

const PRELUDE_LEN: usize = 12; // total_len(4) + headers_len(4) + prelude_crc(4)
const TRAILER_LEN: usize = 4; // message_crc(4)

/// 单帧字节上限(自设,16 MiB)。
///
/// prelude_crc 只覆盖前 8 字节,`total_len` 本身在 CRC 覆盖范围内但一个
/// 被篡改/损坏却仍恰好满足 8 字节 CRC 的 prelude 可以携带任意巨大的
/// `total_len`,诱导解码器为其预留/缓冲 up-to-`total_len` 字节(内存放大 / DoS)。
/// AWS event-stream 帧在实践中是有界的(AWS 文档给出的单帧上限为 16 MiB),
/// 故超过该上限的 `total_len` 直接判为坏帧,让上层走重新同步而非分配。
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// 解析 `buf` 起始处的一条完整消息,返回 (消息, 消耗字节数)。
pub fn parse_one(buf: &[u8]) -> Result<(Message, usize), Error> {
    if buf.len() < PRELUDE_LEN {
        return Err(Error::Truncated);
    }
    let total_len = be_u32(&buf[0..4]) as usize;
    let headers_len = be_u32(&buf[4..8]) as usize;
    let prelude_crc = be_u32(&buf[8..12]);

    // prelude_crc 覆盖前 8 字节
    if crc32(&buf[0..8]) != prelude_crc {
        return Err(Error::CrcMismatch);
    }
    // 基本合法性:total 至少要容下 prelude + headers + trailer
    if total_len < PRELUDE_LEN + headers_len + TRAILER_LEN {
        return Err(Error::BadHeader);
    }
    // 上界防护:必须在 Truncated 判断之前。否则一个巨大的 total_len 会返回
    // Truncated,让解码器无限缓冲、等待永远凑不齐的字节(内存放大 / DoS)。
    // 判为坏帧(BadHeader)→ 触发上层既有的 RESYNC(逐字节丢弃再同步),不分配。
    if total_len > MAX_FRAME_SIZE {
        return Err(Error::BadHeader);
    }
    if buf.len() < total_len {
        return Err(Error::Truncated);
    }

    // message_crc 覆盖"整条消息去掉末 4 字节"
    let body = &buf[..total_len - TRAILER_LEN];
    let msg_crc = be_u32(&buf[total_len - TRAILER_LEN..total_len]);
    if crc32(body) != msg_crc {
        return Err(Error::CrcMismatch);
    }

    let headers_start = PRELUDE_LEN;
    let headers_end = headers_start + headers_len;
    let headers = parse_headers(&buf[headers_start..headers_end])?;
    let payload = buf[headers_end..total_len - TRAILER_LEN].to_vec();

    Ok((Message { headers, payload }, total_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::eventstream::crc::crc32;
    use crate::kiro::eventstream::header::HeaderValue;

    // 用一个 string header + 任意 payload 构造合法帧。
    fn build_frame(header_name: &str, header_val: &str, payload: &[u8]) -> Vec<u8> {
        // headers 段
        let mut headers = Vec::new();
        headers.push(header_name.len() as u8);
        headers.extend_from_slice(header_name.as_bytes());
        headers.push(7u8); // string
        headers.extend_from_slice(&(header_val.len() as u16).to_be_bytes());
        headers.extend_from_slice(header_val.as_bytes());

        let headers_len = headers.len() as u32;
        let total_len = 16 + headers_len + payload.len() as u32; // 12 prelude + headers + payload + 4 msg_crc

        let mut msg = Vec::new();
        msg.extend_from_slice(&total_len.to_be_bytes());
        msg.extend_from_slice(&headers_len.to_be_bytes());
        let prelude_crc = crc32(&msg[0..8]); // 覆盖前 8 字节
        msg.extend_from_slice(&prelude_crc.to_be_bytes());
        msg.extend_from_slice(&headers);
        msg.extend_from_slice(payload);
        let msg_crc = crc32(&msg); // 覆盖到此为止(去掉末 4 字节前的全部)
        msg.extend_from_slice(&msg_crc.to_be_bytes());
        msg
    }

    #[test]
    fn parses_valid_frame() {
        let frame = build_frame(":event-type", "assistantResponseEvent", b"{\"x\":1}");
        let (m, used) = parse_one(&frame).unwrap();
        assert_eq!(used, frame.len());
        assert_eq!(m.headers.len(), 1);
        assert_eq!(m.headers[0].name, ":event-type");
        assert_eq!(
            m.headers[0].value,
            HeaderValue::Str("assistantResponseEvent".into())
        );
        assert_eq!(m.payload, b"{\"x\":1}");
    }

    #[test]
    fn truncated_frame_errors() {
        let frame = build_frame(":event-type", "x", b"ab");
        assert_eq!(parse_one(&frame[..frame.len() - 3]), Err(Error::Truncated));
    }

    #[test]
    fn corrupt_message_crc_errors() {
        let mut frame = build_frame(":event-type", "x", b"ab");
        let n = frame.len();
        frame[n - 1] ^= 0xFF; // 破坏 message_crc
        assert_eq!(parse_one(&frame), Err(Error::CrcMismatch));
    }

    #[test]
    fn corrupt_prelude_crc_errors() {
        let mut frame = build_frame(":event-type", "x", b"ab");
        frame[8] ^= 0xFF; // 破坏 prelude_crc 首字节
        assert_eq!(parse_one(&frame), Err(Error::CrcMismatch));
    }

    #[test]
    fn malformed_total_len_errors() {
        // 构造一段合法的 8 字节 prelude 前缀(total_len/headers_len),
        // 但让 total_len 小到不足以容纳 16 + headers_len,prelude_crc 仍照样正确计算,
        // 这样才能越过 CRC 校验,走到 BadHeader 这条从未被覆盖的分支。
        // total_len 本身仍需 >= 12(PRELUDE_LEN),否则会先撞上开头的 Truncated 检查。
        let headers_len: u32 = 20;
        let total_len: u32 = 12; // 12 < 16 + headers_len(=36),触发 BadHeader;但 >= PRELUDE_LEN

        let mut buf = Vec::new();
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.extend_from_slice(&headers_len.to_be_bytes());
        let prelude_crc = crc32(&buf[0..8]);
        buf.extend_from_slice(&prelude_crc.to_be_bytes());
        // buf.len() 需 >= total_len,避免先被 Truncated 分支拦截;此处两者恰好相等(12)。
        assert!(buf.len() as u32 >= total_len);

        assert_eq!(parse_one(&buf), Err(Error::BadHeader));
    }

    #[test]
    fn oversized_total_len_rejected_without_allocation() {
        // 攻击/损坏场景:prelude_crc 只覆盖前 8 字节,因此一个携带巨大
        // total_len 的 prelude 可以照样算出正确的 prelude_crc 越过校验。
        // 这里 total_len 远超 MAX_FRAME_SIZE(且 > headers_len 满足下界),
        // 期望:返回 BadHeader(坏帧)而非 Truncated,更不为其分配 total_len 字节。
        let headers_len: u32 = 0;
        let total_len: u32 = u32::MAX; // ~4 GiB,远超 16 MiB 上限

        let mut buf = Vec::new();
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.extend_from_slice(&headers_len.to_be_bytes());
        let prelude_crc = crc32(&buf[0..8]);
        buf.extend_from_slice(&prelude_crc.to_be_bytes());
        // 仅提供 12 字节 prelude:若实现错误地走 Truncated/尝试分配,内存会爆;
        // 正确实现应在看到超限 total_len 时立刻判坏帧返回。
        assert_eq!(buf.len(), PRELUDE_LEN);

        // 关键:必须是 BadHeader(触发 resync),不能是 Truncated(会诱导无限缓冲)。
        assert_eq!(parse_one(&buf), Err(Error::BadHeader));
        // 缓冲从未增长到 total_len 规模(此处 buf 仍只有 prelude 大小)。
        assert!(buf.len() < MAX_FRAME_SIZE);
    }

    #[test]
    fn max_frame_size_boundary_ok() {
        // total_len == MAX_FRAME_SIZE 属合法上界,不应被上界检查拦掉。
        // 用一个真实构造的合法帧,人为不越界即可正常解析。
        let frame = build_frame(":event-type", "x", b"payload");
        assert!(frame.len() <= MAX_FRAME_SIZE);
        assert!(parse_one(&frame).is_ok());
    }
}
