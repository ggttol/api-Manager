//! Subscription quota is account-local. A cooldown never changes the user's preferred account.
use axum::http::{header, HeaderMap};
use serde_json::Value;
use std::collections::HashSet;

use super::{
    store::{Account, Accounts},
    CodexError,
};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Cooldown {
    pub until: i64,
    pub reason: &'static str,
}

pub(super) enum Usage {
    Unknown,
    Available,
    Exhausted(Option<i64>),
}

pub(super) fn cooling(account: &Account, now: i64) -> Option<i64> {
    account.cooldown_until.filter(|until| *until > now)
}

pub(super) fn select(
    accounts: &Accounts,
    visited: &HashSet<String>,
    now: i64,
) -> Result<String, CodexError> {
    let eligible = |record: &&super::store::Record| {
        record.account.enabled
            && record.verified
            && cooling(&record.account, now).is_none()
            && !visited.contains(&record.account.id)
    };
    let preferred = accounts.active_account_id.as_deref().and_then(|id| {
        accounts
            .accounts
            .iter()
            .filter(eligible)
            .find(|record| record.account.id == id)
    });
    if let Some(record) = preferred.or_else(|| accounts.accounts.iter().find(eligible)) {
        return Ok(record.account.id.clone());
    }
    let mut enabled = false;
    let mut earliest = None;
    for record in &accounts.accounts {
        if record.account.enabled && record.verified {
            enabled = true;
            if let Some(until) = cooling(&record.account, now) {
                earliest = Some(earliest.map_or(until, |current: i64| current.min(until)));
            }
        }
    }
    if enabled {
        Err(CodexError::cooling(earliest.unwrap_or(now.saturating_add(60)),
            "All eligible Codex accounts are temporarily rate-limited or quota-exhausted; retry after the indicated delay"))
    } else {
        Err(CodexError::unavailable(
            "No enabled Codex subscription account; authorize one in the Codex administration page",
        ))
    }
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|value| value.is_finite())
}

fn timestamp(value: &Value, now: i64) -> Option<i64> {
    let parsed = if let Some(value) = number(value) {
        // Accept Unix seconds and the millisecond form without confusing relative durations.
        (if value >= 1_000_000_000_000.0 {
            value / 1000.0
        } else {
            value
        })
        .ceil() as i64
    } else {
        chrono::DateTime::parse_from_rfc3339(value.as_str()?)
            .ok()?
            .timestamp()
    };
    chrono::DateTime::from_timestamp(parsed, 0)?;
    (parsed > now).then_some(parsed)
}

fn reset(value: &Value, now: i64) -> Option<i64> {
    let absolute = ["resets_at", "reset_at", "reset_time", "reset_timestamp"]
        .iter()
        .filter_map(|key| timestamp(value.get(*key)?, now))
        .max();
    let relative = ["resets_in_seconds", "reset_after_seconds"]
        .iter()
        .filter_map(|key| number(value.get(*key)?))
        .filter(|seconds| *seconds > 0.0 && *seconds < i64::MAX as f64)
        .filter_map(|seconds| now.checked_add(seconds.ceil() as i64))
        .max();
    absolute.into_iter().chain(relative).max()
}

pub(super) fn usage(value: &Value, now: i64) -> Usage {
    let rate = value.get("rate_limit").unwrap_or(value);
    let denied = rate.get("allowed").and_then(Value::as_bool) == Some(false)
        || rate.get("limit_reached").and_then(Value::as_bool) == Some(true);
    let mut known = rate.get("allowed").and_then(Value::as_bool).is_some()
        || rate.get("limit_reached").and_then(Value::as_bool).is_some();
    let mut exhausted = false;
    let mut blocked_reset = None;
    let mut next_window = None;
    for key in ["primary_window", "secondary_window"] {
        if let Some(window) = rate.get(key) {
            let percent = window.get("used_percent").and_then(number);
            if percent.is_none() {
                if let Some(until) = reset(window, now) {
                    next_window =
                        Some(next_window.map_or(until, |current: i64| current.min(until)));
                }
            }
            if let Some(percent) = percent {
                known = true;
                if percent >= 100.0 {
                    exhausted = true;
                    if let Some(until) = reset(window, now) {
                        blocked_reset =
                            Some(blocked_reset.map_or(until, |current: i64| current.max(until)));
                    }
                }
            }
        }
    }
    if denied || exhausted {
        Usage::Exhausted(
            blocked_reset
                .or_else(|| reset(rate, now))
                .or(if denied && !exhausted {
                    next_window
                } else {
                    None
                }),
        )
    } else if known {
        Usage::Available
    } else {
        Usage::Unknown
    }
}

pub(super) fn is_quota_error(value: &Value, now: i64) -> bool {
    let error = value
        .get("error")
        .filter(|error| error.is_object())
        .unwrap_or(value);
    ["type", "code"].iter().any(|key| {
        matches!(
            error.get(*key).and_then(Value::as_str),
            Some(
                "usage_limit_reached" | "insufficient_quota" | "quota_exceeded" | "quota_exhausted"
            )
        )
    }) || matches!(usage(value, now), Usage::Exhausted(_))
}

pub(super) fn is_cooldown_error(value: &Value, now: i64) -> bool {
    is_quota_error(value, now)
        || ["type", "code"].iter().any(|key| {
            matches!(
                value
                    .get("error")
                    .filter(|error| error.is_object())
                    .unwrap_or(value)
                    .get(*key)
                    .and_then(Value::as_str),
                Some("rate_limit_exceeded" | "rate_limited")
            )
        })
}

