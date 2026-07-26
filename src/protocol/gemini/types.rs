//! Gemini(Google Generative Language v1beta)请求/响应/models DTO。
//!
//! 净室实现:形状照 Google Generative Language v1beta 公开规范(线上 JSON 字段名/结构)
//! 由本仓从公开文档重新推导实现,不含任何第三方专有代码。
//!
//! 注意:Gemini 官方 SDK 对未识别/大小写不符的键会**静默丢弃**,故本模块所有结构体
//! 一律 `#[serde(rename_all = "camelCase")]`,漏改一处即是真 bug。
use serde::{Deserialize, Serialize};

/// `generateContent` 请求体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentRequest {
    pub contents: Vec<Content>,
    #[serde(default)]
    pub system_instruction: Option<Content>,
    #[serde(default)]
    pub tools: Option<Vec<GeminiTool>>,
    #[serde(default)]
    pub tool_config: Option<serde_json::Value>,
    #[serde(default)]
    pub generation_config: Option<GenerationConfig>,
}

/// 一轮对话内容("user"/"model";systemInstruction 的 content role 可空)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    #[serde(default)]
    pub role: Option<String>,
    pub parts: Vec<Part>,
}

/// 一个 Part 只填其一;用扁平 struct 而非 enum 以容忍未知字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<InlineData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
}

/// 内联二进制数据(base64)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineData {
    pub mime_type: String,
    pub data: String,
}

/// 模型发起的函数调用。
///
/// `id` 是可选字段:并行调用同名函数时,客户端靠它把 `functionResponse` 对回具体某次调用。
/// 客户端不带时由转换层生成确定性 id(见 `convert::ToolIdAlloc`),不回落成函数名本身。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// 客户端回填的函数执行结果。
///
/// `id` 语义同 [`FunctionCall::id`]:带了就用它对回调用,缺失时按同名出现序号配对。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub response: serde_json::Value,
}

/// 工具定义(照 Gemini 公开规范)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiTool {
    pub function_declarations: Vec<FunctionDeclaration>,
}

/// 单个函数声明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDeclaration {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

/// 生成参数(MVP:其它字段忽略)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

/// `generateContent` 非流响应体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    pub candidates: Vec<Candidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<UsageMetadata>,
}

/// 单个候选回复。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub content: Content,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    pub index: u32,
}

/// token 用量统计。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    pub prompt_token_count: u32,
    pub candidates_token_count: u32,
    pub total_token_count: u32,
}

/// `models.list` 响应体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiModelList {
    pub models: Vec<GeminiModel>,
}

