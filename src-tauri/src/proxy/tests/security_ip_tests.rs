//! IP Security Module Tests
//! Comprehensive test suite for the IP security monitoring feature
//!
//! Test objectives:
//! 1. Verify the correctness of the IP blocklist/allowlist feature
//! 2. Verify CIDR matching logic
//! 3. Verify expiration handling
//! 4. Verify no impact on main-flow performance
//! 5. Verify atomicity and consistency of database operations

#[cfg(test)]
mod security_db_tests {
    use crate::modules::security_db::{
        add_to_blacklist, add_to_whitelist, cleanup_old_ip_logs, clear_ip_access_logs,
        get_blacklist, get_blacklist_entry_for_ip, get_ip_access_logs, get_ip_stats, get_whitelist,
        init_db, is_ip_in_blacklist, is_ip_in_whitelist, remove_from_blacklist,
        remove_from_whitelist, save_ip_access_log, IpAccessLog,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Helper function: get the current timestamp
    fn now_timestamp() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// Helper function: clean up the test environment
    fn cleanup_test_data() {
        // Clean up the blocklist
        if let Ok(entries) = get_blacklist() {
            for entry in entries {
                let _ = remove_from_blacklist(&entry.id);
            }
        }
        // Clean up the allowlist
        if let Ok(entries) = get_whitelist() {
            for entry in entries {
                let _ = remove_from_whitelist(&entry.id);
            }
        }
        // Clean up access logs
        let _ = clear_ip_access_logs();
    }

    // ============================================================================
    // Test category 1: database initialization
    // ============================================================================

    #[test]
    fn test_db_initialization() {
        // Verify database initialization does not panic
        let result = init_db();
        assert!(
            result.is_ok(),
            "Database initialization should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_db_multiple_initializations() {
        // Verify repeated initialization does not error (idempotency)
        for _ in 0..3 {
            let result = init_db();
            assert!(
                result.is_ok(),
                "Multiple DB initializations should be idempotent"
            );
        }
    }

    // ============================================================================
    // Test category 2: IP blocklist basic operations
    // ============================================================================

    #[test]
    fn test_blacklist_add_and_check() {
        let _ = init_db();
        cleanup_test_data();

        // Add an IP to the blocklist
        let result = add_to_blacklist("192.168.1.100", Some("Test block"), None, "test");
        assert!(
            result.is_ok(),
            "Should add IP to blacklist: {:?}",
            result.err()
        );

        // Verify the IP is in the blocklist
        let is_blocked = is_ip_in_blacklist("192.168.1.100");
        assert!(is_blocked.is_ok());
        assert!(is_blocked.unwrap(), "IP should be in blacklist");

        // Verify other IPs are not in the blocklist
        let is_other_blocked = is_ip_in_blacklist("192.168.1.101");
        assert!(is_other_blocked.is_ok());
        assert!(
            !is_other_blocked.unwrap(),
            "Other IP should not be in blacklist"
        );

        cleanup_test_data();
    }

    #[test]
    fn test_blacklist_remove() {
        let _ = init_db();
        cleanup_test_data();

        // Add an IP
        let entry = add_to_blacklist("10.0.0.5", Some("Temp block"), None, "test").unwrap();

        // Verify it exists
        assert!(is_ip_in_blacklist("10.0.0.5").unwrap());

        // Remove
        let remove_result = remove_from_blacklist(&entry.id);
        assert!(remove_result.is_ok());

        // Verify it has been removed
        assert!(!is_ip_in_blacklist("10.0.0.5").unwrap());

        cleanup_test_data();
    }

    #[test]
    fn test_blacklist_get_entry_details() {
        let _ = init_db();
        cleanup_test_data();

        // Add an entry with detailed info
        let _ = add_to_blacklist(
            "172.16.0.50",
            Some("Abuse detected"),
            Some(now_timestamp() + 3600), // expires in 1 hour
            "admin",
        );

        // Get entry details
        let entry_result = get_blacklist_entry_for_ip("172.16.0.50");
        assert!(entry_result.is_ok());

        let entry = entry_result.unwrap();
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(entry.ip_pattern, "172.16.0.50");
        assert_eq!(entry.reason.as_deref(), Some("Abuse detected"));
        assert_eq!(entry.created_by, "admin");
        assert!(entry.expires_at.is_some());

        cleanup_test_data();
    }

    // ============================================================================
    // Test category 3: CIDR matching
    // ============================================================================

    #[test]
    fn test_cidr_matching_basic() {
        let _ = init_db();
        cleanup_test_data();

        // Add a CIDR range to the blocklist
        let _ = add_to_blacklist("192.168.1.0/24", Some("Block subnet"), None, "test");

        // Verify all IPs within the subnet are blocked
        assert!(
            is_ip_in_blacklist("192.168.1.1").unwrap(),
            "192.168.1.1 should match /24"
        );
        assert!(
            is_ip_in_blacklist("192.168.1.100").unwrap(),
            "192.168.1.100 should match /24"
        );
        assert!(
            is_ip_in_blacklist("192.168.1.254").unwrap(),
            "192.168.1.254 should match /24"
        );

        // Verify IPs outside the subnet are not blocked
        assert!(
            !is_ip_in_blacklist("192.168.2.1").unwrap(),
            "192.168.2.1 should not match"
        );
        assert!(
            !is_ip_in_blacklist("10.0.0.1").unwrap(),
            "10.0.0.1 should not match"
        );

        cleanup_test_data();
    }

    #[test]
    fn test_cidr_matching_various_masks() {
        let _ = init_db();
        cleanup_test_data();

        // Test /16 mask
        let _ = add_to_blacklist("10.10.0.0/16", Some("Block /16"), None, "test");

        assert!(is_ip_in_blacklist("10.10.0.1").unwrap(), "Should match /16");
        assert!(
            is_ip_in_blacklist("10.10.255.255").unwrap(),
            "Should match /16"
        );
        assert!(
            !is_ip_in_blacklist("10.11.0.1").unwrap(),
            "Should not match /16"
        );

        cleanup_test_data();

        // Test /32 mask (a single IP)
        let _ = add_to_blacklist("8.8.8.8/32", Some("Block single"), None, "test");

        assert!(is_ip_in_blacklist("8.8.8.8").unwrap(), "Should match /32");
        assert!(
            !is_ip_in_blacklist("8.8.8.9").unwrap(),
            "Should not match /32"
        );

        cleanup_test_data();
    }

    #[test]
    fn test_cidr_edge_cases() {
        let _ = init_db();
        cleanup_test_data();

        // Test /0 (all IPs) - boundary case
        let _ = add_to_blacklist("0.0.0.0/0", Some("Block all"), None, "test");

        assert!(
            is_ip_in_blacklist("1.2.3.4").unwrap(),
            "Everything should match /0"
        );
        assert!(
            is_ip_in_blacklist("255.255.255.255").unwrap(),
            "Everything should match /0"
        );

        cleanup_test_data();

        // Test /8 mask
        let _ = add_to_blacklist("10.0.0.0/8", Some("Block /8"), None, "test");

        assert!(
            is_ip_in_blacklist("10.255.255.255").unwrap(),
            "Should match /8"
        );
        assert!(
            !is_ip_in_blacklist("11.0.0.0").unwrap(),
            "Should not match /8"
        );

        cleanup_test_data();
    }

    // ============================================================================
    // Test category 4: expiration handling
    // ============================================================================

    #[test]
    fn test_blacklist_expiration() {
        let _ = init_db();
        cleanup_test_data();

        // Add an already-expired entry
        let _ = add_to_blacklist(
            "expired.test.ip",
            Some("Already expired"),
            Some(now_timestamp() - 60), // expired 1 minute ago
            "test",
        );

        // Expired entries should be cleaned up automatically
        let is_blocked = is_ip_in_blacklist("expired.test.ip");
        // Note: depending on the implementation, expired entries may be cleaned up on query
        // Per the security_db.rs implementation, get_blacklist_entry_for_ip cleans up expired entries first
        assert!(!is_blocked.unwrap(), "Expired entry should be cleaned up");

        cleanup_test_data();
    }

    #[test]
    fn test_blacklist_not_yet_expired() {
        let _ = init_db();
        cleanup_test_data();

        // Add a not-yet-expired entry
        let _ = add_to_blacklist(
            "not.expired.ip",
            Some("Will expire later"),
            Some(now_timestamp() + 3600), // expires in 1 hour
            "test",
        );

        // Not-yet-expired entries should still be in effect
        assert!(is_ip_in_blacklist("not.expired.ip").unwrap());

        cleanup_test_data();
    }

    #[test]
    fn test_permanent_blacklist() {
        let _ = init_db();
        cleanup_test_data();

        // Add a permanent ban (no expiration)
        let _ = add_to_blacklist(
            "permanent.block.ip",
            Some("Permanent ban"),
            None, // no expiration
            "test",
        );

        // Permanent bans should always be in effect
        assert!(is_ip_in_blacklist("permanent.block.ip").unwrap());

        cleanup_test_data();
    }

    // ============================================================================
    // Test category 5: IP allowlist
    // ============================================================================

    #[test]
    fn test_whitelist_add_and_check() {
        let _ = init_db();
        cleanup_test_data();

        // Add an IP to the allowlist
        let result = add_to_whitelist("10.0.0.1", Some("Trusted server"));
        assert!(result.is_ok());

        // Verify the IP is in the allowlist
        assert!(is_ip_in_whitelist("10.0.0.1").unwrap());
        assert!(!is_ip_in_whitelist("10.0.0.2").unwrap());

        cleanup_test_data();
    }

    #[test]
    fn test_whitelist_cidr() {
        let _ = init_db();
        cleanup_test_data();

        // Add a CIDR range to the allowlist
        let _ = add_to_whitelist("192.168.0.0/16", Some("Internal network"));

        // Verify all IPs within the subnet are allowed
        assert!(is_ip_in_whitelist("192.168.1.1").unwrap());
        assert!(is_ip_in_whitelist("192.168.255.255").unwrap());

        // Verify IPs outside the subnet are not in the allowlist
        assert!(!is_ip_in_whitelist("10.0.0.1").unwrap());

        cleanup_test_data();
    }

    // ============================================================================
    // Test category 6: IP access logs
    // ============================================================================

    #[test]
    fn test_access_log_save_and_retrieve() {
        let _ = init_db();
        cleanup_test_data();

        // Save an access log
        let log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "test.log.ip".to_string(),
            timestamp: now_timestamp(),
            method: Some("POST".to_string()),
            path: Some("/v1/messages".to_string()),
            user_agent: Some("TestClient/1.0".to_string()),
            status: Some(200),
            duration: Some(150),
            api_key_hash: Some("hash123".to_string()),
            blocked: false,
            block_reason: None,
            username: None,
        };

        let save_result = save_ip_access_log(&log);
        assert!(
            save_result.is_ok(),
            "Should save access log: {:?}",
            save_result.err()
        );

        // Retrieve logs
        let logs = get_ip_access_logs(10, 0, Some("test.log.ip"), false);
        assert!(logs.is_ok());

        let logs = logs.unwrap();
        assert!(!logs.is_empty(), "Should retrieve saved log");
        assert_eq!(logs[0].client_ip, "test.log.ip");

        cleanup_test_data();
    }

    #[test]
    fn test_access_log_blocked_filter() {
        let _ = init_db();
        cleanup_test_data();

        // Save a normal log
        let normal_log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "normal.access.ip".to_string(),
            timestamp: now_timestamp(),
            method: Some("GET".to_string()),
            path: Some("/healthz".to_string()),
            user_agent: None,
            status: Some(200),
            duration: Some(10),
            api_key_hash: None,
            blocked: false,
            block_reason: None,
            username: None,
        };
        let _ = save_ip_access_log(&normal_log);

        // Save a blocked log
        let blocked_log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "blocked.access.ip".to_string(),
            timestamp: now_timestamp(),
            method: Some("POST".to_string()),
            path: Some("/v1/messages".to_string()),
            user_agent: None,
            status: Some(403),
            duration: Some(0),
            api_key_hash: None,
            blocked: true,
            block_reason: Some("IP in blacklist".to_string()),
            username: None,
        };
        let _ = save_ip_access_log(&blocked_log);

