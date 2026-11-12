use antigravity_tools_lib::proxy::{
    config::{StreamTimeoutConfig, UploadTimeoutConfig},
    pipeline::{
        rotation::{record_attempted_account, RequestRetryState},
        terminal::{apply_terminal, FailureStatusTracker, TerminalNextStep, UpstreamTerminal},
    },
    sticky_config::SchedulingMode,
    upstream::client::{PayloadMediaKind, UpstreamClient},
};
use axum::http::StatusCode;
use std::collections::HashSet;

#[test]
fn classified_upload_failures_retry_once_without_cross_account_replay_or_penalty() {
    for protocol in ["claude", "gemini", "openai", "responses"] {
        for mode in [
            SchedulingMode::CacheFirst,
            SchedulingMode::Balance,
            SchedulingMode::PerformanceFirst,
        ] {
            for terminal in [UpstreamTerminal::Upload, UpstreamTerminal::UploadStalled] {
                let mut tracker = FailureStatusTracker::default();
                let handling = apply_terminal(
                    &format!("{}connection interrupted", terminal.marker()),
                    &mut tracker,
                    protocol,
                    1,
                    4,
                    1.0,
                    |_| panic!("transport failures must not penalize an account"),
                )
                .expect("upload failure classification");
                assert_eq!(handling.terminal, terminal);
                assert_eq!(tracker.final_status(), StatusCode::GATEWAY_TIMEOUT);
                assert!(!handling.disposition.cross_account_replay_allowed);
                let mut retries = RequestRetryState::default();
                let first = retries.plan_terminal("account-a", handling.disposition, mode, true);
                assert_eq!(first.next, TerminalNextStep::RetrySameAccount);
                assert!(!first.abandon_sticky_account);
                let second = retries.plan_terminal("account-a", handling.disposition, mode, true);
                assert_eq!(second.next, TerminalNextStep::Stop);
                assert!(!second.abandon_sticky_account);
            }
        }
    }
}

#[test]
fn balance_transport_reset_retries_then_excludes_the_failed_account() {
    let mut tracker = FailureStatusTracker::default();
    let handling = apply_terminal(
        &format!(
            "{}connection reset",
            UpstreamTerminal::TransportReset.marker()
        ),
        &mut tracker,
        "responses",
        1,
        4,
        1.0,
        |_| panic!("transport reset must not penalize the account"),
    )
    .unwrap();
    assert_eq!(tracker.final_status(), StatusCode::BAD_GATEWAY);
    let mut retries = RequestRetryState::default();
    let first = retries.plan_terminal(
        "account-a",
        handling.disposition,
        SchedulingMode::Balance,
        true,
    );
    assert_eq!(first.next, TerminalNextStep::RetrySameAccount);
    assert!(!first.abandon_sticky_account);
    let second = retries.plan_terminal(
        "account-a",
        handling.disposition,
        SchedulingMode::Balance,
        true,
    );
    assert_eq!(second.next, TerminalNextStep::Rotate);
    assert!(second.abandon_sticky_account);
    let mut attempted = HashSet::new();
    let mut force_rotate = false;
    record_attempted_account(&mut attempted, "account-a", 2, &mut force_rotate);
    assert!(force_rotate);
    assert!(attempted.contains("account-a"));
    assert!(!attempted.contains("account-b"));
}

#[test]
fn cache_first_preserves_binding_while_global_exhaustion_stops_immediately() {
    let mut retries = RequestRetryState::default();
    let first_byte = UpstreamTerminal::FirstByte.disposition();
    assert_eq!(
        retries
            .plan_terminal("account-a", first_byte, SchedulingMode::CacheFirst, true)
            .next,
        TerminalNextStep::RetrySameAccount
    );
    let exhausted_retry =
        retries.plan_terminal("account-a", first_byte, SchedulingMode::CacheFirst, true);
    assert_eq!(exhausted_retry.next, TerminalNextStep::Stop);
    assert!(!exhausted_retry.abandon_sticky_account);
    for terminal in [UpstreamTerminal::GlobalBudget, UpstreamTerminal::ReplayCap] {
        let plan = retries.plan_terminal(
            "account-b",
            terminal.disposition(),
            SchedulingMode::CacheFirst,
            true,
        );
        assert_eq!(plan.next, TerminalNextStep::Stop);
        assert!(!plan.abandon_sticky_account);
    }
}

#[test]
fn payload_endpoint_limits_and_fallback_cap_follow_configuration() {
    let mut upload = UploadTimeoutConfig::default();
    let small = UpstreamClient::plan_upstream_timeouts_with_observed(
        PayloadMediaKind::TextChat,
        1024 * 1024,
        &upload,
        None,
    );
    assert!(
        !small.large_body,
        "token estimates must not impose upload byte limits"
    );
    assert!(small.endpoint_attempt_limit > 1);
    let large = UpstreamClient::plan_upstream_timeouts_with_observed(
        PayloadMediaKind::TextChat,
        upload.large_body_threshold_bytes(),
        &upload,
        None,
    );
    assert!(large.large_body);
    assert_eq!(
        large.endpoint_attempt_limit,
        upload.large_body_endpoint_attempts
    );
    let stream = StreamTimeoutConfig::default();
    let fallback = |cfg: &UploadTimeoutConfig| {
        UpstreamClient::calculate_first_byte_wait_secs(
            PayloadMediaKind::TextChat,
            1,
            cfg,
            &stream,
            1024,
            false,
        )
    };
    assert_eq!(fallback(&upload), 45);
    upload.fallback_first_byte_cap_secs = 12;
    assert_eq!(fallback(&upload), 12);
    upload.fallback_first_byte_cap_secs = 0;
    assert!(
        fallback(&upload) > 45,
        "zero disables only the fallback-specific cap"
    );
}
