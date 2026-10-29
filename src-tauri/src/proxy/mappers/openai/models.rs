// OpenAI 数据模型

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
    pub n: Option<u32>, // [NEW] 支持多候选结果数量
    #[serde(
        default,
        rename = "max_tokens",
        alias = "max_completion_tokens",
        alias = "maxCompletionTokens",
        alias = "maxTokens",
        alias = "max_output_tokens",
        alias = "maxOutputTokens"
    )]
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
    pub text: Option<ResponsesTextConfig>,
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
    // [NEW] Thinking/Extended Thinking 支持 (兼容 Anthropic/Claude 协议)
    #[serde(default)]
    pub thinking: Option<ThinkingConfig>,
    // Codex Responses API reasoning controls. Applied only to tiered Flash models.
    #[serde(default)]
    pub reasoning: Option<ReasoningConfig>,
    // [NEW] OpenAI o1/o3/o4 reasoning_effort (and aliases) support
    #[serde(
        default,
        rename = "reasoning_effort",
        alias = "reasoningEffort",
        alias = "thinkingLevel",
        alias = "thinking_level"
    )]
    pub reasoning_effort: Option<String>,
    // [NEW] Direct imageSize support (for Gemini native parameter)
    #[serde(default, rename = "imageSize")]
    pub image_size: Option<String>,
    /// Client/session id used to store and restore full thinking blocks.
    #[serde(default, rename = "session_id")]
    pub session_id: Option<String>,
    // [NEW] OpenAI stream_options support (include_usage gate)
    #[serde(default, rename = "stream_options")]
    pub stream_options: Option<StreamOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

/// Thinking 配置 (兼容 Anthropic 和 OpenAI 扩展协议)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub thinking_type: Option<String>, // "enabled", "disabled", or "adaptive"
    #[serde(
        default,
        rename = "budget_tokens",
        alias = "budgetTokens",
        alias = "max_tokens",
        alias = "maxTokens"
    )]
    pub budget_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>, // "low", "high", or "max"
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReasoningConfig {
    pub effort: Option<String>,
    #[serde(
        default,
        rename = "max_tokens",
        alias = "maxTokens",
        alias = "budget_tokens",
        alias = "budgetTokens"
    )]
    pub max_tokens: Option<u32>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesTextConfig {
    pub format: Option<ResponsesTextFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesTextFormat {
    pub r#type: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub schema: Option<Value>,
    pub strict: Option<bool>,
}

impl OpenAIRequest {
    pub fn effective_response_format(&self) -> Option<ResponseFormat> {
        self.response_format.clone().or_else(|| {
            let format = self.text.as_ref()?.format.as_ref()?;
            match format.r#type.as_str() {
                "json_object" => Some(ResponseFormat {
                    r#type: "json_object".into(),
                    json_schema: None,
                }),
                "json_schema" => Some(ResponseFormat {
                    r#type: "json_schema".into(),
                    json_schema: Some(ResponseFormatJSONSchema {
                        name: format.name.clone(),
                        description: format.description.clone(),
                        schema: format.schema.clone(),
                        strict: format.strict,
                    }),
                }),
                _ => None,
            }
        })
    }

    pub fn expects_json_output(&self) -> bool {
        self.effective_response_format()
            .is_some_and(|f| matches!(f.r#type.as_str(), "json_object" | "json_schema"))
    }

    pub fn validate_response_format(&self) -> Result<(), String> {
        let Some(format) = self.effective_response_format() else {
            return Ok(());
        };
        if format.r#type != "json_schema" {
            return Ok(());
        }
        if format
            .json_schema
            .as_ref()
            .and_then(|s| s.schema.as_ref())
            .is_none_or(Value::is_null)
        {
            return Err("json_schema output requires a schema".into());
        }
        let tools_disabled = self.tool_choice.as_ref().and_then(Value::as_str) == Some("none");
        if !tools_disabled && self.tools.as_ref().is_some_and(|tools| !tools.is_empty()) {
            return Err("This upstream does not support json_schema output together with active tools; use tool_choice: none or request these in separate turns".into());
        }
        Ok(())
    }
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
    // [NEW] OpenAI 官方多模态音频入参: {"type":"input_audio","input_audio":{"data":"<base64>","format":"wav"}}
    #[serde(rename = "input_audio", alias = "audio")]
    InputAudio { input_audio: OpenAIInputAudio },
    // [NEW] 视频多模态输入: {"type":"video_url","video_url":{"url":"..."}}
    #[serde(rename = "video_url")]
    VideoUrl { video_url: OpenAIVideoUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAIVideoUrl {
    pub url: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "mimeType",
        alias = "mime_type",
        alias = "format"
    )]
    pub mime_type: Option<String>,
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
    /// 可选的显式 MIME/格式 (如 "audio/wav" 或 "wav")，缺省时从 data URL / 扩展名推断
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "mimeType",
        alias = "mime_type",
        alias = "format"
    )]
    pub mime_type: Option<String>,
}

