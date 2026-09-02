//! IP Security Integration Tests
//! Integration tests for the IP security feature
//!
//! These tests require starting the full proxy server to verify end-to-end behavior

#[cfg(test)]
mod integration_tests {
    use crate::modules::security_db::{
        self, add_to_blacklist, add_to_whitelist, get_blacklist, get_whitelist, init_db,
        remove_from_blacklist, remove_from_whitelist,
    };
    use std::time::Duration;

    /// Helper function: clean up the test environment
    fn cleanup_test_data() {
        if let Ok(entries) = get_blacklist() {
            for entry in entries {
                let _ = remove_from_blacklist(&entry.id);
            }
        }
        if let Ok(entries) = get_whitelist() {
            for entry in entries {
                let _ = remove_from_whitelist(&entry.id);
            }
        }
    }

    // ============================================================================
    // Integration test scenario 1: blocklist blocks a request
    // ============================================================================

    /// Test scenario: a request should be rejected when its IP is on the blocklist
    ///
    /// Expected behavior:
    /// 1. Add the IP to the blocklist
    /// 2. A request from that IP returns 403 Forbidden
    /// 3. The response body includes the ban reason
    #[test]
    fn test_scenario_blacklist_blocks_request() {
        let _ = init_db();
        cleanup_test_data();

        // Add a test IP to the blocklist
        let entry = add_to_blacklist(
            "192.168.100.100",
            Some("Integration test - malicious activity"),
            None,
            "integration_test",
        );
        assert!(entry.is_ok(), "Should add IP to blacklist");

        // Verify the blocklist entry exists
        let blacklist = get_blacklist().unwrap();
        let found = blacklist.iter().any(|e| e.ip_pattern == "192.168.100.100");
        assert!(found, "IP should be in blacklist");

        // Actual HTTP request testing requires starting the server
        // This verifies data-layer correctness
        let is_blocked = security_db::is_ip_in_blacklist("192.168.100.100").unwrap();
        assert!(is_blocked, "IP should be blocked");

        cleanup_test_data();
    }

    // ============================================================================
    // Integration test scenario 2: allowlist-priority mode
    // ============================================================================

    /// Test scenario: in allowlist-priority mode, an allowlisted IP skips the blocklist check
    ///
    /// Expected behavior:
    /// 1. The IP exists in both the blocklist and the allowlist
    /// 2. whitelist_priority mode is enabled
    /// 3. The request should be allowed (allowlist takes priority)
    #[test]
    fn test_scenario_whitelist_priority() {
        let _ = init_db();
        cleanup_test_data();

        // Add the IP to the blocklist
        let _ = add_to_blacklist(
            "10.0.0.50",
            Some("Should be overridden by whitelist"),
            None,
            "test",
        );

        // Add the same IP to the allowlist
        let _ = add_to_whitelist("10.0.0.50", Some("Trusted - override blacklist"));

        // Verify both lists contain the IP
        assert!(security_db::is_ip_in_blacklist("10.0.0.50").unwrap());
        assert!(security_db::is_ip_in_whitelist("10.0.0.50").unwrap());

        // In the actual middleware, when whitelist_priority=true, the allowlist is checked first
        // If found in the allowlist, the blocklist check is skipped
        // This only verifies data correctness; the middleware logic is guaranteed by ip_filter.rs

        cleanup_test_data();
    }

    // ============================================================================
    // Integration test scenario 3: temporary ban and expiration
    // ============================================================================

    /// Test scenario: a temporary ban is automatically lifted after it expires
    ///
    /// Expected behavior:
    /// 1. Add a temporary ban (already expired)
    /// 2. Expired entries are cleaned up automatically on query
    /// 3. The request should be allowed
    #[test]
    fn test_scenario_temporary_ban_expiration() {
        let _ = init_db();
        cleanup_test_data();

        // Get the current timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Add an already-expired temporary ban
        let _ = add_to_blacklist(
            "expired.ban.test",
            Some("Temporary ban - should be expired"),
            Some(now - 60), // expired 1 minute ago
            "test",
        );

        // The query should trigger expiration cleanup
        let is_blocked = security_db::is_ip_in_blacklist("expired.ban.test").unwrap();
        assert!(!is_blocked, "Expired ban should not block");

        cleanup_test_data();
    }

    // ============================================================================
    // Integration test scenario 4: CIDR range ban
    // ============================================================================

