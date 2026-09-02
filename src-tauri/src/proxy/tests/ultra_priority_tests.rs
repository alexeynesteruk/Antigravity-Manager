//! Ultra Priority Tests for High-End Models (Opus 4.6/4.5)
//!
//! These tests verify the logic that prioritizes Ultra accounts for high-end models (e.g. Claude Opus 4.6/4.5).
//!
//! ## Background
//! A user's account pool contains many Gemini Pro accounts and a small number of Ultra accounts. When requesting
//! the Claude Opus 4.6 model, a quota-first selection strategy might pick a Pro account, but Pro accounts
//! cannot access Opus 4.6, causing the API to return an error.
//!
//! ## Solution
//! When a user requests a high-end model, prefer Ultra accounts; only fall back to Pro/Free accounts when no Ultra account is available.
//!
//! ## Test Coverage
//! - `test_is_ultra_required_model`: verifies the model detection logic
//! - `test_ultra_priority_for_high_end_models`: verifies Ultra takes priority over Pro (even if Pro has a higher quota)
//! - `test_ultra_accounts_sorted_by_quota`: verifies sorting by quota among Ultra accounts
//! - `test_full_sorting_mixed_accounts`: verifies full sorting of a mixed account pool

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::proxy::token_manager::ProxyToken;

/// Create a ProxyToken for testing
fn create_test_token(
    email: &str,
    tier: Option<&str>,
    health_score: f32,
    reset_time: Option<i64>,
    remaining_quota: Option<i32>,
    supported_models: Vec<&str>,
) -> ProxyToken {
    let mut model_quotas = HashMap::new();
    // Simulate quota: give all supported models the same remaining quota
    for m in supported_models {
        model_quotas.insert(m.to_string(), remaining_quota.unwrap_or(100));
    }

    ProxyToken {
        account_id: email.to_string(),
        access_token: "test_token".to_string(),
        refresh_token: "test_refresh".to_string(),
        expires_in: 3600,
        timestamp: chrono::Utc::now().timestamp() + 3600,
        email: email.to_string(),
        account_path: PathBuf::from("/tmp/test"),
        project_id: None,
        subscription_tier: tier.map(|s| s.to_string()),
        remaining_quota,
        protected_models: HashSet::new(),
        health_score,
        reset_time,
        validation_blocked: false,
        validation_blocked_until: 0,
        validation_url: None,
        model_quotas,
        model_limits: std::collections::HashMap::new(),
    }
}

/// List of high-end models that require an Ultra account
const ULTRA_REQUIRED_MODELS: &[&str] = &[
    "claude-opus-4-6",
    "claude-opus-4-5",
    "opus", // wildcard match
];

/// Check whether a model requires an Ultra account
fn is_ultra_required_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    ULTRA_REQUIRED_MODELS.iter().any(|m| lower.contains(m))
}

/// Test the is_ultra_required_model helper function
#[test]
fn test_is_ultra_required_model() {
    // Should be recognized as a high-end model
    assert!(is_ultra_required_model("claude-opus-4-6"));
    assert!(is_ultra_required_model("claude-opus-4-5"));
    assert!(is_ultra_required_model("Claude-Opus-4-6")); // case-insensitive
    assert!(is_ultra_required_model("CLAUDE-OPUS-4-5")); // case-insensitive
    assert!(is_ultra_required_model("opus")); // wildcard match
    assert!(is_ultra_required_model("opus-4-6-latest"));
    assert!(is_ultra_required_model("models/claude-opus-4-6"));

    // Should be recognized as a regular model
    assert!(!is_ultra_required_model("claude-sonnet-4-6"));
    assert!(!is_ultra_required_model("claude-sonnet"));
    assert!(!is_ultra_required_model("gemini-1.5-flash"));
    assert!(!is_ultra_required_model("gemini-2.0-pro"));
    assert!(!is_ultra_required_model("claude-haiku"));
}

