use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 统一用量与缓存计算引擎（CanonicalUsage）
/// 权威收拢 Google Gemini 原生 usageMetadata，并提供向四大协议专属格式的标准散开方法
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalUsage {
    /// 全量总上下文输入 (Total Prompt Context = uncached + cached)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_input_tokens: Option<u32>,
    /// 真正未命中的增量输入 (Uncached Input = total_input_tokens - cached_tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncached_input_tokens: Option<u32>,
    /// 命中的缓存 Token (Cached Tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// 模型输出总量 (Output Tokens, 含思维链)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    /// 思考思维链 Token 消耗 (Reasoning Tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    /// 上游实际报告的总消耗；缺失时不由其他字段猜测
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
}

impl CanonicalUsage {
    /// 从 Google upstream 原生 usageMetadata 或包含 usageMetadata 的任意 JSON 中权威抽取收拢
    pub fn from_gemini(raw: &Value) -> Self {
        let usage = raw.get("usageMetadata").unwrap_or(raw);

        let total_input = usage
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("prompt_tokens").and_then(Value::as_u64))
            .or_else(|| usage.get("total_input_tokens").and_then(Value::as_u64))
            .map(|v| v.min(u32::MAX as u64) as u32);
        let native_input = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .map(|v| v.min(u32::MAX as u64) as u32);

