//! AWS 事件流 header 段解析(公开 wire 规范)。
use super::Error;

/// 单个 header 的取值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderValue {
    BoolTrue,
    BoolFalse,
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Bytes(Vec<u8>),
    Str(String),
    Timestamp(i64),
    Uuid([u8; 16]),
}

/// 一个 header:名字 + 取值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: HeaderValue,
}

/// 带边界检查的字节游标:按需取定长切片/整数,越界返回 `Error::Truncated`。
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let s = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
}

fn read_value(c: &mut Cursor, value_type: u8) -> Result<HeaderValue, Error> {
    Ok(match value_type {
        0 => HeaderValue::BoolTrue,
        1 => HeaderValue::BoolFalse,
        2 => HeaderValue::Byte(c.u8()? as i8),
        3 => {
            let b = c.take(2)?;
            HeaderValue::Short(i16::from_be_bytes([b[0], b[1]]))
        }
        4 => {
            let b = c.take(4)?;
            HeaderValue::Int(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        }
        5 => {
            let b = c.take(8)?;
            HeaderValue::Long(i64::from_be_bytes(
                b.try_into().map_err(|_| Error::BadHeader)?,
            ))
        }
        6 => {
            let n = c.u16()? as usize;
            HeaderValue::Bytes(c.take(n)?.to_vec())
        }
        7 => {
            let n = c.u16()? as usize;
            let s = core::str::from_utf8(c.take(n)?).map_err(|_| Error::BadHeader)?;
            HeaderValue::Str(s.to_owned())
        }
        8 => {
            let b = c.take(8)?;
            HeaderValue::Timestamp(i64::from_be_bytes(
                b.try_into().map_err(|_| Error::BadHeader)?,
            ))
        }
        9 => {
            let b = c.take(16)?;
            HeaderValue::Uuid(b.try_into().map_err(|_| Error::BadHeader)?)
        }
        _ => return Err(Error::BadHeader),
    })
}

/// 单帧 header 条数上限(自设)。
///
/// headers 段本身只受 headers_length(u32)约束,而单条 header 最小仅 3 字节
/// (name_len=1 + 1 字节名字 + bool 类型,无取值字节)。逼近单帧上限的 headers 段
/// 因此能展开出数百万个 `Header`,每个都带独立分配的 `String`,线上字节被放大成
/// 数十倍的堆内存(内存放大 / OOM)。AWS 事件流实际用到的 header 数是个位数,
/// 取一个宽松但安全的上限,超限判为坏帧,交由上层重新同步。
pub const MAX_HEADERS: usize = 128;

/// 解析整个 header 段(buf 恰为 headers_length 字节)。
pub fn parse_headers(buf: &[u8]) -> Result<Vec<Header>, Error> {
    let mut c = Cursor::new(buf);
    let mut out = Vec::new();
    while c.pos < buf.len() {
        if out.len() >= MAX_HEADERS {
            return Err(Error::BadHeader);
        }
        let name_len = c.u8()? as usize;
        let name = core::str::from_utf8(c.take(name_len)?)
            .map_err(|_| Error::BadHeader)?
            .to_owned();
        let vt = c.u8()?;
        let value = read_value(&mut c, vt)?;
        out.push(Header { name, value });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 手工拼一个 header 段:`:message-type`(string)= "event"
    fn one_string_header() -> Vec<u8> {
        let name = b":message-type";
        let val = b"event";
        let mut b = Vec::new();
        b.push(name.len() as u8); // name_len
        b.extend_from_slice(name); // name
        b.push(7u8); // value_type = string
        b.extend_from_slice(&(val.len() as u16).to_be_bytes()); // value_len (BE u16)
        b.extend_from_slice(val); // value
        b
    }

    #[test]
    fn parses_single_string_header() {
        let hs = parse_headers(&one_string_header()).unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].name, ":message-type");
        assert_eq!(hs[0].value, HeaderValue::Str("event".into()));
    }

    #[test]
    fn truncated_header_errors() {
        let mut b = one_string_header();
        b.truncate(b.len() - 2); // 砍掉尾部,value 不完整
        assert_eq!(
            parse_headers(&b),
            Err(crate::kiro::eventstream::Error::Truncated)
        );
    }

    #[test]
    fn unknown_value_type_errors() {
        // name_len=1, name="x", value_type=99 非法
        let b = vec![1u8, b'x', 99u8];
        assert_eq!(
            parse_headers(&b),
            Err(crate::kiro::eventstream::Error::BadHeader)
        );
    }

    // 拼 n 条最小 header:name_len=1 + 名字 + BoolTrue(无取值字节),每条 3 字节。
    fn minimal_headers(n: usize) -> Vec<u8> {
        let mut b = Vec::new();
        for _ in 0..n {
            b.push(1u8);
            b.push(b'x');
            b.push(0u8); // value_type = bool true
        }
        b
    }

    #[test]
    fn header_count_at_limit_ok() {
        let hs = parse_headers(&minimal_headers(MAX_HEADERS)).unwrap();
        assert_eq!(hs.len(), MAX_HEADERS);
    }

    #[test]
    fn too_many_headers_errors() {
        // 每条 header 只占 3 字节线上字节却各自分配一个 String,条数不设限时
        // 一个大 headers 段可把内存放大数十倍;超过上限直接判坏帧。
        assert_eq!(
            parse_headers(&minimal_headers(MAX_HEADERS + 1)),
            Err(crate::kiro::eventstream::Error::BadHeader)
        );
    }

    #[test]
    fn parses_all_value_types() {
        // 依次拼出全部 10 种 value_type 的 header,手工拼字节,一次性往返校验。
        fn push_header(b: &mut Vec<u8>, name: &str, value_type: u8, value_bytes: &[u8]) {
            b.push(name.len() as u8);
            b.extend_from_slice(name.as_bytes());
            b.push(value_type);
            b.extend_from_slice(value_bytes);
        }

        let mut b = Vec::new();
        push_header(&mut b, ":t0", 0, &[]); // BoolTrue,无取值字节
        push_header(&mut b, ":t1", 1, &[]); // BoolFalse,无取值字节
        push_header(&mut b, ":t2", 2, &[0xFFu8]); // Byte,1 字节,负数 -1
        push_header(&mut b, ":t3", 3, &(-2i16).to_be_bytes()); // Short,2 字节 BE
        push_header(&mut b, ":t4", 4, &(-70000i32).to_be_bytes()); // Int,4 字节 BE
        push_header(&mut b, ":t5", 5, &(-5_000_000_000i64).to_be_bytes()); // Long,8 字节 BE
        {
            let val = b"xy";
            let mut vb = (val.len() as u16).to_be_bytes().to_vec();
            vb.extend_from_slice(val);
            push_header(&mut b, ":t6", 6, &vb); // Bytes,u16 BE len + 原始字节
        }
        {
            let val = "héllo";
            let mut vb = (val.len() as u16).to_be_bytes().to_vec();
            vb.extend_from_slice(val.as_bytes());
            push_header(&mut b, ":t7", 7, &vb); // Str,u16 BE len + utf8
        }
        push_header(&mut b, ":t8", 8, &1_700_000_000_123i64.to_be_bytes()); // Timestamp,8 字节 BE
        let uuid_bytes: [u8; 16] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10,
        ];
        push_header(&mut b, ":t9", 9, &uuid_bytes); // Uuid,16 字节原始

        let hs = parse_headers(&b).unwrap();
        assert_eq!(hs.len(), 10);
        assert_eq!(hs[0].value, HeaderValue::BoolTrue);
        assert_eq!(hs[1].value, HeaderValue::BoolFalse);
        assert_eq!(hs[2].value, HeaderValue::Byte(-1));
        assert_eq!(hs[3].value, HeaderValue::Short(-2));
        assert_eq!(hs[4].value, HeaderValue::Int(-70000));
        assert_eq!(hs[5].value, HeaderValue::Long(-5_000_000_000));
        assert_eq!(hs[6].value, HeaderValue::Bytes(b"xy".to_vec()));
        assert_eq!(hs[7].value, HeaderValue::Str("héllo".into()));
        assert_eq!(hs[8].value, HeaderValue::Timestamp(1_700_000_000_123));
        assert_eq!(hs[9].value, HeaderValue::Uuid(uuid_bytes));
    }
}