/// Simulates the sorting logic in token_manager.rs (updated: tier always takes priority)
fn compare_tokens_for_model(a: &ProxyToken, b: &ProxyToken, _target_model: &str) -> Ordering {
    let tier_priority = |tier: &Option<String>| {
        let t = tier.as_deref().unwrap_or("").to_lowercase();
        if t.contains("ultra") {
            0
        } else if t.contains("pro") {
            1
        } else if t.contains("free") {
            2
        } else {
            3
        }
    };

    // Priority 0: always prioritize subscription tier (Ultra > Pro > Free)
    let tier_cmp = tier_priority(&a.subscription_tier).cmp(&tier_priority(&b.subscription_tier));
    if tier_cmp != Ordering::Equal {
        return tier_cmp;
    }

    // Priority 1: Quota (higher is better)
    // Note: this is simplified to use remaining_quota directly; production code actually reads model_quotas.get(target)
    let quota_a = a.remaining_quota.unwrap_or(0);
    let quota_b = b.remaining_quota.unwrap_or(0);
    let quota_cmp = quota_b.cmp(&quota_a);
    if quota_cmp != Ordering::Equal {
        return quota_cmp;
    }

    // Priority 2: Health score
    let health_cmp = b
        .health_score
        .partial_cmp(&a.health_score)
        .unwrap_or(Ordering::Equal);
    if health_cmp != Ordering::Equal {
        return health_cmp;
    }

    Ordering::Equal
}

/// Simulates the filtering logic
fn filter_tokens_by_capability(tokens: Vec<ProxyToken>, target_model: &str) -> Vec<ProxyToken> {
    tokens
        .into_iter()
        .filter(|t| t.model_quotas.contains_key(target_model))
        .collect()
}

/// Test high-end model sorting: Ultra accounts take priority over Pro accounts (even if Pro has a higher quota)
#[test]
fn test_ultra_priority_for_high_end_models() {
    // Create test accounts: Ultra low quota vs Pro high quota
    // Ultra account supports Opus 4.6
    let ultra_low_quota = create_test_token(
        "ultra@test.com",
        Some("ULTRA"),
        1.0,
        None,
        Some(20),
        vec!["claude-opus-4-6", "claude-sonnet-4-6"],
    );
    // Pro account does not support Opus 4.6 (assumed)
    let pro_high_quota = create_test_token(
        "pro@test.com",
        Some("PRO"),
        1.0,
        None,
        Some(80),
        vec!["claude-sonnet-4-6"],
    );

    // 1. Verify filtering logic
    let tokens = vec![ultra_low_quota.clone(), pro_high_quota.clone()];
    let filtered = filter_tokens_by_capability(tokens, "claude-opus-4-6");
    assert_eq!(
        filtered.len(),
        1,
        "Pro account should be filtered out for Opus 4.6"
    );
    assert_eq!(filtered[0].email, "ultra@test.com");

    // 2. Verify sorting logic (for Sonnet, which both support)
    // Even though Pro has a higher quota, Ultra still ranks first because the new policy is "Ultra First"
    assert_eq!(
        compare_tokens_for_model(&ultra_low_quota, &pro_high_quota, "claude-sonnet-4-6"),
        Ordering::Less, // Ultra ranks first
        "Sonnet should now prefer Ultra account over Pro (Strict Tier Policy)"
    );
}

#[test]
fn test_capability_filtering() {
    // Ultra account: has Opus 4.6
    let ultra = create_test_token(
        "ultra@test.com",
        Some("ULTRA"),
        1.0,
        None,
        Some(100),
        vec!["claude-opus-4-6"],
    );
    // Pro account: no Opus 4.6
    let pro = create_test_token(
        "pro@test.com",
        Some("PRO"),
        1.0,
        None,
        Some(100),
        vec!["claude-sonnet-3-5"],
    );

    // Future Pro account: has Opus 4.6 (simulating a possible future rollout)
    let future_pro = create_test_token(
        "future_pro@test.com",
        Some("PRO"),
        1.0,
        None,
        Some(50),
        vec!["claude-opus-4-6"],
    );

    let pool = vec![ultra.clone(), pro.clone(), future_pro.clone()];

    // 1. Request Opus 4.6
    let filtered_opus = filter_tokens_by_capability(pool.clone(), "claude-opus-4-6");
    assert_eq!(filtered_opus.len(), 2, "Should retain Ultra and Future Pro");
    // Verify Pro is removed
    assert!(!filtered_opus.iter().any(|t| t.email == "pro@test.com"));

    // 2. Sort filtered_opus: Ultra should rank before Future Pro (tier priority)
    let mut sorted_opus = filtered_opus.clone();
    sorted_opus.sort_by(|a, b| compare_tokens_for_model(a, b, "claude-opus-4-6"));
    assert_eq!(
        sorted_opus[0].email, "ultra@test.com",
        "Ultra should be prioritized over Pro even if Pro has capability"
    );
    assert_eq!(sorted_opus[1].email, "future_pro@test.com");
}

