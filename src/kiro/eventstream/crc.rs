//! CRC-32/ISO-HDLC(反射多项式 0xEDB88320),AWS 事件流帧校验所用。
use crc::{CRC_32_ISO_HDLC, Crc};

const ENGINE: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

/// 计算一段字节的 CRC-32/ISO-HDLC 校验值。
pub fn crc32(data: &[u8]) -> u32 {
    ENGINE.checksum(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_answer_vector() {
        // CRC-32/ISO-HDLC 目录标准 KAT(公开):"123456789" -> 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn crc32_empty_is_zero() {
        assert_eq!(crc32(b""), 0);
    }
}
