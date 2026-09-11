use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

// Node.js proxy uses 2 hours TTL
const SIGNATURE_TTL: Duration = Duration::from_secs(2 * 60 * 60);
const MIN_SIGNATURE_LENGTH: usize = 50;

// Different cache limits for different layers
const TOOL_CACHE_LIMIT: usize = 500; // Layer 1: Tool-specific signatures
const FAMILY_CACHE_LIMIT: usize = 200; // Layer 2: Model family mappings
const SESSION_CACHE_LIMIT: usize = 1000;
const SESSION_SIGNATURE_HISTORY_LIMIT: usize = 64;

/// Cache entry with timestamp for TTL
#[derive(Clone, Debug)]
struct CacheEntry<T> {
    data: T,
    timestamp: SystemTime,
}

/// Bounded signature history for one session.
///
/// Claude uses conversation message counts as keys. Gemini response payloads do not
/// expose those counts, so their stable response IDs receive a private generated key
/// that lets chunks of the same response refine one entry without conflating later
/// responses.
#[derive(Clone, Debug, Default)]
struct SessionSignatureHistory {
    signatures: HashMap<usize, SessionSignatureEntry>,
    gemini_response_counts: HashMap<String, usize>,
    next_generated_count: usize,
}

/// A retained signature and its logical conversation position.
#[derive(Clone, Debug)]
struct SessionSignatureEntry {
    signature: String,
    message_count: usize,
}

impl<T> CacheEntry<T> {
    fn new(data: T) -> Self {
        Self {
            data,
            timestamp: SystemTime::now(),
        }
    }

    fn is_expired(&self) -> bool {
        self.timestamp.elapsed().unwrap_or(Duration::ZERO) > SIGNATURE_TTL
    }
}

fn enforce_capacity<T>(cache: &mut HashMap<String, CacheEntry<T>>, limit: usize) {
    cache.retain(|_, entry| !entry.is_expired());
    while cache.len() > limit {
        let oldest_key = cache
            .iter()
            .min_by_key(|(_, entry)| entry.timestamp)
            .map(|(key, _)| key.clone());
        if let Some(key) = oldest_key {
            cache.remove(&key);
        } else {
            break;
        }
    }
}

/// Triple-layer signature cache to handle:
/// 1. Signature recovery for tool calls (when clients strip them)
/// 2. Cross-model compatibility checks (preventing Claude signatures on Gemini models)
/// 3. Session-based signature tracking (preventing cross-session pollution)
pub struct SignatureCache {
    /// Layer 1: Tool Use ID -> Thinking Signature
    /// Key: tool_use_id (e.g., "toolu_01...")
    /// Value: The thought signature that generated this tool call
    tool_signatures: Mutex<HashMap<String, CacheEntry<String>>>,

    /// Layer 2: Signature -> Model Family
    /// Key: thought signature string
    /// Value: Model family identifier (e.g., "claude-3-5-sonnet", "gemini-2.0-flash")
    thinking_families: Mutex<HashMap<String, CacheEntry<String>>>,

    /// Layer 3: Session ID -> bounded history of thinking signatures.
    /// This prevents signature pollution between different conversations while
    /// preventing an active session from retaining unbounded history.
    session_signatures: Mutex<HashMap<String, CacheEntry<SessionSignatureHistory>>>,

    /// Layer 4: Session ID -> Assistant Reasoning Text History (NEW v4.2.0)
    /// Key: session fingerprint
    /// Value: A vector of reasoning contents (index corresponds to assistant turn index)
    session_reasonings: Mutex<HashMap<String, CacheEntry<Vec<String>>>>,
}

impl SignatureCache {
    fn new() -> Self {
        Self {
            tool_signatures: Mutex::new(HashMap::new()),
            thinking_families: Mutex::new(HashMap::new()),
            session_signatures: Mutex::new(HashMap::new()),
            session_reasonings: Mutex::new(HashMap::new()),
        }
    }

