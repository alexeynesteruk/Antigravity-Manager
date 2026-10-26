use super::{quota::QuotaData, token::TokenData};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub(crate) fn default_priority() -> u8 {
    50
}

pub(crate) fn validate_priority(priority: u8) -> Result<(), String> {
    if !(1..=100).contains(&priority) {
        return Err("priority must be an integer between 1 and 100".to_string());
    }
    Ok(())
}

pub(crate) fn deserialize_priority<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u8, D::Error> {
    let priority = u8::deserialize(deserializer)?;
    validate_priority(priority).map_err(serde::de::Error::custom)?;
    Ok(priority)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveLimitStatus {
    pub model: String,
    pub status: u16,
    pub reason: String,
    pub until: i64,
    pub detected_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Account data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    /// Lower values are selected first when assigning a new proxy account.
    #[serde(
        default = "default_priority",
        deserialize_with = "deserialize_priority"
    )]
    pub priority: u8,
    pub token: TokenData,
    /// Optional device fingerprint, used to pin machine information when switching accounts
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_profile: Option<DeviceProfile>,
    /// Device fingerprint history (recorded on generation/collection), excludes the baseline
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub device_history: Vec<DeviceProfileVersion>,
    pub quota: Option<QuotaData>,
    /// Disabled accounts are ignored by the proxy token pool (e.g. revoked refresh_token -> invalid_grant).
    #[serde(default)]
    pub disabled: bool,
    /// Optional human-readable reason for disabling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// Unix timestamp when the account was disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<i64>,
    /// User manually disabled proxy feature (does not affect app usage).
    #[serde(default)]
    pub proxy_disabled: bool,
    /// Optional human-readable reason for proxy disabling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_disabled_reason: Option<String>,
    /// Unix timestamp when the proxy was disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_disabled_at: Option<i64>,
    /// List of models disabled due to quota protection [NEW #621]
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub protected_models: HashSet<String>,
    /// Temporary live upstream throttles observed from actual generation requests.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub live_limited_models: HashMap<String, LiveLimitStatus>,
    /// [NEW] 403 validation blocked status (VALIDATION_REQUIRED)
    #[serde(default)]
    pub validation_blocked: bool,
    /// [NEW] Validation block expiry timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_blocked_until: Option<i64>,
    /// [NEW] Validation block reason
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_blocked_reason: Option<String>,
    /// [NEW] Validation link URL (#1522)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_url: Option<String>,
    pub created_at: i64,
    pub last_used: i64,
    /// Bound proxy ID (None = use the global proxy pool)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_id: Option<String>,
    /// Proxy binding time
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_bound_at: Option<i64>,
    /// User custom label
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_label: Option<String>,
}

impl Account {
    pub fn new(id: String, email: String, token: TokenData) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id,
            email,
            name: None,
            priority: default_priority(),
            token,
            device_profile: None,
            device_history: Vec::new(),
            quota: None,
            disabled: false,
            disabled_reason: None,
            disabled_at: None,
            proxy_disabled: false,
            proxy_disabled_reason: None,
            proxy_disabled_at: None,
            protected_models: HashSet::new(),
            live_limited_models: HashMap::new(),
            validation_blocked: false,
            validation_blocked_until: None,
            validation_blocked_reason: None,
            validation_url: None,
            created_at: now,
            last_used: now,
            proxy_id: None,
            proxy_bound_at: None,
            custom_label: None,
        }
    }

    pub fn update_last_used(&mut self) {
        self.last_used = chrono::Utc::now().timestamp();
    }

    pub fn update_quota(&mut self, mut quota: QuotaData) {
        if let Some(existing) = &self.quota {
            quota.merge_previous_observations(existing);
        } else {
            quota.ensure_subscription_tier();
        }
        self.quota = Some(quota);
    }
}

/// Account index data (accounts.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountIndex {
    pub version: String,
    pub accounts: Vec<AccountSummary>,
    pub current_account_id: Option<String>,
    #[serde(default)]
    pub current_target_ide: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_classic_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_ide_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_agy_account_id: Option<String>,
}

/// 客户端独立当前账号状态（同时指示经典款、Antigravity IDE、Antigravity CLI 三端状态）
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ActiveClientsStatus {
    pub classic_account_id: Option<String>,
    pub classic_email: Option<String>,
    pub ide_account_id: Option<String>,
    pub ide_email: Option<String>,
    pub agy_account_id: Option<String>,
    pub agy_email: Option<String>,
}

/// Account summary information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSummary {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub proxy_disabled: bool,
    /// List of protected models [NEW] used by the UI to show a lock icon
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub protected_models: HashSet<String>,
    pub created_at: i64,
    pub last_used: i64,
}

impl AccountIndex {
    pub fn new() -> Self {
        Self {
            version: "2.0".to_string(),
            accounts: Vec::new(),
            current_account_id: None,
            current_target_ide: None,
            current_classic_account_id: None,
            current_ide_account_id: None,
            current_agy_account_id: None,
        }
    }
}

impl Default for AccountIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Device fingerprint (telemetry-related fields in storage.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub machine_id: String,
    pub mac_machine_id: String,
    pub dev_device_id: String,
    pub sqm_id: String,
}

/// Fingerprint history version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceProfileVersion {
    pub id: String,
    pub created_at: i64,
    pub label: String,
    pub profile: DeviceProfile,
    #[serde(default)]
    pub is_current: bool,
}

/// Exported account item (used for backup/migration)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountExportItem {
    pub email: String,
    pub refresh_token: String,
}

/// Export accounts response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountExportResponse {
    pub accounts: Vec<AccountExportItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_priority_defaults_and_rejects_invalid_values() {
        let token = TokenData::new(
            "test".into(),
            "test".into(),
            3600,
            None,
            None,
            None,
            false,
            None,
        );
        let account = Account::new("test".into(), "test@test.invalid".into(), token);
        assert_eq!(account.priority, 50);
        let mut json = serde_json::to_value(account).unwrap();
        json.as_object_mut().unwrap().remove("priority");
        assert_eq!(
            serde_json::from_value::<Account>(json.clone())
                .unwrap()
                .priority,
            50
        );
        for priority in [1, 100] {
            json["priority"] = serde_json::json!(priority);
            assert_eq!(
                serde_json::from_value::<Account>(json.clone())
                    .unwrap()
                    .priority,
                priority
            );
        }
        for value in [
            serde_json::json!(0),
            serde_json::json!(101),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("50"),
            serde_json::Value::Null,
        ] {
            json["priority"] = value;
            assert!(serde_json::from_value::<Account>(json.clone()).is_err());
        }
        assert!(
            crate::modules::account::update_account_priority("missing", 0)
                .unwrap_err()
                .contains("priority")
        );
        assert!(
            crate::modules::account::update_account_priority("missing", 101)
                .unwrap_err()
                .contains("priority")
        );
    }
}
