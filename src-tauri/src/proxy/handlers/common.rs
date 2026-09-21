use crate::proxy::server::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use tokio::time::{sleep, Duration};
use tracing::{debug, info};

// ===== 统一重试与退避策略 =====

/// 重试策略枚举
#[derive(Debug, Clone)]
pub enum RetryStrategy {
    /// 不重试，直接返回错误
    NoRetry,
    /// 固定延迟
    FixedDelay(Duration),
    /// 线性退避：base_ms * (attempt + 1)
    LinearBackoff { base_ms: u64 },
    /// 指数退避：base_ms * 2^attempt，上限 max_ms
    ExponentialBackoff { base_ms: u64, max_ms: u64 },
    /// [NEW] 原地重试 (Grace Retry)：在当前账号上小窗口等待后直接重试，不计入常规切换
    GraceRetry(Duration),
}

#[derive(Debug, Default)]
pub struct RequestRetryState {
    grace_retried_accounts: HashSet<String>,
}

impl RequestRetryState {
    pub fn determine_strategy(
        &mut self,
        account_id: &str,
        status_code: u16,
        error_text: &str,
        retry_after: Option<&str>,
        retried_without_thinking: bool,
    ) -> RetryStrategy {
        self.determine_strategy_adaptive(
            account_id,
            status_code,
            error_text,
            retry_after,
            retried_without_thinking,
            0,
            1,
        )
    }

    /// Adaptive retry decision aware of the current attempt and account-pool size.
    pub fn determine_strategy_adaptive(
        &mut self,
        account_id: &str,
        status_code: u16,
        error_text: &str,
        retry_after: Option<&str>,
        retried_without_thinking: bool,
        attempt: usize,
        pool_size: usize,
    ) -> RetryStrategy {
        let allow_grace_retry = !self.grace_retried_accounts.contains(account_id);
        let strategy = determine_retry_strategy_adaptive(
            status_code,
            error_text,
            retry_after,
            retried_without_thinking,
            allow_grace_retry,
            attempt,
            pool_size,
        );
        if matches!(strategy, RetryStrategy::GraceRetry(_)) {
            self.grace_retried_accounts.insert(account_id.to_string());
        }
        strategy
    }
}

pub fn next_rotation_attempt(
    used_attempts: &mut usize,
    max_attempts: usize,
    _retry_same_account: bool,
) -> Option<usize> {
    if *used_attempts >= max_attempts {
        return None;
    }

    let attempt = *used_attempts;
    *used_attempts += 1;
    Some(attempt)
}

#[derive(Debug, Default)]
pub struct FailureStatusTracker {
    saw_failure: bool,
    last_non_rate_limit: Option<StatusCode>,
}

impl FailureStatusTracker {
    pub fn record(&mut self, status: StatusCode) {
        self.saw_failure = true;
        if status != StatusCode::TOO_MANY_REQUESTS {
            self.last_non_rate_limit = Some(status);
        }
    }

    pub fn final_status(&self) -> StatusCode {
        self.last_non_rate_limit.unwrap_or_else(|| {
            if self.saw_failure {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::BAD_GATEWAY
            }
        })
    }
}

/// Calculate the bounded retry budget for a request.
///
/// A single account gets three attempts (initial request plus two backoff retries).
/// Pools get two complete rotations, bounded to four through twelve attempts.
pub fn calculate_max_retry_attempts(pool_size: usize) -> usize {
    if pool_size <= 1 {
        3
    } else {
        (pool_size * 2).clamp(4, 12)
    }
}

pub(crate) fn is_invalid_signature_error(status: u16, error_text: &str) -> bool {
    status == 400
        && [
            "invalid thought signature",
            "invalid `signature`",
            "invalid signature",
            "thought_signature",
            "thoughtsignature",
            "thinking.signature",
            "thinking.thinking",
            "corrupted thought signature",
        ]
        .iter()
        .any(|needle| {
            error_text
                .as_bytes()
                .windows(needle.len())
                .any(|part| part.eq_ignore_ascii_case(needle.as_bytes()))
        })
}