pub(super) fn from_429(headers: &HeaderMap, value: &Value, now: i64) -> Cooldown {
    let error = value
        .get("error")
        .filter(|error| error.is_object())
        .unwrap_or(value);
    let known_quota = is_quota_error(value, now);
    let reached = headers
        .get("x-codex-rate-limit-reached-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let mut quota = known_quota || matches!(reached, "primary" | "secondary" | "both" | "credits");
    let mut until = reset(error, now).into_iter().chain(reset(value, now)).max();
    if let Usage::Exhausted(reset) = usage(value, now) {
        quota = true;
        until = until.into_iter().chain(reset).max();
    }
    if let Some(retry) = headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
    {
        let retry = retry
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .and_then(|seconds| now.checked_add((seconds.ceil() as i64).max(1)))
            .or_else(|| {
                chrono::DateTime::parse_from_rfc2822(retry)
                    .ok()
                    .map(|date| date.timestamp())
                    .filter(|until| *until > now)
            });
        until = until.into_iter().chain(retry).max();
    }
    for (name, value) in headers {
        if let Some(prefix) = name.as_str().strip_suffix("-used-percent") {
            if value
                .to_str()
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .is_some_and(|v| v >= 100.0)
            {
                quota = true;
                if let Some(value) = headers
                    .get(format!("{prefix}-reset-at").as_str())
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| timestamp(&Value::String(v.into()), now))
                {
                    until = Some(until.map_or(value, |current| current.max(value)));
                }
            }
        }
        if (name.as_str().ends_with("-primary-reset-at") && matches!(reached, "primary" | "both"))
            || (name.as_str().ends_with("-secondary-reset-at")
                && matches!(reached, "secondary" | "both"))
        {
            if let Some(reset) = value
                .to_str()
                .ok()
                .and_then(|value| timestamp(&Value::String(value.into()), now))
            {
                until = until.into_iter().chain(Some(reset)).max();
            }
        }
    }
    Cooldown {
        until: until.unwrap_or(now.saturating_add(if quota { 300 } else { 60 })),
        reason: if quota {
            "quota_exhausted"
        } else {
            "rate_limited"
        },
    }
}

pub(super) fn apply(account: &mut Account, cooldown: &Cooldown, now: i64) {
    if cooling(account, now).is_none_or(|until| until <= cooldown.until) {
        account.cooldown_until = Some(cooldown.until);
        account.cooldown_reason = Some(cooldown.reason.into());
    }
    account.last_error =
        Some("Codex account is temporarily quota-exhausted or rate-limited".into());
}

pub(super) fn apply_usage(account: &mut Account, observation: Usage, now: i64, may_recover: bool) {
    match observation {
        Usage::Exhausted(until) => apply(
            account,
            &Cooldown {
                until: until.unwrap_or(now.saturating_add(300)),
                reason: "quota_exhausted",
            },
            now,
        ),
        Usage::Available
            if may_recover && account.cooldown_reason.as_deref() == Some("quota_exhausted") =>
        {
            account.cooldown_until = None;
            account.cooldown_reason = None;
            account.last_error = None;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reset_windows_use_exhausted_limits_not_unrelated_weekly_window() {
        let now = 1_800_000_000;
        let value = json!({"rate_limit":{"allowed":false,"primary_window":{"used_percent":100,"reset_at":now+120},"secondary_window":{"used_percent":20,"reset_at":now+604800}}});
        let Usage::Exhausted(until) = usage(&value, now) else {
            panic!("expected exhausted usage")
        };
        assert_eq!(until, Some(now + 120));
        let missing_reset = json!({"rate_limit":{"allowed":false,
            "primary_window":{"used_percent":100},
            "secondary_window":{"used_percent":20,"reset_at":now+604800}}});
        assert!(matches!(usage(&missing_reset, now), Usage::Exhausted(None)));
        assert_eq!(
            from_429(&HeaderMap::new(), &missing_reset, now).until,
            now + 300
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, "180".parse().unwrap());
        let cooldown = from_429(
            &headers,
            &json!({"error":{"type":"usage_limit_reached","resets_at":now+120}}),
            now,
        );
        assert_eq!(
            cooldown,
            Cooldown {
                until: now + 180,
                reason: "quota_exhausted"
            }
        );
        assert_eq!(
            from_429(&headers, &json!({"rate_limit":{"allowed":false}}), now).until,
            now + 180
        );
        headers.insert(
            header::RETRY_AFTER,
            chrono::DateTime::from_timestamp(now + 240, 0)
                .unwrap()
                .to_rfc2822()
                .parse()
                .unwrap(),
        );
        assert_eq!(from_429(&headers, &json!({}), now).until, now + 240);
        assert_eq!(
            from_429(&HeaderMap::new(), &json!({}), now),
            Cooldown {
                until: now + 60,
                reason: "rate_limited"
            }
        );
        assert!(matches!(
            usage(&json!({"code_review_rate_limit":{"allowed":false}}), now),
            Usage::Unknown
        ));
        let transient = json!({"error":{"code":"rate_limit_exceeded"}});
        assert!(is_cooldown_error(&transient, now));
        assert!(!is_quota_error(&transient, now));
        assert_eq!(
            from_429(&headers, &transient, now),
            Cooldown {
                until: now + 240,
                reason: "rate_limited"
            }
        );
        let mut account = Account {
            id: "account".into(),
            email: None,
            label: "account".into(),
            plan_type: None,
            enabled: true,
            expires_at: None,
            last_used_at: None,
            last_error: None,
            cooldown_until: Some(now + 240),
            cooldown_reason: Some("rate_limited".into()),
        };
        assert_eq!(account.cooldown_until, Some(now + 240));
        assert_eq!(account.cooldown_reason.as_deref(), Some("rate_limited"));
    }
}