/// Test sorting: sort by quota among Ultra accounts
#[test]
fn test_ultra_accounts_sorted_by_quota() {
    let ultra_high = create_test_token(
        "ultra_high@test.com",
        Some("ULTRA"),
        1.0,
        None,
        Some(80),
        vec!["claude-opus-4-6"],
    );
    let ultra_low = create_test_token(
        "ultra_low@test.com",
        Some("ULTRA"),
        1.0,
        None,
        Some(20),
        vec!["claude-opus-4-6"],
    );

    // Opus 4.6: both Ultra, higher quota first
    assert_eq!(
        compare_tokens_for_model(&ultra_high, &ultra_low, "claude-opus-4-6"),
        Ordering::Less, // ultra_high ranks first
        "Among Ultra accounts, higher quota should come first"
    );
}

/// Test the full sorting scenario: mixed account pool
#[test]
fn test_full_sorting_mixed_accounts() {
    fn sort_tokens_for_model(tokens: &mut Vec<ProxyToken>, target_model: &str) {
        tokens.sort_by(|a, b| compare_tokens_for_model(a, b, target_model));
    }

    // Create a mixed account pool (all support every model, to simplify testing)
    let supported = vec!["claude-opus-4-6", "claude-sonnet-4-6"];
    let ultra_high = create_test_token(
        "ultra_high@test.com",
        Some("ULTRA"),
        1.0,
        None,
        Some(80),
        supported.clone(),
    );
    let ultra_low = create_test_token(
        "ultra_low@test.com",
        Some("ULTRA"),
        1.0,
        None,
        Some(20),
        supported.clone(),
    );
    let pro_high = create_test_token(
        "pro_high@test.com",
        Some("PRO"),
        1.0,
        None,
        Some(90),
        supported.clone(),
    );
    let pro_low = create_test_token(
        "pro_low@test.com",
        Some("PRO"),
        1.0,
        None,
        Some(30),
        supported.clone(),
    );
    let free = create_test_token(
        "free@test.com",
        Some("FREE"),
        1.0,
        None,
        Some(100),
        supported.clone(),
    );

    // High-end model (Opus 4.6) sorting
    let mut tokens_opus = vec![
        pro_high.clone(),
        free.clone(),
        ultra_low.clone(),
        pro_low.clone(),
        ultra_high.clone(),
    ];
    sort_tokens_for_model(&mut tokens_opus, "claude-opus-4-6");

    let emails_opus: Vec<&str> = tokens_opus.iter().map(|t| t.email.as_str()).collect();
    // Expected order: Ultra(high quota) > Ultra(low quota) > Pro(high quota) > Pro(low quota) > Free
    assert_eq!(
        emails_opus,
        vec![
            "ultra_high@test.com",
            "ultra_low@test.com",
            "pro_high@test.com",
            "pro_low@test.com",
            "free@test.com"
        ],
        "Opus 4.6 should sort Ultra first, then by quota within each tier"
    );

    // Regular model (Sonnet) sorting
    let mut tokens_sonnet = vec![
        pro_high.clone(),
        free.clone(),
        ultra_low.clone(),
        pro_low.clone(),
        ultra_high.clone(),
    ];
    sort_tokens_for_model(&mut tokens_sonnet, "claude-sonnet-4-6");

    let emails_sonnet: Vec<&str> = tokens_sonnet.iter().map(|t| t.email.as_str()).collect();
    // Expected order: Ultra > Pro > Free (strict tier)
    // Within Ultra, by quota: high > low
    // Within Pro, by quota: high > low
    assert_eq!(
        emails_sonnet,
        vec![
            "ultra_high@test.com",
            "ultra_low@test.com",
            "pro_high@test.com",
            "pro_low@test.com",
            "free@test.com"
        ],
        "Sonnet should now sort Ultra first, then Pro, then Free"
    );
}