/// OpenAI `input_audio` 内容块: base64 音频 + 格式标识
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAIInputAudio {
    /// base64 编码的音频数据 (也兼容传入 data: URL)
    pub data: String,
    /// "wav" | "mp3" | "m4a" | "ogg" | "flac" | "aiff" ... 亦接受完整 MIME
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "mimeType",
        alias = "mime_type"
    )]
    pub format: Option<String>,
}

impl OpenAIInputAudio {
    /// 归一化后的 Gemini MIME 类型
    pub fn mime_type(&self) -> String {
        crate::proxy::audio::normalize_audio_mime(self.format.as_deref().unwrap_or("mp3"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenAIMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenAIContent>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "thought")]
    pub reasoning_content: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "thoughtSignature",
        alias = "thought_signature"
    )]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolFunction>,

    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "thoughtSignature",
        alias = "thought_signature"
    )]
    pub signature: Option<String>,

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
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
    pub fn merge(&mut self, reported: Self) {
        self.prompt_tokens = reported.prompt_tokens.or(self.prompt_tokens);
        self.completion_tokens = reported.completion_tokens.or(self.completion_tokens);
        self.prompt_tokens_details = reported
            .prompt_tokens_details
            .or(self.prompt_tokens_details.take());
        self.completion_tokens_details = reported
            .completion_tokens_details
            .or(self.completion_tokens_details.take());
        self.input_tokens_by_modality = reported
            .input_tokens_by_modality
            .or(self.input_tokens_by_modality.take());
        self.raw_output_tokens = reported.raw_output_tokens.or(self.raw_output_tokens);
        self.total_thought_tokens = reported.total_thought_tokens.or(self.total_thought_tokens);
        self.total_tool_use_tokens = reported
            .total_tool_use_tokens
            .or(self.total_tool_use_tokens);
        self.gemini_total_tokens = reported.gemini_total_tokens.or(self.gemini_total_tokens);
        if let Some(raw_output) = self.raw_output_tokens {
            self.completion_tokens = Some(
                self.total_thought_tokens
                    .map_or(raw_output, |thoughts| raw_output.saturating_add(thoughts)),
            );
        }
        self.total_tokens = self.gemini_total_tokens;
    }

    pub fn to_responses_usage_value(&self) -> Value {
        let cached_tokens = self
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens);
        let reasoning_tokens = self
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens);

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

impl From<&crate::proxy::pipeline::CanonicalUsage> for OpenAIUsage {
    fn from(c: &crate::proxy::pipeline::CanonicalUsage) -> Self {
        Self {
            prompt_tokens: c.total_input_tokens,
            completion_tokens: c.output_tokens,
            total_tokens: c.total_tokens,
            prompt_tokens_details: c.cached_tokens.map(|cached| PromptTokensDetails {
                cached_tokens: Some(cached),
            }),
            completion_tokens_details: c.reasoning_tokens.map(|reasoning| {
                CompletionTokensDetails {
                    reasoning_tokens: Some(reasoning),
                }
            }),
            input_tokens_by_modality: None,
            raw_output_tokens: None,
            total_thought_tokens: c.reasoning_tokens,
            total_tool_use_tokens: None,
            gemini_total_tokens: c.total_tokens,
        }
    }
}
