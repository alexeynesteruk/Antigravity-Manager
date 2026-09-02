//! Tests RateLimitTracker::parse_from_error's handling of the 404 status code:
//! - Short lockout (5s)
//! - Does not accumulate the failure count
//! - Difference from the 5xx lockout duration

use crate::proxy::rate_limit::{RateLimitReason, RateLimitTracker};

#[test]
fn test_parse_from_error_404_short_lockout() {
    let tracker = RateLimitTracker::new();
    let backoff_steps = vec![60, 300, 1800, 7200];

    let info = tracker.parse_from_error("acc_404", 404, None, "Not Found", None, &backoff_steps);
    assert!(info.is_some(), "404 should return Some(RateLimitInfo)");
    let info = info.unwrap();
    assert_eq!(info.retry_after_sec, 5, "404 should lock out for 5 seconds");
    assert_eq!(
        info.reason,
        RateLimitReason::ServerError,
        "404 reason should be ServerError"
    );
}

#[test]
fn test_404_does_not_accumulate_failure_count() {
    let tracker = RateLimitTracker::new();
    let backoff_steps = vec![60, 300, 1800, 7200];

    // Repeated 404s should always lock out for 5s (unlike 429 QuotaExhausted, which escalates)
    for i in 1..=5 {
        // Clear the previous rate-limit record, simulating hitting 404 again after rotation
        tracker.clear("acc_404_repeat");
        let info = tracker.parse_from_error(
            "acc_404_repeat",
            404,
            None,
            "Not Found",
            None,
            &backoff_steps,
        );
        assert!(info.is_some(), "404 attempt {} should return Some", i);
        assert_eq!(
            info.unwrap().retry_after_sec,
            5,
            "404 attempt {} should still lock for 5s, not escalate",
            i
        );
    }
}

#[test]
fn test_404_vs_5xx_lockout_duration() {
    let tracker = RateLimitTracker::new();
    let backoff_steps = vec![60, 300, 1800, 7200];

    // 404 → 5s lockout
    let info_404 =
        tracker.parse_from_error("acc_cmp_404", 404, None, "Not Found", None, &backoff_steps);
    assert_eq!(
        info_404.unwrap().retry_after_sec,
        5,
        "404 should lock for 5s"
    );

    // 503 → 8s lockout
    let info_503 = tracker.parse_from_error(
        "acc_cmp_503",
        503,
        None,
        "Service Unavailable",
        None,
        &backoff_steps,
    );
    assert_eq!(
        info_503.unwrap().retry_after_sec,
        8,
        "503 should lock for 8s"
    );
}
