// HTTP 会话历史存储
// 为 /v1/responses POST 提供 previous_response_id 链式历史支持
// 这样即使客户端用 HTTP 而不是 WebSocket，也能实现多轮对话

use crate::proxy::handlers::openai::get_cached_tool_call;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const SESSION_TTL_SECS: u64 = 3600; // 1小时过期

#[derive(Debug, Clone)]
pub struct HttpSessionEntry {
    /// 对话历史：instructions + 所有 input items（包括历史response输出）
    pub input_items: Vec<Value>,
    /// 系统指令
    pub instructions: String,
    /// 模型名
    pub model: String,
    /// 上次访问时间（用于TTL淘汰）
    pub last_accessed: Instant,
}

#[derive(Debug)]
struct SessionNode {
    parent: Option<Arc<SessionNode>>,
    input_delta: Vec<Value>,
    response_output: Vec<Value>,
    instructions: String,
    model: String,
    routing_session_id: String,
}

#[derive(Debug, Clone)]
pub struct SessionParent(Arc<SessionNode>);

impl SessionParent {
    pub fn routing_session_id(&self) -> &str {
        &self.0.routing_session_id
    }
}

struct StoredSession {
    node: Arc<SessionNode>,
    last_accessed: Instant,
}

struct HttpSessionStore {
    sessions: HashMap<String, StoredSession>,
}

impl HttpSessionStore {
    fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    fn get(&mut self, response_id: &str) -> Option<(HttpSessionEntry, SessionParent)> {
        let expired = self.sessions.get(response_id).is_some_and(|stored| {
            stored.last_accessed.elapsed() >= Duration::from_secs(SESSION_TTL_SECS)
        });
        if expired {
            self.sessions.remove(response_id);
            return None;
        }
        let stored = self.sessions.get_mut(response_id)?;
        stored.last_accessed = Instant::now();
        let node = stored.node.clone();
        Some((
            HttpSessionEntry {
                input_items: materialize_history(&node),
                instructions: node.instructions.clone(),
                model: node.model.clone(),
                last_accessed: stored.last_accessed,
            },
            SessionParent(node),
        ))
    }

    fn insert(&mut self, response_id: String, entry: HttpSessionEntry) {
        self.insert_delta(
            response_id,
            None,
            entry.input_items,
            Vec::new(),
            entry.instructions,
            entry.model,
            None,
        );
    }

    fn insert_delta(
        &mut self,
        response_id: String,
        parent: Option<SessionParent>,
        input_delta: Vec<Value>,
        response_output: Vec<Value>,
        instructions: String,
        model: String,
        routing_session_id: Option<String>,
    ) {
        let routing_session_id = routing_session_id.unwrap_or_else(|| response_id.clone());
        self.sessions.insert(
            response_id,
            StoredSession {
                node: Arc::new(SessionNode {
                    parent: parent.map(|parent| parent.0),
                    input_delta,
                    response_output,
                    instructions,
                    model,
                    routing_session_id,
                }),
                last_accessed: Instant::now(),
            },
        );
        // 顺便淘汰过期 session（惰性清理）
        self.evict_expired();
    }

    fn evict_expired(&mut self) {
        let ttl = Duration::from_secs(SESSION_TTL_SECS);
        self.sessions
            .retain(|_, stored| stored.last_accessed.elapsed() < ttl);
    }
}

fn materialize_history(node: &Arc<SessionNode>) -> Vec<Value> {
    let mut chain = Vec::new();
    let mut current = Some(node.clone());
    while let Some(node) = current {
        chain.push(node.clone());
        current = node.parent.clone();
    }

    let capacity = chain
        .iter()
        .map(|node| node.input_delta.len() + node.response_output.len())
        .sum();
    let mut history = Vec::with_capacity(capacity);
    for node in chain.into_iter().rev() {
        history.extend(node.input_delta.iter().cloned());
        history.extend(node.response_output.iter().cloned());
    }
    history
}

static STORE: OnceLock<Mutex<HttpSessionStore>> = OnceLock::new();

fn store() -> &'static Mutex<HttpSessionStore> {
    STORE.get_or_init(|| Mutex::new(HttpSessionStore::new()))
}

/// 根据 previous_response_id 查找历史会话
pub async fn get_session(previous_response_id: &str) -> Option<HttpSessionEntry> {
    store()
        .lock()
        .await
        .get(previous_response_id)
        .map(|(entry, _)| entry)
}

