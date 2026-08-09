//! Kiro 数据面请求体 DTO(照 Kiro 数据面契约 §2;文本 MVP,代码自写)。
use serde::{Deserialize, Serialize};

/// Kiro 数据面请求体顶层。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroRequest {
    pub conversation_state: ConversationState,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub profile_arn: Option<String>,
}

/// 会话状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationState {
    /// 本轮代理续跑标识。真实客户端每个请求发一个新的 UUID;此前我们**根本不发这个字段**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_continuation_id: Option<String>,
    pub agent_task_type: String,
    pub chat_trigger_type: String,
    pub current_message: CurrentMessage,
    /// 会话标识。**同一次会话内应当保持不变**,故优先取客户端 `metadata.user_id` 里的
    /// session UUID;取不到才新生成。此前是每请求一个新的 32 位无连字符十六进制 ——
    /// 既不是 UUID 形状,也让每个请求看起来都是一段全新对话。
    pub conversation_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub history: Vec<HistoryItem>,
}

/// 当前消息包装。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentMessage {
    pub user_input_message: UserInputMessage,
}

/// 用户输入消息(文本 MVP + 工具/图片)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputMessage {
    pub content: String,
    pub model_id: String,
    pub origin: String,
    pub user_input_message_context: UserInputMessageContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageBlock>>,
}

/// 用户输入消息上下文(照 Kiro 数据面契约/观测:工具定义 + 工具执行结果;均可缺省)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputMessageContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_results: Option<Vec<ToolResultWire>>,
}

/// 单个工具规格包装(照 Kiro 数据面契约/观测)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub tool_specification: ToolSpecInner,
}

/// 工具规格内层。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpecInner {
    pub name: String,
    /// **恒为字符串,绝不为 null。** 此前是 `Option<String>`,客户端没给描述时会序列化成
    /// `"description": null` —— 而真实客户端在这个位置永远是个字符串(没有就是空串)。
    pub description: String,
    pub input_schema: InputSchemaJson,
}

/// 工具 JSON Schema 包装。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSchemaJson {
    pub json: serde_json::Value,
}

/// 单条工具执行结果(照 Kiro 数据面契约/观测)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultWire {
    pub tool_use_id: String,
    pub content: Vec<ToolResultText>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// 工具执行结果的文本内容块。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultText {
    pub text: String,
}

/// 助手回复里的单个工具调用(照 Kiro 数据面契约/观测)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUseWire {
    pub tool_use_id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// 图片内容块(照 Kiro 数据面契约/观测)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageBlock {
    pub format: String,
    pub source: ImageSource,
}

/// 图片来源(base64 字节)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSource {
    pub bytes: String,
}

/// 历史记录条目(untagged,靠内层字段名区分:`userInputMessage` / `assistantResponseMessage`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HistoryItem {
    UserInputMessage {
        #[serde(rename = "userInputMessage")]
        user_input_message: UserInputMessage,
    },
    AssistantResponseMessage {
        #[serde(rename = "assistantResponseMessage")]
        assistant_response_message: AssistantResponseMessage,
    },
}