    /// Global singleton instance
    pub fn global() -> &'static SignatureCache {
        static INSTANCE: OnceLock<SignatureCache> = OnceLock::new();
        INSTANCE.get_or_init(SignatureCache::new)
    }

    /// Store a tool call signature
    pub fn cache_tool_signature(&self, tool_use_id: &str, signature: String) {
        if signature.len() < MIN_SIGNATURE_LENGTH {
            return;
        }

        if let Ok(mut cache) = self.tool_signatures.lock() {
            tracing::debug!(
                "[SignatureCache] Caching tool signature for id: {}",
                tool_use_id
            );
            cache.insert(tool_use_id.to_string(), CacheEntry::new(signature));

            if cache.len() > TOOL_CACHE_LIMIT {
                enforce_capacity(&mut cache, TOOL_CACHE_LIMIT);
            }
        }
    }

    /// Retrieve a signature for a tool_use_id
    pub fn get_tool_signature(&self, tool_use_id: &str) -> Option<String> {
        if let Ok(cache) = self.tool_signatures.lock() {
            if let Some(entry) = cache.get(tool_use_id) {
                if !entry.is_expired() {
                    tracing::debug!(
                        "[SignatureCache] Hit tool signature for id: {}",
                        tool_use_id
                    );
                    return Some(entry.data.clone());
                }
            }
        }
        None
    }

    pub fn delete_tool_signature(&self, tool_use_id: &str) {
        if let Ok(mut cache) = self.tool_signatures.lock() {
            cache.remove(tool_use_id);
        }
    }

    /// Store model family for a signature
    pub fn cache_thinking_family(&self, signature: String, family: String) {
        if signature.len() < MIN_SIGNATURE_LENGTH {
            return;
        }

        if let Ok(mut cache) = self.thinking_families.lock() {
            tracing::debug!(
                "[SignatureCache] Caching thinking family for sig (len={}): {}",
                signature.len(),
                family
            );
            cache.insert(signature, CacheEntry::new(family));

            if cache.len() > FAMILY_CACHE_LIMIT {
                enforce_capacity(&mut cache, FAMILY_CACHE_LIMIT);
            }
        }
    }

    /// Get model family for a signature
    pub fn get_signature_family(&self, signature: &str) -> Option<String> {
        if let Ok(cache) = self.thinking_families.lock() {
            if let Some(entry) = cache.get(signature) {
                if !entry.is_expired() {
                    return Some(entry.data.clone());
                } else {
                    tracing::debug!("[SignatureCache] Signature family entry expired");
                }
            }
        }
        None
    }

    // ===== Layer 3: Session-based Signature Storage =====

    /// Store the thinking signature for a session at a specific message count.
    /// Completions may arrive out of order, so this preserves newer counts instead
    /// of interpreting an older completion as a conversation rewind.
    ///
    /// # Arguments
    /// * `session_id` - Session fingerprint (e.g., "sid-a1b2c3d4...")
    /// * `signature` - The thought signature to store
    /// * `message_count` - The logical conversation position of the signature
    pub fn cache_session_signature(
        &self,
        session_id: &str,
        signature: String,
        message_count: usize,
    ) {
        self.cache_session_signature_at(session_id, signature, message_count);
    }

    /// Store or refine a Gemini signature. `response_id` must identify one upstream
    /// response; repeated stream chunks for that response may replace only a shorter
    /// partial signature, while another response always receives a distinct slot.
    pub fn cache_gemini_session_signature(
        &self,
        session_id: &str,
        signature: String,
        response_id: &str,
    ) {
        if signature.len() < MIN_SIGNATURE_LENGTH || response_id.is_empty() {
            return;
        }

        if let Ok(mut cache) = self.session_signatures.lock() {
            let entry = cache
                .entry(session_id.to_string())
                .or_insert_with(|| CacheEntry::new(SessionSignatureHistory::default()));
            entry.timestamp = SystemTime::now();

            let history = &mut entry.data;
            let message_count = if let Some(count) = history.gemini_response_counts.get(response_id)
            {
                *count
            } else {
                history.next_generated_count = history
                    .next_generated_count
                    .max(history.signatures.keys().max().copied().unwrap_or_default())
                    .saturating_add(1);
                let count = history.next_generated_count;
                history
                    .gemini_response_counts
                    .insert(response_id.to_owned(), count);
                count
            };
            Self::store_session_signature(history, signature, message_count);
            Self::enforce_session_history_limit(history);

            if cache.len() > SESSION_CACHE_LIMIT {
                enforce_capacity(&mut cache, SESSION_CACHE_LIMIT);
            }
        }
    }

    fn cache_session_signature_at(
        &self,
        session_id: &str,
        signature: String,
        message_count: usize,
    ) {
        if signature.len() < MIN_SIGNATURE_LENGTH {
            return;
        }

        if let Ok(mut cache) = self.session_signatures.lock() {
            let entry = cache
                .entry(session_id.to_string())
                .or_insert_with(|| CacheEntry::new(SessionSignatureHistory::default()));
            entry.timestamp = SystemTime::now();
            Self::store_session_signature(&mut entry.data, signature, message_count);
            Self::enforce_session_history_limit(&mut entry.data);

            if cache.len() > SESSION_CACHE_LIMIT {
                enforce_capacity(&mut cache, SESSION_CACHE_LIMIT);
            }
        }
    }

    fn store_session_signature(
        history: &mut SessionSignatureHistory,
        signature: String,
        message_count: usize,
    ) {
        let should_store = match history.signatures.get(&message_count) {
            None => true,
            Some(existing) => signature.len() > existing.signature.len(),
        };

        if should_store {
            history.signatures.insert(
                message_count,
                SessionSignatureEntry {
                    signature,
                    message_count,
                },
            );
        }
    }

    fn enforce_session_history_limit(history: &mut SessionSignatureHistory) {
        while history.signatures.len() > SESSION_SIGNATURE_HISTORY_LIMIT {
            let Some(oldest_count) = history.signatures.keys().min().copied() else {
                break;
            };
            history.signatures.remove(&oldest_count);
            history
                .gemini_response_counts
                .retain(|_, count| *count != oldest_count);
        }
    }

    /// Retrieve the latest thinking signature for a session.
    /// Returns None if not found or expired.
    pub fn get_session_signature(&self, session_id: &str) -> Option<String> {
        if let Ok(cache) = self.session_signatures.lock() {
            if let Some(entry) = cache.get(session_id) {
                if !entry.is_expired() {
                    // Find the signature with the maximum message_count (the latest one)
                    if let Some(sig_entry) = entry
                        .data
                        .signatures
                        .values()
                        .max_by_key(|e| e.message_count)
                    {
                        tracing::debug!(
                            "[SignatureCache] Session {} (latest, msg_count={}) -> HIT (len={})",
                            session_id,
                            sig_entry.message_count,
                            sig_entry.signature.len()
                        );
                        return Some(sig_entry.signature.clone());
                    }
                } else {
                    tracing::debug!("[SignatureCache] Session {} -> EXPIRED", session_id);
                }
            }
        }
        None
    }

    /// Retrieve the thinking signature for a session at a specific message count.
    /// Returns None if not found or expired.
    pub fn get_session_signature_at(
        &self,
        session_id: &str,
        message_count: usize,
    ) -> Option<String> {
        if let Ok(cache) = self.session_signatures.lock() {
            if let Some(entry) = cache.get(session_id) {
                if !entry.is_expired() {
                    if let Some(sig_entry) = entry.data.signatures.get(&message_count) {
                        tracing::debug!(
                            "[SignatureCache] Session {} (msg_count={}) -> HIT (len={})",
                            session_id,
                            message_count,
                            sig_entry.signature.len()
                        );
                        return Some(sig_entry.signature.clone());
                    }
                }
            }
        }
        None
    }

    /// Store reasoning text for a specific assistant turn in a session
    pub fn cache_session_reasoning(&self, session_id: &str, reasoning: String, turn_index: usize) {
        if reasoning.trim().is_empty() {
            return;
        }

        if let Ok(mut cache) = self.session_reasonings.lock() {
            let entry = cache
                .entry(session_id.to_string())
                .or_insert_with(|| CacheEntry::new(Vec::new()));

            // Update timestamp to refresh TTL
            entry.timestamp = std::time::SystemTime::now();

            if turn_index >= entry.data.len() {
                entry.data.resize(turn_index + 1, String::new());
            }

            // Only update if the new reasoning is longer to prevent overwriting with partial content
            let old_len = entry.data[turn_index].len();
            if reasoning.len() > old_len {
                tracing::debug!(
                    "[SignatureCache] Session {} (turn={}) -> caching reasoning text (len: {} -> {})",
                    session_id,
                    turn_index,
                    old_len,
                    reasoning.len()
                );
                entry.data[turn_index] = reasoning;
            }

            if cache.len() > SESSION_CACHE_LIMIT {
                enforce_capacity(&mut cache, SESSION_CACHE_LIMIT);
            }
        }
    }

    /// Retrieve reasoning text for a specific assistant turn in a session
    pub fn get_session_reasoning(&self, session_id: &str, turn_index: usize) -> Option<String> {
        if let Ok(cache) = self.session_reasonings.lock() {
            if let Some(entry) = cache.get(session_id) {
                if !entry.is_expired() && turn_index < entry.data.len() {
                    let text = &entry.data[turn_index];
                    if !text.trim().is_empty() {
                        tracing::debug!(
                            "[SignatureCache] Session {} (turn={}) -> Hit reasoning text cache (len: {})",
                            session_id,
                            turn_index,
                            text.len()
                        );
                        return Some(text.clone());
                    }
                }
            }
        }
        None
    }

    /// 删除指定会话的缓存签名
    #[allow(dead_code)] // 预留给管理接口或调试使用
    pub fn delete_session_signature(&self, session_id: &str) {
        if let Ok(mut cache) = self.session_signatures.lock() {
            if cache.remove(session_id).is_some() {
                tracing::debug!(
                    "[SignatureCache] Deleted session signature for: {}",
                    session_id
                );
            }
        }
    }

    /// Clear all caches (for testing or manual reset)
    #[allow(dead_code)] // Used in tests
    pub fn clear(&self) {
        if let Ok(mut cache) = self.tool_signatures.lock() {
            cache.clear();
        }
        if let Ok(mut cache) = self.thinking_families.lock() {
            cache.clear();
        }
        if let Ok(mut cache) = self.session_signatures.lock() {
            cache.clear();
        }
        if let Ok(mut cache) = self.session_reasonings.lock() {
            cache.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_signature_cache() {
        let cache = SignatureCache::new();
        let sig = "x".repeat(60); // Valid length

        cache.cache_tool_signature("tool_1", sig.clone());
        assert_eq!(cache.get_tool_signature("tool_1"), Some(sig));
        assert_eq!(cache.get_tool_signature("tool_2"), None);
    }

    #[test]
    fn test_min_length() {
        let cache = SignatureCache::new();
        cache.cache_tool_signature("tool_short", "short".to_string());
        assert_eq!(cache.get_tool_signature("tool_short"), None);
    }

    #[test]
    fn test_thinking_family() {
        let cache = SignatureCache::new();
        let sig = "y".repeat(60);

        cache.cache_thinking_family(sig.clone(), "claude".to_string());
        assert_eq!(cache.get_signature_family(&sig), Some("claude".to_string()));
    }

    #[test]
    fn test_active_signature_caches_enforce_capacity() {
        let cache = SignatureCache::new();
        for i in 0..=TOOL_CACHE_LIMIT {
            cache.cache_tool_signature(&format!("tool-{i}"), "x".repeat(MIN_SIGNATURE_LENGTH));
        }
        assert_eq!(
            cache
                .tool_signatures
                .lock()
                .ok()
                .map(|entries| entries.len()),
            Some(TOOL_CACHE_LIMIT)
        );

        for i in 0..=FAMILY_CACHE_LIMIT {
            cache.cache_thinking_family(format!("{i:0>50}"), format!("family-{i}"));
        }
        assert_eq!(
            cache
                .thinking_families
                .lock()
                .ok()
                .map(|entries| entries.len()),
            Some(FAMILY_CACHE_LIMIT)
        );

        for i in 0..=SESSION_CACHE_LIMIT {
            cache.cache_session_signature(
                &format!("session-{i}"),
                "y".repeat(MIN_SIGNATURE_LENGTH),
                1,
            );
        }
        assert_eq!(
            cache
                .session_signatures
                .lock()
                .ok()
                .map(|entries| entries.len()),
            Some(SESSION_CACHE_LIMIT)
        );
    }

    #[test]
    fn test_session_signature() {
        let cache = SignatureCache::new();
        let sig1 = "a".repeat(60);
        let sig2 = "b".repeat(80); // Longer, should replace
        let sig3 = "c".repeat(40); // Too short, should be ignored

        // Initially empty
        assert!(cache.get_session_signature("sid-test123").is_none());

        // Store first signature
        cache.cache_session_signature("sid-test123", sig1.clone(), 5);
        assert_eq!(
            cache.get_session_signature("sid-test123"),
            Some(sig1.clone())
        );

        // Longer signature should replace (same msg count)
        cache.cache_session_signature("sid-test123", sig2.clone(), 5);
        assert_eq!(
            cache.get_session_signature("sid-test123"),
            Some(sig2.clone())
        );

        // Shorter valid signature should NOT replace (same msg count)
        cache.cache_session_signature("sid-test123", sig1.clone(), 5);
        assert_eq!(
            cache.get_session_signature("sid-test123"),
            Some(sig2.clone())
        );

        // A delayed completion for an earlier turn must not erase newer history.
        cache.cache_session_signature("sid-test123", sig1.clone(), 3);
        assert_eq!(
            cache.get_session_signature("sid-test123"),
            Some(sig2.clone())
        );
        assert_eq!(
            cache.get_session_signature_at("sid-test123", 3),
            Some(sig1.clone())
        );

        // Too short signatures are ignored.
        cache.cache_session_signature("sid-test123", sig3, 1);
        assert_eq!(cache.get_session_signature("sid-test123"), Some(sig2));

        // Different session should be isolated
        assert!(cache.get_session_signature("sid-other").is_none());
    }

    #[test]
    fn test_session_signature_history_is_bounded() {
        let cache = SignatureCache::new();
        for count in 0..=SESSION_SIGNATURE_HISTORY_LIMIT {
            cache.cache_session_signature("active", format!("{count:0>50}"), count);
        }

        let history = cache
            .session_signatures
            .lock()
            .ok()
            .expect("session signature cache lock must be available");
        let history = &history["active"].data;
        assert_eq!(history.signatures.len(), SESSION_SIGNATURE_HISTORY_LIMIT);
        assert!(history
            .signatures
            .contains_key(&SESSION_SIGNATURE_HISTORY_LIMIT));
        assert!(!history.signatures.contains_key(&0));
    }

    #[test]
    fn test_gemini_response_identity_only_merges_partial_updates() {
        let cache = SignatureCache::new();
        let first = "a".repeat(MIN_SIGNATURE_LENGTH);
        let first_complete = "a".repeat(MIN_SIGNATURE_LENGTH + 1);
        let second = "b".repeat(MIN_SIGNATURE_LENGTH);

        cache.cache_gemini_session_signature("gemini", first, "response-1");
        cache.cache_gemini_session_signature("gemini", first_complete.clone(), "response-1");
        cache.cache_gemini_session_signature("gemini", second.clone(), "response-2");

        assert_eq!(cache.get_session_signature("gemini"), Some(second));
        let history = cache
            .session_signatures
            .lock()
            .ok()
            .expect("session signature cache lock must be available");
        assert_eq!(history["gemini"].data.signatures.len(), 2);
        assert!(history["gemini"]
            .data
            .signatures
            .values()
            .any(|entry| entry.signature == first_complete));
    }

    #[test]
    fn test_clear_all_caches() {
        let cache = SignatureCache::new();
        let sig = "x".repeat(60);

        cache.cache_tool_signature("tool_1", sig.clone());
        cache.cache_thinking_family(sig.clone(), "model".to_string());
        cache.cache_session_signature("sid-1", sig.clone(), 1);

        assert!(cache.get_tool_signature("tool_1").is_some());
        assert!(cache.get_signature_family(&sig).is_some());
        assert!(cache.get_session_signature("sid-1").is_some());

        cache.clear();

        assert!(cache.get_tool_signature("tool_1").is_none());
        assert!(cache.get_signature_family(&sig).is_none());
        assert!(cache.get_session_signature("sid-1").is_none());
    }
}
