//! 上游 `ListAvailableModels` 响应的 serde 形状 + 归约成扁平的 [`ModelInfo`]。
//!
//! 字段名为照观测的 wire 事实(camelCase),`#[serde(default)]` 使缺字段也能解析。
//! 归约口径(取 `modelId`/`modelName`/`rateMultiplier`/`tokenLimits.maxOutputTokens`、
//! provider 由 modelId 前缀推断)为本项目自写。

use serde::Deserialize;

/// `ListAvailableModels` 顶层响应。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModelsResponse {
    /// 默认模型(如 `{"modelId":"auto"}`);仅信息性,归约时并入 models。
    #[serde(default)]
    pub default_model: Option<UpstreamModel>,
    /// 可用模型清单。
    #[serde(default)]
    pub models: Vec<UpstreamModel>,
}

/// 上游单个模型条目。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamModel {
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub rate_multiplier: Option<f64>,
    #[serde(default)]
    pub token_limits: Option<TokenLimits>,
}

/// 模型的 token 上限。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenLimits {
    #[serde(default)]
    pub max_input_tokens: Option<u32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

/// 归约后的扁平模型信息(缓存与并集用;不直接对外序列化)。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    /// 规范化(小写、trim)后的模型 id。
    pub id: String,
    /// 展示名;上游无则回落成 id。
    pub display_name: String,
    /// 归属方(由 id 前缀推断)。
    pub owned_by: String,
    /// 最大输出 token;上游无则 0(调用方按需回落)。
    pub max_tokens: u32,
    /// 费率倍率;上游无则 None。
    pub rate_multiplier: Option<f64>,
}

impl AvailableModelsResponse {
    /// 归约成扁平 [`ModelInfo`] 列表:合并 `default_model` 与 `models`,按规范化 id 去重
    /// (先出现者优先),空 id 丢弃。
    pub fn to_model_infos(&self) -> Vec<ModelInfo> {
        let mut out: Vec<ModelInfo> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let all = self.default_model.iter().chain(self.models.iter());
        for m in all {
            let id = normalize_id(&m.model_id);
            if id.is_empty() || !seen.insert(id.clone()) {
                continue;
            }
            let display_name = m
                .model_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone());
            let max_tokens = m
                .token_limits
                .as_ref()
                .and_then(|t| t.max_output_tokens)
                .unwrap_or(0);
            out.push(ModelInfo {
                owned_by: guess_owned_by(&id),
                id,
                display_name,
                max_tokens,
                rate_multiplier: m.rate_multiplier,
            });
        }
        out
    }
}

/// 规范化模型 id:trim + 小写。
pub fn normalize_id(id: &str) -> String {
    id.trim().to_ascii_lowercase()
}

/// 由模型 id 前缀/关键字推断归属方(与展示表格 owned_by 列一致)。
pub fn guess_owned_by(id: &str) -> String {
    let l = id.to_ascii_lowercase();
    if l == "auto" {
        "kiro"
    } else if l.contains("claude") {
        "anthropic"
    } else if l.contains("gpt") {
        "openai"
    } else if l.contains("deepseek") {
        "deepseek"
    } else if l.contains("minimax") {
        "minimax"
    } else if l.contains("glm") {
        "glm"
    } else if l.contains("qwen") {
        "qwen"
    } else {
        "unknown"
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_upstream_shape_and_reduces() {
        let r: AvailableModelsResponse = serde_json::from_str(
            r#"{
                "defaultModel": { "modelId": "auto" },
                "models": [
                    { "modelId": "claude-sonnet-5", "modelName": "Claude Sonnet 5",
                      "rateMultiplier": 1.3,
                      "tokenLimits": { "maxInputTokens": 1000000, "maxOutputTokens": 64000 } },
                    { "modelId": "gpt-5.6-sol", "modelName": "GPT-5.6 Sol" }
                ]
            }"#,
        )
        .unwrap();
        let infos = r.to_model_infos();
        assert_eq!(infos.len(), 3);
        assert_eq!(infos[0].id, "auto");
        assert_eq!(infos[0].owned_by, "kiro");
        assert_eq!(infos[1].id, "claude-sonnet-5");
        assert_eq!(infos[1].owned_by, "anthropic");
        assert_eq!(infos[1].max_tokens, 64000);
        assert_eq!(infos[1].rate_multiplier, Some(1.3));
        assert_eq!(infos[2].owned_by, "openai");
        // 上游无 tokenLimits → max_tokens 回落 0
        assert_eq!(infos[2].max_tokens, 0);
    }

    #[test]
    fn dedupes_by_normalized_id() {
        let r: AvailableModelsResponse = serde_json::from_str(
            r#"{"models":[
                {"modelId":"Claude-Sonnet-5"},
                {"modelId":"claude-sonnet-5"},
                {"modelId":"  "}
            ]}"#,
        )
        .unwrap();
        let infos = r.to_model_infos();
        // 大小写归一后同一 id 只留一个;空 id 丢弃
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, "claude-sonnet-5");
        // 无 display_name → 回落成 id
        assert_eq!(infos[0].display_name, "claude-sonnet-5");
    }

    #[test]
    fn guesses_provider_by_prefix() {
        assert_eq!(guess_owned_by("auto"), "kiro");
        assert_eq!(guess_owned_by("claude-opus-4.8"), "anthropic");
        assert_eq!(guess_owned_by("gpt-5.6-terra"), "openai");
        assert_eq!(guess_owned_by("deepseek-3.2"), "deepseek");
        assert_eq!(guess_owned_by("minimax-m2.5"), "minimax");
        assert_eq!(guess_owned_by("glm-5"), "glm");
        assert_eq!(guess_owned_by("qwen3-coder-next"), "qwen");
        assert_eq!(guess_owned_by("something-else"), "unknown");
    }
}
