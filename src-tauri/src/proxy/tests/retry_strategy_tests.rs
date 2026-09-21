//! Retry strategy regression tests: adaptive budgets, quota evidence and rotation.

use crate::proxy::handlers::common::{
    calculate_max_retry_attempts, determine_retry_strategy, determine_retry_strategy_adaptive,
    next_rotation_attempt, should_rotate_account, RequestRetryState, RetryStrategy,
};
use std::time::Duration;

#[test]
fn test_retry_strategy_404() {
    assert!(matches!(
        determine_retry_strategy(404, "", false),
        RetryStrategy::FixedDelay(d) if d == Duration::from_millis(300)
    ));
}

#[test]
fn test_retry_strategy_single_account_without_delay_uses_backoff() {
    assert!(matches!(
        determine_retry_strategy(429, "rate limited", false),
        RetryStrategy::GraceRetry(d) if d == Duration::from_millis(3000)
    ));
}

#[test]
fn test_resource_exhausted_with_reset_delay_is_not_hard_quota() {
    let body = r#"{"error":{"message":"RESOURCE_EXHAUSTED","details":[{"quotaResetDelay":"4s"}]}}"#;
    assert!(matches!(
        determine_retry_strategy_adaptive(429, body, None, false, true, 0, 1),
        RetryStrategy::GraceRetry(d) if d == Duration::from_millis(4200)
    ));
}

#[test]
fn test_hard_quota_evidence_rotates_immediately() {
    for evidence in ["credits exhausted", "zero_quota", "weekly quota exceeded"] {
        assert!(matches!(
            determine_retry_strategy_adaptive(429, evidence, None, false, true, 0, 1),
            RetryStrategy::FixedDelay(d) if d == Duration::from_millis(50)
        ));
    }
}

#[test]
fn test_retry_strategy_503_and_529_escape_multi_account_pool() {
    for status in [503, 529] {
        assert!(matches!(
            determine_retry_strategy_adaptive(status, "", None, false, true, 0, 3),
            RetryStrategy::FixedDelay(d) if d == Duration::from_millis(50)
        ));
    }
}

#[test]
fn test_retry_strategy_503_single_account_backoff() {
    for status in [503, 529] {
        assert!(matches!(
            determine_retry_strategy(status, "", false),
            RetryStrategy::ExponentialBackoff {
                base_ms: 5000,
                max_ms: 30000
            }
        ));
    }
}

#[test]
fn test_retry_strategy_500() {
    assert!(matches!(
        determine_retry_strategy(500, "", false),
        RetryStrategy::LinearBackoff { base_ms: 3000 }
    ));
}

#[test]
fn test_retry_strategy_401_403() {
    for status in [401, 403] {
        assert!(matches!(
            determine_retry_strategy(status, "", false),
            RetryStrategy::FixedDelay(d) if d == Duration::from_millis(200)
        ));
    }
}

#[test]
fn test_retry_strategy_other() {
    for status in [200, 201, 301, 418, 502] {
        assert!(matches!(
            determine_retry_strategy(status, "", false),
            RetryStrategy::NoRetry
        ));
    }
}

#[test]
fn test_retry_strategy_400_thinking_signature() {
    for signature in [
        "Invalid `signature` for thinking",
        "Error with thinking.signature",
        "thinking.thinking block failed",
        "Corrupted thought signature detected",
    ] {
        assert!(matches!(
            determine_retry_strategy(400, signature, false),
            RetryStrategy::FixedDelay(d) if d == Duration::from_millis(200)
        ));
    }
}

#[test]
fn test_retry_strategy_400_no_signature() {
    assert!(matches!(
        determine_retry_strategy(400, "bad request", false),
        RetryStrategy::NoRetry
    ));
}

#[test]
fn test_rotate_account_true_cases() {
    for status in [429, 401, 403, 404, 500, 503, 529] {
        assert!(should_rotate_account(status, None));
    }
}

#[test]
fn test_rotate_account_false_cases() {
    for status in [400, 200, 502] {
        assert!(!should_rotate_account(status, None));
    }
    assert!(!should_rotate_account(
        429,
        Some(&RetryStrategy::GraceRetry(Duration::from_millis(1)))
    ));
}

#[test]
fn test_calculate_max_retry_attempts_adaptive() {
    assert_eq!(calculate_max_retry_attempts(0), 3);
    assert_eq!(calculate_max_retry_attempts(1), 3);
    assert_eq!(calculate_max_retry_attempts(2), 4);
    assert_eq!(calculate_max_retry_attempts(3), 6);
    assert_eq!(calculate_max_retry_attempts(5), 10);
    assert_eq!(calculate_max_retry_attempts(8), 12);
    assert_eq!(calculate_max_retry_attempts(20), 12);
}

#[test]
fn test_adaptive_retry_multi_account_round_1_fast_rotates() {
    let body = r#"{"error":{"message":"RESOURCE_EXHAUSTED","details":[{"quotaResetDelay":"4s"}]}}"#;
    assert!(matches!(
        determine_retry_strategy_adaptive(429, body, None, false, true, 0, 3),
        RetryStrategy::FixedDelay(d) if d == Duration::from_millis(50)
    ));
}

#[test]
fn test_adaptive_retry_multi_account_round_2_small_gap_micro_waits() {
    let body = r#"{"error":{"message":"RESOURCE_EXHAUSTED","details":[{"quotaResetDelay":"3s"}]}}"#;
    assert!(matches!(
        determine_retry_strategy_adaptive(429, body, None, false, true, 3, 3),
        RetryStrategy::GraceRetry(d) if d == Duration::from_millis(3200)
    ));
}

#[test]
fn test_adaptive_retry_multi_account_round_2_large_gap_rotates() {
    let body =
        r#"{"error":{"message":"RESOURCE_EXHAUSTED","details":[{"quotaResetDelay":"15s"}]}}"#;
    assert!(matches!(
        determine_retry_strategy_adaptive(429, body, None, false, true, 3, 3),
        RetryStrategy::FixedDelay(d) if d == Duration::from_millis(50)
    ));
}

#[test]
fn test_retry_attempt_budget_completes_single_and_multi_account_rounds() {
    let mut state = RequestRetryState::default();
    let mut used = 0;
    let mut retry_same = false;
    let mut single_attempts = 0;
    while let Some(attempt) = next_rotation_attempt(&mut used, 3, retry_same) {
        single_attempts += 1;
        let strategy =
            state.determine_strategy_adaptive("one", 429, "rate limited", None, false, attempt, 1);
        retry_same = matches!(strategy, RetryStrategy::GraceRetry(_));
    }
    assert_eq!(single_attempts, 3);

    let mut used = 0;
    let mut retry_same = false;
    let mut multi_attempts = 0;
    while let Some(attempt) = next_rotation_attempt(&mut used, 4, retry_same) {
        multi_attempts += 1;
        let strategy =
            determine_retry_strategy_adaptive(429, "rate limited", None, false, true, attempt, 2);
        retry_same = matches!(strategy, RetryStrategy::GraceRetry(_));
    }
    assert_eq!(multi_attempts, 4);
}