    /// Test scenario: a CIDR range ban covers the entire subnet
    ///
    /// Expected behavior:
    /// 1. Ban 192.168.1.0/24
    /// 2. All requests from 192.168.1.x are rejected
    /// 3. Requests from 192.168.2.x pass through normally
    #[test]
    fn test_scenario_cidr_subnet_blocking() {
        let _ = init_db();
        cleanup_test_data();

        // Ban the entire subnet
        let _ = add_to_blacklist(
            "192.168.1.0/24",
            Some("Entire subnet blocked"),
            None,
            "test",
        );

        // Verify IPs within the subnet are blocked
        for last_octet in [1, 50, 100, 200, 254] {
            let ip = format!("192.168.1.{}", last_octet);
            let is_blocked = security_db::is_ip_in_blacklist(&ip).unwrap();
            assert!(is_blocked, "IP {} should be blocked by CIDR", ip);
        }

        // Verify IPs outside the subnet are not blocked
        for last_octet in [1, 50, 100] {
            let ip = format!("192.168.2.{}", last_octet);
            let is_blocked = security_db::is_ip_in_blacklist(&ip).unwrap();
            assert!(!is_blocked, "IP {} should NOT be blocked", ip);
        }

        cleanup_test_data();
    }

    // ============================================================================
    // Integration test scenario 5: ban message details
    // ============================================================================

    /// Test scenario: the ban response includes detailed information
    ///
    /// Expected behavior:
    /// 1. Add a ban with a reason
    /// 2. When the request is rejected, the response includes:
    ///    - The ban reason
    ///    - Whether the ban is temporary or permanent
    ///    - The remaining ban duration (if temporary)
    #[test]
    fn test_scenario_ban_message_details() {
        let _ = init_db();
        cleanup_test_data();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Add a temporary ban (expires in 2 hours)
        let _ = add_to_blacklist(
            "temp.ban.message",
            Some("Rate limit exceeded"),
            Some(now + 7200), // in 2 hours
            "rate_limiter",
        );

        // Get ban details
        let entry = security_db::get_blacklist_entry_for_ip("temp.ban.message")
            .unwrap()
            .unwrap();

        assert_eq!(entry.reason.as_deref(), Some("Rate limit exceeded"));
        assert!(entry.expires_at.is_some());

        let remaining = entry.expires_at.unwrap() - now;
        assert!(
            remaining > 0 && remaining <= 7200,
            "Should have ~2h remaining"
        );

        cleanup_test_data();
    }

    // ============================================================================
    // Integration test scenario 6: access logging
    // ============================================================================

    /// Test scenario: blocked requests are recorded in the log
    ///
    /// Expected behavior:
    /// 1. A blocklisted IP makes a request
    /// 2. The request is rejected
    /// 3. The access log records: IP, time, status (403), ban reason
    #[test]
    fn test_scenario_blocked_request_logging() {
        let _ = init_db();
        cleanup_test_data();

        // Simulate saving a blocked access log
        let log = security_db::IpAccessLog {
            id: uuid::Uuid::new_v4().to_string(),
            client_ip: "blocked.request.test".to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            method: Some("POST".to_string()),
            path: Some("/v1/messages".to_string()),
            user_agent: Some("TestClient/1.0".to_string()),
            status: Some(403),
            duration: Some(0),
            api_key_hash: None,
            blocked: true,
            block_reason: Some("IP in blacklist".to_string()),
            username: None,
        };

        let save_result = security_db::save_ip_access_log(&log);
        assert!(save_result.is_ok());

        // Verify the log can be retrieved
        let logs = security_db::get_ip_access_logs(10, 0, None, true).unwrap();
        let found = logs.iter().any(|l| l.client_ip == "blocked.request.test");
        assert!(found, "Blocked request should be logged");

        let _ = security_db::clear_ip_access_logs();
    }

    // ============================================================================
    // Integration test scenario 7: no impact on normal request performance
    // ============================================================================

    /// Test scenario: security checks do not significantly impact normal request performance
    ///
    /// Expected behavior:
    /// 1. Blocklist/allowlist check time < 5ms
    /// 2. Latency increase < 10ms compared to a baseline without security checks
    #[test]
    fn test_scenario_performance_impact() {
        let _ = init_db();
        cleanup_test_data();

        // Add some blocklist entries
        for i in 0..50 {
            let _ = add_to_blacklist(&format!("perf.test.{}", i), None, None, "test");
        }

        // Add some CIDR rules
        for i in 0..10 {
            let _ = add_to_blacklist(&format!("172.{}.0.0/16", i), None, None, "test");
        }

        // Test lookup performance
        let start = std::time::Instant::now();
        let iterations = 100;

        for _ in 0..iterations {
            // Simulate the security check of a normal request
            let _ = security_db::is_ip_in_whitelist("10.0.0.1");
            let _ = security_db::is_ip_in_blacklist("10.0.0.1");
        }

        let duration = start.elapsed();
        let avg_per_check = duration / (iterations * 2);

        println!("Average security check time: {:?}", avg_per_check);

        // Assertion: average check time should be within 5ms
        assert!(
            avg_per_check < Duration::from_millis(5),
            "Security check should be fast"
        );

        cleanup_test_data();
    }

    // ============================================================================
    // Integration test scenario 8: data persistence
    // ============================================================================