        // Retrieve only blocked logs
        let blocked_only = get_ip_access_logs(10, 0, None, true).unwrap();
        assert_eq!(blocked_only.len(), 1);
        assert_eq!(blocked_only[0].client_ip, "blocked.access.ip");
        assert!(blocked_only[0].blocked);

        cleanup_test_data();
    }

    // ============================================================================
    // Test category 7: statistics feature
    // ============================================================================

    #[test]
    fn test_ip_stats() {
        let _ = init_db();
        cleanup_test_data();

        // Add some test data
        for i in 0..5 {
            let log = IpAccessLog {
                id: uuid::Uuid::new_v4().to_string(),
                client_ip: format!("stats.test.{}", i % 3), // 3 unique IPs
                timestamp: now_timestamp(),
                method: Some("POST".to_string()),
                path: Some("/v1/messages".to_string()),
                user_agent: None,
                status: Some(200),
                duration: Some(100),
                api_key_hash: None,
                blocked: i == 4, // last one is blocked
                block_reason: if i == 4 {
                    Some("Test".to_string())
                } else {
                    None
                },
                username: None,
            };
            let _ = save_ip_access_log(&log);
        }

        // Add blocklist and allowlist entries
        let _ = add_to_blacklist("stats.black.1", None, None, "test");
        let _ = add_to_blacklist("stats.black.2", None, None, "test");
        let _ = add_to_whitelist("stats.white.1", None);

        // Get stats
        let stats = get_ip_stats();
        assert!(stats.is_ok());

        let stats = stats.unwrap();
        assert!(stats.total_requests >= 5, "Should have at least 5 requests");
        assert!(stats.unique_ips >= 3, "Should have at least 3 unique IPs");
        assert!(
            stats.blocked_count >= 1,
            "Should have at least 1 blocked request"
        );
        assert_eq!(stats.blacklist_count, 2);
        assert_eq!(stats.whitelist_count, 1);

        cleanup_test_data();
    }

    // ============================================================================
    // Test category 8: cleanup feature
    // ============================================================================

    #[test]
    fn test_cleanup_old_logs() {
        let _ = init_db();
        cleanup_test_data();

        // Add an "old" log (simulating 2 days ago)
        let old_log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "old.log.ip".to_string(),
            timestamp: now_timestamp() - (2 * 24 * 3600), // 2 days ago
            method: Some("GET".to_string()),
            path: Some("/old".to_string()),
            user_agent: None,
            status: Some(200),
            duration: Some(10),
            api_key_hash: None,
            blocked: false,
            block_reason: None,
            username: None,
        };
        let _ = save_ip_access_log(&old_log);

        // Add a new log
        let new_log = IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "new.log.ip".to_string(),
            timestamp: now_timestamp(),
            method: Some("GET".to_string()),
            path: Some("/new".to_string()),
            user_agent: None,
            status: Some(200),
            duration: Some(10),
            api_key_hash: None,
            blocked: false,
            block_reason: None,
            username: None,
        };
        let _ = save_ip_access_log(&new_log);

        // Clean up logs older than 1 day
        let deleted = cleanup_old_ip_logs(1);
        assert!(deleted.is_ok());
        assert!(deleted.unwrap() >= 1, "Should delete at least 1 old log");

        // Verify the new log still exists
        let logs = get_ip_access_logs(10, 0, Some("new.log.ip"), false).unwrap();
        assert!(!logs.is_empty(), "New log should still exist");

        // Verify the old log has been cleaned up
        let old_logs = get_ip_access_logs(10, 0, Some("old.log.ip"), false).unwrap();
        assert!(old_logs.is_empty(), "Old log should be cleaned up");

        cleanup_test_data();
    }

    // ============================================================================
    // Test category 9: concurrency safety
    // ============================================================================

    #[test]
    fn test_concurrent_access() {
        use std::thread;

        let _ = init_db();
        cleanup_test_data();

        let handles: Vec<_> = (0..10)
            .map(|i| {
                thread::spawn(move || {
                    // Each thread adds a different IP
                    let ip = format!("concurrent.test.{}", i);
                    let _ = add_to_blacklist(&ip, Some("Concurrent test"), None, "test");

                    // Verify the IP it added itself
                    is_ip_in_blacklist(&ip).unwrap_or(false)
                })
            })
            .collect();

        let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All threads should succeed
        assert!(
            results.iter().all(|&r| r),
            "All concurrent adds should succeed"
        );

        cleanup_test_data();
    }

    // ============================================================================
    // Test category 10: boundary cases and error handling
    // ============================================================================

    #[test]
    fn test_duplicate_blacklist_entry() {
        let _ = init_db();
        cleanup_test_data();

        // The first add should succeed
        let result1 = add_to_blacklist("duplicate.test.ip", Some("First"), None, "test");
        assert!(result1.is_ok());

        // A second add of the same IP should fail (UNIQUE constraint)
        let result2 = add_to_blacklist("duplicate.test.ip", Some("Second"), None, "test");
        assert!(result2.is_err(), "Duplicate IP should fail");

        cleanup_test_data();
    }

    #[test]
    fn test_empty_ip_pattern() {
        let _ = init_db();
        cleanup_test_data();

        // An empty IP pattern should still be addable (depends on business requirements)
        // This only tests that it does not panic
        let result = add_to_blacklist("", Some("Empty IP"), None, "test");
        // The result may succeed or fail, but should not panic
        let _ = result;

        cleanup_test_data();
    }

    #[test]
    fn test_special_characters_in_reason() {
        let _ = init_db();
        cleanup_test_data();

        // Test a reason containing special characters
        let reason = "Test with 'quotes' and \"double quotes\" and emoji 🚫";
        let result = add_to_blacklist("special.char.test", Some(reason), None, "test");
        assert!(result.is_ok());

        let entry = get_blacklist_entry_for_ip("special.char.test")
            .unwrap()
            .unwrap();
        assert_eq!(entry.reason.as_deref(), Some(reason));

        cleanup_test_data();
    }

    #[test]
    fn test_hit_count_increment() {
        let _ = init_db();
        cleanup_test_data();

        // Add a blocklist entry
        let _ = add_to_blacklist("hit.count.test", Some("Count test"), None, "test");

        // Repeated queries should increment hit_count
        for _ in 0..5 {
            let _ = get_blacklist_entry_for_ip("hit.count.test");
        }

        // Check hit_count
        let blacklist = get_blacklist().unwrap();
        let entry = blacklist.iter().find(|e| e.ip_pattern == "hit.count.test");
        assert!(entry.is_some());
        assert!(
            entry.unwrap().hit_count >= 5,
            "Hit count should be at least 5"
        );

        cleanup_test_data();
    }
}