pub async fn get_session_with_parent(
    previous_response_id: &str,
) -> Option<(HttpSessionEntry, SessionParent)> {
    store().lock().await.get(previous_response_id)
}

/// 保存新的会话状态（以 response_id 为 key）
pub async fn save_session(response_id: String, entry: HttpSessionEntry) {
    store().lock().await.insert(response_id, entry);
}

/// 保存 Responses 本轮增量；父节点的 Arc 强引用保证分支共享祖先。
pub async fn save_session_delta(
    response_id: String,
    parent: Option<SessionParent>,
    input_delta: Vec<Value>,
    response_output: Vec<Value>,
    instructions: String,
    model: String,
    routing_session_id: String,
) {
    store().lock().await.insert_delta(
        response_id,
        parent,
        input_delta,
        response_output,
        instructions,
        model,
        Some(routing_session_id),
    );
}

pub struct PreparedSessionInput {
    pub merged: Vec<Value>,
    pub delta: Vec<Value>,
    pub reset_parent: bool,
}

fn items_semantically_equal(a: &Value, b: &Value) -> bool {
    let mut a = a.clone();
    let mut b = b.clone();
    if let Some(object) = a.as_object_mut() {
        object.remove("id");
    }
    if let Some(object) = b.as_object_mut() {
        object.remove("id");
    }
    a == b
}
/// 合并请求历史，并在客户端回放完整历史时仅提取新增项。
pub fn prepare_session_input(
    history: Vec<Value>,
    new_input: Vec<Value>,
    tool_call_cache: &HashMap<String, Value>,
) -> PreparedSessionInput {
    prepare_session_input_with_storage(history, new_input, tool_call_cache, true)
}

/// Restore the complete model input while optionally retaining a delta for storage.
pub fn prepare_session_input_with_storage(
    history: Vec<Value>,
    new_input: Vec<Value>,
    tool_call_cache: &HashMap<String, Value>,
    retain_delta: bool,
) -> PreparedSessionInput {
    let reset_parent = new_input.iter().any(|item| {
        matches!(
            item.get("type").and_then(Value::as_str),
            Some("compaction") | Some("compaction_summary")
        )
    });
    let exact_replay = !history.is_empty() && new_input.starts_with(&history);
    let replayed_through = if reset_parent || exact_replay {
        None
    } else {
        new_input.iter().rposition(|item| {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                return false;
            };
            history.iter().any(|history_item| {
                history_item.get("id").and_then(Value::as_str) == Some(id)
                    && items_semantically_equal(history_item, item)
            })
        })
    };

    // IDs are generated afresh when clients replay a Responses history, but tool
    // payloads (including call_id, name, arguments, and output) define its meaning.

    // Semantic prefix match: check if new_input starts with history semantically
    let semantic_prefix_match = !history.is_empty()
        && new_input.len() >= history.len()
        && history
            .iter()
            .zip(new_input.iter())
            .all(|(h, n)| items_semantically_equal(h, n));

    // Semantic suffix find: find last item of history in new_input
    let semantic_suffix_idx = if !history.is_empty()
        && !reset_parent
        && !exact_replay
        && replayed_through.is_none()
        && !semantic_prefix_match
    {
        let last_h = &history[history.len() - 1];
        new_input
            .iter()
            .rposition(|n| items_semantically_equal(last_h, n))
    } else {
        None
    };

    let (delta_source, replace_parent) = if reset_parent || history.is_empty() {
        (new_input.clone(), reset_parent)
    } else if exact_replay {
        (new_input[history.len()..].to_vec(), false)
    } else if semantic_prefix_match {
        (new_input[history.len()..].to_vec(), false)
    } else if let Some(index) = replayed_through {
        (new_input[index + 1..].to_vec(), false)
    } else if let Some(index) = semantic_suffix_idx {
        (new_input[index + 1..].to_vec(), false)
    } else {
        // A previous_response_id continuation commonly contains only a new user
        // turn or tool result, so it has no replay boundary in the stored input.
        // Only an explicit compaction above replaces its parent; otherwise append
        // the unmatched input and retain every earlier turn.
        (new_input.clone(), false)
    };

    let delta = merge_history_with_new_input(Vec::new(), &[], delta_source, tool_call_cache);
    let stored_delta = if retain_delta {
        delta.clone()
    } else {
        Vec::new()
    };
    let merged = if replace_parent || history.is_empty() {
        delta
    } else {
        merge_history_with_new_input(history, &[], delta, tool_call_cache)
    };

    PreparedSessionInput {
        merged,
        delta: stored_delta,
        reset_parent: replace_parent,
    }
}