/// Determine a retry strategy using the default single-account semantics.
pub fn determine_retry_strategy(
    status_code: u16,
    error_text: &str,
    retried_without_thinking: bool,
) -> RetryStrategy {
    determine_retry_strategy_adaptive(
        status_code,
        error_text,
        None,
        retried_without_thinking,
        true,
        0,
        1,
    )
}

/// Adaptive retry strategy aware of retry-delay hints, attempt number and pool size.
pub fn determine_retry_strategy_adaptive(
    status_code: u16,
    error_text: &str,
    retry_after: Option<&str>,
    retried_without_thinking: bool,
    allow_grace_retry: bool,
    attempt: usize,
    pool_size: usize,
) -> RetryStrategy {
    let lower = error_text.to_lowercase();
    match status_code {
        400 if !retried_without_thinking && is_invalid_signature_error(status_code, error_text) => {
            RetryStrategy::FixedDelay(Duration::from_millis(200))
        }
        429 => {
            let parsed_delay = crate::proxy::upstream::retry::parse_retry_delay_with_source(
                error_text,
                retry_after,
            );
            // RESOURCE_EXHAUSTED is a transient classification unless the response
            // also carries deterministic evidence that the account is exhausted.
            let hard_quota = parsed_delay.is_none()
                && (lower.contains("quota_exhausted")
                    || lower.contains("exceeded your current quota")
                    || lower.contains("insufficient_quota")
                    || lower.contains("credits")
                    || lower.contains("zero_quota")
                    || lower.contains("weekly quota"));
            if hard_quota {
                return RetryStrategy::FixedDelay(Duration::from_millis(50));
            }

            if pool_size <= 1 {
                if let Some(delay) = parsed_delay {
                    let wait_ms = delay.actual_wait_ms();
                    return if allow_grace_retry && wait_ms <= 30_000 {
                        RetryStrategy::GraceRetry(Duration::from_millis(wait_ms))
                    } else {
                        RetryStrategy::FixedDelay(Duration::from_millis(wait_ms.min(30_000)))
                    };
                }
                let backoff_ms = (3_000 * (attempt as u64 + 1)).min(10_000);
                return if allow_grace_retry {
                    RetryStrategy::GraceRetry(Duration::from_millis(backoff_ms))
                } else {
                    RetryStrategy::FixedDelay(Duration::from_millis(backoff_ms))
                };
            }

            // First pass quickly escapes each account; the second pass can wait
            // for a short reset window or continue looking for a healthy account.
            if attempt < pool_size {
                return RetryStrategy::FixedDelay(Duration::from_millis(50));
            }
            if let Some(delay) = parsed_delay {
                let wait_ms = delay.actual_wait_ms();
                if wait_ms <= 5_000 && allow_grace_retry {
                    return RetryStrategy::GraceRetry(Duration::from_millis(wait_ms));
                }
                if pool_size > 2 && attempt + 1 < pool_size * 2 {
                    return RetryStrategy::FixedDelay(Duration::from_millis(50));
                }
                return RetryStrategy::FixedDelay(Duration::from_millis(wait_ms.min(12_000)));
            }
            let backoff_ms = (2_000 * (attempt.saturating_sub(pool_size) as u64 + 1)).min(5_000);
            RetryStrategy::FixedDelay(Duration::from_millis(backoff_ms))
        }
        503 | 529 => {
            if pool_size > 1 && attempt < pool_size {
                RetryStrategy::FixedDelay(Duration::from_millis(50))
            } else {
                RetryStrategy::ExponentialBackoff {
                    base_ms: 5_000,
                    max_ms: 30_000,
                }
            }
        }
        500 => RetryStrategy::LinearBackoff { base_ms: 3_000 },
        401 | 403 => RetryStrategy::FixedDelay(Duration::from_millis(200)),
        404 => RetryStrategy::FixedDelay(Duration::from_millis(300)),
        _ => RetryStrategy::NoRetry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_recovery_matches_retry_classification_once() {
        for error in [
            "INVALID THOUGHT SIGNATURE.",
            "Corrupted thought signature",
            "thinking.thinking: Field required",
            "Invalid `signature`",
        ] {
            assert!(is_invalid_signature_error(400, error));
            assert!(matches!(
                determine_retry_strategy(400, error, false),
                RetryStrategy::FixedDelay(_)
            ));
            assert!(matches!(
                determine_retry_strategy(400, error, true),
                RetryStrategy::NoRetry
            ));
        }
        assert!(!is_invalid_signature_error(500, "Invalid signature"));
        assert!(!is_invalid_signature_error(
            400,
            "Unrelated invalid request"
        ));
    }

    #[test]
    fn adaptive_budget_and_status_tracking() {
        assert_eq!(calculate_max_retry_attempts(1), 3);
        assert_eq!(calculate_max_retry_attempts(3), 6);
        assert_eq!(calculate_max_retry_attempts(20), 12);

        let mut tracker = FailureStatusTracker::default();
        tracker.record(StatusCode::SERVICE_UNAVAILABLE);
        tracker.record(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(tracker.final_status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

/// 执行退避策略并返回是否应该继续重试
pub async fn apply_retry_strategy(
    strategy: RetryStrategy,
    attempt: usize,
    max_attempts: usize,
    status_code: u16,
    trace_id: &str,
) -> bool {
    match strategy {
        RetryStrategy::NoRetry => {
            debug!(
                "[{}] Non-retryable error {}, stopping",
                trace_id, status_code
            );
            false
        }

        RetryStrategy::FixedDelay(duration) => {
            let base_ms = duration.as_millis() as u64;
            info!(
                "[{}] ⏱️ Retry with fixed delay: status={}, attempt={}/{}, delay={}ms",
                trace_id,
                status_code,
                attempt + 1,
                max_attempts,
                base_ms
            );
            sleep(duration).await;
            true
        }

        RetryStrategy::LinearBackoff { base_ms } => {
            let calculated_ms = base_ms * (attempt as u64 + 1);
            info!(
                "[{}] ⏱️ Retry with linear backoff: status={}, attempt={}/{}, delay={}ms",
                trace_id,
                status_code,
                attempt + 1,
                max_attempts,
                calculated_ms
            );
            sleep(Duration::from_millis(calculated_ms)).await;
            true
        }

        RetryStrategy::ExponentialBackoff { base_ms, max_ms } => {
            let calculated_ms = (base_ms * 2_u64.pow(attempt as u32)).min(max_ms);
            info!(
                "[{}] ⏱️ Retry with exponential backoff: status={}, attempt={}/{}, delay={}ms",
                trace_id,
                status_code,
                attempt + 1,
                max_attempts,
                calculated_ms
            );
            sleep(Duration::from_millis(calculated_ms)).await;
            true
        }

        RetryStrategy::GraceRetry(duration) => {
            info!(
                "[{}] ⚡ Grace Retry: Performing micro-wait ({}ms) on current account...",
                trace_id,
                duration.as_millis()
            );
            sleep(duration).await;
            true // 原地重试在 handlers 层面通过 should_rotate_account 判断是否切换
        }
    }
}

/// 判断是否应该轮换账号
pub fn should_rotate_account(status_code: u16, strategy: Option<&RetryStrategy>) -> bool {
    // [NEW] 如果识别为 Grace Retry，则显式要求不轮换账号
    if let Some(RetryStrategy::GraceRetry(_)) = strategy {
        return false;
    }

    match status_code {
        // Account- and node-scoped failures should escape to another account.
        429 | 401 | 403 | 404 | 500 | 503 | 529 => true,
        _ => false,
    }
}

/// Detects model capabilities and configuration
/// POST /v1/models/detect
pub async fn handle_detect_model(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let model_name = body.get("model").and_then(|v| v.as_str()).unwrap_or("");

    if model_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "Missing 'model' field").into_response();
    }

    // 1. Resolve mapping
    let mapped_model = crate::proxy::common::model_mapping::resolve_model_route(
        model_name,
        &*state.custom_mapping.read().await,
    );

    // 2. Resolve capabilities
    let config = crate::proxy::mappers::common_utils::resolve_request_config(
        model_name,
        &mapped_model,
        &None, // We don't check tools for static capability detection
        None,  // size
        None,  // quality
        None,  // image_size
        None,  // body (not needed for static detection)
    );

    // 3. Construct response
    let mut response = json!({
        "model": model_name,
        "mapped_model": mapped_model,
        "type": config.request_type,
        "features": {
            "has_web_search": config.inject_google_search,
            "is_image_gen": config.request_type == "image_gen"
        }
    });

    if let Some(img_conf) = config.image_config {
        if let Some(obj) = response.as_object_mut() {
            obj.insert("config".to_string(), img_conf);
        }
    }

    Json(response).into_response()
}

/// [Issue #3414] 从形如 "All accounts limited. Wait 29s." 或其他明确冷却提示中解析等待秒数
pub fn extract_retry_after_seconds(error_text: &str) -> Option<u64> {
    if let Some(pos) = error_text.find("Wait ") {
        let rest = &error_text[pos + 5..];
        if let Some(s_pos) = rest.find('s') {
            if let Ok(sec) = rest[..s_pos].trim().parse::<u64>() {
                if sec > 0 {
                    return Some(sec);
                }
            }
        }
    }
    None
}

/// [Issue #3414] 统一构造带有 X-Mapped-Model、可选 X-Account-Email 以及 Retry-After 的 HeaderMap
pub fn build_token_error_headers<'a>(
    mapped_model: Option<&'a str>,
    account_email: Option<&'a str>,
    error_text: &str,
) -> axum::http::HeaderMap {
    use axum::http::header::{HeaderName, HeaderValue};
    let mut headers = axum::http::HeaderMap::new();

    if let Some(model) = mapped_model {
        if let Ok(val) = HeaderValue::from_str(model) {
            headers.insert(HeaderName::from_static("x-mapped-model"), val);
        }
    }
    if let Some(email) = account_email {
        if let Ok(val) = HeaderValue::from_str(email) {
            headers.insert(HeaderName::from_static("x-account-email"), val);
        }
    }
    if let Some(sec) = extract_retry_after_seconds(error_text) {
        if let Ok(val) = HeaderValue::from_str(&sec.to_string()) {
            headers.insert(axum::http::header::RETRY_AFTER, val);
        }
    }
    headers
}

#[cfg(test)]
mod retry_after_tests {
    use super::*;

    #[test]
    fn test_extract_retry_after_seconds() {
        assert_eq!(
            extract_retry_after_seconds("All accounts limited. Wait 29s."),
            Some(29)
        );
        assert_eq!(
            extract_retry_after_seconds("Token error: All accounts limited. Wait 5s."),
            Some(5)
        );
        assert_eq!(extract_retry_after_seconds("Token pool is empty"), None);
        assert_eq!(
            extract_retry_after_seconds("All accounts failed or unhealthy."),
            None
        );
    }

    #[test]
    fn test_build_token_error_headers() {
        let headers = build_token_error_headers(
            Some("gemini-2.5-pro"),
            Some("test@example.com"),
            "All accounts limited. Wait 45s.",
        );
        assert_eq!(
            headers.get("x-mapped-model").unwrap().to_str().unwrap(),
            "gemini-2.5-pro"
        );
        assert_eq!(
            headers.get("x-account-email").unwrap().to_str().unwrap(),
            "test@example.com"
        );
        assert_eq!(headers.get("retry-after").unwrap().to_str().unwrap(), "45");

        let headers_no_wait = build_token_error_headers(
            Some("gemini-2.5-pro"),
            None,
            "All accounts failed or unhealthy.",
        );
        assert!(headers_no_wait.get("retry-after").is_none());
        assert_eq!(
            headers_no_wait
                .get("x-mapped-model")
                .unwrap()
                .to_str()
                .unwrap(),
            "gemini-2.5-pro"
        );
    }
}
