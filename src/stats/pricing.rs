//! 模型定价与单次请求估算成本(USD)。
//!
//! 口径:Anthropic 公开定价(200K context 标准档),按模型名前缀分档。estimated_cost 为
//! **USD 等值基线**——无论上游是否发 meteringEvent(真实 credits),每条用量都能给出一个
//! 可汇总的美元度量,填充 `UsageRecord.estimated_cost` 与日报/汇总的 `total_cost`。
//!
//! 注意:本中转当前非流式解码 `input_tokens=0`(占位)、`output_tokens` 为字符数/4 估算,
//! 故 estimated_cost 目前只反映 output 侧、且为估算值(input 侧待后续能取到真实 input 后补齐)。
//! 定价表本身与真实 token 数一一对应,先把字段接通,精度随 token 计量改进而提升。

/// 模型定价(每百万 tokens,美元)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPricing {
    /// 输入每百万 tokens 成本。
    pub input_per_mtok: f64,
    /// 输出每百万 tokens 成本。
    pub output_per_mtok: f64,
}

/// 按模型名(小写、前缀/包含匹配)取定价。
pub fn get_model_pricing(model: &str) -> ModelPricing {
    let m = model.to_lowercase();
    if m.contains("opus") {
        // Opus 4.5+:$5 / $25。
        ModelPricing {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
        }
    } else if m.contains("haiku") {
        // Haiku 4.5:$1 / $5。
        ModelPricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
        }
    } else {
        // Sonnet 4.x / Sonnet 5 默认:$3 / $15。
        ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
        }
    }
}

/// 单次请求的估算 USD 成本 = input×单价 + output×单价(均按每百万 tokens)。
pub fn calculate_cost(model: &str, input_tokens: i32, output_tokens: i32) -> f64 {
    let pricing = get_model_pricing(model);
    let input_cost = (input_tokens.max(0) as f64) * pricing.input_per_mtok / 1_000_000.0;
    let output_cost = (output_tokens.max(0) as f64) * pricing.output_per_mtok / 1_000_000.0;
    input_cost + output_cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_haiku_sonnet_tiers() {
        assert_eq!(
            get_model_pricing("claude-opus-4.6"),
            ModelPricing {
                input_per_mtok: 5.0,
                output_per_mtok: 25.0
            }
        );
        assert_eq!(
            get_model_pricing("claude-haiku-4.5"),
            ModelPricing {
                input_per_mtok: 1.0,
                output_per_mtok: 5.0
            }
        );
        assert_eq!(
            get_model_pricing("claude-sonnet-4.5"),
            ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0
            }
        );
        // 未知 → 默认 sonnet 档
        assert_eq!(
            get_model_pricing("something-else"),
            ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0
            }
        );
        // 大小写不敏感
        assert_eq!(
            get_model_pricing("CLAUDE-OPUS-4.8"),
            ModelPricing {
                input_per_mtok: 5.0,
                output_per_mtok: 25.0
            }
        );
    }

    #[test]
    fn cost_is_input_plus_output() {
        // sonnet: 1M input @ $3 + 1M output @ $15 = $18
        let c = calculate_cost("claude-sonnet-4.5", 1_000_000, 1_000_000);
        assert!((c - 18.0).abs() < 1e-9, "got {c}");
        // opus output-only(本中转当前 input=0 的典型场景):40000 output @ $25/M = $1.0
        let out_only = calculate_cost("claude-opus-4.6", 0, 40_000);
        assert!((out_only - 1.0).abs() < 1e-9, "got {out_only}");
    }

    #[test]
    fn zero_tokens_is_zero_cost() {
        assert_eq!(calculate_cost("claude-sonnet-4.5", 0, 0), 0.0);
    }

    #[test]
    fn negative_tokens_clamped_to_zero() {
        assert_eq!(calculate_cost("claude-sonnet-4.5", -5, -5), 0.0);
    }
}
