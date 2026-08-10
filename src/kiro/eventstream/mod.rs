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
    /// **帧边界已确知**、但帧内数据损坏(prelude CRC 通过,而 message CRC 或 header 段有问题)。
    ///
    /// 与 [`CrcMismatch`](Self::CrcMismatch) / [`BadHeader`](Self::BadHeader) 的区别在于
    /// **要不要逐字节再同步**:prelude CRC 通过意味着 `total_len` 是可信的,整帧跳过即可,
    /// 一步到位落在下一帧的起点。逐字节扫会把这一帧的整段 payload 当噪声重扫一遍,既慢,
    /// 又可能从 payload 字节里凑出一个"看着像 prelude"的假帧头。
    CorruptFrame {
        /// 这一帧的总长度,调用方据此整帧跳过。
        total_len: usize,
    },
}