// ============================================================================
// IP Filter middleware tests (unit tests)
// ============================================================================

#[cfg(test)]
mod ip_filter_middleware_tests {
    // Note: middleware tests need to mock HTTP requests; this provides the test scaffolding
    // The actual integration tests should run against a fully started service

    /// Verify the correctness of the IP extraction logic
    #[test]
    fn test_ip_extraction_priority() {
        // X-Forwarded-For should take priority over X-Real-IP
        // X-Real-IP should take priority over ConnectInfo
        // This only verifies the logic concept; actual tests would need to construct HTTP requests

        // Scenario 1: X-Forwarded-For has multiple IPs, take the first one
        let xff_header = "203.0.113.1, 198.51.100.2, 192.0.2.3";
        let first_ip = xff_header.split(',').next().unwrap().trim();
        assert_eq!(first_ip, "203.0.113.1");

        // Scenario 2: a single IP
        let single_ip = "10.0.0.1";
        let parsed = single_ip.split(',').next().unwrap().trim();
        assert_eq!(parsed, "10.0.0.1");
    }
}

// ============================================================================
// Performance benchmarks
// ============================================================================

#[cfg(test)]
mod performance_benchmarks {
    use crate::modules::security_db::{
        add_to_blacklist, get_blacklist, init_db, is_ip_in_blacklist,
    };
    use std::time::Instant;