/// 把上一轮的 response output items 转成 input items 追加到历史中
/// 同时把新的 user input items 追加进去
/// 返回合并后的 input items
pub fn merge_history_with_new_input(
    mut history: Vec<Value>,
    response_output: &[Value],
    new_input: Vec<Value>,
    tool_call_cache: &HashMap<String, Value>,
) -> Vec<Value> {
    // 检测新输入中是否包含 compaction / compaction_summary，如果包含，说明客户端正在发送压缩后的全新完整历史
    let has_compaction = new_input.iter().any(|item| {
        let t = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        t == "compaction" || t == "compaction_summary"
    });

    if has_compaction {
        tracing::info!(
            "[Session] Compaction detected in new input. Overwriting stale history (new items: {})",
            new_input.len()
        );
        // 过滤掉 compaction 本身
        let mut filtered = Vec::new();
        for item in new_input {
            let t = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if t == "compaction" || t == "compaction_summary" {
                continue;
            }
            filtered.push(item);
        }
        repair_tool_calls(&mut filtered, tool_call_cache);
        return dedupe_input_items(filtered);
    }

    // 追加上一轮 response output（assistant消息、工具调用等）
    for item in response_output {
        history.push(item.clone());
    }

    // 追加新的 input items
    for item in new_input {
        let t = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t == "compaction" || t == "compaction_summary" {
            continue;
        }
        history.push(item);
    }

    // 修复工具调用（确保function_call_output前有对应的function_call）
    repair_tool_calls(&mut history, tool_call_cache);

    // 去重
    dedupe_input_items(history)
}

fn repair_tool_calls(items: &mut Vec<Value>, tool_call_cache: &HashMap<String, Value>) {
    let mut call_present = std::collections::HashSet::new();
    for item in items.iter() {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if item_type == "function_call" || item_type == "custom_tool_call" {
            if let Some(call_id) = item.get("call_id").and_then(|v| v.as_str()) {
                call_present.insert(call_id.to_string());
            }
        }
    }

    let mut new_items = Vec::new();
    let mut inserted = std::collections::HashSet::new();
    for item in items.drain(..) {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if item_type == "function_call_output" || item_type == "custom_tool_call_output" {
            if let Some(call_id) = item.get("call_id").and_then(|v| v.as_str()) {
                if !call_id.is_empty()
                    && !call_present.contains(call_id)
                    && !inserted.contains(call_id)
                {
                    if let Some(cached_call) = tool_call_cache
                        .get(call_id)
                        .cloned()
                        .or_else(|| get_cached_tool_call(call_id))
                    {
                        new_items.push(cached_call.clone());
                        inserted.insert(call_id.to_string());
                    }
                }
            }
        }
        new_items.push(item);
    }
    *items = new_items;
}

