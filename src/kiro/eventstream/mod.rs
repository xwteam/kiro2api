//! AWS 事件流(application/vnd.amazon.eventstream)解码器。
//! 帧/头/CRC 均按该公开 wire 规范实现。
pub mod crc;
pub mod decoder;
pub mod frame;
pub mod header;

/// 解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 数据不足以构成一个完整单元(需要更多字节)。
    Truncated,
    /// CRC 校验不匹配(prelude 或整条消息)。
    CrcMismatch,
    /// header 段格式非法(长度越界、未知 value_type、非法 UTF-8 等)。
    BadHeader,
}
