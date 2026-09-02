// OpenAI data models

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenAIRequest {
    pub model: String,
    #[serde(default)]
    pub messages: Vec<OpenAIMessage>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub n: Option<u32>, // [NEW] Support multiple candidate result counts
    #[serde(rename = "max_tokens")]
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    #[serde(rename = "top_p")]
    pub top_p: Option<f64>,
    #[serde(rename = "presence_penalty")]
    pub presence_penalty: Option<f64>,
    #[serde(rename = "frequency_penalty")]
    pub frequency_penalty: Option<f64>,
    pub seed: Option<i64>,
    pub stop: Option<Value>,
    pub response_format: Option<ResponseFormat>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(rename = "tool_choice")]
    pub tool_choice: Option<Value>,
    #[serde(rename = "parallel_tool_calls")]
    pub parallel_tool_calls: Option<bool>,
    // Codex proprietary fields
    pub instructions: Option<String>,
    pub input: Option<Value>,
    // [NEW] Image generation parameters (for Chat API compatibility)
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub quality: Option<String>,
    #[serde(default, rename = "personGeneration")]
    pub person_generation: Option<String>,
    // [NEW] Thinking/Extended Thinking support (compatible with the Anthropic/Claude protocol)
    #[serde(default)]
    pub thinking: Option<ThinkingConfig>,
    // [NEW] Direct imageSize support (for Gemini native parameter)
    #[serde(default, rename = "imageSize")]
    pub image_size: Option<String>,
}

/// Thinking configuration (compatible with the Anthropic and OpenAI extension protocols)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub thinking_type: Option<String>, // "enabled", "disabled", or "adaptive"
    #[serde(rename = "budget_tokens", alias = "budgetTokens")]
    pub budget_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>, // "low", "high", or "max"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFormatJSONSchema {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFormat {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<ResponseFormatJSONSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum OpenAIContent {
    String(String),
    Array(Vec<OpenAIContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum OpenAIContentBlock {
    #[serde(rename = "text", alias = "input_text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: OpenAIImageUrl },
    #[serde(rename = "audio_url")]
    AudioUrl { audio_url: AudioUrlContent },
    // [NEW] OpenAI's official multimodal audio input: {"type":"input_audio","input_audio":{"data":"<base64>","format":"wav"}}
    #[serde(rename = "input_audio", alias = "audio")]
    InputAudio { input_audio: OpenAIInputAudio },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAIImageUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioUrlContent {
    pub url: String,
    /// Optional explicit MIME/format (e.g. "audio/wav" or "wav"); inferred from the data URL / extension when absent
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "mimeType",
        alias = "mime_type",
        alias = "format"
    )]
    pub mime_type: Option<String>,
}

/// OpenAI `input_audio` content block: base64 audio + format identifier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAIInputAudio {
    /// Base64-encoded audio data (also accepts a data: URL)
    pub data: String,
    /// "wav" | "mp3" | "m4a" | "ogg" | "flac" | "aiff" ... a full MIME type is also accepted
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "mimeType", alias = "mime_type")]
    pub format: Option<String>,
}

impl OpenAIInputAudio {
    /// The normalized Gemini MIME type
    pub fn mime_type(&self) -> String {
        crate::proxy::audio::normalize_audio_mime(self.format.as_deref().unwrap_or("mp3"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenAIContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolFunction>,

    // [NEW] Fields for apply_patch_call
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<ApplyPatchOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyPatchOperation {
    pub r#type: String,
    pub diff: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAIUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: OpenAIMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
    #[serde(skip)]
    pub input_tokens_by_modality: Option<Value>,
    #[serde(skip)]
    pub raw_output_tokens: Option<u32>,
    #[serde(skip)]
    pub total_thought_tokens: Option<u32>,
    #[serde(skip)]
    pub total_tool_use_tokens: Option<u32>,
    #[serde(skip)]
    pub gemini_total_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionTokensDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
}

impl OpenAIUsage {
    pub fn to_responses_usage_value(&self) -> Value {
        let cached_tokens = self
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
            .unwrap_or(0);
        let reasoning_tokens = self
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens)
            .unwrap_or(0);

        json!({
            "input_tokens": self.prompt_tokens,
            "input_tokens_details": {
                "cached_tokens": cached_tokens
            },
            "output_tokens": self.completion_tokens,
            "output_tokens_details": {
                "reasoning_tokens": reasoning_tokens
            },
            "total_tokens": self.total_tokens
        })
    }
}
