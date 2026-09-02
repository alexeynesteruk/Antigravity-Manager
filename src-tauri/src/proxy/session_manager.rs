use crate::proxy::mappers::claude::models::{ClaudeRequest, MessageContent};
use crate::proxy::mappers::openai::models::{OpenAIContent, OpenAIRequest};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Session manager utility
pub struct SessionManager;

impl SessionManager {
    /// Generate a stable session fingerprint from a Claude request
    ///
    /// Design rationale:
    /// - Only hash the content of the first user message; don't mix in the model name or timestamp
    /// - Ensure all turns of the same conversation use the same session_id
    /// - Maximize the prompt caching hit rate
    ///
    /// Priority:
    /// 1. metadata.user_id (explicitly provided by the client)
    /// 2. SHA256 hash of the first user message
    pub fn extract_session_id(request: &ClaudeRequest) -> String {
        // 1. Prefer the user_id in metadata
        if let Some(metadata) = &request.metadata {
            if let Some(user_id) = &metadata.user_id {
                if !user_id.is_empty() && !user_id.contains("session-") {
                    tracing::debug!("[SessionManager] Using explicit user_id: {}", user_id);
                    return user_id.clone();
                }
            }
        }

        // 2. Fallback: SHA256 hash based on the first user message
        let mut hasher = Sha256::new();

        let mut content_found = false;
        for msg in &request.messages {
            if msg.role != "user" {
                continue;
            }

            let text = match &msg.content {
                MessageContent::String(s) => s.clone(),
                MessageContent::Array(blocks) => blocks
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

            let clean_text = text.trim();
            // [FIX #1732] Lower the admission threshold (10 -> 3) to ensure even short messages generate a stable session anchor
            // Also exclude messages containing system markers to prevent ID drift caused by protocol injection
            if clean_text.len() >= 3
                && !clean_text.contains("<system-reminder>")
                && !clean_text.contains("[System")
            {
                hasher.update(clean_text.as_bytes());
                content_found = true;
                break; // Always anchor on the first valid user message
            }
        }

        if !content_found {
            // If no meaningful content was found, fall back to hashing the last message
            if let Some(last_msg) = request.messages.last() {
                hasher.update(format!("{:?}", last_msg.content).as_bytes());
            }
        }

        let hash = format!("{:x}", hasher.finalize());
        let sid = format!("sid-{}", &hash[..16]);

        tracing::debug!(
            "[SessionManager] Generated session_id: {} (content_found: {}, model: {})",
            sid,
            content_found,
            request.model
        );
        sid
    }

    /// Generate a stable session fingerprint from an OpenAI request
    pub fn extract_openai_session_id(request: &OpenAIRequest) -> String {
        let mut hasher = Sha256::new();

        let mut content_found = false;
        for msg in &request.messages {
            if msg.role != "user" {
                continue;
            }
            if let Some(content) = &msg.content {
                let text = match content {
                    OpenAIContent::String(s) => s.clone(),
                    OpenAIContent::Array(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            crate::proxy::mappers::openai::models::OpenAIContentBlock::Text {
                                text,
                            } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                };

                let clean_text = text.trim();
                if clean_text.len() > 10 && !clean_text.contains("<system-reminder>") {
                    hasher.update(clean_text.as_bytes());
                    content_found = true;
                    break;
                }
            }
        }

        if !content_found {
            if let Some(last_msg) = request.messages.last() {
                hasher.update(format!("{:?}", last_msg.content).as_bytes());
            }
        }

        let hash = format!("{:x}", hasher.finalize());
        let sid = format!("sid-{}", &hash[..16]);
        tracing::debug!("[SessionManager-OpenAI] Generated fingerprint: {}", sid);
        sid
    }

    /// Generate a stable session fingerprint from a native Gemini request (JSON)
    pub fn extract_gemini_session_id(request: &Value, _model_name: &str) -> String {
        let mut hasher = Sha256::new();

        let mut content_found = false;
        if let Some(contents) = request.get("contents").and_then(|v| v.as_array()) {
            for content in contents {
                if content.get("role").and_then(|v| v.as_str()) != Some("user") {
                    continue;
                }

                if let Some(parts) = content.get("parts").and_then(|v| v.as_array()) {
                    let mut text_parts = Vec::new();
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            text_parts.push(text);
                        }
                    }

                    let combined_text = text_parts.join(" ");
                    let clean_text = combined_text.trim();
                    if clean_text.len() > 10 && !clean_text.contains("<system-reminder>") {
                        hasher.update(clean_text.as_bytes());
                        content_found = true;
                        break;
                    }
                }
            }
        }

        if !content_found {
            // Fallback: summarize the first user part of the whole body
            hasher.update(request.to_string().as_bytes());
        }

        let hash = format!("{:x}", hasher.finalize());
        let sid = format!("sid-{}", &hash[..16]);
        tracing::debug!("[SessionManager-Gemini] Generated fingerprint: {}", sid);
        sid
    }
}
