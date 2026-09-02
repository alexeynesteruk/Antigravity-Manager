// Claude protocol handler

use axum::{
    body::Body,
    extract::{Json, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::time::Duration;
use tracing::{debug, error, info};

use crate::proxy::common::client_adapter::CLIENT_ADAPTERS; // [NEW] Import Adapter Registry
use crate::proxy::debug_logger;
use crate::proxy::mappers::claude::{
    clean_cache_control_from_messages, close_tool_loop_for_thinking, create_claude_sse_stream,
    filter_invalid_thinking_blocks_with_family, merge_consecutive_messages,
    models::{Message, MessageContent},
    transform_claude_request_in, transform_response, ClaudeRequest,
};
use crate::proxy::mappers::context_manager::ContextManager;
use crate::proxy::mappers::estimation_calibrator::get_calibrator;
use crate::proxy::model_specs;
use crate::proxy::server::AppState;
use crate::proxy::upstream::client::mask_email;
use axum::http::HeaderMap;
use std::sync::{atomic::Ordering, Arc}; // [NEW]

// ===== Task #6: OpenCode variants thinking config mapping =====
// Helper structs for parsing thinking hints from raw JSON
#[derive(Debug, Clone)]
struct ThinkingHint {
    budget_tokens: Option<u32>,
    level: Option<String>,
}

/// Extract thinking hints from raw request JSON (OpenCode variants compatibility)
/// Checks multiple possible paths for budget and level configuration
fn extract_thinking_hint(body: &Value) -> ThinkingHint {
    let mut hint = ThinkingHint {
        budget_tokens: None,
        level: None,
    };

    // Try to extract budget_tokens from various paths
    // Priority: thinking.budget_tokens > thinking.budgetTokens > thinking.budget > thinkingConfig.thinkingBudget
    if let Some(budget) = body
        .get("thinking")
        .and_then(|t| t.get("budget_tokens"))
        .and_then(|b| b.as_u64())
    {
        hint.budget_tokens = Some(budget as u32);
    } else if let Some(budget) = body
        .get("thinking")
        .and_then(|t| t.get("budgetTokens"))
        .and_then(|b| b.as_u64())
    {
        hint.budget_tokens = Some(budget as u32);
    } else if let Some(budget) = body
        .get("thinking")
        .and_then(|t| t.get("budget"))
        .and_then(|b| b.as_u64())
    {
        hint.budget_tokens = Some(budget as u32);
    } else if let Some(budget) = body
        .get("thinkingConfig")
        .and_then(|t| t.get("thinkingBudget"))
        .and_then(|b| b.as_u64())
    {
        hint.budget_tokens = Some(budget as u32);
    }

    // Try to extract level from thinkingLevel
    if let Some(level) = body.get("thinkingLevel").and_then(|l| l.as_str()) {
        hint.level = Some(level.to_lowercase());
    }

    hint
}

/// Map thinking level to suggested budget tokens
fn level_to_budget(level: &str, cap: u64) -> u32 {
    let base = match level {
        "minimal" => 1024,
        "low" => 8192,
        "medium" => 16384,
        "high" => 24576,
        _ => 8192, // default to low
    };
    base.min(cap as u32)
}

/// Map thinking level to effort level for output_config
fn level_to_effort(level: &str) -> String {
    match level {
        "minimal" | "low" => "low".to_string(),
        "medium" => "medium".to_string(),
        "high" => "high".to_string(),
        _ => "low".to_string(),
    }
}

/// Apply thinking hints to ClaudeRequest
fn apply_thinking_hints(
    request: &mut crate::proxy::mappers::claude::models::ClaudeRequest,
    hint: &ThinkingHint,
    trace_id: &str,
    budget_cap: u64, // [NEW]
) {
    let mut applied = false;

    // If budget is provided, set/override thinking config
    if let Some(budget) = hint.budget_tokens {
        request.thinking = Some(crate::proxy::mappers::claude::models::ThinkingConfig {
            type_: "enabled".to_string(),
            budget_tokens: Some(budget),
            effort: None,
        });
        tracing::debug!(
            "[{}] Applied thinking hint: budget_tokens={}",
            trace_id,
            budget
        );
        applied = true;
    }

    // If level is provided
    if let Some(ref level) = hint.level {
        // Map to output_config.effort if not already set
        if request.output_config.is_none() {
            request.output_config = Some(crate::proxy::mappers::claude::models::OutputConfig {
                effort: Some(level_to_effort(level)),
            });
            tracing::debug!("[{}] Applied thinking hint: effort={}", trace_id, level);
            applied = true;
        }

        // If no budget provided but level is, map level to budget
        if hint.budget_tokens.is_none() {
            let budget = level_to_budget(level, budget_cap);
            request.thinking = Some(crate::proxy::mappers::claude::models::ThinkingConfig {
                type_: "enabled".to_string(),
                budget_tokens: Some(budget),
                effort: None,
            });
            tracing::debug!(
                "[{}] Applied thinking hint: level={} -> budget_tokens={}",
                trace_id,
                level,
                budget
            );
            applied = true;
        }
    }

    if applied {
        tracing::info!("[{}] Applied OpenCode thinking hints to request", trace_id);
    }
}

const MAX_RETRY_ATTEMPTS: usize = 3;

// ===== Model Constants for Background Tasks =====
// These can be adjusted for performance/cost optimization or overridden by custom_mapping
const INTERNAL_BACKGROUND_TASK: &str = "internal-background-task"; // Unified virtual ID for all background tasks

// ===== Layer 3: XML Summary Prompt Template =====
// Borrowed from Practical-Guide-to-Context-Engineering + Claude Code official practice
// This prompt generates a structured 8-section XML summary for context compression
const CONTEXT_SUMMARY_PROMPT: &str = r#"You are a context compression specialist. Your task is to create a structured XML snapshot of the conversation history.

This snapshot will become the Agent's ONLY memory of the past. All key details, plans, errors, and user instructions MUST be preserved.

First, think through the entire history in a private <scratchpad>. Review the user's overall goal, the agent's actions, tool outputs, file modifications, and any unresolved issues. Identify every piece of information critical for future actions.

After reasoning, generate the final <state_snapshot> XML object. Information must be extremely dense. Omit any irrelevant conversational filler.

The structure MUST be as follows:

<state_snapshot>
  <overall_goal>
    <!-- Describe the user's high-level goal in one concise sentence -->
  </overall_goal>

  <technical_context>
    <!-- Tech stack: frameworks, languages, toolchain, dependency versions -->
  </technical_context>

  <file_system_state>
    <!-- List files that were created, read, modified, or deleted. Note their status -->
  </file_system_state>

  <code_changes>
    <!-- Key code snippets (preserve function signatures and important logic) -->
  </code_changes>

  <debugging_history>
    <!-- List all errors encountered, with stack traces, and how they were fixed -->
  </debugging_history>

  <current_plan>
    <!-- Step-by-step plan. Mark completed steps -->
  </current_plan>

  <user_preferences>
    <!-- User's work preferences for this project (test commands, code style, etc.) -->
  </user_preferences>

  <key_decisions>
    <!-- Critical architectural decisions and design choices -->
  </key_decisions>

  <latest_thinking_signature>
    <!-- [CRITICAL] Preserve the last valid thinking signature -->
    <!-- Format: base64-encoded signature string -->
    <!-- This MUST be copied exactly as-is, no modifications -->
  </latest_thinking_signature>
</state_snapshot>

**IMPORTANT**:
1. Code snippets must be complete, including function signatures and key logic
2. Error messages must be preserved verbatim, including line numbers and stacks
3. File paths must use absolute paths
4. The thinking signature must be copied exactly, no modifications
"#;

// ===== Jitter Configuration (REMOVED) =====
// Jitter was causing connection instability, reverted to fixed delays
// const JITTER_FACTOR: f64 = 0.2;

// ===== Unified backoff strategy module =====

// [REMOVED] apply_jitter function
// Jitter logic removed to restore stability (v3.3.16 fix)

// ===== Unified backoff strategy module =====
// Removed the local duplicate definition, use the unified implementation in common instead
use super::common::{
    apply_retry_strategy, determine_retry_strategy, should_rotate_account, RetryStrategy,
};

// ===== End of backoff strategy module =====

#[cfg(test)]
mod variant_tests {
    use super::*;

    fn request_with_effort(model: &str, effort: &str, budget_tokens: u32) -> ClaudeRequest {
        serde_json::from_value(json!({
            "model": model,
            "messages": [{"role": "user", "content": "test"}],
            "thinking": {"type": "enabled", "budget_tokens": budget_tokens},
            "output_config": {"effort": effort}
        }))
        .expect("test request must deserialize")
    }

    #[test]
    fn applies_flash_low_effort_and_removes_output_config_before_serialization() {
        let mut request = request_with_effort("gemini-3.5-flash", "low", 10_000);
        let effort = crate::proxy::common::variant_mapping::tier_from_effort(
            request
                .output_config
                .as_ref()
                .and_then(|config| config.effort.as_deref()),
        );

        apply_variant(&mut request, effort, Some(10_000)).expect("Gemini 3.5 Flash must resolve");

        assert_eq!(request.model, "gemini-3.5-flash-extra-low");
        assert!(request.output_config.is_none());
        assert!(serde_json::to_value(request)
            .expect("resolved request must serialize")
            .get("output_config")
            .is_none());
    }

    #[test]
    fn applies_pro_high_effort_over_low_budget() {
        let mut request = request_with_effort("gemini-3.1-pro", "high", 1_000);
        let effort = crate::proxy::common::variant_mapping::tier_from_effort(
            request
                .output_config
                .as_ref()
                .and_then(|config| config.effort.as_deref()),
        );

        apply_variant(&mut request, effort, Some(1_000)).expect("Gemini 3.1 Pro must resolve");

        assert_eq!(request.model, "gemini-pro-agent");
    }

    #[test]
    fn invalid_effort_falls_back_to_budget_tokens_for_gemini_3_model() {
        // Given a Gemini 3 model ("gemini-3-flash") with an unrecognized
        // effort value ("max"), tier_from_effort returns None, so
        // apply_variant falls back to budget-based tier inference.
        let mut request = request_with_effort("gemini-3-flash", "max", 4_000);
        let effort = crate::proxy::common::variant_mapping::tier_from_effort(
            request
                .output_config
                .as_ref()
                .and_then(|config| config.effort.as_deref()),
        );

        // tier_from_effort(Some("max")) → None (invalid value)
        assert_eq!(effort, None);

        // With effort=None and budget=4_000, infer_tier → Medium →
        // resolve_with_tier("gemini-3-flash", None, Some(4_000)) →
        // SPEC_35_FLASH_LOW → physical id "gemini-3.5-flash-low"
        apply_variant(&mut request, effort, Some(4_000))
            .expect("gemini-3-flash must resolve even without valid effort");

        assert_eq!(request.model, "gemini-3.5-flash-low");
        assert!(request.output_config.is_none());
        // SPEC_35_FLASH_LOW has thinking_budget=4_000, preserve_client_budget=false
        assert_eq!(
            request.thinking.as_ref().and_then(|t| t.budget_tokens),
            Some(4_000)
        );
    }

    #[test]
    fn claude_model_without_variant_mapping_preserves_output_config_on_none() {
        // claude-sonnet-4-5 is NOT in GEMINI_FAMILIES and NOT in
        // resolve_non_variant_model, so resolve_with_tier returns None,
        // and apply_variant returns None without mutating the request.
        let mut request = request_with_effort("claude-sonnet-4-5", "high", 10_000);
        let effort = crate::proxy::common::variant_mapping::tier_from_effort(
            request
                .output_config
                .as_ref()
                .and_then(|config| config.effort.as_deref()),
        );

        let result = apply_variant(&mut request, effort, Some(10_000));
        assert!(
            result.is_none(),
            "unregistered Claude model must return None"
        );

        // Model and output_config must remain untouched.
        assert_eq!(request.model, "claude-sonnet-4-5");
        assert_eq!(
            request
                .output_config
                .as_ref()
                .and_then(|c| c.effort.as_deref()),
            Some("high")
        );
    }
}

fn apply_variant(
    request: &mut ClaudeRequest,
    effort_tier: Option<crate::proxy::common::variant_mapping::VariantTier>,
    client_budget: Option<u32>,
) -> Option<crate::proxy::common::variant_mapping::RealModelSpec> {
    let spec = crate::proxy::common::variant_mapping::resolve_with_tier(
        &request.model,
        effort_tier,
        client_budget,
    )?;

    request.model = spec.id.to_string();
    if spec.thinking_budget == 0 {
        request.thinking = None;
        request.tools = None;
    } else {
        request.thinking = Some(crate::proxy::mappers::claude::models::ThinkingConfig {
            type_: "enabled".to_string(),
            budget_tokens: Some(spec.effective_thinking_budget(client_budget)),
            effort: None,
        });
    }
    request.output_config = None;
    request.max_tokens = Some(spec.max_output_tokens);

    Some(spec)
}

/// Handle a Claude messages request
///
/// Handles the chat message request flow
pub async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // [FIX] Keep a full copy of the original request body for logging
    // This ensures every parameter is fully logged, even if the struct definition is missing a field
    let original_body = body.clone();

    tracing::debug!(
        "handle_messages called. Body JSON len: {}",
        body.to_string().len()
    );

    // Generate a random Trace ID for tracking
    let trace_id: String =
        rand::Rng::sample_iter(rand::thread_rng(), &rand::distributions::Alphanumeric)
            .take(6)
            .map(char::from)
            .collect::<String>()
            .to_lowercase();
    let debug_cfg = state.debug_logging.read().await.clone();

    // [NEW] Detect Client Adapter
    // Check whether a matching client adapter exists (e.g. opencode)
    let client_adapter = CLIENT_ADAPTERS
        .iter()
        .find(|a| a.matches(&headers))
        .cloned();
    if let Some(_adapter) = &client_adapter {
        tracing::debug!(
            "[{}] Client Adapter detected: Applying custom strategies",
            trace_id
        );
    }

    // Decide whether this request should be handled by z.ai (Anthropic passthrough) or the existing Google flow.
    let zai = state.zai.read().await.clone();
    let zai_enabled =
        zai.enabled && !matches!(zai.dispatch_mode, crate::proxy::ZaiDispatchMode::Off);
    let google_accounts = state.token_manager.len();

    // [CRITICAL REFACTOR] Parse the request first to get model info (for smart fallback decisions)
    let mut request: crate::proxy::mappers::claude::models::ClaudeRequest =
        match serde_json::from_value(body.clone()) {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "type": "error",
                        "error": {
                            "type": "invalid_request_error",
                            "message": format!("Invalid request body: {}", e)
                        }
                    })),
                )
                    .into_response();
            }
        };

    // [Task #6] Apply OpenCode variants thinking hints from raw JSON
    // Since no account has been obtained yet, fall back to the model's default limit for now
    let temp_cap = model_specs::get_thinking_budget(&request.model, None);
    let thinking_hint = extract_thinking_hint(&original_body);
    apply_thinking_hints(&mut request, &thinking_hint, &trace_id, temp_cap);

    // [Variant] Resolve canonical model + variant → real model + real params.
    let client_budget = original_body
        .get("thinking")
        .and_then(|t| t.get("budget_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    let effort_hint = request
        .output_config
        .as_ref()
        .and_then(|config| config.effort.clone());
    let effort_tier =
        crate::proxy::common::variant_mapping::tier_from_effort(effort_hint.as_deref());
    let canonical_model = request.model.clone();
    if let Some(spec) = apply_variant(&mut request, effort_tier, client_budget) {
        tracing::info!(
            "[{}] [Variant] canonical='{}' effort_hint={:?} budget_hint={:?} -> real_model='{}' budget={} maxOut={}",
            trace_id, canonical_model, effort_hint, client_budget, spec.id, spec.thinking_budget, spec.max_output_tokens
        );
    }

    if debug_logger::is_enabled(&debug_cfg) {
        // [FIX] Log using the original body copy, to avoid losing any fields
        let original_payload = json!({
            "kind": "original_request",
            "protocol": "anthropic",
            "trace_id": trace_id,
            "original_model": request.model,
            "request": original_body,  // Use the original request body, not the struct serialization
        });
        debug_logger::write_debug_payload(
            &debug_cfg,
            Some(&trace_id),
            "original_request",
            &original_payload,
        )
        .await;
    }

    // [Issue #703 Fix] Smart fallback decision: needs the normalized model name for quota-protection checks
    let normalized_model =
        crate::proxy::common::model_mapping::normalize_to_standard_id(&request.model)
            .unwrap_or_else(|| request.model.clone());

    let use_zai = if !zai_enabled {
        false
    } else {
        match zai.dispatch_mode {
            crate::proxy::ZaiDispatchMode::Off => false,
            crate::proxy::ZaiDispatchMode::Exclusive => true,
            crate::proxy::ZaiDispatchMode::Fallback => {
                if google_accounts == 0 {
                    // No Google accounts, use the fallback
                    tracing::info!(
                        "[{}] No Google accounts available, using fallback provider",
                        trace_id
                    );
                    true
                } else {
                    // [Issue #703 Fix] Smart check: whether a usable Google account exists
                    let has_available = state
                        .token_manager
                        .has_available_account("claude", &normalized_model)
                        .await;
                    if !has_available {
                        tracing::info!(
                            "[{}] All Google accounts unavailable (rate-limited or quota-protected for {}), using fallback provider",
                            trace_id,
                            request.model
                        );
                    }
                    !has_available
                }
            }
            crate::proxy::ZaiDispatchMode::Pooled => {
                // Treat z.ai as exactly one extra slot in the pool.
                // No strict guarantees: it may get 0 requests if selection never hits.
                let total = google_accounts.saturating_add(1).max(1);
                let slot = state.provider_rr.fetch_add(1, Ordering::Relaxed) % total;
                slot == 0
            }
        }
    };

    // [CRITICAL FIX] Pre-clean the cache_control field from all messages (Issue #744)
    // Must be handled before serialization, to ensure both z.ai and the Google Flow are
    // unaffected by historical message cache markers
    clean_cache_control_from_messages(&mut request.messages);

    // [FIX #813] Merge consecutive same-role messages (Consecutive User Messages)
    // This is critical for the z.ai (direct Anthropic passthrough) path, since the raw
    // structure must conform to the protocol
    merge_consecutive_messages(&mut request.messages);

    // Get model family for signature validation
    let target_family = if use_zai {
        Some("claude")
    } else {
        let mapped_model =
            crate::proxy::common::model_mapping::map_claude_model_to_gemini(&request.model);
        if mapped_model.contains("gemini") {
            Some("gemini")
        } else {
            Some("claude")
        }
    };

    // [CRITICAL FIX] Filter and repair Thinking block signatures (Enhanced with family check)
    filter_invalid_thinking_blocks_with_family(&mut request.messages, target_family);

    // [New] Recover from broken tool loops (where signatures were stripped)
    // This prevents "Assistant message must start with thinking" errors by closing the loop with synthetic messages
    if state.experimental.read().await.enable_tool_loop_recovery {
        close_tool_loop_for_thinking(&mut request.messages);
    }

    let experimental_cfg = state.experimental.read().await;
    let compression_level = if experimental_cfg.compression_level == "disabled" {
        if experimental_cfg.enable_usage_scaling {
            "high".to_string()
        } else {
            "disabled".to_string()
        }
    } else {
        experimental_cfg.compression_level.clone()
    };

    if compression_level != "disabled" {
        // [ACC-P RTK] All of Low, Medium, and High levels apply static RTK denoise folding to incoming tool-result logs
        for msg in &mut request.messages {
            crate::proxy::mappers::context_manager::ContextManager::clean_tool_message(msg);
        }

        // [ACC-P Caveman] Medium and High levels apply Caveman purification to older conversation history beyond the most recent 4 messages (~2 turns)
        if compression_level == "medium" || compression_level == "high" {
            let total_msgs = request.messages.len();
            let start_protection_idx = total_msgs.saturating_sub(4);
            for (i, msg) in request.messages.iter_mut().enumerate() {
                if i >= start_protection_idx {
                    continue;
                }
                if msg.role == "user" || msg.role == "assistant" {
                    match &mut msg.content {
                        crate::proxy::mappers::claude::models::MessageContent::String(s) => {
                            let cleaned =
                                crate::proxy::mappers::caveman_cleaner::CavemanCleaner::clean(s);
                            if cleaned != *s {
                                *s = cleaned;
                            }
                        }
                        crate::proxy::mappers::claude::models::MessageContent::Array(blocks) => {
                            for block in blocks {
                                if let crate::proxy::mappers::claude::models::ContentBlock::Text {
                                    text,
                                } = block
                                {
                                    let cleaned = crate::proxy::mappers::caveman_cleaner::CavemanCleaner::clean(text);
                                    if cleaned != *text {
                                        *text = cleaned;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ===== [Issue #467 Fix] Intercept Claude Code Warmup requests =====
    // Claude Code sends a warmup request every 10 seconds to keep the connection warm,
    // and these requests consume a lot of quota. Once a warmup request is detected, return a
    // simulated response directly.
    if is_warmup_request(&request) {
        tracing::info!(
            "[{}] 🔥 Intercepted Warmup request, returning simulated response (saving quota)",
            trace_id
        );
        return create_warmup_response(&request, request.stream);
    }

    if use_zai {
        // Re-serialize the fixed request body
        let mut new_body = match serde_json::to_value(&request) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize fixed request for z.ai: {}", e);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };

        // Inject cache_control into the XML summary message if it is a Forked session
        inject_cache_control_to_forked_summary(&mut new_body);

        return crate::proxy::providers::zai_anthropic::forward_anthropic_json(
            &state,
            axum::http::Method::POST,
            "/v1/messages",
            &headers,
            new_body,
            request.messages.len(), // [NEW v4.0.0] Pass message count
        )
        .await;
    }

    // The Google Flow continues to use the request object
    // (the code below doesn't need to call filter_invalid_thinking_blocks again)

    // [NEW] Obtain the context-control configuration
    let experimental = state.experimental.read().await;
    let scaling_enabled = experimental.enable_usage_scaling;
    let threshold_l1 = experimental.context_compression_threshold_l1;
    let threshold_l2 = experimental.context_compression_threshold_l2;
    let threshold_l3 = experimental.context_compression_threshold_l3;

    // Get the latest "meaningful" message content (for logging and background-task detection)
    // Strategy: iterate in reverse, first filter to messages with role "user", then find the
    // first non-"Warmup" and non-empty text message among them
    // Get the latest "meaningful" message content (for logging and background-task detection)
    // Strategy: iterate in reverse, first filter to all user-related messages (role="user")
    // then extract their text content, skipping "Warmup" or system-preset reminders
    let meaningful_msg = request
        .messages
        .iter()
        .rev()
        .filter(|m| m.role == "user")
        .find_map(|m| {
            let content = match &m.content {
                crate::proxy::mappers::claude::models::MessageContent::String(s) => s.to_string(),
                crate::proxy::mappers::claude::models::MessageContent::Array(arr) => {
                    // For an array, extract and concatenate all Text blocks, ignoring ToolResult
                    arr.iter()
                        .filter_map(|block| match block {
                            crate::proxy::mappers::claude::models::ContentBlock::Text { text } => {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                }
            };

            // Filter rules:
            // 1. Ignore empty messages
            // 2. Ignore "Warmup" messages
            // 3. Ignore messages containing a <system-reminder> tag
            if content.trim().is_empty()
                || content.starts_with("Warmup")
                || content.contains("<system-reminder>")
            {
                None
            } else {
                Some(content)
            }
        });

    // If still not found after filtering (e.g. a pure tool call), fall back to the raw display of the last message
    let latest_msg = meaningful_msg.unwrap_or_else(|| {
        request
            .messages
            .last()
            .map(|m| match &m.content {
                crate::proxy::mappers::claude::models::MessageContent::String(s) => s.clone(),
                crate::proxy::mappers::claude::models::MessageContent::Array(_) => {
                    "[Complex/Tool Message]".to_string()
                }
            })
            .unwrap_or_else(|| "[No Messages]".to_string())
    });

    // INFO level: a concise one-line summary
    info!(
        "[{}] Claude Request | Model: {} | Stream: {} | Messages: {} | Tools: {}",
        trace_id,
        request.model,
        request.stream,
        request.messages.len(),
        request.tools.is_some()
    );

    // DEBUG level: detailed debug info
    debug!(
        "========== [{}] CLAUDE REQUEST DEBUG START ==========",
        trace_id
    );
    debug!("[{}] Model: {}", trace_id, request.model);
    debug!("[{}] Stream: {}", trace_id, request.stream);
    debug!("[{}] Max Tokens: {:?}", trace_id, request.max_tokens);
    debug!("[{}] Temperature: {:?}", trace_id, request.temperature);
    debug!("[{}] Message Count: {}", trace_id, request.messages.len());
    debug!("[{}] Has Tools: {}", trace_id, request.tools.is_some());
    debug!(
        "[{}] Has Thinking Config: {}",
        trace_id,
        request.thinking.is_some()
    );
    debug!("[{}] Content Preview: {:.100}...", trace_id, latest_msg);

    // Print detailed info for every message
    for (idx, msg) in request.messages.iter().enumerate() {
        let content_preview = match &msg.content {
            crate::proxy::mappers::claude::models::MessageContent::String(s) => {
                let char_count = s.chars().count();
                if char_count > 200 {
                    // [Fix] Use chars().take() to truncate safely, avoiding a UTF-8 char-boundary panic
                    let preview: String = s.chars().take(200).collect();
                    format!("{}... (total {} chars)", preview, char_count)
                } else {
                    s.clone()
                }
            }
            crate::proxy::mappers::claude::models::MessageContent::Array(arr) => {
                format!("[Array with {} blocks]", arr.len())
            }
        };
        debug!(
            "[{}] Message[{}] - Role: {}, Content: {}",
            trace_id, idx, msg.role, content_preview
        );
    }

    debug!(
        "[{}] Full Claude Request JSON: {}",
        trace_id,
        serde_json::to_string_pretty(&request).unwrap_or_default()
    );
    debug!(
        "========== [{}] CLAUDE REQUEST DEBUG END ==========",
        trace_id
    );

    // 1. Obtain the session ID (content-hash-based approach deprecated, now uses TokenManager's internal time-window locking)
    let _session_id: Option<&str> = None;

    // 2. Obtain the UpstreamClient
    let upstream = state.upstream.clone();

    // 3. Prepare the closure
    let mut request_for_body = request.clone();
    let token_manager = state.token_manager;

    let pool_size = token_manager.len();
    // [FIX] Ensure max_attempts is at least 2 to allow for internal retries (e.g. stripping signatures)
    // even if the user has only 1 account.
    let max_attempts = MAX_RETRY_ATTEMPTS.min(pool_size.saturating_add(1)).max(2);

    let mut last_error = String::new();
    let mut retried_without_thinking = false;
    let mut last_email: Option<String> = None;
    let mut last_mapped_model: Option<String> = None;
    let mut last_status = StatusCode::SERVICE_UNAVAILABLE; // Default to 503 if no response reached
    let mut force_rotate = false;

    for attempt in 0..max_attempts {
        // 2. Model route resolution
        let mut mapped_model = crate::proxy::common::model_mapping::resolve_model_route(
            &request_for_body.model,
            &*state.custom_mapping.read().await,
        );
        last_mapped_model = Some(mapped_model.clone());

        // Convert Claude tools into a Value array for web-search probing
        let tools_val: Option<Vec<Value>> = request_for_body.tools.as_ref().map(|list| {
            list.iter()
                .map(|t| serde_json::to_value(t).unwrap_or(json!({})))
                .collect()
        });

        let config = crate::proxy::mappers::common_utils::resolve_request_config(
            &request_for_body.model,
            &mapped_model,
            &tools_val,
            request.size.as_deref(),    // [NEW] Pass size parameter
            request.quality.as_deref(), // [NEW] Pass quality parameter
            None,                       // image_size
            None,                       // body
        );

        // 0. Try to extract session_id for sticky scheduling (Phase 2/3)
        // Use SessionManager to generate a stable session fingerprint
        let session_id_str =
            crate::proxy::session_manager::SessionManager::extract_session_id(&request_for_body);
        let session_id = Some(session_id_str.as_str());

        let (access_token, project_id, email, account_id, _wait_ms) = match token_manager
            .get_token(
                &config.request_type,
                force_rotate,
                session_id,
                &config.final_model,
            )
            .await
        {
            Ok(t) => t,
            Err(e) => {
                let safe_message = if e.contains("invalid_grant") {
                    "OAuth refresh failed (invalid_grant): refresh_token likely revoked/expired; reauthorize account(s) to restore service.".to_string()
                } else {
                    e
                };
                let headers = [("X-Mapped-Model", mapped_model.as_str())];
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    headers,
                    Json(json!({
                        "type": "error",
                        "error": {
                            "type": "overloaded_error",
                            "message": format!("No available accounts: {}", safe_message)
                        }
                    })),
                )
                    .into_response();
            }
        };

        last_email = Some(email.clone());
        info!("✓ Using account: {} (type: {})", email, config.request_type);

        // ===== [Optimization] Smart background-task detection and downgrade =====
        // Uses the new detection system, supporting 5 major keyword categories and a multi-Flash-model strategy
        let background_task_type = detect_background_task_type(&request_for_body);

        // Pass along the mapped model name
        let mut request_with_mapped = request_for_body.clone();

        if let Some(task_type) = background_task_type {
            // Background task detected, force downgrade to a Flash model
            let virtual_model_id = select_background_model(task_type);

            // [FIX] The route must be re-resolved based on the virtual ID, to support user-defined
            // mappings (e.g. internal-task -> gemini-3); otherwise the generic ID would be used
            // directly, and the downstream would fail to recognize it or fall back to a static default
            let resolved_model = crate::proxy::common::model_mapping::resolve_model_route(
                virtual_model_id,
                &*state.custom_mapping.read().await,
            );

            info!(
                "[{}][AUTO] Background task detected (type: {:?}), route redirect: {} -> {} (final physical model: {})",
                trace_id, task_type, mapped_model, virtual_model_id, resolved_model
            );

            // Override the user's custom mapping (updating both the variable and the Request object)
            mapped_model = resolved_model.clone();
            request_with_mapped.model = resolved_model;

            // Background task purification:
            // 1. Remove the tools definitions (background tasks don't need tools)
            request_with_mapped.tools = None;

            // 2. Remove the Thinking config (Flash models don't support it)
            request_with_mapped.thinking = None;

            // 3. Clean Thinking Blocks from historical messages, to prevent Invalid Argument
            // Use ContextManager's unified strategy (Aggressive)
            crate::proxy::mappers::context_manager::ContextManager::purify_history(
                &mut request_with_mapped.messages,
                crate::proxy::mappers::context_manager::PurificationStrategy::Aggressive,
            );
        }

        // ===== [3-Layer Progressive Compression + Calibrated Estimation] Context Management =====
        // [ENHANCED] Integrates the 3-layer compression framework from 3.3.47 + the dynamic
        // calibration mechanism from PR #925
        // [NEW] The compression logic only runs when scaling_enabled is true (linked mechanism)
        // Layer 1 (60%): Tool message trimming - Does NOT break cache
        // Layer 2 (75%): Thinking purification - Breaks cache but preserves signatures
        // Layer 3 (90%): Fork conversation + XML summary - Ultimate optimization
        let mut is_purified = false;
        let mut compression_applied = false;

        if !retried_without_thinking && compression_level == "high" {
            // Added the scaling_enabled linked check
            // 1. Determine context limit (Flash: ~1M, Pro: ~2M)
            let context_limit = if mapped_model.contains("flash") {
                1_000_000
            } else {
                2_000_000
            };

            // 2. [ENHANCED] Use the calibrator to improve estimation accuracy (PR #925)
            let raw_estimated = ContextManager::estimate_token_usage(&request_with_mapped);
            let calibrator = get_calibrator();
            let mut estimated_usage = calibrator.calibrate(raw_estimated);
            let mut usage_ratio = estimated_usage as f32 / context_limit as f32;

            info!(
                "[{}] [ContextManager] Context pressure: {:.1}% (raw: {}, calibrated: {} / {}), Calibration factor: {:.2}",
                trace_id, usage_ratio * 100.0, raw_estimated, estimated_usage, context_limit, calibrator.get_factor()
            );

            // ===== Layer 1: Tool Message Trimming (L1 threshold) =====
            // Borrowed from Practical-Guide-to-Context-Engineering
            // Advantage: Completely cache-friendly (only removes messages, doesn't modify content)
            if usage_ratio > threshold_l1 && !compression_applied {
                if ContextManager::trim_tool_messages(&mut request_with_mapped.messages, 5) {
                    info!(
                        "[{}] [Layer-1] Tool trimming triggered (usage: {:.1}%, threshold: {:.1}%)",
                        trace_id,
                        usage_ratio * 100.0,
                        threshold_l1 * 100.0
                    );
                    compression_applied = true;

                    // Re-estimate after trimming (with calibration)
                    let new_raw = ContextManager::estimate_token_usage(&request_with_mapped);
                    let new_usage = calibrator.calibrate(new_raw);
                    let new_ratio = new_usage as f32 / context_limit as f32;

                    info!(
                        "[{}] [Layer-1] Compression result: {:.1}% → {:.1}% (saved {} tokens)",
                        trace_id,
                        usage_ratio * 100.0,
                        new_ratio * 100.0,
                        estimated_usage - new_usage
                    );

                    // If compression is sufficient, skip further layers
                    if new_ratio < 0.7 {
                        estimated_usage = new_usage;
                        usage_ratio = new_ratio;
                        // Success, no need for Layer 2
                    } else {
                        // Still high pressure, update for Layer 2
                        usage_ratio = new_ratio;
                        compression_applied = false; // Allow Layer 2 to run
                    }
                }
            }

            // ===== Layer 2: Thinking Content Compression (L2 threshold) =====
            // NEW: Preserve signatures while compressing thinking text
            // This prevents signature chain breakage (Issue #902)
            if usage_ratio > threshold_l2 && !compression_applied {
                info!(
                    "[{}] [Layer-2] Thinking compression triggered (usage: {:.1}%, threshold: {:.1}%)",
                    trace_id, usage_ratio * 100.0, threshold_l2 * 100.0
                );

                // Use new signature-preserving compression
                if ContextManager::compress_thinking_preserve_signature(
                    &mut request_with_mapped.messages,
                    4, // Protect last 4 messages (~2 turns)
                ) {
                    is_purified = true; // Still breaks cache, but preserves signatures
                    compression_applied = true;

                    let new_raw = ContextManager::estimate_token_usage(&request_with_mapped);
                    let new_usage = calibrator.calibrate(new_raw);
                    let new_ratio = new_usage as f32 / context_limit as f32;

                    info!(
                        "[{}] [Layer-2] Compression result: {:.1}% → {:.1}% (saved {} tokens)",
                        trace_id,
                        usage_ratio * 100.0,
                        new_ratio * 100.0,
                        estimated_usage - new_usage
                    );

                    usage_ratio = new_ratio;
                }
            }

            // ===== Layer 3: Fork Conversation + XML Summary (L3 threshold) =====
            // Ultimate optimization: Generate structured summary and start fresh conversation
            // Advantage: Completely cache-friendly (append-only), extreme compression ratio
            if usage_ratio > threshold_l3 && !compression_applied {
                info!(
                    "[{}] [Layer-3] Context pressure ({:.1}%) exceeded threshold ({:.1}%), attempting Fork+Summary",
                    trace_id, usage_ratio * 100.0, threshold_l3 * 100.0
                );

                // Clone token_manager Arc to avoid borrow issues
                let token_manager_clone = token_manager.clone();

                match try_compress_with_summary(
                    &request_with_mapped,
                    &trace_id,
                    &token_manager_clone,
                )
                .await
                {
                    Ok(forked_request) => {
                        info!(
                            "[{}] [Layer-3] Fork successful: {} → {} messages",
                            trace_id,
                            request_with_mapped.messages.len(),
                            forked_request.messages.len()
                        );

                        request_with_mapped = forked_request;
                        is_purified = false; // Fork doesn't break cache!

                        // Re-estimate after fork (with calibration)
                        let new_raw = ContextManager::estimate_token_usage(&request_with_mapped);
                        let new_usage = calibrator.calibrate(new_raw);
                        let new_ratio = new_usage as f32 / context_limit as f32;

                        info!(
                            "[{}] [Layer-3] Compression result: {:.1}% → {:.1}% (saved {} tokens)",
                            trace_id,
                            usage_ratio * 100.0,
                            new_ratio * 100.0,
                            estimated_usage - new_usage
                        );
                    }
                    Err(e) => {
                        error!(
                            "[{}] [Layer-3] Fork+Summary failed: {}, falling back to error response",
                            trace_id, e
                        );

                        // Return friendly error to user
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "type": "error",
                                "error": {
                                    "type": "invalid_request_error",
                                    "message": format!("Context too long and automatic compression failed: {}", e),
                                    "suggestion": "Please use /compact or /clear command in Claude Code, or switch to a model with larger context window."
                                }
                            }))
                        ).into_response();
                    }
                }
            }
        }

        // [FIX] Estimate AFTER purification to get accurate token count for calibrator learning
        // Only estimate for calibrator when content was not purified, to avoid skewed learning
        let raw_estimated = if !is_purified {
            ContextManager::estimate_token_usage(&request_with_mapped)
        } else {
            0 // Don't record calibration data when content was purified
        };

        request_with_mapped.model = mapped_model.clone();

        // Generate a Trace ID (simply uses a timestamp suffix)
        // let _trace_id = format!("req_{}", chrono::Utc::now().timestamp_subsec_millis());

        let token_obj = token_manager.get_token_by_id(&account_id);
        let gemini_body = match transform_claude_request_in(
            &request_with_mapped,
            &project_id,
            retried_without_thinking,
            Some(account_id.as_str()),
            &session_id_str,
            token_obj.as_ref(),
        ) {
            Ok(b) => {
                debug!(
                    "[{}] Transformed Gemini Body: {}",
                    trace_id,
                    serde_json::to_string_pretty(&b).unwrap_or_default()
                );
                b
            }
            Err(e) => {
                let headers = [
                    ("X-Mapped-Model", request_with_mapped.model.as_str()),
                    ("X-Account-Email", email.as_str()),
                ];
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    headers,
                    Json(json!({
                        "type": "error",
                        "error": {
                            "type": "api_error",
                            "message": format!("Transform error: {}", e)
                        }
                    })),
                )
                    .into_response();
            }
        };

        if debug_logger::is_enabled(&debug_cfg) {
            let payload = json!({
                "kind": "v1internal_request",
                "protocol": "anthropic",
                "trace_id": trace_id,
                "original_model": request.model,
                "mapped_model": request_with_mapped.model,
                "request_type": config.request_type,
                "attempt": attempt,
                "v1internal_request": gemini_body.clone(),
            });
            debug_logger::write_debug_payload(
                &debug_cfg,
                Some(&trace_id),
                "v1internal_request",
                &payload,
            )
            .await;
        }

        // 4. Upstream call - auto-conversion logic
        let client_wants_stream = request.stream;
        // [AUTO-CONVERSION] Automatically convert non-stream requests to stream, to benefit from looser quota
        let force_stream_internally = !client_wants_stream;
        let actual_stream = client_wants_stream || force_stream_internally;

        if force_stream_internally {
            info!(
                "[{}] 🔄 Auto-converting non-stream request to stream for better quota",
                trace_id
            );
        }

        let method = if actual_stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let query = if actual_stream { Some("alt=sse") } else { None };
        // [FIX #765/1522] Prepare Robust Beta Headers for Claude models
        let mut extra_headers = std::collections::HashMap::new();
        if mapped_model.to_lowercase().contains("claude") {
            extra_headers.insert(
                "anthropic-beta".to_string(),
                "claude-code-20250219".to_string(),
            );
            tracing::debug!(
                "[{}] Added Comprehensive Beta Headers for Claude model",
                trace_id
            );
        }

        // [NEW] Inject Beta Headers from Client Adapter
        if let Some(adapter) = &client_adapter {
            let mut temp_headers = HeaderMap::new();
            adapter.inject_beta_headers(&mut temp_headers);
            for (k, v) in temp_headers {
                if let Some(name) = k {
                    if let Ok(v_str) = v.to_str() {
                        extra_headers.insert(name.to_string(), v_str.to_string());
                        tracing::debug!("[{}] Added Adapter Header: {}: {}", trace_id, name, v_str);
                    }
                }
            }
        }

        // Upstream call configuration continued...

        let call_result = match upstream
            .call_v1_internal_with_headers(
                method,
                &access_token,
                gemini_body,
                query,
                extra_headers.clone(),
                Some(account_id.as_str()),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_error = e.clone();
                debug!(
                    "Request failed on attempt {}/{}: {}",
                    attempt + 1,
                    max_attempts,
                    e
                );
                continue;
            }
        };

        // [NEW] Log endpoint fallback to the debug file
        if !call_result.fallback_attempts.is_empty() && debug_logger::is_enabled(&debug_cfg) {
            let fallback_entries: Vec<Value> = call_result
                .fallback_attempts
                .iter()
                .map(|a| {
                    json!({
                        "endpoint_url": a.endpoint_url,
                        "status": a.status,
                        "error": a.error,
                    })
                })
                .collect();
            let payload = json!({
                "kind": "endpoint_fallback",
                "protocol": "anthropic",
                "trace_id": trace_id,
                "original_model": request.model,
                "mapped_model": request_with_mapped.model,
                "attempt": attempt,
                "account": mask_email(&email),
                "fallback_attempts": fallback_entries,
            });
            debug_logger::write_debug_payload(
                &debug_cfg,
                Some(&trace_id),
                "endpoint_fallback",
                &payload,
            )
            .await;
        }

        let response = call_result.response;
        // [NEW] Extract the actual upstream endpoint URL that was called, for logging and diagnostics
        let upstream_url = response.url().to_string();
        let status = response.status();
        last_status = status;

        // Success
        if status.is_success() {
            // [Smart Rate Limiting] Request succeeded, reset the account's consecutive-failure count
            token_manager.mark_account_success(&email);

            // Determine context limit based on model
            let context_limit = crate::proxy::mappers::claude::utils::get_context_limit_for_model(
                &request_with_mapped.model,
            );

            // Handle the streaming response
            if actual_stream {
                let meta = json!({
                    "protocol": "anthropic",
                    "trace_id": trace_id,
                    "original_model": request.model,
                    "mapped_model": request_with_mapped.model,
                    "request_type": config.request_type,
                    "attempt": attempt,
                    "status": status.as_u16(),
                    "upstream_url": upstream_url,
                });
                let gemini_stream = debug_logger::wrap_stream_with_debug(
                    Box::pin(response.bytes_stream()),
                    debug_cfg.clone(),
                    trace_id.clone(),
                    "upstream_response",
                    meta,
                );

                let current_message_count = request_with_mapped.messages.len();

                // [FIX #MCP] Extract registered tool names for MCP fuzzy matching
                let registered_tool_names: Vec<String> = request_with_mapped
                    .tools
                    .as_ref()
                    .map(|tools| tools.iter().filter_map(|t| t.name.clone()).collect())
                    .unwrap_or_default();

                // [FIX #530/#529/#859] Enhanced Peek logic to handle heartbeats and slow start
                // We must pre-read until we find a MEANINGFUL content block (like message_start).
                // If we only get heartbeats (ping) and then the stream dies, we should rotate account.
                let mut claude_stream = create_claude_sse_stream(
                    gemini_stream,
                    trace_id.clone(),
                    email.clone(),
                    Some(session_id_str.clone()),
                    scaling_enabled,
                    context_limit,
                    Some(raw_estimated), // [FIX] Pass estimated tokens for calibrator learning
                    current_message_count, // [NEW v4.0.0] Pass message count for rewind detection
                    client_adapter.clone(), // [NEW] Pass client adapter
                    registered_tool_names, // [FIX #MCP] Pass tool names for fuzzy matching
                );

                let mut first_data_chunk = None;
                let mut retry_this_account = false;

                // Loop to skip heartbeats during peek
                loop {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(300),
                        claude_stream.next(),
                    )
                    .await
                    {
                        Ok(Some(Ok(bytes))) => {
                            if bytes.is_empty() {
                                continue;
                            }

                            let text = String::from_utf8_lossy(&bytes);
                            // Skip SSE comments/pings
                            if text.trim().starts_with(":") {
                                debug!("[{}] Skipping peek heartbeat: {}", trace_id, text.trim());
                                continue;
                            }

                            // We found real data!
                            first_data_chunk = Some(bytes);
                            break;
                        }
                        Ok(Some(Err(e))) => {
                            tracing::warn!(
                                "[{}] Stream error during peek: {}, retrying...",
                                trace_id,
                                e
                            );
                            last_error = format!("Stream error during peek: {}", e);
                            retry_this_account = true;
                            break;
                        }
                        Ok(None) => {
                            tracing::warn!(
                                "[{}] Stream ended during peek (Empty Response), retrying...",
                                trace_id
                            );
                            last_error = "Empty response stream during peek".to_string();
                            retry_this_account = true;
                            break;
                        }
                        Err(_) => {
                            tracing::warn!(
                                "[{}] Timeout waiting for first data (60s), retrying...",
                                trace_id
                            );
                            last_error = "Timeout waiting for first data".to_string();
                            retry_this_account = true;
                            break;
                        }
                    }
                }

                if retry_this_account {
                    continue;
                }

                match first_data_chunk {
                    Some(bytes) => {
                        // We have data! Construct the combined stream
                        let stream_rest = claude_stream;
                        let combined_stream = futures::stream::once(async move { Ok(bytes) })
                            .chain(stream_rest.map(|result| -> Result<Bytes, std::io::Error> {
                                match result {
                                    Ok(b) => Ok(b),
                                    Err(e) => Ok(Bytes::from(format!(
                                        "data: {{\"error\":\"{}\"}}\n\n",
                                        e
                                    ))),
                                }
                            }));

                        // [NEW] Add a 60-second idle timeout protection for the Claude stream
                        let combined_stream = async_stream::stream! {
                            let mut s = Box::pin(combined_stream);
                            loop {
                                match tokio::time::timeout(std::time::Duration::from_secs(300), s.next()).await {
                                    Ok(Some(item)) => yield item,
                                    Ok(None) => break,
                                    Err(_) => {
                                        tracing::error!("[Claude-SSE] Idle timeout after 300s, terminating stream");
                                        yield Ok::<Bytes, std::io::Error>(Bytes::from("data: {\"type\": \"message_stop\"}\n\ndata: [DONE]\n\n"));
                                        break;
                                    }
                                }
                            }
                        };

                        // Determine the format the client expects
                        if client_wants_stream {
                            // The client already wants a stream, return SSE directly
                            return Response::builder()
                                .status(StatusCode::OK)
                                .header(header::CONTENT_TYPE, "text/event-stream")
                                .header(header::CACHE_CONTROL, "no-cache")
                                .header(header::CONNECTION, "keep-alive")
                                .header("X-Accel-Buffering", "no")
                                .header("X-Account-Email", &email)
                                .header("X-Mapped-Model", &request_with_mapped.model)
                                .header(
                                    "X-Context-Purified",
                                    if is_purified { "true" } else { "false" },
                                )
                                .body(Body::from_stream(combined_stream))
                                .unwrap();
                        } else {
                            // The client wants non-streaming, so we need to collect the full response and convert it to JSON
                            use crate::proxy::mappers::claude::collect_stream_to_json;

                            match collect_stream_to_json(Box::pin(combined_stream)).await {
                                Ok(full_response) => {
                                    info!(
                                        "[{}] ✓ Stream collected and converted to JSON",
                                        trace_id
                                    );
                                    return Response::builder()
                                        .status(StatusCode::OK)
                                        .header(header::CONTENT_TYPE, "application/json")
                                        .header("X-Account-Email", &email)
                                        .header("X-Mapped-Model", &request_with_mapped.model)
                                        .header(
                                            "X-Context-Purified",
                                            if is_purified { "true" } else { "false" },
                                        )
                                        .body(Body::from(
                                            serde_json::to_string(&full_response).unwrap(),
                                        ))
                                        .unwrap();
                                }
                                Err(e) => {
                                    return (
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        format!("Stream collection error: {}", e),
                                    )
                                        .into_response();
                                }
                            }
                        }
                    }

                    None => {
                        tracing::warn!(
                            "[{}] Stream ended immediately (Empty Response), retrying...",
                            trace_id
                        );
                        last_error = "Empty response stream (None)".to_string();
                        continue;
                    }
                }
            } else {
                // Handle the non-streaming response
                let bytes = match response.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        return (
                            StatusCode::BAD_GATEWAY,
                            format!("Failed to read body: {}", e),
                        )
                            .into_response()
                    }
                };

                // Debug print
                if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                    debug!("Upstream Response for Claude request: {}", text);
                }

                let gemini_resp: Value = match serde_json::from_slice(&bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        return (StatusCode::BAD_GATEWAY, format!("Parse error: {}", e))
                            .into_response()
                    }
                };

                // Unwrap the response field (v1internal format)
                let raw = gemini_resp.get("response").unwrap_or(&gemini_resp);

                // Convert into the Gemini Response struct
                let gemini_response: crate::proxy::mappers::claude::models::GeminiResponse =
                    match serde_json::from_value(raw.clone()) {
                        Ok(r) => r,
                        Err(e) => {
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Convert error: {}", e),
                            )
                                .into_response()
                        }
                    };

                // Determine context limit based on model
                let context_limit =
                    crate::proxy::mappers::claude::utils::get_context_limit_for_model(
                        &request_with_mapped.model,
                    );

                // Convert
                // [FIX #765] Pass session_id and model_name for signature caching
                let s_id_owned = session_id.map(|s| s.to_string());
                // Convert
                let claude_response = match transform_response(
                    &gemini_response,
                    scaling_enabled,
                    context_limit,
                    s_id_owned,
                    request_with_mapped.model.clone(),
                    request_with_mapped.messages.len(), // [NEW v4.0.0] Pass message count for rewind detection
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Transform error: {}", e),
                        )
                            .into_response()
                    }
                };

                // [Optimization] Log the closed-loop consumption summary
                let cache_info = if let Some(cached) = claude_response.usage.cache_read_input_tokens
                {
                    format!(", Cached: {}", cached)
                } else {
                    String::new()
                };

                tracing::info!(
                    "[{}] Request finished. Model: {}, Tokens: In {}, Out {}{}",
                    trace_id,
                    request_with_mapped.model,
                    claude_response.usage.input_tokens,
                    claude_response.usage.output_tokens,
                    cache_info
                );

                return (
                    StatusCode::OK,
                    [
                        ("X-Account-Email", email.as_str()),
                        ("X-Mapped-Model", request_with_mapped.model.as_str()),
                    ],
                    Json(claude_response),
                )
                    .into_response();
            }
        }

        // 1. Immediately extract the status code and headers (to prevent response from being moved)
        let status_code = status.as_u16();
        last_status = status;
        let retry_after = response
            .headers()
            .get("Retry-After")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());

        // 2. Obtain the error text and take ownership of the Response
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| format!("HTTP {}", status));
        last_error = format!("HTTP {}: {}", status_code, error_text);
        debug!("[{}] Upstream Error Response: {}", trace_id, error_text);
        if debug_logger::is_enabled(&debug_cfg) {
            let payload = json!({
                "kind": "upstream_response_error",
                "protocol": "anthropic",
                "trace_id": trace_id,
                "original_model": request.model,
                "mapped_model": request_with_mapped.model,
                "request_type": config.request_type,
                "attempt": attempt,
                "status": status_code,
                "upstream_url": upstream_url,
                "account": mask_email(&email),
                "error_text": error_text,
            });
            debug_logger::write_debug_payload(
                &debug_cfg,
                Some(&trace_id),
                "upstream_response_error",
                &payload,
            )
            .await;
        }

        // 3. Mark the rate-limit status (for UI display) - uses the async version to support real-time quota refresh
        // [NEW] Pass in the model actually used, to implement model-level rate limiting so different models' quotas don't interfere with each other
        if status_code == 429
            || status_code == 529
            || status_code == 503
            || status_code == 500
            || status_code == 404
        {
            token_manager
                .mark_rate_limited_async_baseline(
                    &email,
                    status_code,
                    retry_after.as_deref(),
                    &error_text,
                    Some(&request_with_mapped.model),
                )
                .await;
        }

        // 4. Handle a 400 error (Thinking signature invalidated or block-order error)
        // [FIX 2026-08-28] Use case-insensitive matching and cover Google's exact phrasing:
        // "Invalid thought signature." / "thoughtSignature" / "thought_signature"
        let lower_err = error_text.to_lowercase();
        if status_code == 400
            && !retried_without_thinking
            && (lower_err.contains("invalid thought signature")
                || lower_err.contains("invalid `signature`")
                || lower_err.contains("invalid signature")
                || lower_err.contains("thought_signature")
                || lower_err.contains("thoughtsignature")
                || lower_err.contains("thinking.signature: field required")
                || lower_err.contains("thinking.thinking: field required")
                || lower_err.contains("thinking.signature")
                || lower_err.contains("thinking.thinking")
                || lower_err.contains("corrupted thought signature")
                || lower_err.contains("failed to deserialise")
                || lower_err.contains("thinking block")
                || lower_err.contains("found `text`")
                || lower_err.contains("found 'text'")
                || lower_err.contains("must be `thinking`")
                || lower_err.contains("must be 'thinking'"))
        {
            // Existing logic for thinking signature.
            retried_without_thinking = true;

            // Use WARN level, since this shouldn't happen often (already actively filtered)
            tracing::warn!(
                "[{}] Unexpected thinking signature error (should have been filtered). \
                 Retrying with all thinking blocks removed.",
                trace_id
            );

            // [NEW] Append the repair prompt to the last user message
            if let Some(last_msg) = request_for_body.messages.last_mut() {
                if last_msg.role == "user" {
                    let repair_prompt = "\n\n[System Recovery] Your previous output contained an invalid signature. Please regenerate the response without the corrupted signature block.";

                    match &mut last_msg.content {
                        crate::proxy::mappers::claude::models::MessageContent::String(s) => {
                            s.push_str(repair_prompt);
                        }
                        crate::proxy::mappers::claude::models::MessageContent::Array(blocks) => {
                            blocks.push(
                                crate::proxy::mappers::claude::models::ContentBlock::Text {
                                    text: repair_prompt.to_string(),
                                },
                            );
                        }
                    }
                    tracing::debug!("[{}] Appended repair prompt to last user message", trace_id);
                }
            }

            // [IMPROVED] No longer disabling Thinking mode!
            // Since we've already converted historical Thinking Blocks into Text, the current
            // request can be treated as a new Thinking session.
            // Keep the thinking config enabled and let the model regenerate its reasoning, to
            // avoid degrading into a simple "OK" reply.
            // request_for_body.thinking = None;

            // Clean all Thinking Blocks from historical messages, converting them to Text to preserve context
            for msg in request_for_body.messages.iter_mut() {
                if let crate::proxy::mappers::claude::models::MessageContent::Array(blocks) =
                    &mut msg.content
                {
                    let mut new_blocks = Vec::with_capacity(blocks.len());
                    for block in blocks.drain(..) {
                        match block {
                            crate::proxy::mappers::claude::models::ContentBlock::Thinking { thinking, .. } => {
                                // Downgrade to text
                                if !thinking.is_empty() {
                                    tracing::debug!("[Fallback] Converting thinking block to text (len={})", thinking.len());
                                    new_blocks.push(crate::proxy::mappers::claude::models::ContentBlock::Text {
                                        text: thinking
                                    });
                                }
                            },
                            crate::proxy::mappers::claude::models::ContentBlock::RedactedThinking { .. } => {
                                // Redacted thinking isn't useful, just discard it
                            },
                            _ => new_blocks.push(block),
                        }
                    }
                    *blocks = new_blocks;
                }
            }

            // [NEW] Heal session after stripping thinking blocks to prevent "naked ToolResult" rejection
            // This ensures that any ToolResult in history is properly "closed" with synthetic messages
            // if its preceding Thinking block was just converted to Text.
            crate::proxy::mappers::claude::thinking_utils::close_tool_loop_for_thinking(
                &mut request_for_body.messages,
            );

            // Strip the -thinking suffix from the model name
            if request_for_body.model.contains("claude-") {
                let mut m = request_for_body.model.clone();
                m = m.replace("-thinking", "");
                if m.contains("claude-sonnet-4-6-") {
                    m = "claude-sonnet-4-6".to_string();
                } else if m.contains("claude-sonnet-4-5-") {
                    m = "claude-sonnet-4-6".to_string();
                } else if m.contains("claude-opus-4-6-") {
                    m = "claude-opus-4-6".to_string();
                } else if m.contains("claude-opus-4-5-") || m.contains("claude-opus-4-") {
                    m = "claude-opus-4-5".to_string();
                }
                request_for_body.model = m;
            }

            // [FIX] Force a retry: since we've already stripped the thinking block, this is a new,
            // retryable request. Don't use determine_retry_strategy, since it would return NoRetry
            // because retried_without_thinking=true
            if apply_retry_strategy(
                RetryStrategy::FixedDelay(Duration::from_millis(200)),
                attempt,
                max_attempts,
                status_code,
                &trace_id,
            )
            .await
            {
                continue;
            }
        }

        // 5. Uniformly handle all retryable errors
        // [REMOVED] No longer special-cases QUOTA_EXHAUSTED, allowing account rotation
        // The old logic would return directly once the first account's quota was exhausted,
        // preventing "balanced" mode from switching accounts

        // [FIX] On 403, set the is_forbidden status to avoid the account being selected again
        if status_code == 403 {
            // Check for VALIDATION_REQUIRED error - temporarily block account
            if error_text.contains("VALIDATION_REQUIRED")
                || error_text.contains("verify your account")
                || error_text.contains("validation_url")
            {
                tracing::warn!(
                    "[Claude] VALIDATION_REQUIRED detected on account {}, temporarily blocking",
                    email
                );
                let block_minutes = 10i64;
                let block_until = chrono::Utc::now().timestamp() + (block_minutes * 60);
                if let Err(e) = token_manager
                    .set_validation_block_public(&account_id, block_until, &error_text)
                    .await
                {
                    tracing::error!("Failed to set validation block: {}", e);
                }
            }

            // Set the is_forbidden status
            if let Err(e) = token_manager.set_forbidden(&account_id, &error_text).await {
                tracing::error!("Failed to set forbidden status for {}: {}", email, e);
            } else {
                tracing::warn!("[Claude] Account {} marked as forbidden due to 403", email);
            }
        }

        // Determine the retry strategy
        let retry_strategy =
            determine_retry_strategy(status_code, &error_text, retried_without_thinking);

        // Execute the backoff
        if apply_retry_strategy(
            retry_strategy.clone(),
            attempt,
            max_attempts,
            status_code,
            &trace_id,
        )
        .await
        {
            // Determine whether an account rotation is needed
            if !should_rotate_account(status_code, Some(&retry_strategy)) {
                debug!(
                    "[{}] Keeping same account for status {} (Grace Retry or Server Issue)",
                    trace_id, status_code
                );
            }
            continue;
        } else {
            // 5. Enhanced 400 error handling: a friendly Prompt Too Long message
            if status_code == 400
                && (error_text.contains("too long")
                    || error_text.contains("exceeds")
                    || error_text.contains("limit"))
            {
                return (
                    StatusCode::BAD_REQUEST,
                    [("X-Account-Email", email.as_str())],
                    Json(json!({
                        "id": "err_prompt_too_long",
                        "type": "error",
                        "error": {
                            "type": "invalid_request_error",
                            "message": "Prompt is too long (server-side context limit reached).",
                            "suggestion": "Please: 1) Executive '/compact' in Claude Code 2) Reduce conversation history 3) Switch to gemini-1.5-pro (2M context limit)"
                        }
                    }))
                ).into_response();
            }

            // Non-retryable error, return directly
            error!(
                "[{}] Non-retryable error {}: {}",
                trace_id, status_code, error_text
            );
            return (
                status,
                [
                    ("X-Account-Email", email.as_str()),
                    ("X-Mapped-Model", request_with_mapped.model.as_str()),
                ],
                error_text,
            )
                .into_response();
        }
    }

    if let Some(email) = last_email {
        // [FIX] Include X-Mapped-Model in exhaustion error
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Account-Email",
            header::HeaderValue::from_str(&email).unwrap(),
        );
        if let Some(model) = last_mapped_model {
            if let Ok(v) = header::HeaderValue::from_str(&model) {
                headers.insert("X-Mapped-Model", v);
            }
        }

        let error_type = match last_status.as_u16() {
            400 => "invalid_request_error",
            401 => "authentication_error",
            403 => "permission_error",
            429 => "rate_limit_error",
            529 => "overloaded_error",
            _ => "api_error",
        };

        // [FIX] Return 503 on a 403, to prevent the Claude Code client from being kicked back to the login page
        let response_status = if last_status.as_u16() == 403 {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            last_status
        };

        (response_status, headers, Json(json!({
            "type": "error",
            "error": {
                "id": "err_retry_exhausted",
                "type": error_type,
                "message": format!("All {} attempts failed. Last status: {}. Error: {}", max_attempts, last_status, last_error)
            }
        }))).into_response()
    } else {
        // Fallback if no email (e.g. mapping error before token)
        let mut headers = HeaderMap::new();
        if let Some(model) = last_mapped_model {
            if let Ok(v) = header::HeaderValue::from_str(&model) {
                headers.insert("X-Mapped-Model", v);
            }
        }

        let error_type = match last_status.as_u16() {
            400 => "invalid_request_error",
            401 => "authentication_error",
            403 => "permission_error",
            429 => "rate_limit_error",
            529 => "overloaded_error",
            _ => "api_error",
        };

        // [FIX] Return 503 on a 403, to prevent the Claude Code client from being kicked back to the login page
        let response_status = if last_status.as_u16() == 403 {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            last_status
        };

        (response_status, headers, Json(json!({
            "type": "error",
            "error": {
                "id": "err_retry_exhausted",
                "type": error_type,
                "message": format!("All {} attempts failed. Last status: {}. Error: {}", max_attempts, last_status, last_error)
            }
        }))).into_response()
    }
}

