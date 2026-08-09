//! 服务可服务模型的**唯一目录**。
//!
//! 为什么要有这一份:此前管理接口 `GET /api/admin/models` 回落一份 17 条的静态目录,而三个
//! 协议侧的 `/models` 各自硬编码了一组**互不相同的三条**清单(OpenAI 与 Gemini 甚至含
//! `gpt-5.6-sol`,Anthropic 侧则是三条 claude)。于是"先列模型再按列出的 id 调用"这条
//! 客户端标准流程在本服务上不成立:列出来的只是全部可用模型的一个残缺子集,而且换个协议
//! 列出来的还不一样。
//!
//! 目录内容与 [`crate::kiro::convert::map_model`] 能识别的模型集合保持一致(除路由别名
//! `auto` 外),故"列出来的就是能用的"。改这里即三处协议端点与管理端点同步生效。
//!
//! 注:管理端点在**上游模型并集缓存非空**时优先回并集(那反映各账号档位的真实可用性),
//! 本目录是它的回落项;协议端点则一律用本目录 —— 客户端的模型发现需要稳定、可预期的
//! 结果,不应随缓存冷热在重启前后变来变去,也不该为一次 `/models` 去打上游。

/// 目录条目:模型 id、展示名、上下文上限(token)。
pub struct CatalogEntry {
    pub id: &'static str,
    pub display_name: &'static str,
    pub max_tokens: u32,
}

/// 全量目录(顺序即各端点的输出顺序)。
pub const CATALOG: &[CatalogEntry] = &[
    e("claude-sonnet-4.5", "Claude Sonnet 4.5", 200_000),
    e("claude-sonnet-4.6", "Claude Sonnet 4.6", 200_000),
    e("claude-sonnet-5", "Claude Sonnet 5", 200_000),
    e("claude-opus-4.5", "Claude Opus 4.5", 200_000),
    e("claude-opus-4.6", "Claude Opus 4.6", 200_000),
    e("claude-opus-4.7", "Claude Opus 4.7", 200_000),
    e("claude-opus-4.8", "Claude Opus 4.8", 200_000),
    e("claude-opus-5", "Claude Opus 5", 200_000),
    e("claude-haiku-4.5", "Claude Haiku 4.5", 200_000),
    e("claude-fable-5", "Claude Fable 5", 200_000),
    e("deepseek-3.2", "DeepSeek 3.2", 128_000),
    e("glm-5", "GLM-5", 128_000),
    e("qwen3-coder-next", "Qwen3 Coder Next", 256_000),
    e("minimax-m2.1", "MiniMax M2.1", 192_000),
    e("minimax-m2.5", "MiniMax M2.5", 192_000),
    e("gpt-5.6-terra", "GPT-5.6 Terra", 400_000),
    e("gpt-5.6-luna", "GPT-5.6 Luna", 400_000),
    e("gpt-5.6-sol", "GPT-5.6 Sol", 128_000),
];

const fn e(id: &'static str, display_name: &'static str, max_tokens: u32) -> CatalogEntry {
    CatalogEntry {
        id,
        display_name,
        max_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 目录里的每个 id 都必须能被 `map_model` 识别 —— 否则 `/models` 会列出一个调用即 400
    /// 的模型,"列出来的就是能用的"这条契约当场破产。
    #[test]
    fn every_catalogued_id_is_actually_servable() {
        for entry in CATALOG {
            assert!(
                crate::kiro::convert::map_model(entry.id).is_some(),
                "目录里的 {} 无法被 map_model 识别,调用它会直接 400",
                entry.id
            );
        }
    }

    /// 三个协议的 `/models` 必须列出**同一批** id。
    ///
    /// 修复前它们各自硬编码三条且互不相同(OpenAI 含 `gpt-5.6-sol`、Anthropic 只有三条
    /// claude、Gemini 又是另一组),客户端换个协议就看到不一样的"可用模型"。
    #[tokio::test]
    async fn all_three_protocol_model_lists_agree_with_the_catalog() {
        let expected: Vec<String> = CATALOG.iter().map(|e| e.id.to_string()).collect();

        let axum::Json(openai) = crate::protocol::openai::handler::models().await;
        let got: Vec<String> = openai.data.iter().map(|m| m.id.clone()).collect();
        assert_eq!(got, expected, "OpenAI /v1/models 应与目录一致");

        let axum::Json(anthropic) = crate::protocol::anthropic::handler::anthropic_models().await;
        let got: Vec<String> = anthropic.data.iter().map(|m| m.id.clone()).collect();
        assert_eq!(got, expected, "Anthropic /claude/v1/models 应与目录一致");

        let axum::Json(gemini) = crate::protocol::gemini::handler::models().await;
        let got: Vec<String> = gemini
            .models
            .iter()
            .map(|m| m.name.trim_start_matches("models/").to_string())
            .collect();
        assert_eq!(got, expected, "Gemini /v1beta/models 应与目录一致");
    }

    /// id 不得重复(重复会让客户端的模型下拉出现两条一样的)。
    #[test]
    fn catalog_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for entry in CATALOG {
            assert!(seen.insert(entry.id), "目录里 {} 重复", entry.id);
        }
    }
}
