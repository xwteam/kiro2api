//! 增量、容错的事件流解码器(自行设计)。
//! 思路:维护一个滑动字节缓冲,反复对缓冲头部尝试解析一条完整帧:
//!   - 成功 → 产出消息,缓冲头前移 total_len;
//!   - Truncated → 数据还不够,等下次 push;
//!   - CrcMismatch/BadHeader → 判定缓冲头 1 字节为噪声,丢弃它并重试(逐字节再同步),
//!     累计坏帧计数,超过自设上限则清空缓冲防呆。
use super::Error;
use super::frame::{MAX_FRAME_SIZE, Message, parse_one};

/// 连续坏字节的容忍上限(自设):超过则清空缓冲,避免无限累积。
const RESYNC_BUDGET: usize = 4096;

/// 内部缓冲上限(自设)。任何单帧都 <= `MAX_FRAME_SIZE`(见 frame.rs),因此
/// 缓冲无需超过"一帧上限 + 一次 push 的余量"。给一帧上限留出宽裕余量后仍
/// 超过该界,说明是无法解析的噪声/攻击流,直接清空缓冲防止内存放大 / DoS。
const MAX_BUFFER_SIZE: usize = MAX_FRAME_SIZE + 64 * 1024;

#[derive(Default)]
pub struct StreamDecoder {
    buf: Vec<u8>,
    errors: u32,
    dropped_since_progress: usize,
}

