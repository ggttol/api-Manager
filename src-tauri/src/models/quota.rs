use serde::{Deserialize, Serialize};

/// 单个配额桶 (对应 retrieveUserQuotaSummary 里的一个 bucket)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaBucket {
    /// 桶 ID,如 "gemini-weekly" / "gemini-5h" / "3p-weekly" / "3p-5h"
    pub bucket_id: String,
    /// 窗口类型: "weekly" / "5h"
    pub window: String,
    /// 剩余比例 0.0-1.0
    pub remaining_fraction: f64,
    /// 重置时间 (RFC3339)
    pub reset_time: String,
    /// Successful bucket observation time in milliseconds; absent in older snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<i64>,
    /// First observed early reset, in seconds; normal cycles start seven days before reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_start: Option<i64>,
    /// Usage recorded by this instance, populated only when returning the account list.
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub cycle_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl QuotaBucket {
    pub(crate) fn weekly_cycle_bounds(&self, now: i64) -> Option<(i64, i64)> {
        let window = format!("{} {}", self.window, self.bucket_id).to_lowercase();
        if !(window.contains("week") || window.contains("7d"))
            || !(0.0..=1.0).contains(&self.remaining_fraction)
        {
            return None;
        }
        let end = chrono::DateTime::parse_from_rfc3339(&self.reset_time)
            .ok()?
            .timestamp();
        let normal_start = end.checked_sub(7 * 24 * 60 * 60)?;
        let start = self.cycle_start.unwrap_or(normal_start);
        (normal_start <= start && start <= now && now < end).then_some((start, end))
    }

    pub(crate) fn retain_cycle_boundary(&mut self, previous: &Self, observed_at: i64) {
        if self.reset_time == previous.reset_time {
            self.cycle_start = previous.cycle_start;
        }
        let observed_secs = observed_at.div_euclid(1000);
        if self.weekly_cycle_bounds(observed_secs).is_some()
            && previous.weekly_cycle_bounds(observed_secs).is_some()
            && self.remaining_fraction > previous.remaining_fraction + 1e-9
        {
            self.cycle_start = Some(observed_secs);
        }
    }
}

/// 一个模型组 (如 Gemini Models / Claude and GPT models)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaGroup {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub buckets: Vec<QuotaBucket>,
}

/// 模型配额信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelQuota {
    pub name: String,
    pub percentage: i32,
    pub reset_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_images: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_mime_types: Option<std::collections::HashMap<String, bool>>,
}

/// 配额数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaData {
    pub models: Vec<ModelQuota>,
    pub last_updated: i64,
    #[serde(default)]
    pub is_forbidden: bool,
    #[serde(default)]
    pub forbidden_reason: Option<String>,
    #[serde(default)]
    pub subscription_tier: Option<String>,
    #[serde(default)]
    pub model_forwarding_rules: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub quota_groups: Option<Vec<QuotaGroup>>,
}

impl QuotaData {
    pub fn new() -> Self {
        Self {
            models: Vec::new(),
            last_updated: chrono::Utc::now().timestamp(),
            is_forbidden: false,
            forbidden_reason: None,
            subscription_tier: None,
            model_forwarding_rules: std::collections::HashMap::new(),
            quota_groups: None,
        }
    }

    pub fn add_model(&mut self, model: ModelQuota) {
        self.models.push(model);
    }

    pub fn ensure_subscription_tier(&mut self) {
        self.subscription_tier = self.subscription_tier.as_deref().and_then(|raw| {
            let normalized = normalize_subscription_tier(raw);
            is_known_tier(&normalized).then_some(normalized)
        });
    }
}

pub fn is_known_tier(tier: &str) -> bool {
    matches!(tier, "ULTRA" | "PRO" | "FREE")
}

/// Normalize official tier ids/names. Unknown values remain unchanged so callers can
/// distinguish an authoritative unknown response from a network failure.
pub fn normalize_subscription_tier(tier: &str) -> String {
    let lower = tier.trim().to_lowercase();
    if lower.is_empty() {
        return String::new();
    }
    if lower.contains("ultra") || lower.contains("helium") {
        return "ULTRA".to_string();
    }
    if lower.contains("free") || lower.contains("starter") {
        return "FREE".to_string();
    }
    if lower.contains("pro") || lower.contains("premium") || lower.contains("advanced") {
        return "PRO".to_string();
    }
    tier.trim().to_string()
}

/// Unknown authoritative tiers are safely treated as FREE. This function never inspects
/// model names: the available-model catalog is shared across subscription tiers.
pub fn resolve_subscription_tier(raw_tier: Option<&str>) -> String {
    raw_tier
        .map(normalize_subscription_tier)
        .filter(|tier| is_known_tier(tier))
        .unwrap_or_else(|| "FREE".to_string())
}

/// Lower value wins when selecting a token (ULTRA, PRO, FREE).
pub fn tier_priority(tier: Option<&str>) -> u8 {
    match normalize_subscription_tier(tier.unwrap_or("")).as_str() {
        "ULTRA" => 0,
        "PRO" => 1,
        _ => 2,
    }
}

impl Default for QuotaData {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_mapping_is_canonical_and_unknown_is_free() {
        assert_eq!(normalize_subscription_tier("g1-pro-tier"), "PRO");
        assert_eq!(normalize_subscription_tier("g1-ultra-tier"), "ULTRA");
        assert_eq!(normalize_subscription_tier("free-tier"), "FREE");
        assert_eq!(
            normalize_subscription_tier("standard-tier"),
            "standard-tier"
        );
        assert_eq!(resolve_subscription_tier(Some("standard-tier")), "FREE");
        assert_eq!(resolve_subscription_tier(None), "FREE");
    }

    #[test]
    fn quota_group_deserializes_without_buckets() {
        let group: QuotaGroup =
            serde_json::from_str(r#"{"display_name":"Gemini Models"}"#).unwrap();
        assert!(group.buckets.is_empty());
    }
}