fn dedupe_input_items(items: Vec<Value>) -> Vec<Value> {
    use std::collections::{HashMap, HashSet};
    let mut referenced_call_ids = HashSet::new();
    for item in &items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if item_type == "function_call_output" || item_type == "custom_tool_call_output" {
            if let Some(call_id) = item.get("call_id").and_then(|v| v.as_str()) {
                if !call_id.is_empty() {
                    referenced_call_ids.insert(call_id.to_string());
                }
            }
        }
    }

    let mut keep_map: HashMap<String, usize> = HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if item_id.is_empty() {
            continue;
        }
        let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
        let is_referenced = !call_id.is_empty() && referenced_call_ids.contains(call_id);
        if let Some(&existing_idx) = keep_map.get(item_id) {
            let existing_call_id = items[existing_idx]
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let existing_referenced =
                !existing_call_id.is_empty() && referenced_call_ids.contains(existing_call_id);
            if is_referenced || !existing_referenced {
                keep_map.insert(item_id.to_string(), idx);
            }
        } else {
            keep_map.insert(item_id.to_string(), idx);
        }
    }

    let mut keep_indices = std::collections::HashSet::new();
    for (_, idx) in keep_map {
        keep_indices.insert(idx);
    }

    let mut filtered = Vec::new();
    for (idx, item) in items.into_iter().enumerate() {
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if !item_id.is_empty() && !keep_indices.contains(&idx) {
            continue;
        }
        filtered.push(item);
    }
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(text: &str) -> HttpSessionEntry {
        HttpSessionEntry {
            input_items: vec![json!({
                "id": format!("msg-{text}"),
                "type": "message",
                "role": "user",
                "content": text
            })],
            instructions: "be concise".to_string(),
            model: "gemini-3.7-flash-high".to_string(),
            last_accessed: Instant::now(),
        }
    }

    #[test]
    fn responses_store_false_restores_full_input_without_retaining_delta() {
        let history = vec![
            json!({"id": "old", "role": "user", "content": "x".repeat(32768)}),
            json!({"id": "answer", "role": "assistant", "content": "prior answer"}),
        ];
        let next = json!({"id": "new", "role": "user", "content": "y".repeat(32768)});
        let mut full_input = history.clone();
        full_input.push(next.clone());
        for replay in [vec![next], full_input.clone()] {
            let prepared =
                prepare_session_input_with_storage(history.clone(), replay, &HashMap::new(), false);
            assert!(prepared.delta.is_empty());
            assert_eq!(prepared.merged, full_input);
        }
        let prepared =
            prepare_session_input_with_storage(Vec::new(), history.clone(), &HashMap::new(), false);
        assert!(prepared.delta.is_empty());
        assert_eq!(prepared.merged, history);
    }

    #[test]
    fn responses_store_false_full_tool_replay_needs_no_global_cache() {
        let call_id = format!("uncached-{}", uuid::Uuid::new_v4());
        let input = vec![
            json!({"id":"call-item", "type":"function_call", "call_id":call_id, "name":"shell_command", "arguments":"{\"command\":\"pwd\"}"}),
            json!({"type":"function_call_output", "call_id":call_id, "output":"/synthetic/workspace"}),
            json!({"role":"user", "content":"continue"}),
        ];
        assert!(get_cached_tool_call(&call_id).is_none());
        let prepared =
            prepare_session_input_with_storage(Vec::new(), input.clone(), &HashMap::new(), false);
        assert!(prepared.delta.is_empty());
        assert_eq!(prepared.merged, input);
    }

    #[test]
    fn session_chain_stores_delta_and_materializes_history() {
        let mut store = HttpSessionStore::new();
        store.insert("resp-1".to_string(), entry("first"));
        let (root, parent) = store.get("resp-1").expect("root");
        let mut replay = root.input_items.clone();
        replay.push(json!({"id": "msg-second", "content": "second"}));
        let prepared = prepare_session_input(root.input_items, replay, &HashMap::new());
        assert_eq!(prepared.delta.len(), 1);
        assert_eq!(prepared.merged.len(), 2);
        store.insert_delta(
            "resp-2".to_string(),
            Some(parent),
            prepared.delta,
            vec![json!({"id": "out-second", "content": "answer"})],
            "be concise".to_string(),
            "gemini-3.7-flash-high".to_string(),
            Some("routing-root".to_string()),
        );

        let (previous, _) = store.get("resp-2").expect("child");
        assert_eq!(previous.input_items[0]["content"], "first");
        assert_eq!(previous.input_items.len(), 3);
        assert_eq!(store.sessions["resp-2"].node.input_delta.len(), 1);
        assert_eq!(store.sessions["resp-2"].node.response_output.len(), 1);
    }

    #[test]
    fn old_response_id_branches_share_parent() {
        let mut store = HttpSessionStore::new();
        store.insert("resp-root".to_string(), entry("root"));
        let (_, parent_a) = store.get("resp-root").expect("parent a");
        let (_, parent_b) = store.get("resp-root").expect("parent b");
        assert!(Arc::ptr_eq(&parent_a.0, &parent_b.0));

        store.insert_delta(
            "resp-a".to_string(),
            Some(parent_a),
            vec![json!({"content": "branch a"})],
            Vec::new(),
            String::new(),
            String::new(),
            Some("routing-root".to_string()),
        );
        store.insert_delta(
            "resp-b".to_string(),
            Some(parent_b),
            vec![json!({"content": "branch b"})],
            Vec::new(),
            String::new(),
            String::new(),
            Some("routing-root".to_string()),
        );

        let parent_a = store.sessions["resp-a"].node.parent.as_ref().unwrap();
        let parent_b = store.sessions["resp-b"].node.parent.as_ref().unwrap();
        assert!(Arc::ptr_eq(parent_a, parent_b));
        assert_eq!(
            store.sessions["resp-a"].node.routing_session_id,
            "routing-root"
        );
        assert_eq!(
            store.sessions["resp-b"].node.routing_session_id,
            "routing-root"
        );
    }

    #[test]
    fn prepare_session_input_prevents_duplication_on_full_history_replay_without_ids() {
        // Test case when client resends full history without IDs:
        // history has 2 messages, new_input has 3 messages (the same 2 + 1 new), but no "id" field.
        let history = vec![
            json!({"role": "user", "type": "message", "content": "hello"}),
            json!({"role": "assistant", "type": "message", "content": "hi there"}),
        ];
        let new_input = vec![
            json!({"role": "user", "type": "message", "content": "hello"}),
            json!({"role": "assistant", "type": "message", "content": "hi there"}),
            json!({"role": "user", "type": "message", "content": "next question"}),
        ];

        let prepared = prepare_session_input(history, new_input, &HashMap::new());
        // Delta should be the 1 new message, not all 3 messages!
        assert_eq!(prepared.delta.len(), 1);
        assert_eq!(prepared.delta[0]["content"], "next question");
        // Merged history should be 3 messages, NOT 5 (2 + 3)!
        assert_eq!(prepared.merged.len(), 3);
    }

    #[test]
    fn prepare_session_input_appends_unmatched_continuation() {
        let history = vec![
            json!({"role": "user", "type": "message", "content": "msg 1"}),
            json!({"role": "assistant", "type": "message", "content": "msg 2"}),
        ];
        let new_input = vec![
            json!({"role": "user", "type": "message", "content": "different 1"}),
            json!({"role": "assistant", "type": "message", "content": "different 2"}),
        ];

        let prepared = prepare_session_input(history.clone(), new_input.clone(), &HashMap::new());
        assert!(!prepared.reset_parent);
        assert_eq!(prepared.delta, new_input);
        assert_eq!(prepared.merged, [history, prepared.delta].concat());
    }

    #[test]
    fn tool_items_with_different_payloads_append_to_the_parent() {
        let history = vec![json!({
            "id": "old-call",
            "type": "function_call",
            "call_id": "call-a",
            "name": "shell",
            "arguments": "{\"command\":\"pwd\"}"
        })];
        let new_input = vec![
            json!({
                "id": "old-call",
                "type": "function_call",
                "call_id": "call-b",
                "name": "shell",
                "arguments": "{\"command\":\"ls\"}"
            }),
            json!({
                "type": "function_call_output",
                "call_id": "call-b",
                "output": "file.txt"
            }),
        ];

        let prepared = prepare_session_input(history, new_input.clone(), &HashMap::new());
        assert!(!prepared.reset_parent);
        assert_eq!(prepared.delta, new_input);
        assert_eq!(prepared.merged.len(), 2);
        assert_eq!(prepared.merged[0]["call_id"], "call-b");
        assert_eq!(prepared.merged[1]["call_id"], "call-b");
    }

    #[test]
    fn explicit_compaction_replaces_parent_history() {
        let mut store = HttpSessionStore::new();
        store.insert("first".to_string(), entry("A"));
        let (old, _parent) = store.get("first").expect("old history");
        let replacement = vec![
            json!({"type": "compaction", "summary": "C"}),
            json!({"role": "user", "type": "message", "content": "D"}),
        ];
        let prepared = prepare_session_input(old.input_items, replacement.clone(), &HashMap::new());
        assert!(prepared.reset_parent);
        store.insert_delta(
            "second".to_string(),
            None,
            prepared.delta,
            vec![json!({"role": "assistant", "type": "message", "content": "E"})],
            String::new(),
            String::new(),
            None,
        );

        let (stored, _) = store.get("second").expect("replacement history");
        assert_eq!(
            stored.input_items,
            vec![
                replacement[1].clone(),
                json!({"role": "assistant", "type": "message", "content": "E"}),
            ]
        );
    }

    #[test]
    fn expired_session_is_not_revived_by_lookup() {
        let mut store = HttpSessionStore::new();
        store.insert("expired".to_string(), entry("old"));
        store.sessions.get_mut("expired").unwrap().last_accessed =
            Instant::now() - Duration::from_secs(SESSION_TTL_SECS + 1);

        assert!(store.get("expired").is_none());
        assert!(!store.sessions.contains_key("expired"));
    }
}