impl StreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入新到达的字节。
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// 取出当前缓冲里能解析出的所有完整消息(必要时跳过噪声重新同步)。
    pub fn drain(&mut self) -> Vec<Message> {
        let mut out = Vec::new();
        let mut start = 0usize;
        loop {
            match parse_one(&self.buf[start..]) {
                Ok((msg, used)) => {
                    out.push(msg);
                    start += used;
                    self.dropped_since_progress = 0;
                }
                Err(Error::Truncated) => break, // 等更多字节
                Err(_) => {
                    // 头部 1 字节判为噪声,丢弃后重试(逐字节再同步)。
                    if start < self.buf.len() {
                        start += 1;
                        self.errors = self.errors.saturating_add(1);
                        self.dropped_since_progress += 1;
                        if self.dropped_since_progress >= RESYNC_BUDGET {
                            // 防呆:噪声太多,放弃当前缓冲。
                            self.buf.clear();
                            self.dropped_since_progress = 0;
                            return out;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        // 丢弃已消费/已跳过的前缀。
        if start > 0 {
            self.buf.drain(0..start);
        }
        // 缓冲上限防护:走到这里剩余缓冲要么是"某帧的合法前缀(Truncated,等更多字节)",
        // 要么是"末尾一小段无法判定的噪声"。任何合法帧都 <= MAX_FRAME_SIZE,因此残留缓冲
        // 一旦超过 MAX_BUFFER_SIZE,就不可能再凑成合法帧(纯属噪声/攻击流),清空防呆,
        // 避免内存无限增长(DoS)。
        if self.buf.len() > MAX_BUFFER_SIZE {
            self.buf.clear();
            self.dropped_since_progress = 0;
            self.errors = self.errors.saturating_add(1);
        }
        out
    }

    /// 累计跳过的坏字节/坏帧次数。
    pub fn errors(&self) -> u32 {
        self.errors
    }

    /// 仅测试:当前内部缓冲字节数,用于断言缓冲未被诱导膨胀。
    #[cfg(test)]
    pub(crate) fn buf_len_for_test(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::eventstream::crc::crc32;

    fn frame(payload: &[u8]) -> Vec<u8> {
        // 最小合法帧:一个 string header :t=e + payload
        let mut headers = Vec::new();
        headers.push(2u8);
        headers.extend_from_slice(b":t");
        headers.push(7u8);
        headers.extend_from_slice(&1u16.to_be_bytes());
        headers.extend_from_slice(b"e");
        let hlen = headers.len() as u32;
        let total = 16 + hlen + payload.len() as u32;
        let mut m = Vec::new();
        m.extend_from_slice(&total.to_be_bytes());
        m.extend_from_slice(&hlen.to_be_bytes());
        let pc = crc32(&m[0..8]);
        m.extend_from_slice(&pc.to_be_bytes());
        m.extend_from_slice(&headers);
        m.extend_from_slice(payload);
        let mc = crc32(&m);
        m.extend_from_slice(&mc.to_be_bytes());
        m
    }

    #[test]
    fn two_concatenated_frames() {
        let mut d = StreamDecoder::new();
        let mut buf = frame(b"AA");
        buf.extend_from_slice(&frame(b"BBB"));
        d.push(&buf);
        let msgs = d.drain();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].payload, b"AA");
        assert_eq!(msgs[1].payload, b"BBB");
    }

    #[test]
    fn reassembles_across_pushes() {
        let mut d = StreamDecoder::new();
        let f = frame(b"hello");
        let (a, b) = f.split_at(7); // 半帧
        d.push(a);
        assert_eq!(d.drain().len(), 0); // 还不完整
        d.push(b);
        let msgs = d.drain();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, b"hello");
    }

    #[test]
    fn recovers_after_garbage_prefix() {
        let mut d = StreamDecoder::new();
        let mut buf = vec![0xFF, 0xFF, 0xFF]; // 前缀垃圾(prelude CRC 不符)
        buf.extend_from_slice(&frame(b"ok"));
        d.push(&buf);
        let msgs = d.drain();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, b"ok");
        assert!(d.errors() >= 1);
    }

    // 构造一个"prelude_crc 合法但 total_len 巨大"的 12 字节 prelude。
    // prelude_crc 只覆盖前 8 字节,故这种损坏/攻击 prelude 能越过 CRC 校验。
    fn oversized_prelude(total_len: u32, headers_len: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.extend_from_slice(&headers_len.to_be_bytes());
        let pc = crc32(&buf[0..8]);
        buf.extend_from_slice(&pc.to_be_bytes());
        buf
    }

    #[test]
    fn huge_total_len_does_not_allocate_and_resyncs() {
        // 攻击场景:声称 total_len ≈ 4 GiB。若解码器把它当 Truncated 处理,
        // 就会无限等待/缓冲直到凑满 total_len 字节 → 内存放大 / DoS。
        // 期望:被判坏帧走 resync,缓冲不膨胀,且后面拼接的合法帧仍能解析出来。
        let mut d = StreamDecoder::new();
        let mut buf = oversized_prelude(u32::MAX, 0);
        buf.extend_from_slice(&frame(b"ok")); // 噪声之后紧跟一条合法帧
        d.push(&buf);
        let msgs = d.drain();

        // 合法帧被找回(逐字节 resync 越过坏 prelude)。
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, b"ok");
        assert!(d.errors() >= 1);
        // 从未为那个巨大的 total_len 分配缓冲:残留缓冲远小于上限。
        assert!(d.buf_len_for_test() < 64 * 1024);
    }

    #[test]
    fn oversized_buffer_is_capped_and_cleared() {
        // 直接喂入一大段无法解析的字节(远超单帧上限),验证缓冲不会无限增长:
        // drain 后应被清空(MAX_BUFFER_SIZE 防呆),而不是一直驻留内存。
        let mut d = StreamDecoder::new();
        // 用重复的合法 prelude_crc 的坏 prelude 拼成超大噪声,确保不会意外解析成帧。
        let chunk = oversized_prelude(u32::MAX, 0);
        let mut big = Vec::new();
        while big.len() <= MAX_BUFFER_SIZE {
            big.extend_from_slice(&chunk);
        }
        d.push(&big);
        let _ = d.drain();
        // 超限噪声被清空,缓冲归零。
        assert_eq!(d.buf_len_for_test(), 0);
    }
}
