use crate::proxy::token_manager::ProxyToken;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub max_output_tokens: Option<u64>,
    pub thinking_budget: Option<u64>,
    pub is_thinking: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpecsConfig {
    models: HashMap<String, ModelSpec>,
    aliases: HashMap<String, String>,
}

static SPECS: Lazy<SpecsConfig> = Lazy::new(|| {
    let json_str = include_str!("../../resources/model_specs.json");
    serde_json::from_str(json_str).expect("Failed to parse model_specs.json")
});

/// Get the normalized model ID (based on alias)
pub fn resolve_alias(model_id: &str) -> String {
    SPECS
        .aliases
        .get(model_id)
        .cloned()
        .unwrap_or_else(|| model_id.to_string())
}

/// Get the model output token limit (dynamic takes priority)
pub fn get_max_output_tokens(model_id: &str, token: Option<&ProxyToken>) -> u64 {
    let std_id = resolve_alias(model_id);

    // 1. Try reading from the account's dynamic data
    if let Some(t) = token {
        if let Some(&limit) = t.model_limits.get(&std_id) {
            return limit;
        }
        // If the raw ID wasn't found, try looking up with the normalized ID
        if let Some(&limit) = t.model_limits.get(model_id) {
            return limit;
        }
    }

    // 2. Fall back to static JSON
    if let Some(spec) = SPECS.models.get(&std_id) {
        if let Some(limit) = spec.max_output_tokens {
            return limit;
        }
    }

    // 3. Global fallback
    65535
}

/// Get the thinking chain budget (dynamic takes priority)
pub fn get_thinking_budget(model_id: &str, _token: Option<&ProxyToken>) -> u64 {
    let std_id = resolve_alias(model_id);

    // 1. First try to infer from the token's quota info (if quota later returns a specific budget)
    // Currently the ProxyToken struct does not directly cache each model's thinking_budget,
    // but it could be filled in via a model_limits ratio or directly from JSON.

    // 2. Static JSON config
    if let Some(spec) = SPECS.models.get(&std_id) {
        if let Some(budget) = spec.thinking_budget {
            return budget;
        }
    }

    // 3. Default safe limit
    if std_id.contains("claude") {
        16000
    } else {
        24576
    }
}

/// Determine whether this is a thinking model
#[allow(dead_code)]
pub fn is_thinking_model(model_id: &str) -> bool {
    let std_id = resolve_alias(model_id);
    if let Some(spec) = SPECS.models.get(&std_id) {
        return spec.is_thinking.unwrap_or(false);
    }
    model_id.contains("-thinking") || model_id.contains("thinking")
}