/// List available models
pub async fn handle_list_models(State(state): State<AppState>) -> impl IntoResponse {
    use crate::proxy::common::model_mapping::get_all_dynamic_models;

    let only_raw = *state.only_raw_quota_models.read().await;
    let model_ids =
        get_all_dynamic_models(&state.custom_mapping, Some(&state.token_manager), only_raw).await;

    let data: Vec<_> = model_ids
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "object": "model",
                "created": 1706745600,
                "owned_by": "antigravity"
            })
        })
        .collect();

    Json(json!({
        "object": "list",
        "data": data
    }))
}

/// Count tokens (placeholder)
pub async fn handle_count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let zai = state.zai.read().await.clone();
    let zai_enabled =
        zai.enabled && !matches!(zai.dispatch_mode, crate::proxy::ZaiDispatchMode::Off);

    if zai_enabled {
        return crate::proxy::providers::zai_anthropic::forward_anthropic_json(
            &state,
            axum::http::Method::POST,
            "/v1/messages/count_tokens",
            &headers,
            body,
            0, // [NEW v4.0.0] Tokens count doesn't need rewind detection
        )
        .await;
    }

    Json(json!({
        "input_tokens": 0,
        "output_tokens": 0
    }))
    .into_response()
}

#[cfg(test)]
mod opus_variant_tests {
    use crate::proxy::common::variant_mapping;
    use crate::proxy::mappers::claude::models::ThinkingConfig;