/// 单个模型描述(`name` 如 `models/claude-sonnet-4.5`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiModel {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub supported_generation_methods: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- 解析(camelCase 线格式 → 结构体)---

    #[test]
    fn parses_simple_contents_request() {
        let raw = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let req: GenerateContentRequest = serde_json::from_str(raw).expect("解析失败");
        assert_eq!(req.contents.len(), 1);
        assert_eq!(req.contents[0].role.as_deref(), Some("user"));
        assert_eq!(req.contents[0].parts[0].text.as_deref(), Some("hi"));
    }

    #[test]
    fn parses_request_with_system_instruction() {
        let raw = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"systemInstruction":{"parts":[{"text":"s"}]}}"#;
        let req: GenerateContentRequest = serde_json::from_str(raw).expect("解析失败");
        let sys = req.system_instruction.expect("systemInstruction 应存在");
        assert_eq!(sys.role, None);
        assert_eq!(sys.parts[0].text.as_deref(), Some("s"));
    }

    #[test]
    fn parses_inline_data_part() {
        let raw = r#"{"inlineData":{"mimeType":"image/png","data":"AAAA"}}"#;
        let part: Part = serde_json::from_str(raw).expect("解析失败");
        let inline = part.inline_data.expect("inlineData 应存在");
        assert_eq!(inline.mime_type, "image/png");
        assert_eq!(inline.data, "AAAA");
    }

    #[test]
    fn parses_function_response_part() {
        let raw = r#"{"functionResponse":{"name":"get_weather","response":{"temp":20}}}"#;
        let part: Part = serde_json::from_str(raw).expect("解析失败");
        let fr = part.function_response.expect("functionResponse 应存在");
        assert_eq!(fr.id, None);
        assert_eq!(fr.name, "get_weather");
        assert_eq!(fr.response["temp"], 20);
    }

    #[test]
    fn parses_function_call_and_response_ids() {
        let raw = r#"{"functionCall":{"id":"call-1","name":"get_weather","args":{}}}"#;
        let part: Part = serde_json::from_str(raw).expect("解析失败");
        let fc = part.function_call.expect("functionCall 应存在");
        assert_eq!(fc.id.as_deref(), Some("call-1"));

        let raw = r#"{"functionResponse":{"id":"call-1","name":"get_weather","response":{}}}"#;
        let part: Part = serde_json::from_str(raw).expect("解析失败");
        let fr = part.function_response.expect("functionResponse 应存在");
        assert_eq!(fr.id.as_deref(), Some("call-1"));
    }

    #[test]
    fn parses_tools_with_function_declarations() {
        let raw = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"functionDeclarations":[{"name":"f","parameters":{"type":"object"}}]}]}"#;
        let req: GenerateContentRequest = serde_json::from_str(raw).expect("解析失败");
        let tools = req.tools.expect("tools 应存在");
        assert_eq!(tools[0].function_declarations[0].name, "f");
        assert_eq!(
            tools[0].function_declarations[0].parameters["type"],
            "object"
        );
    }

    // --- 序列化(结构体 → camelCase 线格式)---

    #[test]
    fn serializes_generate_content_response_camel_case() {
        let resp = GenerateContentResponse {
            candidates: vec![Candidate {
                content: Content {
                    role: Some("model".to_string()),
                    parts: vec![Part {
                        text: Some("hi".to_string()),
                        inline_data: None,
                        function_call: None,
                        function_response: None,
                    }],
                },
                finish_reason: Some("STOP".to_string()),
                index: 0,
            }],
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 1,
                candidates_token_count: 2,
                total_token_count: 3,
            }),
        };
        let v = serde_json::to_value(&resp).expect("序列化失败");
        assert_eq!(v["candidates"][0]["content"]["parts"][0]["text"], "hi");
        assert_eq!(v["candidates"][0]["finishReason"], "STOP");
        assert_eq!(v["usageMetadata"]["totalTokenCount"], 3);

        let s = serde_json::to_string(&resp).expect("序列化失败");
        assert!(!s.contains("finish_reason"));
        assert!(!s.contains("usage_metadata"));
        assert!(!s.contains("inline_data"));
    }

    #[test]
    fn serializes_function_call_part_camel_case() {
        let part = Part {
            text: None,
            inline_data: None,
            function_call: Some(FunctionCall {
                id: Some("tu1".to_string()),
                name: "get_weather".to_string(),
                args: serde_json::json!({"city": "Paris"}),
            }),
            function_response: None,
        };
        let v = serde_json::to_value(&part).expect("序列化失败");
        assert_eq!(v["functionCall"]["id"], "tu1");
        assert_eq!(v["functionCall"]["name"], "get_weather");
        assert_eq!(v["functionCall"]["args"]["city"], "Paris");

        let s = serde_json::to_string(&part).expect("序列化失败");
        assert!(!s.contains("function_call"));
        assert!(!s.contains("inline_data"));
    }

    /// `id` 为 `None` 时不进线格式(老客户端看不到多余字段)。
    #[test]
    fn omits_function_call_id_when_absent() {
        let part = Part {
            text: None,
            inline_data: None,
            function_call: Some(FunctionCall {
                id: None,
                name: "f".to_string(),
                args: serde_json::json!({}),
            }),
            function_response: None,
        };
        let s = serde_json::to_string(&part).expect("序列化失败");
        assert!(
            !s.contains("\"id\""),
            "id 为 None 不应出现在线格式;实际:{s}"
        );
    }

    #[test]
    fn serializes_gemini_model_list_camel_case() {
        let list = GeminiModelList {
            models: vec![GeminiModel {
                name: "models/claude-sonnet-4.5".to_string(),
                display_name: Some("Claude Sonnet 4.5".to_string()),
                supported_generation_methods: vec!["generateContent".to_string()],
            }],
        };
        let v = serde_json::to_value(&list).expect("序列化失败");
        assert_eq!(v["models"][0]["name"], "models/claude-sonnet-4.5");
        assert_eq!(
            v["models"][0]["supportedGenerationMethods"][0],
            "generateContent"
        );

        let s = serde_json::to_string(&list).expect("序列化失败");
        assert!(!s.contains("display_name"));
        assert!(!s.contains("supported_generation_methods"));
    }
}