        let cached = usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("cachedContentTokenCount").and_then(Value::as_u64))
            .or_else(|| {
                usage
                    .get("cached_content_token_count")
                    .and_then(Value::as_u64)
            })
            .or_else(|| usage.get("total_cached_tokens").and_then(Value::as_u64))
            .or_else(|| usage.get("cached_tokens").and_then(Value::as_u64))
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
            .map(|v| v.min(u32::MAX as u64) as u32);

        let reasoning = usage
            .get("total_thought_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("totalThoughtTokens").and_then(Value::as_u64))
            .or_else(|| usage.get("thoughtsTokenCount").and_then(Value::as_u64))
            .or_else(|| {
                usage
                    .get("completion_tokens_details")
                    .and_then(|d| d.get("reasoning_tokens"))
                    .and_then(Value::as_u64)
            })
            .or_else(|| {
                usage
                    .get("output_tokens_details")
                    .and_then(|d| d.get("reasoning_tokens"))
                    .and_then(Value::as_u64)
            })
            .or_else(|| {
                usage
                    .get("output_tokens_details")
                    .and_then(|d| d.get("thinking_tokens"))
                    .and_then(Value::as_u64)
            })
            .map(|v| v.min(u32::MAX as u64) as u32)
            .or_else(|| {
                // Some Gemini variants report thoughts only as modality details.
                // A numeric scalar above, including zero, remains authoritative.
                usage
                    .get("candidatesTokensDetails")?
                    .as_array()?
                    .iter()
                    .filter(|detail| {
                        detail
                            .get("modality")
                            .and_then(Value::as_str)
                            .is_some_and(|modality| {
                                modality.eq_ignore_ascii_case("THINKING")
                                    || modality.eq_ignore_ascii_case("REASONING")
                            })
                    })
                    .filter_map(|detail| detail.get("tokenCount").and_then(Value::as_u64))
                    .map(|count| count.min(u32::MAX as u64) as u32)
                    .reduce(u32::saturating_add)
            });

        let raw_output = usage
            .get("total_output_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("candidatesTokenCount").and_then(Value::as_u64))
            .or_else(|| usage.get("candidates_token_count").and_then(Value::as_u64))
            .or_else(|| usage.get("completion_tokens").and_then(Value::as_u64))
            .or_else(|| usage.get("output_tokens").and_then(Value::as_u64))
            .map(|v| v.min(u32::MAX as u64) as u32);

        // Gemini candidates / Interactions output 不含思考；OpenAI completion / output 已含思考。
        // tool-use prompt 是输入，不能再次加到输出。
        let has_separate_thoughts = [
            "total_output_tokens",
            "candidatesTokenCount",
            "candidates_token_count",
        ]
        .into_iter()
        .any(|field| usage.get(field).and_then(Value::as_u64).is_some());
        let output = if has_separate_thoughts {
            raw_output
                .map(|output| reasoning.map_or(output, |thoughts| output.saturating_add(thoughts)))
        } else {
            raw_output
        };

        // 计算全量与增量
        // 如果输入数据是 Anthropic 格式（raw_input 存的是 uncached），做加和还原全量
        let cache_creation_input_tokens = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .map(|v| v.min(u32::MAX as u64) as u32);
        let cache_read_input_tokens = usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .map(|v| v.min(u32::MAX as u64) as u32);
        let (canonical_total_input, uncached_input) = if let Some(input) = total_input {
            (
                Some(input),
                Some(cached.map_or(input, |cached| input.saturating_sub(cached))),
            )
        } else if cache_read_input_tokens.is_some() || cache_creation_input_tokens.is_some() {
            (
                native_input.map(|input| {
                    [cache_read_input_tokens, cache_creation_input_tokens]
                        .into_iter()
                        .flatten()
                        .fold(input, u32::saturating_add)
                }),
                native_input,
            )
        } else {
            (
                native_input,
                native_input
                    .map(|input| cached.map_or(input, |cached| input.saturating_sub(cached))),
            )
        };

        let total = usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("totalTokenCount").and_then(Value::as_u64))
            .map(|v| v.min(u32::MAX as u64) as u32);

        Self {
            total_input_tokens: canonical_total_input,
            uncached_input_tokens: uncached_input,
            cached_tokens: cached,
            cache_creation_input_tokens,
            output_tokens: output,
            reasoning_tokens: reasoning,
            total_tokens: total,
        }
    }

    /// 缩放算法（对齐 Claude Code 上下文阈值）：超过 30k 且开启 scaling 时按阶梯缩放
    pub fn scale_claude_tokens(total_raw: u32, context_limit: u32) -> u32 {
        if total_raw <= 30_000 {
            return total_raw;
        }
        const TARGET_MAX: f64 = 195_000.0;
        let ratio = total_raw as f64 / context_limit.max(1) as f64;
        if ratio <= 0.5 {
            let display_ratio = ratio * 0.6;
            (display_ratio * TARGET_MAX) as u32
        } else if ratio <= 0.7 {
            let progress = (ratio - 0.5) / 0.2;
            let display_ratio = 0.3 + progress * 0.2;
            (display_ratio * TARGET_MAX) as u32
        } else if ratio <= 0.85 {
            let progress = (ratio - 0.7) / 0.15;
            let display_ratio = 0.5 + progress * 0.2;
            (display_ratio * TARGET_MAX) as u32
        } else {
            let progress = (ratio - 0.85) / 0.15;
            let display_ratio = 0.7 + progress * 0.27;
            (display_ratio.min(0.97) * TARGET_MAX) as u32
        }
    }

    /// 散开为 Anthropic Claude 格式：
    /// Claude 规范要求 input_tokens 仅表达未缓存增量，缓存单列在 cache_read_input_tokens
    pub fn to_claude_usage(&self, scaling_enabled: bool, context_limit: u32) -> Value {
        let (reported_input, reported_cache) = if let Some(total_input) = self
            .total_input_tokens
            .filter(|input| scaling_enabled && *input > 30_000)
        {
            let scaled_total = Self::scale_claude_tokens(total_input, context_limit);
            if let Some(cached) = self.cached_tokens {
                let cache_ratio = (cached as f64) / (total_input as f64);
                let sc_cache = (scaled_total as f64 * cache_ratio) as u32;
                (Some(scaled_total.saturating_sub(sc_cache)), Some(sc_cache))
            } else {
                (Some(scaled_total), None)
            }
        } else {
            (self.uncached_input_tokens, self.cached_tokens)
        };

        json!({
            "input_tokens": reported_input,
            "output_tokens": self.output_tokens,
            "output_tokens_details": self.reasoning_tokens.map(|thinking| json!({"thinking_tokens":thinking})),
            "cache_read_input_tokens": reported_cache,
            "cache_creation_input_tokens": self.cache_creation_input_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modality_thoughts_fallback_respects_zero_and_saturates() {
        let details = json!([
            {"modality":"TEXT","tokenCount":99},
            {"modality":"THINKING","tokenCount":5},
            {"modality":"reasoning","tokenCount":7}
        ]);
        assert_eq!(
            CanonicalUsage::from_gemini(&json!({"candidatesTokensDetails":details}))
                .reasoning_tokens,
            Some(12)
        );
        assert_eq!(
            CanonicalUsage::from_gemini(
                &json!({"thoughtsTokenCount":0,"candidatesTokensDetails":details})
            )
            .reasoning_tokens,
            Some(0)
        );
        assert_eq!(
            CanonicalUsage::from_gemini(&json!({"candidatesTokensDetails":[
                {"modality":"THINKING","tokenCount":u64::MAX},
                {"modality":"REASONING","tokenCount":1}
            ]}))
            .reasoning_tokens,
            Some(u32::MAX)
        );
        assert_eq!(
            CanonicalUsage::from_gemini(&json!({"promptTokenCount":u64::MAX})).total_input_tokens,
            Some(u32::MAX)
        );
    }

    #[test]
    fn reported_components_do_not_invent_missing_usage() {
        for (raw, input, output, total) in [
            (
                json!({"totalTokenCount":123,"cachedContentTokenCount":80}),
                None,
                None,
                Some(123),
            ),
            (
                json!({"promptTokenCount":0,"candidatesTokenCount":0,"totalTokenCount":0}),
                Some(0),
                Some(0),
                Some(0),
            ),
            (json!({"promptTokenCount":100}), Some(100), None, None),
            (json!({"candidatesTokenCount":2}), None, Some(2), None),
            (json!({"input_tokens":0}), Some(0), None, None),
            (
                json!({"promptTokenCount":100,"candidatesTokenCount":2,"totalTokenCount":123}),
                Some(100),
                Some(2),
                Some(123),
            ),
        ] {
            let usage = CanonicalUsage::from_gemini(&raw);
            assert_eq!(usage.total_input_tokens, input);
            assert_eq!(usage.output_tokens, output);
            assert_eq!(usage.total_tokens, total);
        }
    }

    #[test]
    fn numeric_aliases_and_native_anthropic_components_preserve_reported_values() {
        let aliases = CanonicalUsage::from_gemini(&json!({
            "promptTokenCount":null,"input_tokens":null,"prompt_tokens":100,
            "cache_read_input_tokens":null,"cachedContentTokenCount":0,
            "total_thought_tokens":null,"thoughtsTokenCount":3,
            "total_output_tokens":20,"total_tokens":null,"totalTokenCount":123
        }));
        assert_eq!(aliases.total_input_tokens, Some(100));
        assert_eq!(aliases.cached_tokens, Some(0));
        assert_eq!(aliases.reasoning_tokens, Some(3));
        assert_eq!(aliases.output_tokens, Some(23));
        assert_eq!(aliases.total_tokens, Some(123));

        let native = CanonicalUsage::from_gemini(&json!({
            "input_tokens":20,"cache_read_input_tokens":80,
            "cache_creation_input_tokens":10,"output_tokens":2
        }));
        assert_eq!(native.total_input_tokens, Some(110));
        assert_eq!(native.uncached_input_tokens, Some(20));
        assert_eq!(native.total_tokens, None);
        let no_input = CanonicalUsage::from_gemini(&json!({
            "cache_read_input_tokens":80,"cache_creation_input_tokens":10
        }));
        assert_eq!(no_input.total_input_tokens, None);
        assert_eq!(no_input.uncached_input_tokens, None);
        assert_eq!(no_input.total_tokens, None);
    }

    #[test]
    fn test_canonical_usage_from_gemini_and_diffusion() {
        let gemini_raw = json!({
            "usageMetadata": {
                "promptTokenCount": 99000,
                "cachedContentTokenCount": 94000,
                "candidatesTokenCount": 3100,
                "thoughtsTokenCount": 1000,
                "totalTokenCount": 103100,
                "candidatesTokensDetails": [
                    { "modality": "TEXT", "tokenCount": 3100 }
                ]
            }
        });

        let usage = CanonicalUsage::from_gemini(&gemini_raw);
        assert_eq!(usage.total_input_tokens, Some(99000));
        assert_eq!(usage.cached_tokens, Some(94000));
        assert_eq!(usage.uncached_input_tokens, Some(5000));
        assert_eq!(usage.output_tokens, Some(4100));
        assert_eq!(usage.reasoning_tokens, Some(1000));
        assert_eq!(usage.total_tokens, Some(103100));

        // 验证散开到 Claude
        let claude_val = usage.to_claude_usage(false, 200000);
        assert_eq!(claude_val["input_tokens"], 5000);
        assert_eq!(claude_val["output_tokens"], 4100);
        assert_eq!(claude_val["output_tokens_details"]["thinking_tokens"], 1000);
        assert_eq!(claude_val["cache_read_input_tokens"], 94000);
    }

    #[test]
    fn test_canonical_usage_anthropic_input() {
        // 模拟 Anthropic 历史格式：input_tokens 存的是未缓存部分 (5000)，cached 存的是 (94000)
        let raw = json!({
            "input_tokens": 5000,
            "cache_read_input_tokens": 94000,
            "cache_creation_input_tokens": 0,
            "output_tokens": 110,
            "output_tokens_details": {"thinking_tokens": 100}
        });
        let usage = CanonicalUsage::from_gemini(&raw);
        assert_eq!(usage.total_input_tokens, Some(99000));
        assert_eq!(usage.cached_tokens, Some(94000));
        assert_eq!(usage.uncached_input_tokens, Some(5000));
        assert_eq!(usage.output_tokens, Some(110));
        assert_eq!(usage.reasoning_tokens, Some(100));
    }

    #[test]
    fn cache_missing_and_explicit_zero_remain_distinct() {
        for cached in [None, Some(0), Some(80)] {
            let mut raw = json!({"promptTokenCount": 100, "candidatesTokenCount": 2});
            if let Some(value) = cached {
                raw["cachedContentTokenCount"] = json!(value);
            }
            let usage = CanonicalUsage::from_gemini(&raw);
            assert_eq!(usage.cached_tokens, cached);
            let chat = crate::proxy::mappers::openai::models::OpenAIUsage::from(&usage);
            assert_eq!(
                chat.prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens),
                cached
            );
            assert_eq!(
                chat.to_responses_usage_value()["input_tokens_details"]["cached_tokens"],
                json!(cached)
            );
            assert_eq!(
                usage.to_claude_usage(false, 200000)["cache_read_input_tokens"],
                json!(cached)
            );
        }
    }
}