/// 历史记录中的助手回复消息(文本 MVP + 工具调用)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantResponseMessage {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<Vec<ToolUseWire>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> KiroRequest {
        KiroRequest {
            conversation_state: ConversationState {
                agent_continuation_id: None,
                chat_trigger_type: "MANUAL".to_string(),
                agent_task_type: "vibe".to_string(),
                conversation_id: "conv-1".to_string(),
                current_message: CurrentMessage {
                    user_input_message: UserInputMessage {
                        content: "hi".to_string(),
                        model_id: "claude-sonnet-4.5".to_string(),
                        origin: "AI_EDITOR".to_string(),
                        user_input_message_context: UserInputMessageContext::default(),
                        images: None,
                    },
                },
                history: vec![],
            },
            profile_arn: None,
        }
    }

    #[test]
    fn serializes_camel_case_with_required_fields() {
        let v = serde_json::to_value(sample()).expect("序列化失败");
        assert_eq!(v["conversationState"]["chatTriggerType"], "MANUAL");
        assert_eq!(v["conversationState"]["agentTaskType"], "vibe");
        assert_eq!(v["conversationState"]["conversationId"], "conv-1");
        let uim = &v["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(uim["content"], "hi");
        assert_eq!(uim["modelId"], "claude-sonnet-4.5");
        assert_eq!(uim["origin"], "AI_EDITOR");
        assert_eq!(uim["userInputMessageContext"], serde_json::json!({}));
    }

    #[test]
    fn omits_empty_history_and_absent_profile_arn() {
        let v = serde_json::to_value(sample()).expect("序列化失败");
        assert!(v["conversationState"].get("history").is_none());
        assert!(v.get("profileArn").is_none());
    }

    #[test]
    fn serializes_history_items_untagged() {
        let mut req = sample();
        req.conversation_state.history = vec![
            HistoryItem::UserInputMessage {
                user_input_message: UserInputMessage {
                    content: "hello".to_string(),
                    model_id: "claude-sonnet-4.5".to_string(),
                    origin: "AI_EDITOR".to_string(),
                    user_input_message_context: UserInputMessageContext::default(),
                    images: None,
                },
            },
            HistoryItem::AssistantResponseMessage {
                assistant_response_message: AssistantResponseMessage {
                    content: "world".to_string(),
                    tool_uses: None,
                },
            },
        ];
        let v = serde_json::to_value(&req).expect("序列化失败");
        let history = v["conversationState"]["history"]
            .as_array()
            .expect("history 应为数组");
        assert!(history[0].get("userInputMessage").is_some());
        assert_eq!(history[0]["userInputMessage"]["content"], "hello");
        assert!(history[1].get("assistantResponseMessage").is_some());
        assert_eq!(history[1]["assistantResponseMessage"]["content"], "world");
    }

    #[test]
    fn includes_profile_arn_when_present() {
        let mut req = sample();
        req.profile_arn = Some("arn:aws:codewhisperer:us-east-1:1:profile/x".to_string());
        let v = serde_json::to_value(&req).expect("序列化失败");
        assert_eq!(
            v["profileArn"],
            "arn:aws:codewhisperer:us-east-1:1:profile/x"
        );
    }

    // --- 工具调用 / 图片 wire DTO(照 Kiro 数据面契约/观测)---

    #[test]
    fn empty_user_input_message_context_serializes_to_empty_object() {
        let v = serde_json::to_value(UserInputMessageContext::default()).expect("序列化失败");
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn context_with_tools_serializes_tool_specification() {
        let ctx = UserInputMessageContext {
            tools: Some(vec![ToolSpec {
                tool_specification: ToolSpecInner {
                    name: "get_weather".to_string(),
                    description: "d".to_string(),
                    input_schema: InputSchemaJson {
                        json: serde_json::json!({"type": "object"}),
                    },
                },
            }]),
            tool_results: None,
        };
        let v = serde_json::to_value(&ctx).expect("序列化失败");
        assert_eq!(v["tools"][0]["toolSpecification"]["name"], "get_weather");
        assert_eq!(
            v["tools"][0]["toolSpecification"]["inputSchema"]["json"]["type"],
            "object"
        );
    }

    #[test]
    fn context_with_tool_results_serializes_tool_use_id_content_status() {
        let ctx = UserInputMessageContext {
            tools: None,
            tool_results: Some(vec![ToolResultWire {
                tool_use_id: "tu1".to_string(),
                content: vec![ToolResultText {
                    text: "ok".to_string(),
                }],
                status: "success".to_string(),
                is_error: None,
            }]),
        };
        let v = serde_json::to_value(&ctx).expect("序列化失败");
        assert_eq!(v["toolResults"][0]["toolUseId"], "tu1");
        assert_eq!(v["toolResults"][0]["content"][0]["text"], "ok");
        assert_eq!(v["toolResults"][0]["status"], "success");
        assert!(v["toolResults"][0].get("isError").is_none());
    }

    #[test]
    fn assistant_response_message_serializes_tool_uses() {
        let msg = AssistantResponseMessage {
            content: "x".to_string(),
            tool_uses: Some(vec![ToolUseWire {
                tool_use_id: "tu1".to_string(),
                name: "n".to_string(),
                input: serde_json::json!({}),
            }]),
        };
        let v = serde_json::to_value(&msg).expect("序列化失败");
        assert_eq!(v["toolUses"][0]["toolUseId"], "tu1");
        assert_eq!(v["toolUses"][0]["name"], "n");
    }

    #[test]
    fn user_input_message_serializes_images() {
        let msg = UserInputMessage {
            content: "hi".to_string(),
            model_id: "claude-sonnet-4.5".to_string(),
            origin: "AI_EDITOR".to_string(),
            user_input_message_context: UserInputMessageContext::default(),
            images: Some(vec![ImageBlock {
                format: "png".to_string(),
                source: ImageSource {
                    bytes: "AAAA".to_string(),
                },
            }]),
        };
        let v = serde_json::to_value(&msg).expect("序列化失败");
        assert_eq!(v["images"][0]["format"], "png");
        assert_eq!(v["images"][0]["source"]["bytes"], "AAAA");
    }
}