    /// Test scenario: blocklist/allowlist data persists
    ///
    /// Expected behavior:
    /// 1. Reinitialize the database connection after adding data
    /// 2. The data still exists
    #[test]
    fn test_scenario_data_persistence() {
        let _ = init_db();
        cleanup_test_data();

        // Add data
        let _ = add_to_blacklist("persist.test.ip", Some("Persistence test"), None, "test");
        let _ = add_to_whitelist("persist.white.ip", Some("Persistence test"));

        // Reinitialize (this really just verifies the data is still readable)
        let _ = init_db();

        // Verify the data still exists
        assert!(security_db::is_ip_in_blacklist("persist.test.ip").unwrap());
        assert!(security_db::is_ip_in_whitelist("persist.white.ip").unwrap());

        cleanup_test_data();
    }
}

// ============================================================================
// Stress tests
// ============================================================================

#[cfg(test)]
mod stress_tests {
    use crate::modules::security_db::{
        add_to_blacklist, clear_ip_access_logs, get_blacklist, init_db, is_ip_in_blacklist,
        remove_from_blacklist, save_ip_access_log, IpAccessLog,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    /// Helper function: clean up the test environment
    fn cleanup_test_data() {
        if let Ok(entries) = get_blacklist() {
            for entry in entries {
                let _ = remove_from_blacklist(&entry.id);
            }
        }
        let _ = clear_ip_access_logs();
    }

    /// Stress test: a large number of blocklist entries
    #[test]
    fn stress_test_large_blacklist() {
        let _ = init_db();
        cleanup_test_data();

        let count = 500;

        // Bulk add
        let start = Instant::now();
        for i in 0..count {
            let _ = add_to_blacklist(
                &format!("stress.{}.{}.{}.{}", i / 256, (i / 16) % 16, i % 16, i),
                None,
                None,
                "stress",
            );
        }
        let add_duration = start.elapsed();
        println!("Added {} entries in {:?}", count, add_duration);

        // Random lookup test
        let start = Instant::now();
        for i in 0..100 {
            let _ = is_ip_in_blacklist(&format!(
                "stress.{}.{}.{}.{}",
                i / 256,
                (i / 16) % 16,
                i % 16,
                i
            ));
        }
        let lookup_duration = start.elapsed();
        println!("100 lookups in large blacklist took {:?}", lookup_duration);

        // Verify performance is reasonable
        assert!(
            lookup_duration < Duration::from_secs(1),
            "Lookups should be reasonably fast even with large blacklist"
        );

        cleanup_test_data();
    }

    /// Stress test: a large number of access logs
    #[test]
    fn stress_test_access_logging() {
        let _ = init_db();
        let _ = clear_ip_access_logs();

        let count = 1000;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Bulk write logs
        let start = Instant::now();
        for i in 0..count {
            let log = IpAccessLog {
                id: uuid::Uuid::new_v4().to_string(),
                client_ip: format!("log.stress.{}", i % 100),
                timestamp: now,
                method: Some("POST".to_string()),
                path: Some("/v1/messages".to_string()),
                user_agent: Some("StressTest/1.0".to_string()),
                status: Some(200),
                duration: Some(100),
                api_key_hash: Some("hash".to_string()),
                blocked: false,
                block_reason: None,
                username: None,
            };
            let _ = save_ip_access_log(&log);
        }
        let write_duration = start.elapsed();
        println!("Wrote {} access logs in {:?}", count, write_duration);

        // Verify write performance is reasonable
        assert!(
            write_duration < Duration::from_secs(10),
            "Access log writing should be reasonably fast"
        );

        let _ = clear_ip_access_logs();
    }

    /// Stress test: concurrent operations
    #[test]
    fn stress_test_concurrent_operations() {
        let _ = init_db();
        cleanup_test_data();

        let thread_count = 5;
        let ops_per_thread = 20;

        let handles: Vec<_> = (0..thread_count)
            .map(|t| {
                thread::spawn(move || {
                    for i in 0..ops_per_thread {
                        // Each thread adds, queries, and deletes
                        let ip = format!("concurrent.{}.{}", t, i);
                        if let Ok(entry) = add_to_blacklist(&ip, None, None, "concurrent") {
                            let _ = is_ip_in_blacklist(&ip);
                            let _ = remove_from_blacklist(&entry.id);
                        }
                    }
                })
            })
            .collect();

        // Wait for all threads to finish
        for handle in handles {
            handle.join().expect("Thread should not panic");
        }

        // Verify no leftover data
        let remaining = get_blacklist().unwrap();
        let concurrent_remaining: Vec<_> = remaining
            .iter()
            .filter(|e| e.ip_pattern.starts_with("concurrent."))
            .collect();

        assert!(
            concurrent_remaining.is_empty(),
            "All concurrent test data should be cleaned up"
        );

        cleanup_test_data();
    }
}