    #[test]
    fn claude_opus_preserves_client_budget_when_present() {
        let client_budget = Some(32_768);
        let spec = variant_mapping::resolve("claude-opus-4-6-thinking", client_budget)
            .expect("Claude Opus 4.6 thinking must resolve");
        let request_thinking = ThinkingConfig {
            type_: "enabled".to_string(),
            budget_tokens: Some(spec.effective_thinking_budget(client_budget)),
            effort: None,
        };

        assert_eq!(request_thinking.budget_tokens, client_budget);
    }

    #[test]
    fn claude_opus_falls_back_to_spec_budget_when_client_budget_is_absent() {
        let client_budget = None;
        let spec = variant_mapping::resolve("claude-opus-4-6-thinking", client_budget)
            .expect("Claude Opus 4.6 thinking must resolve");
        let request_thinking = ThinkingConfig {
            type_: "enabled".to_string(),
            budget_tokens: Some(spec.effective_thinking_budget(client_budget)),
            effort: None,
        };

        assert_eq!(request_thinking.budget_tokens, Some(1_024));
    }
}

// Removed the now-defunct simple unit test; a full integration test suite will be added later
/*
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_handle_list_models() {
        // handle_list_models now requires AppState, so the old unit test is skipped here
    }
}
*/

// ===== Background-task detection helper functions =====