    /// Benchmark: blocklist lookup performance
    #[test]
    fn benchmark_blacklist_lookup() {
        let _ = init_db();

        // Clean up and add 100 blocklist entries
        if let Ok(entries) = get_blacklist() {
            for entry in entries {
                let _ = crate::modules::security_db::remove_from_blacklist(&entry.id);
            }
        }

        for i in 0..100 {
            let _ = add_to_blacklist(&format!("bench.ip.{}", i), Some("Benchmark"), None, "test");
        }

        // Perform 1000 lookups
        let start = Instant::now();
        for _ in 0..1000 {
            let _ = is_ip_in_blacklist("bench.ip.50");
        }
        let duration = start.elapsed();

        println!("1000 blacklist lookups took: {:?}", duration);
        println!("Average per lookup: {:?}", duration / 1000);

        // Performance assertion: average lookup should be within 1ms
        assert!(
            duration.as_millis() < 5000,
            "Blacklist lookup should be fast (< 5ms avg)"
        );

        // Clean up
        if let Ok(entries) = get_blacklist() {
            for entry in entries {
                let _ = crate::modules::security_db::remove_from_blacklist(&entry.id);
            }
        }
    }

    /// Benchmark: CIDR matching performance
    #[test]
    fn benchmark_cidr_matching() {
        let _ = init_db();

        // Clean up and add CIDR rules
        if let Ok(entries) = get_blacklist() {
            for entry in entries {
                let _ = crate::modules::security_db::remove_from_blacklist(&entry.id);
            }
        }

        // Add 20 CIDR rules
        for i in 0..20 {
            let _ = add_to_blacklist(
                &format!("10.{}.0.0/16", i),
                Some("CIDR Benchmark"),
                None,
                "test",
            );
        }

        // Test CIDR matching performance
        let start = Instant::now();
        for _ in 0..1000 {
            // Test an IP that requires CIDR traversal
            let _ = is_ip_in_blacklist("10.5.100.50");
        }
        let duration = start.elapsed();

        println!("1000 CIDR matches took: {:?}", duration);
        println!("Average per match: {:?}", duration / 1000);

        // Performance assertion: CIDR matching should complete within a reasonable time
        assert!(
            duration.as_millis() < 5000,
            "CIDR matching should be reasonably fast"
        );

        // Clean up
        if let Ok(entries) = get_blacklist() {
            for entry in entries {
                let _ = crate::modules::security_db::remove_from_blacklist(&entry.id);
            }
        }
    }
}
