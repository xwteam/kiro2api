//! Anthropic `/v1/messages` 流式 SSE 事件构造器(照 Anthropic 公开 Messages streaming 规范自写)。
//! 框架无关:只产出 `event:` 名 + `data:` JSON 串,由 handler 适配到 axum SSE。
use serde_json::json;

/// 一个待发送的 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: &'static str,
    pub data: String,
}

impl SseEvent {
    fn new(event: &'static str, data: serde_json::Value) -> Self {
        Self {
            event,
            data: data.to_string(),
        }
    }
}

/// `message_start`:含空 content 与初始 usage(output_tokens 恒 0)。
pub fn message_start(id: &str, model: &str, input_tokens: u32) -> SseEvent {
    SseEvent::new(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": id, "type": "message", "role": "assistant", "model": model,
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": { "input_tokens": input_tokens, "output_tokens": 0 }
            }
        }),
    )
}

/// `content_block_start`:文本块起始(空文本)。
pub fn content_block_start(index: u32) -> SseEvent {
    SseEvent::new(
        "content_block_start",
        json!({ "type": "content_block_start", "index": index, "content_block": { "type": "text", "text": "" } }),
    )
}

/// `content_block_delta` / `text_delta`:一段文本增量。
pub fn text_delta(index: u32, text: &str) -> SseEvent {
    SseEvent::new(
        "content_block_delta",
        json!({ "type": "content_block_delta", "index": index, "delta": { "type": "text_delta", "text": text } }),
    )
}

/// `content_block_start`:`tool_use` 块起始(带 `id`/`name`,`input` 初始为空对象)。
pub fn tool_use_start(index: u32, id: &str, name: &str) -> SseEvent {
    SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": index,
            "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} }
        }),
    )
}

/// `content_block_delta` / `input_json_delta`:工具入参的一段部分 JSON 文本(累计拼接成完整入参)。
pub fn input_json_delta(index: u32, partial_json: &str) -> SseEvent {
    SseEvent::new(
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": index,
            "delta": { "type": "input_json_delta", "partial_json": partial_json }
        }),
    )
}

/// `content_block_stop`。
pub fn content_block_stop(index: u32) -> SseEvent {
    SseEvent::new(
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": index }),
    )
}

/// `message_delta`:终止原因 + 累计 output_tokens。
pub fn message_delta(stop_reason: &str, output_tokens: u32) -> SseEvent {
    SseEvent::new(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop_reason, "stop_sequence": null },
            "usage": { "output_tokens": output_tokens }
        }),
    )
}

/// `message_stop`。
pub fn message_stop() -> SseEvent {
    SseEvent::new("message_stop", json!({ "type": "message_stop" }))
}

/// `ping`(保活占位,可选发送)。
pub fn ping() -> SseEvent {
    SseEvent::new("ping", json!({ "type": "ping" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(e: &SseEvent) -> serde_json::Value {
        serde_json::from_str(&e.data).expect("data 应为合法 JSON")
    }

    #[test]
    fn message_start_shape() {
        let e = message_start("msg_x", "claude-sonnet-4.5", 7);
        assert_eq!(e.event, "message_start");
        let v = parse(&e);
        assert_eq!(v["type"], "message_start");
        assert_eq!(v["message"]["role"], "assistant");
        assert_eq!(v["message"]["model"], "claude-sonnet-4.5");
        assert_eq!(v["message"]["usage"]["input_tokens"], 7);
        assert!(v["message"]["content"].as_array().unwrap().is_empty());
    }

    #[test]
    fn text_delta_shape() {
        let e = text_delta(0, "hi");
        assert_eq!(e.event, "content_block_delta");
        let v = parse(&e);
        assert_eq!(v["type"], "content_block_delta");
        assert_eq!(v["index"], 0);
        assert_eq!(v["delta"]["type"], "text_delta");
        assert_eq!(v["delta"]["text"], "hi");
    }

    #[test]
    fn tool_use_start_shape() {
        let e = tool_use_start(1, "tu1", "get_weather");
        assert_eq!(e.event, "content_block_start");
        let v = parse(&e);
        assert_eq!(v["type"], "content_block_start");
        assert_eq!(v["index"], 1);
        assert_eq!(v["content_block"]["type"], "tool_use");
        assert_eq!(v["content_block"]["id"], "tu1");
        assert_eq!(v["content_block"]["name"], "get_weather");
        assert!(v["content_block"]["input"].as_object().unwrap().is_empty());
    }

    #[test]
    fn input_json_delta_shape() {
        let e = input_json_delta(1, "{\"ci");
        assert_eq!(e.event, "content_block_delta");
        let v = parse(&e);
        assert_eq!(v["type"], "content_block_delta");
        assert_eq!(v["index"], 1);
        assert_eq!(v["delta"]["type"], "input_json_delta");
        assert_eq!(v["delta"]["partial_json"], "{\"ci");
    }

    #[test]
    fn other_events_shape() {
        assert_eq!(
            parse(&content_block_start(0))["content_block"]["type"],
            "text"
        );
        assert_eq!(parse(&content_block_stop(0))["type"], "content_block_stop");
        let md = parse(&message_delta("end_turn", 3));
        assert_eq!(md["delta"]["stop_reason"], "end_turn");
        assert_eq!(md["usage"]["output_tokens"], 3);
        assert_eq!(parse(&message_stop())["type"], "message_stop");
        assert_eq!(parse(&ping())["type"], "ping");
    }
}