/// Background task type
#[derive(Debug, Clone, Copy, PartialEq)]
enum BackgroundTaskType {
    TitleGeneration,    // Title generation
    SimpleSummary,      // Simple summary
    ContextCompression, // Context compression
    PromptSuggestion,   // Prompt suggestion
    SystemMessage,      // System message
    EnvironmentProbe,   // Environment probe
}

/// Title-generation keywords
// [Not translated] "生成标题" / "为对话起个标题" are runtime-matched against actual client
// prompt text (Chinese-locale clients asking to generate a conversation title). Translating
// them would break detection of those real requests, so they are kept byte-identical.
const TITLE_KEYWORDS: &[&str] = &[
    "write a 5-10 word title",
    "Please write a 5-10 word title",
    "Respond with the title",
    "Generate a title for",
    "Create a brief title",
    "title for the conversation",
    "conversation title",
    "生成标题",
    "为对话起个标题",
];

/// Summary-generation keywords
const SUMMARY_KEYWORDS: &[&str] = &[
    "Summarize this coding conversation",
    "Summarize the conversation",
    "Concise summary",
    "in under 50 characters",
    "compress the context",
    "Provide a concise summary",
    "condense the previous messages",
    "shorten the conversation history",
    "extract key points from",
];

/// Suggestion-generation keywords
const SUGGESTION_KEYWORDS: &[&str] = &[
    "prompt suggestion generator",
    "suggest next prompts",
    "what should I ask next",
    "generate follow-up questions",
    "recommend next steps",
    "possible next actions",
];

