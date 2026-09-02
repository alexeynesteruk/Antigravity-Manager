use serde::{Deserialize, Serialize};

/// A single quota bucket (corresponds to one bucket in retrieveUserQuotaSummary)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaBucket {
    /// Bucket ID, e.g. "gemini-weekly" / "gemini-5h" / "3p-weekly" / "3p-5h"
    pub bucket_id: String,
    /// Window type: "weekly" / "5h"
    pub window: String,
    /// Remaining fraction 0.0-1.0
    pub remaining_fraction: f64,
    /// Reset time (RFC3339)
    pub reset_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A model group (e.g. Gemini Models / Claude and GPT models)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaGroup {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub buckets: Vec<QuotaBucket>,
}

/// Model quota information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelQuota {
    pub name: String,
    pub percentage: i32, // Remaining percentage 0-100
    pub reset_time: String,

    // -- Dynamic parameter parsing and persistence --
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_images: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_mime_types: Option<std::collections::HashMap<String, bool>>,
}

/// Quota data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaData {
    pub models: Vec<ModelQuota>,
    pub last_updated: i64,
    #[serde(default)]
    pub is_forbidden: bool,
    /// Reason access is forbidden (403 details)
    #[serde(default)]
    pub forbidden_reason: Option<String>,
    /// Subscription tier (FREE/PRO/ULTRA)
    #[serde(default)]
    pub subscription_tier: Option<String>,
    /// Model deprecation redirect rules table (old_model_id -> new_model_id)
    #[serde(default)]
    pub model_forwarding_rules: std::collections::HashMap<String, String>,
    /// Quota summary per model group (weekly + 5h dual windows), from retrieveUserQuotaSummary
    #[serde(default)]
    pub quota_groups: Option<Vec<QuotaGroup>>,
}

impl QuotaData {
    pub fn new() -> Self {
        Self {
            models: Vec::new(),
            last_updated: chrono::Utc::now().timestamp(),
            is_forbidden: false,
            forbidden_reason: None,
            subscription_tier: None,
            model_forwarding_rules: std::collections::HashMap::new(),
            quota_groups: None,
        }
    }

    pub fn add_model(&mut self, model: ModelQuota) {
        self.models.push(model);
    }
}

impl Default for QuotaData {
    fn default() -> Self {
        Self::new()
    }
}
