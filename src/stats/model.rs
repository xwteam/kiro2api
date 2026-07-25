//! 统计层共享数据模型。全部为本项目自写,持久化为 JSON(内部字段 snake_case,
//! 磁盘落盘沿用 serde 默认 snake_case;面向前端的 camelCase 转换在 admin handler
//! 完成,本层只管存与取)。
//!
//! 时间:内部一律以 unix 秒(`created_at_unix`)存储,避免落盘依赖系统时钟解析;
//! 需要 RFC3339/日期分桶时由查询侧按注入时钟或固定偏移换算(见 `daykey`)。

use serde::{Deserialize, Serialize};

/// CST(UTC+8)每秒对应的秒偏移。
pub const CST_OFFSET_SECS: i64 = 8 * 3600;

/// 单条用量记录(每次成功中转落一条)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    /// 归属凭据的数值 id(与 credentials.json 的整数 id 对齐)。
    pub credential_id: u32,
    pub model: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    /// 估算成本(USD)。
    pub estimated_cost: f64,
    /// Phase 2 占位:积分消耗。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits_used: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<i32>,
    /// 记录时刻(unix 秒;落盘即此整数,不落 RFC3339 字符串)。
    pub created_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,
    /// 归属 API-KEY 的数值 id(P2)。0 = 全局 key 或开放模式(无 store key 归属)。
    /// 落盘沿用 snake_case;供按 api_key 过滤/聚合/重置用量。
    #[serde(default)]
    pub api_key_id: u32,
    /// 本次请求端到端延迟(毫秒):从进入 relay 内核到上游响应就绪(选账号+跨账号重试+调上游)。
    /// 由 relay 侧 `started.elapsed()` 计得并透传(与每请求 INFO 日志的 `latency_ms` 同源)。
    /// `None` = 未记录延迟的历史记录(旧数据向后兼容);供 usage-summary 计 `avgLatencyMs`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

/// 401/403 失败事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureEvent {
    pub credential_id: u32,
    /// "api" 或 "mcp"。
    pub request_type: String,
    /// 401 或 403。
    pub status_code: u16,
    /// 响应体(截断到 2000 字符)。
    pub response_body: String,
    pub created_at_unix: i64,
}

/// 429 限流事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThrottleEvent {
    pub credential_id: u32,
    pub request_type: String,
    /// 恒为 429。
    pub status_code: u16,
    /// 响应体(截断到 200 字符)。
    pub response_body: String,
    pub created_at_unix: i64,
}

/// 按字符(非字节)截断字符串到 `max` 长度,避免切断多字节码点。
pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => s[..byte_idx].to_string(),
        None => s.to_string(),
    }
}

/// unix 秒 → CST(UTC+8)日期键 "YYYY-MM-DD"。
///
/// 用固定偏移把 unix 秒平移到 CST 后按 86400 取整算历日,不引入时区库依赖以外的
/// 复杂度,且午夜边界行为可被单测钉死。
pub fn cst_daykey(created_at_unix: i64) -> String {
    let shifted = created_at_unix + CST_OFFSET_SECS;
    // chrono 的 from_timestamp 以 UTC 解释;这里把"CST 墙钟"当成 UTC 秒喂进去,
    // 取到的 y/m/d 即 CST 历日。
    match chrono::DateTime::from_timestamp(shifted, 0) {
        Some(dt) => dt.format("%Y-%m-%d").to_string(),
        None => "1970-01-01".to_string(),
    }
}

/// unix 秒 → RFC3339(UTC、秒精度、Z 后缀)。查询侧对外输出时间戳用。
pub fn unix_to_rfc3339(created_at_unix: i64) -> String {
    chrono::DateTime::from_timestamp(created_at_unix, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_respects_multibyte() {
        assert_eq!(truncate_chars("hello", 3), "hel");
        assert_eq!(truncate_chars("hello", 10), "hello");
        // 4 个中文字 → 截到 2 个,不 panic 也不切半个码点
        assert_eq!(truncate_chars("限流很烦", 2), "限流");
    }

    #[test]
    fn cst_daykey_midnight_boundary() {
        // 2026-06-26T16:00:00Z == 2026-06-27T00:00:00 CST → 属于 27 日
        let at_utc = chrono::DateTime::parse_from_rfc3339("2026-06-26T16:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(cst_daykey(at_utc), "2026-06-27");
        // 差一秒(15:59:59Z == 23:59:59 CST)仍属于 26 日
        assert_eq!(cst_daykey(at_utc - 1), "2026-06-26");
    }

    #[test]
    fn cst_daykey_noon_utc_same_day() {
        // 2026-06-26T04:00:00Z == 12:00 CST → 26 日
        let at = chrono::DateTime::parse_from_rfc3339("2026-06-26T04:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(cst_daykey(at), "2026-06-26");
    }

    #[test]
    fn rfc3339_has_z_suffix() {
        let s = unix_to_rfc3339(1_784_462_400);
        assert!(s.ends_with('Z'), "{s}");
        assert_eq!(s, "2026-07-19T12:00:00Z");
    }
}