/// System-message keywords
const SYSTEM_KEYWORDS: &[&str] = &[
    "Warmup",
    "<system-reminder>",
    // Removed: "Caveat: The messages below were generated" - this is a normal Claude Desktop system prompt
    "This is a system message",
];

/// Environment-probe keywords
const PROBE_KEYWORDS: &[&str] = &[
    "check current directory",
    "list available tools",
    "verify environment",
    "test connection",
];

/// Detect a background task and return its task type
fn detect_background_task_type(request: &ClaudeRequest) -> Option<BackgroundTaskType> {
    let last_user_msg = extract_last_user_message_for_detection(request)?;
    let preview = last_user_msg.chars().take(500).collect::<String>();

    // Length filter: background tasks are usually no more than 800 characters
    if last_user_msg.len() > 800 {
        return None;
    }

    // Match by priority
    if matches_keywords(&preview, SYSTEM_KEYWORDS) {
        return Some(BackgroundTaskType::SystemMessage);
    }

    if matches_keywords(&preview, TITLE_KEYWORDS) {
        return Some(BackgroundTaskType::TitleGeneration);
    }

    if matches_keywords(&preview, SUMMARY_KEYWORDS) {
        if preview.contains("in under 50 characters") {
            return Some(BackgroundTaskType::SimpleSummary);
        }
        return Some(BackgroundTaskType::ContextCompression);
    }

    if matches_keywords(&preview, SUGGESTION_KEYWORDS) {
        return Some(BackgroundTaskType::PromptSuggestion);
    }

    if matches_keywords(&preview, PROBE_KEYWORDS) {
        return Some(BackgroundTaskType::EnvironmentProbe);
    }

    None
}

