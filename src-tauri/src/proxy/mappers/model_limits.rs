// Model output token limit management (DEPRECATED: logic has moved to crate::proxy::model_specs)
// Kept for compatibility, redirecting to model_specs.

use crate::proxy::model_specs;

/// Get the output token limit for a model
///
/// # Arguments
/// - `model_name`: the mapped model name
/// - `dynamic_limit`: pass this in if the dynamic limit is already known (deprecated; prefer passing a ProxyToken directly to model_specs)
#[allow(dead_code)]
pub fn get_model_output_limit(model_name: &str, dynamic_limit: Option<u64>) -> u64 {
    // Compatibility logic: if there's no dynamic_limit, call model_specs to get one (no token passed currently)
    if let Some(limit) = dynamic_limit {
        limit
    } else {
        model_specs::get_max_output_tokens(model_name, None)
    }
}
