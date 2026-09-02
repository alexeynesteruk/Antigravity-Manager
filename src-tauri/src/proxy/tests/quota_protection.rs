// ==================================================================================
// Full test suite for the quota protection feature
// Verifies the complete flow from account creation to quota protection policy enforcement
// ==================================================================================

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::models::QuotaProtectionConfig;
    use crate::proxy::common::model_mapping::normalize_to_standard_id;
    use crate::proxy::token_manager::ProxyToken;

    // ==================================================================================
    // Helper function: create a mock account
    // ==================================================================================

    fn create_mock_token(
        account_id: &str,
        email: &str,
        protected_models: Vec<&str>,
        remaining_quota: Option<i32>,
    ) -> ProxyToken {
        ProxyToken {
            account_id: account_id.to_string(),
            access_token: format!("mock_access_token_{}", account_id),
            refresh_token: format!("mock_refresh_token_{}", account_id),
            expires_in: 3600,
            timestamp: chrono::Utc::now().timestamp() + 3600,
            email: email.to_string(),
            account_path: PathBuf::from(format!("/tmp/test_accounts/{}.json", account_id)),
            project_id: Some("test-project".to_string()),
            subscription_tier: Some("PRO".to_string()),
            remaining_quota,
            protected_models: protected_models.iter().map(|s| s.to_string()).collect(),
            health_score: 1.0,
            reset_time: None,
            validation_blocked: false,
            validation_blocked_until: 0,
            validation_url: None,
            model_quotas: std::collections::HashMap::new(),
            model_limits: std::collections::HashMap::new(),
        }
    }

    // ==================================================================================
    // Test 1: normalize_to_standard_id function correctness
    // Verifies that various Claude model names normalize correctly
    // ==================================================================================

    #[test]
    fn test_normalize_to_standard_id_claude_models() {
        // Claude Sonnet family
        assert_eq!(
            normalize_to_standard_id("claude"),
            Some("claude".to_string())
        );
        assert_eq!(
            normalize_to_standard_id("claude-thinking"),
            Some("claude".to_string())
        );

        // Claude Opus family - this is the key test!
        assert_eq!(
            normalize_to_standard_id("claude-opus-4-5-thinking"),
            Some("claude".to_string()),
            "claude-opus-4-5-thinking should normalize to claude"
        );

        // Gemini family
        assert_eq!(
            normalize_to_standard_id("gemini-3-flash"),
            Some("gemini-3-flash".to_string())
        );
        assert_eq!(
            normalize_to_standard_id("gemini-3-pro-high"),
            Some("gemini-3-pro-high".to_string())
        );
        assert_eq!(
            normalize_to_standard_id("gemini-3-pro-low"),
            Some("gemini-3-pro-high".to_string())
        );

        // Unsupported models should return None
        assert_eq!(normalize_to_standard_id("gpt-4"), None);
        assert_eq!(normalize_to_standard_id("unknown-model"), None);
    }

    // ==================================================================================
    // Test 2: quota protection model matching logic
    // Verifies that protected_models.contains() matches correctly after normalization
    // ==================================================================================

    #[test]
    fn test_protected_models_matching() {
        // Create an account with claude in protected_models
        let token = create_mock_token("account-1", "test@example.com", vec!["claude"], Some(50));

        // Test: requesting claude-opus-4-5-thinking should be protected
        let target_model = "claude-opus-4-5-thinking";
        let normalized =
            normalize_to_standard_id(target_model).unwrap_or_else(|| target_model.to_string());

        assert_eq!(normalized, "claude");
        assert!(
            token.protected_models.contains(&normalized),
            "claude-opus-4-5-thinking should match claude in protected_models after normalization"
        );

        // Test: requesting claude-thinking should also be protected
        let target_model_2 = "claude-thinking";
        let normalized_2 =
            normalize_to_standard_id(target_model_2).unwrap_or_else(|| target_model_2.to_string());

        assert!(
            token.protected_models.contains(&normalized_2),
            "claude-thinking should match protected_models after normalization"
        );

        // Test: requesting gemini-3-flash should not be protected (not present in protected_models)
        let target_model_3 = "gemini-3-flash";
        let normalized_3 =
            normalize_to_standard_id(target_model_3).unwrap_or_else(|| target_model_3.to_string());

        assert!(
            !token.protected_models.contains(&normalized_3),
            "gemini-3-flash should not match claude"
        );
    }

    // ==================================================================================
    // Test 3: quota protection filtering during multi-account rotation
    // Simulates multiple accounts, verifying protected accounts are skipped
    // ==================================================================================

    #[test]
    fn test_multi_account_quota_protection_filtering() {
        // Create 3 accounts
        let tokens = vec![
            // Account 1: claude is protected (low quota)
            create_mock_token("account-1", "user1@example.com", vec!["claude"], Some(20)),
            // Account 2: not protected
            create_mock_token("account-2", "user2@example.com", vec![], Some(80)),
            // Account 3: gemini-3-flash is protected
            create_mock_token(
                "account-3",
                "user3@example.com",
                vec!["gemini-3-flash"],
                Some(30),
            ),
        ];

        // Simulate requesting claude-opus-4-5-thinking
        let target_model = "claude-opus-4-5-thinking";
        let normalized_target =
            normalize_to_standard_id(target_model).unwrap_or_else(|| target_model.to_string());

        // Filter out protected accounts
        let available_accounts: Vec<_> = tokens
            .iter()
            .filter(|t| !t.protected_models.contains(&normalized_target))
            .collect();

        // Verify: account 1 is filtered out (claude is protected)
        // Accounts 2 and 3 are available
        assert_eq!(available_accounts.len(), 2);
        assert!(available_accounts
            .iter()
            .any(|t| t.account_id == "account-2"));
        assert!(available_accounts
            .iter()
            .any(|t| t.account_id == "account-3"));
        assert!(!available_accounts
            .iter()
            .any(|t| t.account_id == "account-1"));

        // Simulate requesting gemini-3-flash
        let target_model_2 = "gemini-3-flash";
        let normalized_target_2 =
            normalize_to_standard_id(target_model_2).unwrap_or_else(|| target_model_2.to_string());

        let available_accounts_2: Vec<_> = tokens
            .iter()
            .filter(|t| !t.protected_models.contains(&normalized_target_2))
            .collect();

        // Verify: account 3 is filtered out (gemini-3-flash is protected)
        // Accounts 1 and 2 are available
        assert_eq!(available_accounts_2.len(), 2);
        assert!(available_accounts_2
            .iter()
            .any(|t| t.account_id == "account-1"));
        assert!(available_accounts_2
            .iter()
            .any(|t| t.account_id == "account-2"));
        assert!(!available_accounts_2
            .iter()
            .any(|t| t.account_id == "account-3"));
    }

    // ==================================================================================
    // Test 4: behavior when all accounts are protected
    // Verifies an error is returned when the target model is protected on all accounts
    // ==================================================================================

    #[test]
    fn test_all_accounts_protected_returns_error() {
        // Create 3 accounts, all protecting claude
        let tokens = vec![
            create_mock_token("account-1", "user1@example.com", vec!["claude"], Some(10)),
            create_mock_token("account-2", "user2@example.com", vec!["claude"], Some(15)),
            create_mock_token("account-3", "user3@example.com", vec!["claude"], Some(5)),
        ];

        let target_model = "claude-opus-4-5-thinking";
        let normalized_target =
            normalize_to_standard_id(target_model).unwrap_or_else(|| target_model.to_string());

        let available_accounts: Vec<_> = tokens
            .iter()
            .filter(|t| !t.protected_models.contains(&normalized_target))
            .collect();

        // All accounts are filtered out, should return 0
        assert_eq!(available_accounts.len(), 0);

        // In actual code, this results in an "All accounts failed or unhealthy" error
    }

    // ==================================================================================
    // Test 5: monitored_models config consistency with normalization
    // Verifies monitored_models in config correctly matches normalized model names
    // ==================================================================================

    #[test]
    fn test_monitored_models_normalization_consistency() {
        let config = QuotaProtectionConfig {
            enabled: true,
            threshold_percentage: 60,
            monitored_models: vec![
                "claude".to_string(),
                "gemini-3-pro-high".to_string(),
                "gemini-3-flash".to_string(),
            ],
        };

        // Test whether various model names, after normalization, are in monitored_models
        let test_cases = vec![
            ("claude-opus-4-5-thinking", true), // normalizes to claude
            ("claude-thinking", true),          // normalizes to claude
            ("claude", true),                   // direct match
            ("gemini-3-pro-high", true),        // direct match
            ("gemini-3-pro-low", true),         // normalizes to gemini-3-pro-high
            ("gemini-3-flash", true),           // direct match
            ("gpt-4", false),                   // unsupported model
            ("gemini-2.5-flash", true),         // in the monitored list (normalizes to gemini-3-flash)
        ];

        for (model_name, expected_monitored) in test_cases {
            let standard_id = normalize_to_standard_id(model_name);

            let is_monitored = match &standard_id {
                Some(id) => config.monitored_models.contains(id),
                None => false,
            };

            assert_eq!(
                is_monitored, expected_monitored,
                "Model {} (normalized to {:?}) monitored status should be {}",
                model_name, standard_id, expected_monitored
            );
        }
    }

    // ==================================================================================
    // Test 6: quota threshold trigger logic
    // Verifies protection triggers when quota is below threshold and recovers when above
    // ==================================================================================

    #[test]
    fn test_quota_threshold_trigger_logic() {
        let threshold = 60; // 60% threshold

        // Simulate quota data
        let quota_data = vec![
            ("claude-opus-4-5-thinking", 50, true), // 50% <= 60%, should trigger protection
            ("claude-thinking", 60, true),          // 60% <= 60%, should trigger protection (boundary case)
            ("gemini-3-flash", 61, false),          // 61% > 60%, should not trigger protection
            ("gemini-3-pro-high", 100, false),      // 100% > 60%, should not trigger protection
        ];

        for (model_name, percentage, should_protect) in quota_data {
            let should_trigger = percentage <= threshold;

            assert_eq!(
                should_trigger,
                should_protect,
                "Model {} quota {}% (threshold {}%) should {}trigger protection",
                model_name,
                percentage,
                threshold,
                if should_protect { "" } else { "not " }
            );
        }
    }

    // ==================================================================================
    // Test 7: protection filtering after account priority sorting
    // Verifies fallback to lower-quota accounts when high-quota accounts are protected
    // ==================================================================================

    #[test]
    fn test_priority_fallback_when_protected() {
        // Create 3 accounts, sorted by quota
        let mut tokens = vec![
            create_mock_token("account-high", "high@example.com", vec!["claude"], Some(90)),
            create_mock_token("account-mid", "mid@example.com", vec![], Some(60)),
            create_mock_token("account-low", "low@example.com", vec![], Some(30)),
        ];

        // Sort by quota descending (highest quota first)
        tokens.sort_by(|a, b| {
            let qa = a.remaining_quota.unwrap_or(0);
            let qb = b.remaining_quota.unwrap_or(0);
            qb.cmp(&qa)
        });

        // Verify sort order is correct
        assert_eq!(tokens[0].account_id, "account-high");
        assert_eq!(tokens[1].account_id, "account-mid");
        assert_eq!(tokens[2].account_id, "account-low");

        // Simulate requesting claude-opus-4-5-thinking
        let target_model = "claude-opus-4-5-thinking";
        let normalized_target =
            normalize_to_standard_id(target_model).unwrap_or_else(|| target_model.to_string());

        // Select the first available account in order
        let selected = tokens
            .iter()
            .find(|t| !t.protected_models.contains(&normalized_target));

        // Verify: account-high is skipped, account-mid is selected
        assert!(selected.is_some());
        assert_eq!(
            selected.unwrap().account_id,
            "account-mid",
            "should fall back to account-mid after the high-quota account is protected"
        );
    }

    // ==================================================================================
    // Test 8: model-level protection (same account, different models)
    // Verifies an account can protect some models while leaving others unprotected
    // ==================================================================================

    #[test]
    fn test_model_level_protection_granularity() {
        // Account protects claude but not gemini-3-flash
        let token = create_mock_token("account-1", "user@example.com", vec!["claude"], Some(50));

        // Request claude-opus-4-5-thinking -> protected
        let normalized_claude = normalize_to_standard_id("claude-opus-4-5-thinking")
            .unwrap_or_else(|| "claude-opus-4-5-thinking".to_string());
        assert!(
            token.protected_models.contains(&normalized_claude),
            "Claude request should be protected"
        );

        // Request gemini-3-flash -> not protected
        let normalized_gemini = normalize_to_standard_id("gemini-3-flash")
            .unwrap_or_else(|| "gemini-3-flash".to_string());
        assert!(
            !token.protected_models.contains(&normalized_gemini),
            "Gemini request should not be protected"
        );
    }

    // ==================================================================================
    // Test 9: quota protection enable/disable switch
    // Verifies protection logic is inactive when quota_protection.enabled = false
    // ==================================================================================

    #[test]
    fn test_quota_protection_enabled_flag() {
        let config_enabled = QuotaProtectionConfig {
            enabled: true,
            threshold_percentage: 60,
            monitored_models: vec!["claude".to_string()],
        };

        let config_disabled = QuotaProtectionConfig {
            enabled: false,
            threshold_percentage: 60,
            monitored_models: vec!["claude".to_string()],
        };

        let token = create_mock_token("account-1", "user@example.com", vec!["claude"], Some(50));

        let target_model = "claude-opus-4-5-thinking";
        let normalized_target =
            normalize_to_standard_id(target_model).unwrap_or_else(|| target_model.to_string());

        // Account should be filtered when quota protection is enabled
        let is_protected_when_enabled =
            config_enabled.enabled && token.protected_models.contains(&normalized_target);
        assert!(is_protected_when_enabled, "should be protected when enabled");

        // Should not filter when quota protection is disabled, even if protected_models has values
        let is_protected_when_disabled =
            config_disabled.enabled && token.protected_models.contains(&normalized_target);
        assert!(!is_protected_when_disabled, "should not be protected when disabled");
    }

    // ==================================================================================
    // Test 10: full flow simulation (integration-test style)
    // Simulates the full flow of multiple accounts, quota protection config, and request rotation
    // ==================================================================================

    #[test]
    fn test_full_quota_protection_flow() {
        // 1. Configure quota protection
        let config = QuotaProtectionConfig {
            enabled: true,
            threshold_percentage: 60,
            monitored_models: vec!["claude".to_string(), "gemini-3-flash".to_string()],
        };

        // 2. Create multiple accounts, simulating different quota states
        let accounts = vec![
            // Account A: Claude quota low (50%), should be protected
            create_mock_token("account-a", "a@example.com", vec!["claude"], Some(50)),
            // Account B: Claude quota normal (80%), not protected
            create_mock_token("account-b", "b@example.com", vec![], Some(80)),
            // Account C: both Claude and Gemini are protected
            create_mock_token(
                "account-c",
                "c@example.com",
                vec!["claude", "gemini-3-flash"],
                Some(30),
            ),
            // Account D: only Gemini is protected
            create_mock_token(
                "account-d",
                "d@example.com",
                vec!["gemini-3-flash"],
                Some(40),
            ),
        ];

        // 3. Simulate multiple requests, verify account selection logic

        // Request 1: claude-opus-4-5-thinking
        let target_claude = normalize_to_standard_id("claude-opus-4-5-thinking")
            .unwrap_or_else(|| "claude-opus-4-5-thinking".to_string());

        let available_for_claude: Vec<_> = accounts
            .iter()
            .filter(|a| !config.enabled || !a.protected_models.contains(&target_claude))
            .collect();

        // Accounts A and C are filtered out, B and D are available
        assert_eq!(available_for_claude.len(), 2);
        let claude_account_ids: Vec<_> = available_for_claude
            .iter()
            .map(|a| a.account_id.as_str())
            .collect();
        assert!(claude_account_ids.contains(&"account-b"));
        assert!(claude_account_ids.contains(&"account-d"));

        // Request 2: gemini-3-flash
        let target_gemini = normalize_to_standard_id("gemini-3-flash")
            .unwrap_or_else(|| "gemini-3-flash".to_string());

        let available_for_gemini: Vec<_> = accounts
            .iter()
            .filter(|a| !config.enabled || !a.protected_models.contains(&target_gemini))
            .collect();

        // Accounts C and D are filtered out, A and B are available
        assert_eq!(available_for_gemini.len(), 2);
        let gemini_account_ids: Vec<_> = available_for_gemini
            .iter()
            .map(|a| a.account_id.as_str())
            .collect();
        assert!(gemini_account_ids.contains(&"account-a"));
        assert!(gemini_account_ids.contains(&"account-b"));

        // Request 3: an unmonitored model (gemini-2.5-flash)
        let target_unmonitored = normalize_to_standard_id("gemini-2.5-flash")
            .unwrap_or_else(|| "gemini-2.5-flash".to_string());

        let available_for_unmonitored: Vec<_> = accounts
            .iter()
            .filter(|a| !config.enabled || !a.protected_models.contains(&target_unmonitored))
            .collect();

        // Unmonitored model (Gemini 2.5 Flash actually normalizes to the monitored 3-flash)
        // Of the 4 test accounts, C and D have 3-flash protection enabled, while A and B do not.
        // Therefore, 2 accounts should be available.
        assert_eq!(
            available_for_unmonitored.len(),
            2,
            "Gemini 2.5 Flash shares 3-flash's protection state, 2 accounts should be available"
        );
    }

    // ==================================================================================
    // Test 11: boundary case - empty protected_models
    // ==================================================================================

    #[test]
    fn test_empty_protected_models() {
        let token = create_mock_token(
            "account-1",
            "user@example.com",
            vec![], // No protected models
            Some(50),
        );

        let target = normalize_to_standard_id("claude-opus-4-5-thinking")
            .unwrap_or_else(|| "claude-opus-4-5-thinking".to_string());

        assert!(
            !token.protected_models.contains(&target),
            "empty protected_models should not match any model"
        );
    }

    // ==================================================================================
    // Test 12: boundary case - case sensitivity
    // ==================================================================================

    #[test]
    fn test_model_name_case_sensitivity() {
        // normalize_to_standard_id should be case-insensitive
        assert_eq!(
            normalize_to_standard_id("Claude-Opus-4-5-Thinking"),
            Some("claude".to_string())
        );
        assert_eq!(
            normalize_to_standard_id("CLAUDE-OPUS-4-5-THINKING"),
            Some("claude".to_string())
        );
        assert_eq!(
            normalize_to_standard_id("GEMINI-3-FLASH"),
            Some("gemini-3-flash".to_string())
        );
    }

    // ==================================================================================
    // Test 13: end-to-end scenario - routing switch after quota protection kicks in mid-session
    // Simulates: request 1 -> bind account A -> request 2 -> keep using A -> refresh quota -> A protected -> request 3 -> switch to B
    // ==================================================================================

    #[test]
    fn test_sticky_session_quota_protection_mid_session_single_account() {
        // Scenario: only one account, quota protection kicks in after session binding
        // Expected: return a quota protection error

        let session_id = "session-12345";
        let target_model = "claude-opus-4-5-thinking";
        let normalized_target =
            normalize_to_standard_id(target_model).unwrap_or_else(|| target_model.to_string());

        // Initial state: account A is not protected
        let mut account_a = create_mock_token(
            "account-a",
            "a@example.com",
            vec![], // no protection initially
            Some(70),
        );

        // Simulate session binding table
        let mut session_bindings: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        // === Request 1: bind to account A ===
        session_bindings.insert(session_id.to_string(), account_a.account_id.clone());

        // Verify request 1 succeeded
        let bound_account = session_bindings.get(session_id);
        assert_eq!(bound_account, Some(&"account-a".to_string()));

        // === Request 2: continue using account A ===
        // Account A is still available
        assert!(!account_a.protected_models.contains(&normalized_target));

        // === System triggers a quota refresh, finds account A's quota below the threshold ===
        // Simulate that after the quota refresh, account_a's claude is added to the protection list
        account_a.protected_models.insert("claude".to_string());

        // === Request 3: try to use account A, but it's quota-protected ===
        let accounts = vec![account_a.clone()]; // only one account

        // Check whether the bound account is protected
        let bound_id = session_bindings.get(session_id).unwrap();
        let bound_account = accounts.iter().find(|a| &a.account_id == bound_id).unwrap();
        let is_protected = bound_account.protected_models.contains(&normalized_target);

        assert!(is_protected, "account A should be quota-protected");

        // Try to find another available account
        let available_accounts: Vec<_> = accounts
            .iter()
            .filter(|a| !a.protected_models.contains(&normalized_target))
            .collect();

        // No available accounts
        assert_eq!(available_accounts.len(), 0, "there should be no available accounts");

        // In the actual implementation, this returns an error message
        // Verify a quota-protection-related error is returned
        let error_message = if available_accounts.is_empty() {
            if accounts
                .iter()
                .all(|a| a.protected_models.contains(&normalized_target))
            {
                format!(
                    "All accounts quota-protected for model {}",
                    normalized_target
                )
            } else {
                "All accounts failed or unhealthy.".to_string()
            }
        } else {
            "OK".to_string()
        };

        assert!(
            error_message.contains("quota-protected"),
            "error message should contain quota-protected: {}",
            error_message
        );
    }

    #[test]
    fn test_sticky_session_quota_protection_mid_session_multi_account() {
        // Scenario: multiple accounts, the session-bound account should route to another account once its quota protection kicks in

        let session_id = "session-67890";
        let target_model = "claude-opus-4-5-thinking";
        let normalized_target =
            normalize_to_standard_id(target_model).unwrap_or_else(|| target_model.to_string());

        // Initial state: neither account A nor B is protected
        let mut account_a = create_mock_token("account-a", "a@example.com", vec![], Some(70));
        let account_b = create_mock_token("account-b", "b@example.com", vec![], Some(80));

        let mut session_bindings: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        // === Request 1: bind to account A ===
        session_bindings.insert(session_id.to_string(), account_a.account_id.clone());

        // === Request 2: continue using account A ===
        assert!(!account_a.protected_models.contains(&normalized_target));

        // === System triggers a quota refresh, account A becomes protected ===
        account_a.protected_models.insert("claude".to_string());

        // === Request 3: account A is protected, should unbind and switch to account B ===
        let accounts = vec![account_a.clone(), account_b.clone()];

        // Check the bound account
        let bound_id = session_bindings.get(session_id).unwrap();
        let bound_account = accounts.iter().find(|a| &a.account_id == bound_id).unwrap();
        let is_protected = bound_account.protected_models.contains(&normalized_target);

        assert!(is_protected, "account A should be quota-protected");

        // Simulate unbinding logic
        if is_protected {
            session_bindings.remove(session_id);
        }

        // Look for another available account
        let available_accounts: Vec<_> = accounts
            .iter()
            .filter(|a| !a.protected_models.contains(&normalized_target))
            .collect();

        // Account B should be available
        assert_eq!(available_accounts.len(), 1);
        assert_eq!(available_accounts[0].account_id, "account-b");

        // Rebind to account B
        let new_account = available_accounts[0];
        session_bindings.insert(session_id.to_string(), new_account.account_id.clone());

        // Verify the new binding
        assert_eq!(
            session_bindings.get(session_id),
            Some(&"account-b".to_string()),
            "session should rebind to account B"
        );
    }

    // ==================================================================================
    // Test 14: quota protection real-time sync test
    // Simulates: protected_models updated after quota refresh, TokenManager's in-memory state should sync
    // ==================================================================================

    #[test]
    fn test_quota_protection_sync_after_refresh() {
        // This test simulates the scenario where update_account_quota triggers a TokenManager reload

        // Initial in-memory state
        let mut tokens_in_memory = vec![create_mock_token(
            "account-a",
            "a@example.com",
            vec![],
            Some(70),
        )];

        // Simulate on-disk account data (updated after quota refresh)
        let mut account_on_disk = create_mock_token("account-a", "a@example.com", vec![], Some(50));

        // Simulate quota refresh: quota detected below threshold, protection triggered
        let threshold = 60;
        if account_on_disk.remaining_quota.unwrap_or(100) <= threshold {
            account_on_disk
                .protected_models
                .insert("claude".to_string());
        }

        // Verify on-disk data has been updated
        assert!(
            account_on_disk.protected_models.contains("claude"),
            "the on-disk account should already be protected"
        );

        // At this point, in-memory data is still stale
        assert!(
            !tokens_in_memory[0].protected_models.contains("claude"),
            "the in-memory account has not been synced yet"
        );

        // Simulate trigger_account_reload -> reload_account sync
        tokens_in_memory[0] = account_on_disk.clone();

        // Verify in-memory data has been synced
        assert!(
            tokens_in_memory[0].protected_models.contains("claude"),
            "the in-memory account should be protected after sync"
        );

        // The request should now be filtered correctly
        let target = normalize_to_standard_id("claude-opus-4-5-thinking")
            .unwrap_or_else(|| "claude-opus-4-5-thinking".to_string());

        let available: Vec<_> = tokens_in_memory
            .iter()
            .filter(|t| !t.protected_models.contains(&target))
            .collect();

        assert_eq!(available.len(), 0, "the account should be filtered after sync");
    }

    // ==================================================================================
    // Test 15: dynamic quota protection changes across multiple requests
    // Simulates a full request sequence, including quota protection triggering and recovery
    // ==================================================================================

    #[test]
    fn test_quota_protection_dynamic_changes() {
        let target_model = "claude-opus-4-5-thinking";
        let normalized_target =
            normalize_to_standard_id(target_model).unwrap_or_else(|| target_model.to_string());

        // Account pool
        let mut account_a = create_mock_token("account-a", "a@example.com", vec![], Some(70));
        let mut account_b = create_mock_token("account-b", "b@example.com", vec![], Some(80));

        // === Phase 1: initial state, both accounts available ===
        let accounts = vec![account_a.clone(), account_b.clone()];
        let available: Vec<_> = accounts
            .iter()
            .filter(|t| !t.protected_models.contains(&normalized_target))
            .collect();
        assert_eq!(available.len(), 2, "phase 1: both accounts available");

        // === Phase 2: account A's quota drops, protection triggered ===
        account_a.remaining_quota = Some(40);
        account_a.protected_models.insert("claude".to_string());

        let accounts = vec![account_a.clone(), account_b.clone()];
        let available: Vec<_> = accounts
            .iter()
            .filter(|t| !t.protected_models.contains(&normalized_target))
            .collect();
        assert_eq!(available.len(), 1, "phase 2: only account B available");
        assert_eq!(available[0].account_id, "account-b");

        // === Phase 3: account B also triggers protection ===
        account_b.remaining_quota = Some(30);
        account_b.protected_models.insert("claude".to_string());

        let accounts = vec![account_a.clone(), account_b.clone()];
        let available: Vec<_> = accounts
            .iter()
            .filter(|t| !t.protected_models.contains(&normalized_target))
            .collect();
        assert_eq!(available.len(), 0, "phase 3: no accounts available");

        // === Phase 4: account A's quota recovers (reset), protection lifted ===
        account_a.remaining_quota = Some(100);
        account_a.protected_models.remove("claude");

        let accounts = vec![account_a.clone(), account_b.clone()];
        let available: Vec<_> = accounts
            .iter()
            .filter(|t| !t.protected_models.contains(&normalized_target))
            .collect();
        assert_eq!(available.len(), 1, "phase 4: account A available again");
        assert_eq!(available[0].account_id, "account-a");
    }

    // ==================================================================================
    // Test 16: full error message verification
    // Verifies error messages returned across different scenarios are correct
    // ==================================================================================

    #[test]
    fn test_error_messages_for_quota_protection() {
        let target_model = "claude-opus-4-5-thinking";
        let normalized_target =
            normalize_to_standard_id(target_model).unwrap_or_else(|| target_model.to_string());

        // Scenario 1: all accounts unavailable due to quota protection
        let all_protected = vec![
            create_mock_token("a1", "a1@example.com", vec!["claude"], Some(30)),
            create_mock_token("a2", "a2@example.com", vec!["claude"], Some(20)),
        ];

        let all_are_quota_protected = all_protected
            .iter()
            .all(|a| a.protected_models.contains(&normalized_target));

        assert!(all_are_quota_protected, "all accounts are quota-protected");

        // Generate error message
        let error = format!(
            "All {} accounts are quota-protected for model '{}'. Wait for quota reset or adjust protection threshold.",
            all_protected.len(),
            normalized_target
        );

        assert!(error.contains("quota-protected"));
        assert!(error.contains("claude"));

        // Scenario 2: mixed case (some rate-limited, some quota-protected)
        let mixed = vec![
            create_mock_token("a1", "a1@example.com", vec!["claude"], Some(30)),
            create_mock_token("a2", "a2@example.com", vec![], Some(20)), // assume this one is rate-limited
        ];

        let quota_protected_count = mixed
            .iter()
            .filter(|a| a.protected_models.contains(&normalized_target))
            .count();

        assert_eq!(quota_protected_count, 1);
    }

    // ==================================================================================
    // Test 17: get_model_quota_from_json function correctness
    // Verifies reading a specific model's quota from disk instead of max(all models)
    // ==================================================================================

    #[test]
    fn test_get_model_quota_from_json_reads_correct_model() {
        // Create a mock account JSON file containing quotas for multiple models
        let account_json = serde_json::json!({
            "email": "test@example.com",
            "quota": {
                "models": [
                    { "name": "claude", "percentage": 60 },
                    { "name": "claude-opus-4-5-thinking", "percentage": 40 },
                    { "name": "gemini-3-flash", "percentage": 100 }
                ]
            }
        });

        // Use std::env::temp_dir() to create a temp file
        let temp_dir = std::env::temp_dir();
        let account_path = temp_dir.join(format!("test_quota_{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&account_path, account_json.to_string()).expect("Failed to write temp file");

        // Test reading claude's quota
        let sonnet_quota =
            crate::proxy::token_manager::TokenManager::get_model_quota_from_json_for_test(
                &account_path,
                "claude",
            );
        assert_eq!(
            sonnet_quota,
            Some(60),
            "claude should return 60%, not max(100%)"
        );

        // Test reading gemini-3-flash's quota
        let gemini_quota =
            crate::proxy::token_manager::TokenManager::get_model_quota_from_json_for_test(
                &account_path,
                "gemini-3-flash",
            );
        assert_eq!(gemini_quota, Some(100), "gemini-3-flash should return 100%");

        // Test reading a nonexistent model
        let unknown_quota =
            crate::proxy::token_manager::TokenManager::get_model_quota_from_json_for_test(
                &account_path,
                "unknown-model",
            );
        assert_eq!(unknown_quota, None, "a nonexistent model should return None");

        // Clean up temp file
        let _ = std::fs::remove_file(&account_path);
    }

    // ==================================================================================
    // Test 18: sorting uses the target model's quota instead of max quota
    // Verifies the fixed sorting logic is correct
    // ==================================================================================

    #[test]
    fn test_sorting_uses_target_model_quota_not_max() {
        // Use std::env::temp_dir() to create a temp directory
        let temp_dir = std::env::temp_dir().join(format!("test_sorting_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        // Account A: max=100 (gemini), sonnet=40
        let account_a_json = serde_json::json!({
            "email": "carmelioventori@example.com",
            "quota": {
                "models": [
                    { "name": "claude", "percentage": 40 },
                    { "name": "gemini-3-flash", "percentage": 100 }
                ]
            }
        });

        // Account B: max=100 (gemini), sonnet=100
        let account_b_json = serde_json::json!({
            "email": "kiriyamaleo@example.com",
            "quota": {
                "models": [
                    { "name": "claude", "percentage": 100 },
                    { "name": "gemini-3-flash", "percentage": 100 }
                ]
            }
        });

        // Account C: max=100 (gemini), sonnet=60
        let account_c_json = serde_json::json!({
            "email": "mizusawakai9@example.com",
            "quota": {
                "models": [
                    { "name": "claude", "percentage": 60 },
                    { "name": "gemini-3-flash", "percentage": 100 }
                ]
            }
        });

        // Write temp files
        let path_a = temp_dir.join("account_a.json");
        let path_b = temp_dir.join("account_b.json");
        let path_c = temp_dir.join("account_c.json");

        std::fs::write(&path_a, account_a_json.to_string()).unwrap();
        std::fs::write(&path_b, account_b_json.to_string()).unwrap();
        std::fs::write(&path_c, account_c_json.to_string()).unwrap();

        // Create tokens, with remaining_quota using the max value (simulating the old logic)
        let mut tokens = vec![
            create_mock_token_with_path(
                "a",
                "carmelioventori@example.com",
                vec![],
                Some(100),
                path_a.clone(),
            ),
            create_mock_token_with_path(
                "b",
                "kiriyamaleo@example.com",
                vec![],
                Some(100),
                path_b.clone(),
            ),
            create_mock_token_with_path(
                "c",
                "mizusawakai9@example.com",
                vec![],
                Some(100),
                path_c.clone(),
            ),
        ];

        // Target model: claude
        let target_model = "claude";

        // Use the fixed sorting logic: read the target model's quota
        tokens.sort_by(|a, b| {
            let quota_a =
                crate::proxy::token_manager::TokenManager::get_model_quota_from_json_for_test(
                    &a.account_path,
                    target_model,
                )
                .unwrap_or(0);
            let quota_b =
                crate::proxy::token_manager::TokenManager::get_model_quota_from_json_for_test(
                    &b.account_path,
                    target_model,
                )
                .unwrap_or(0);
            quota_b.cmp(&quota_a) // higher quota first
        });

        // Verify sort result: sonnet quota 100% > 60% > 40%
        assert_eq!(
            tokens[0].email, "kiriyamaleo@example.com",
            "the account with sonnet=100% should rank first"
        );
        assert_eq!(
            tokens[1].email, "mizusawakai9@example.com",
            "the account with sonnet=60% should rank second"
        );
        assert_eq!(
            tokens[2].email, "carmelioventori@example.com",
            "the account with sonnet=40% should rank third"
        );

        // Clean up temp directory
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    // ==================================================================================
    // Test 19: quota matching after model name normalization
    // Verifies requesting claude-opus-4-5-thinking correctly matches claude's quota
    // ==================================================================================

    #[test]
    fn test_quota_matching_with_normalized_model_name() {
        // Account JSON: only records normalized model names
        let account_json = serde_json::json!({
            "email": "test@example.com",
            "quota": {
                "models": [
                    { "name": "claude", "percentage": 75 },
                    { "name": "gemini-3-flash", "percentage": 90 }
                ]
            }
        });

        let temp_dir = std::env::temp_dir();
        let account_path = temp_dir.join(format!("test_normalized_{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&account_path, account_json.to_string()).expect("Failed to write temp file");

        // Request claude-opus-4-5-thinking, should normalize to claude
        let request_model = "claude-opus-4-5-thinking";
        let normalized =
            normalize_to_standard_id(request_model).unwrap_or_else(|| request_model.to_string());

        assert_eq!(normalized, "claude", "should normalize to claude");

        // Read the normalized model's quota
        let quota = crate::proxy::token_manager::TokenManager::get_model_quota_from_json_for_test(
            &account_path,
            &normalized,
        );

        assert_eq!(
            quota,
            Some(75),
            "claude-opus-4-5-thinking should read claude's quota (75%) after normalization"
        );

        // Clean up temp file
        let _ = std::fs::remove_file(&account_path);
    }

    /// Helper function: create a mock token with a custom account_path
    fn create_mock_token_with_path(
        account_id: &str,
        email: &str,
        protected_models: Vec<&str>,
        remaining_quota: Option<i32>,
        account_path: PathBuf,
    ) -> ProxyToken {
        ProxyToken {
            account_id: account_id.to_string(),
            access_token: format!("mock_access_token_{}", account_id),
            refresh_token: format!("mock_refresh_token_{}", account_id),
            expires_in: 3600,
            timestamp: chrono::Utc::now().timestamp() + 3600,
            email: email.to_string(),
            account_path,
            project_id: Some("test-project".to_string()),
            subscription_tier: Some("PRO".to_string()),
            remaining_quota,
            protected_models: protected_models.iter().map(|s| s.to_string()).collect(),
            health_score: 1.0,
            reset_time: None,
            validation_blocked: false,
            validation_blocked_until: 0,
            validation_url: None,
            model_quotas: std::collections::HashMap::new(),
            model_limits: std::collections::HashMap::new(),
        }
    }
}