/// Helper function: keyword matching
fn matches_keywords(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|kw| text.contains(kw))
}

/// Helper function: extract the last user message (for detection)
fn extract_last_user_message_for_detection(request: &ClaudeRequest) -> Option<String> {
    request
        .messages
        .iter()
        .rev()
        .filter(|m| m.role == "user")
        .find_map(|m| {
            let content = match &m.content {
                crate::proxy::mappers::claude::models::MessageContent::String(s) => s.to_string(),
                crate::proxy::mappers::claude::models::MessageContent::Array(arr) => arr
                    .iter()
                    .filter_map(|block| match block {
                        crate::proxy::mappers::claude::models::ContentBlock::Text { text } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            };

            if content.trim().is_empty()
                || content.starts_with("Warmup")
                || content.contains("<system-reminder>")
            {
                None
            } else {
                Some(content)
            }
        })
}

/// Select the appropriate model based on the background task type
fn select_background_model(task_type: BackgroundTaskType) -> &'static str {
    match task_type {
        BackgroundTaskType::TitleGeneration => INTERNAL_BACKGROUND_TASK,
        BackgroundTaskType::SimpleSummary => INTERNAL_BACKGROUND_TASK,
        BackgroundTaskType::SystemMessage => INTERNAL_BACKGROUND_TASK,
        BackgroundTaskType::PromptSuggestion => INTERNAL_BACKGROUND_TASK,
        BackgroundTaskType::EnvironmentProbe => INTERNAL_BACKGROUND_TASK,
        BackgroundTaskType::ContextCompression => INTERNAL_BACKGROUND_TASK,
    }
}

// ===== [Issue #467 Fix] Warmup request interception =====

/// Detect whether this is a Warmup request
///
/// Claude Code sends a warmup request every 10 seconds; its characteristics include:
/// 1. The user message content starts with or contains "Warmup"
/// 2. The tool_result content is a "Warmup" error
/// 3. A message-loop pattern: the assistant sends a tool call, the user returns a Warmup error
fn is_warmup_request(request: &ClaudeRequest) -> bool {
    // [FIX] Only check the LATEST message for Warmup characteristics.
    // Scanning history (take(10)) caused a "poisoned session" bug where one historical Warmup
    // message would cause all subsequent user inputs (e.g. "Continue") to be intercepted
    // and replied with "OK".

    if let Some(msg) = request.messages.last() {
        // We only care if the *current* trigger is a Warmup
        match &msg.content {
            crate::proxy::mappers::claude::models::MessageContent::String(s) => {
                // Check if simple text starts with Warmup (and is short)
                if s.trim().starts_with("Warmup") && s.len() < 100 {
                    return true;
                }
            }
            crate::proxy::mappers::claude::models::MessageContent::Array(arr) => {
                for block in arr {
                    match block {
                        crate::proxy::mappers::claude::models::ContentBlock::Text { text } => {
                            let trimmed = text.trim();
                            if trimmed == "Warmup" || trimmed.starts_with("Warmup\n") {
                                return true;
                            }
                        }
                        crate::proxy::mappers::claude::models::ContentBlock::ToolResult {
                            content,
                            is_error,
                            ..
                        } => {
                            // Check tool result errors
                            let content_str = if let Some(s) = content.as_str() {
                                s.to_string()
                            } else {
                                content.to_string()
                            };

                            // If it's an error and starts with Warmup, it's a warmup signal
                            if *is_error == Some(true) && content_str.trim().starts_with("Warmup") {
                                return true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    false
}

/// Create a simulated response for a Warmup request
///
/// Returns a simple response without consuming upstream quota
fn create_warmup_response(request: &ClaudeRequest, is_stream: bool) -> Response {
    let model = &request.model;
    let message_id = format!("msg_warmup_{}", chrono::Utc::now().timestamp_millis());

    if is_stream {
        // Streaming response: send the standard SSE event sequence
        let events = vec![
            // message_start
            format!(
                "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"{}\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"{}\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{{\"input_tokens\":1,\"output_tokens\":0}}}}}}\n\n",
                message_id, model
            ),
            // content_block_start
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n".to_string(),
            // content_block_delta
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}\n\n".to_string(),
            // content_block_stop
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_string(),
            // message_delta
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":1}}\n\n".to_string(),
            // message_stop
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
        ];

        let body = events.join("");

        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .header("X-Warmup-Intercepted", "true")
            .body(Body::from(body))
            .unwrap()
    } else {
        // Non-streaming response
        let response = json!({
            "id": message_id,
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "OK"
            }],
            "model": model,
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1
            }
        });

        (
            StatusCode::OK,
            [("X-Warmup-Intercepted", "true")],
            Json(response),
        )
            .into_response()
    }
}

// ===== [Helper] Synchronous Upstream Call =====
// Reusable function for making non-streaming calls to Gemini API
// Used by Layer 3 and potentially other internal operations

/// Call Gemini API synchronously and return the response text
///
/// This is used for internal operations that need to wait for a complete response,
/// such as generating summaries or other background tasks.
async fn call_gemini_sync(
    model: &str,
    request: &ClaudeRequest,
    token_manager: &Arc<crate::proxy::TokenManager>,
    trace_id: &str,
) -> Result<String, String> {
    // Get token and transform request
    let (access_token, project_id, _, account_id, _wait_ms) = token_manager
        .get_token("gemini", false, None, model)
        .await
        .map_err(|e| format!("Failed to get account: {}", e))?;

    let token_obj = token_manager.get_token_by_id(&account_id);
    let gemini_body = crate::proxy::mappers::claude::transform_claude_request_in(
        request,
        &project_id,
        false,
        Some(account_id.as_str()),
        trace_id,
        token_obj.as_ref(),
    )
    .map_err(|e| format!("Failed to transform request: {}", e))?;

    // Call Gemini API
    let upstream_url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
        model
    );

    debug!("[{}] Calling Gemini API: {}", trace_id, model);

    let response = reqwest::Client::new()
        .post(&upstream_url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&gemini_body)
        .send()
        .await
        .map_err(|e| format!("API call failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "API returned {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }

    let gemini_response: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    // Extract text from response
    gemini_response
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.get(0))
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Failed to extract text from response".to_string())
}

// ===== [Layer 3] Fork Conversation + XML Summary =====
// This is the ultimate context compression strategy
// Borrowed from Practical-Guide-to-Context-Engineering + Claude Code official practice

/// Try to compress context by generating an XML summary and forking the conversation
///
/// This function:
/// 1. Extracts the last valid thinking signature
/// 2. Calls a cheap model (gemini-2.5-flash-lite) to generate XML summary
/// 3. Creates a new message sequence with summary as prefix
/// 4. Preserves the signature in the summary
/// 5. Returns the forked request
///
/// Returns Ok(forked_request) on success, Err(error_message) on failure
async fn try_compress_with_summary(
    original_request: &ClaudeRequest,
    trace_id: &str,
    token_manager: &Arc<crate::proxy::TokenManager>,
) -> Result<ClaudeRequest, String> {
    info!(
        "[{}] [Layer-3] Starting context compression with XML summary",
        trace_id
    );

    // 1. Extract last valid signature
    let last_signature = ContextManager::extract_last_valid_signature(&original_request.messages);

    if let Some(ref sig) = last_signature {
        debug!(
            "[{}] [Layer-3] Extracted signature (len: {})",
            trace_id,
            sig.len()
        );
    }

    // 2. Build summary request
    let mut summary_messages = original_request.messages.clone();

    // Add instruction to include signature in summary
    let signature_instruction = if let Some(ref sig) = last_signature {
        format!("\n\n**CRITICAL**: The last thinking signature is:\n```\n{}\n```\nYou MUST include this EXACTLY in the <latest_thinking_signature> section.", sig)
    } else {
        "\n\n**Note**: No thinking signature found in history. Leave <latest_thinking_signature> empty.".to_string()
    };

    // Append summary request as the last user message
    summary_messages.push(Message {
        role: "user".to_string(),
        content: MessageContent::String(format!(
            "{}{}",
            CONTEXT_SUMMARY_PROMPT, signature_instruction
        )),
    });

    let summary_request = ClaudeRequest {
        model: INTERNAL_BACKGROUND_TASK.to_string(),
        messages: summary_messages,
        system: None,
        stream: false,
        max_tokens: Some(8000),
        temperature: Some(0.3),
        tools: None,
        thinking: None,
        metadata: None,
        top_p: None,
        top_k: None,
        output_config: None,
        size: None,
        quality: None,
    };

    debug!(
        "[{}] [Layer-3] Calling {} for summary generation",
        trace_id, INTERNAL_BACKGROUND_TASK
    );

    // 3. Call upstream using helper function (reuse existing infrastructure)
    let xml_summary = call_gemini_sync(
        INTERNAL_BACKGROUND_TASK,
        &summary_request,
        token_manager,
        trace_id,
    )
    .await?;

    info!(
        "[{}] [Layer-3] Generated XML summary (len: {} chars)",
        trace_id,
        xml_summary.len()
    );

    // 4. Create forked conversation with summary as prefix
    // Wrap text inside a ContentBlock::Text and attach cache_control to freeze it in upstream's Prompt Cache
    let mut forked_messages = vec![
        Message {
            role: "user".to_string(),
            content: MessageContent::Array(vec![
                crate::proxy::mappers::claude::models::ContentBlock::Text {
                    text: format!(
                        "Context has been compressed. Here is the structured summary of our conversation history:\n\n{}",
                        xml_summary
                    ),
                }
            ]),
        },
        Message {
            role: "assistant".to_string(),
            content: MessageContent::String(
                "I have reviewed the compressed context summary. I understand the current state and will continue from here.".to_string()
            ),
        },
    ];

    // 5. Append the user's latest message (if exists and is not the summary request)
    if let Some(last_msg) = original_request.messages.last() {
        if last_msg.role == "user" {
            // Check if it's not the summary instruction we just added
            if !matches!(&last_msg.content, MessageContent::String(s) if s.contains(CONTEXT_SUMMARY_PROMPT))
            {
                forked_messages.push(last_msg.clone());
            }
        }
    }

    info!(
        "[{}] [Layer-3] Fork successful: {} messages → {} messages",
        trace_id,
        original_request.messages.len(),
        forked_messages.len()
    );

    // 6. Return forked request
    Ok(ClaudeRequest {
        model: original_request.model.clone(),
        messages: forked_messages,
        system: original_request.system.clone(),
        stream: original_request.stream,
        max_tokens: original_request.max_tokens,
        temperature: original_request.temperature,
        tools: original_request.tools.clone(),
        thinking: original_request.thinking.clone(),
        metadata: original_request.metadata.clone(),
        top_p: original_request.top_p,
        top_k: original_request.top_k,
        output_config: original_request.output_config.clone(),
        size: original_request.size.clone(),
        quality: original_request.quality.clone(),
    })
}

/// Injects cache_control ephemeral trigger to first message's content block if it's the XML summary
fn inject_cache_control_to_forked_summary(body: &mut serde_json::Value) {
    if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        if !messages.is_empty() {
            let first_msg = &mut messages[0];
            if let Some(content) = first_msg.get_mut("content") {
                if let Some(content_arr) = content.as_array_mut() {
                    if !content_arr.is_empty() {
                        let is_summary = content_arr[0]
                            .get("text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.contains("Context has been compressed"))
                            .unwrap_or(false);

                        if is_summary {
                            if let Some(obj) = content_arr[0].as_object_mut() {
                                obj.insert(
                                    "cache_control".to_string(),
                                    serde_json::json!({
                                        "type": "ephemeral"
                                    }),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
