use axum::{
    body::Body,
    extract::{rejection::JsonRejection, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use super::{auth, now, parse_json, scheduler, store::Record, CodexError, CodexManager};
use crate::proxy::server::AppState;

const MAX_SESSIONS: usize = 8192;
pub(super) const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_EVENT: usize = 32 * 1024 * 1024;
pub(super) const MAX_COLLECTED: usize = 64 * 1024 * 1024;
pub(super) const STREAM_IDLE: Duration = Duration::from_secs(300);

struct Pin {
    account_id: String,
    touched: Instant,
    version: u64,
    migrated: bool,
}
#[derive(Default)]
pub(super) struct SessionCache {
    pins: HashMap<[u8; 32], Pin>,
    pub(super) anthropic_tools: super::anthropic::ToolCache,
    next_version: u64,
}

impl SessionCache {
    fn prune(&mut self) {
        self.pins
            .retain(|_, pin| pin.touched.elapsed() < SESSION_TTL);
        self.anthropic_tools.prune();
    }
    fn lookup(&mut self, key: &[u8; 32]) -> Option<String> {
        self.pins.get_mut(key).map(|pin| {
            pin.touched = Instant::now();
            pin.account_id.clone()
        })
    }
    fn bind(&mut self, key: [u8; 32], account_id: &str) -> Result<(), CodexError> {
        if let Some(pin) = self.pins.get_mut(&key) {
            if pin.account_id != account_id {
                return Err(CodexError::new(StatusCode::CONFLICT, "Conflicting Codex session account; do not mix conversations from different accounts"));
            }
            pin.touched = Instant::now();
            return Ok(());
        }
        // Never evict a still-live session: fail explicitly rather than silently losing account affinity.
        self.prune();
        if self.pins.len() >= MAX_SESSIONS {
            return Err(CodexError::unavailable(
                "Codex session cache is full; wait for inactive sessions to expire",
            ));
        }
        self.next_version += 1;
        self.pins.insert(
            key,
            Pin {
                account_id: account_id.to_string(),
                touched: Instant::now(),
                version: self.next_version,
                migrated: false,
            },
        );
        Ok(())
    }

    fn aliases(
        &mut self,
        keys: &[[u8; 32]],
        account: &str,
    ) -> Result<Vec<([u8; 32], u64)>, CodexError> {
        let new_keys = keys
            .iter()
            .filter(|key| !self.pins.contains_key(*key))
            .count();
        if self.pins.len() + new_keys > MAX_SESSIONS {
            return Err(CodexError::unavailable(
                "Codex session cache is full; wait for inactive sessions to expire",
            ));
        }
        let mut result = Vec::with_capacity(keys.len());
        let migrated = keys.iter().any(|key| {
            self.pins
                .get(key)
                .is_some_and(|pin| pin.migrated || pin.account_id != account)
        });
        for key in keys {
            self.next_version += 1;
            self.pins.insert(
                *key,
                Pin {
                    account_id: account.to_string(),
                    touched: Instant::now(),
                    version: self.next_version,
                    migrated,
                },
            );
            result.push((*key, self.next_version));
        }
        Ok(result)
    }
}

#[derive(Debug)]
pub(super) struct Selection {
    pub id: String,
    keys: Vec<([u8; 32], u64)>,
    portable: bool,
}

impl Selection {
    fn validate(&self, sessions: &SessionCache) -> Result<(), CodexError> {
        if self.keys.iter().any(|(key, version)| {
            sessions
                .pins
                .get(key)
                .is_none_or(|pin| pin.account_id != self.id || pin.version != *version)
        }) {
            return Err(CodexError::new(StatusCode::CONFLICT,
                "Codex session changed while this request was in flight; retry with consistent full conversation input"));
        }
        Ok(())
    }

    async fn ready(
        &mut self,
        manager: &CodexManager,
        visited: &HashSet<String>,
    ) -> Result<(), CodexError> {
        // Account eligibility and session CAS share a short lock boundary, never network I/O.
        let inner = manager.inner.lock().await;
        let mut sessions = manager.sessions.lock().await;
        self.validate(&sessions)?;
        let record = inner.accounts.accounts.iter().find(|record| record.account.id == self.id)
            .filter(|record| record.account.enabled && record.verified)
            .ok_or_else(|| CodexError::unavailable("The pinned Codex account is unavailable; this conversation cannot move to another account"))?;
        if scheduler::cooling(&record.account, now()).is_some() || visited.contains(&self.id) {
            if !self.portable {
                return Err(bound_cooldown(
                    record.account.cooldown_until.unwrap_or(now() + 60),
                ));
            }
            let next = scheduler::select(&inner.accounts, visited, now())?;
            let keys: Vec<_> = self.keys.iter().map(|(key, _)| *key).collect();
            self.keys = sessions.aliases(&keys, &next)?;
            self.id = next;
        }
        Ok(())
    }
}

fn bound_cooldown(until: i64) -> CodexError {
    CodexError::cooling(until,
        "This Codex conversation contains account-bound response, tool, or encrypted state and cannot fail over; wait for its account quota to reset or start a new conversation with full text and no private state")
}

pub(super) fn scope(headers: &HeaderMap) -> [u8; 32] {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.strip_prefix("Bearer ").unwrap_or(value))
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .or_else(|| {
            headers
                .get("x-goog-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .unwrap_or("");
    Sha256::digest(token.as_bytes()).into()
}
pub(super) fn session_key(scope: &[u8; 32], kind: &str, id: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(scope);
    hash.update(kind.as_bytes());
    hash.update([0]);
    hash.update(id.as_bytes());
    hash.finalize().into()
}
fn scoped_id(scope: &[u8; 32], kind: &str, id: &str) -> String {
    let key = session_key(scope, kind, id);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&key[..16]);
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn identifiers(
    headers: &HeaderMap,
    body: &Value,
    scope: &[u8; 32],
) -> Result<Vec<[u8; 32]>, CodexError> {
    let mut result = Vec::new();
    for name in ["session_id", "thread_id", "x-codex-session-id"] {
        if let Some(value) = headers.get(name) {
            let value = value
                .to_str()
                .map_err(|_| CodexError::bad_request("Invalid Codex session identifier"))?;
            if value.is_empty() || value.len() > 512 {
                return Err(CodexError::bad_request("Invalid Codex session identifier"));
            }
            // Different header spellings describing the same session deliberately share a namespace.
            result.push(session_key(scope, "session", value));
        }
    }
    for (field, kind) in [
        ("prompt_cache_key", "cache"),
        ("previous_response_id", "response"),
    ] {
        if let Some(value) = body.get(field).filter(|value| !value.is_null()) {
            let value = value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 512)
                .ok_or_else(|| {
                    CodexError::bad_request("Invalid Codex cache or response identifier")
                })?;
            result.push(session_key(scope, kind, value));
        }
    }
    result.sort_unstable();
    result.dedup();
    Ok(result)
}

fn continuation(headers: &HeaderMap, body: &Value) -> bool {
    body.get("previous_response_id")
        .is_some_and(|value| !value.is_null())
        || headers.contains_key("x-codex-turn-state")
        || body
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    // Plain assistant history is self-contained; it does not require an
                    // upstream account binding that may have been lost on restart.
                    item.get("role")
                        .and_then(Value::as_str)
                        .is_some_and(|role| role == "tool")
                        || item.get("encrypted_content").is_some()
                        || item
                            .get("type")
                            .and_then(Value::as_str)
                            .is_some_and(|kind| {
                                kind.ends_with("_call_output")
                                    || matches!(kind, "item_reference" | "compaction" | "reasoning")
                            })
                })
            })
}

// Private state has immutable issuer pins, independent of movable session/cache aliases.
fn private_identifiers(headers: &HeaderMap, body: &Value, scope: &[u8; 32]) -> Vec<[u8; 32]> {
    let mut keys = Vec::new();
    if let Some(turn) = headers
        .get("x-codex-turn-state")
        .and_then(|value| value.to_str().ok())
    {
        keys.push(session_key(scope, "turn", turn));
    }
    if let Some(items) = body.get("input").and_then(Value::as_array) {
        for item in items {
            let identity =
                if let Some(encrypted) = item.get("encrypted_content").and_then(Value::as_str) {
                    Some(("encrypted", encrypted))
                } else {
                    match item.get("type").and_then(Value::as_str) {
                        Some(kind) if kind.ends_with("_call_output") => item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(|id| ("call", id)),
                        Some("item_reference" | "reasoning" | "compaction") => item
                            .get("id")
                            .and_then(Value::as_str)
                            .map(|id| ("item", id)),
                        _ if item.get("role").and_then(Value::as_str) == Some("tool") => item
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .map(|id| ("call", id)),
                        _ => None,
                    }
                };
            if let Some((kind, value)) = identity {
                keys.push(session_key(scope, kind, value));
            }
        }
    }
    keys
}

fn portable_input(headers: &HeaderMap, body: &Value) -> bool {
    if continuation(headers, body) {
        return false;
    }
    match body.get("input") {
        Some(Value::String(_)) => true,
        Some(Value::Array(items)) => items.iter().all(|item| {
            item.get("id").is_none()
                && item.get("type").is_none_or(|kind| kind == "message")
                && item
                    .get("role")
                    .and_then(Value::as_str)
                    .is_some_and(|role| {
                        matches!(role, "user" | "assistant" | "system" | "developer")
                    })
                && match item.get("content") {
                    Some(Value::String(_)) => true,
                    Some(Value::Array(parts)) => parts.iter().all(|part| {
                        part.get("id").is_none()
                            && part.get("file_id").is_none()
                            && part.get("encrypted_content").is_none()
                            && match part.get("type").and_then(Value::as_str) {
                                Some("input_text" | "output_text" | "text") => {
                                    part.get("text").is_some_and(Value::is_string)
                                }
                                Some("input_image") => {
                                    part.get("image_url").is_some_and(Value::is_string)
                                }
                                Some("input_file") => part
                                    .get("file_data")
                                    .or_else(|| part.get("file_url"))
                                    .is_some_and(Value::is_string),
                                _ => false,
                            }
                    }),
                    _ => false,
                }
        }),
        _ => false,
    }
}

pub(super) async fn select_account(
    manager: &CodexManager,
    headers: &HeaderMap,
    body: &Value,
    scope: &[u8; 32],
) -> Result<Selection, CodexError> {
    select_account_inner(manager, headers, body, scope, None, false).await
}

// Only the Messages mapper may opt into self-contained tool replay. Native Responses retains
// its strict continuation guard, including for arbitrary function_call_output input.
pub(super) async fn select_messages_account(
    manager: &CodexManager,
    headers: &HeaderMap,
    body: &Value,
    scope: &[u8; 32],
    tool_account: Option<String>,
) -> Result<Selection, CodexError> {
    select_account_inner(manager, headers, body, scope, tool_account, true).await
}

async fn select_account_inner(
    manager: &CodexManager,
    headers: &HeaderMap,
    body: &Value,
    scope: &[u8; 32],
    tool_account: Option<String>,
    self_contained_messages: bool,
) -> Result<Selection, CodexError> {
    let keys = identifiers(headers, body, scope)?;
    let private_keys = private_identifiers(
        headers,
        if self_contained_messages {
            &Value::Null
        } else {
            body
        },
        scope,
    );
    let portable = if self_contained_messages {
        tool_account.is_none()
            && !headers.contains_key("x-codex-turn-state")
            && body.get("previous_response_id").is_none_or(Value::is_null)
    } else {
        portable_input(headers, body)
    };
    let inner = manager.inner.lock().await;
    let mut sessions = manager.sessions.lock().await;
    sessions.prune();
    let migrated = keys
        .iter()
        .any(|key| sessions.pins.get(key).is_some_and(|pin| pin.migrated));
    let immutable_origin = tool_account.is_some()
        || body
            .get("previous_response_id")
            .is_some_and(|value| !value.is_null());
    if migrated
        && !portable
        && ((private_keys.is_empty() && !immutable_origin)
            || private_keys
                .iter()
                .any(|key| !sessions.pins.contains_key(key)))
    {
        return Err(CodexError::new(StatusCode::CONFLICT,
            "Codex private state has no verified issuer after this session changed accounts; restart with full plain-text input"));
    }
    let mut pinned = tool_account;
    for key in keys.iter().chain(&private_keys) {
        if let Some(id) = sessions.lookup(key) {
            if pinned.as_ref().is_some_and(|pinned| pinned != &id) {
                return Err(CodexError::new(
                    StatusCode::CONFLICT,
                    "Codex request references sessions belonging to different accounts",
                ));
            }
            pinned = Some(id);
        }
    }
    if let Some(previous) = body.get("previous_response_id").and_then(Value::as_str) {
        if sessions
            .lookup(&session_key(scope, "response", previous))
            .is_none()
        {
            return Err(CodexError::new(StatusCode::CONFLICT, "Unknown or expired Codex previous_response_id; restart the conversation with full input"));
        }
    }
    if pinned.is_none() && continuation(headers, body) && !self_contained_messages {
        return Err(CodexError::new(StatusCode::CONFLICT, "Codex continuation has no known account affinity; restart the conversation with a new session ID"));
    }
    let id = if let Some(id) = pinned {
        let record = inner.accounts.accounts.iter().find(|record| record.account.id == id)
            .filter(|record| record.account.enabled && record.verified)
            .ok_or_else(|| CodexError::unavailable("The pinned Codex account is unavailable; this conversation cannot move to another account"))?;
        if let Some(until) = scheduler::cooling(&record.account, now()) {
            if !portable {
                return Err(bound_cooldown(until));
            }
            scheduler::select(&inner.accounts, &HashSet::new(), now())?
        } else {
            id
        }
    } else {
        scheduler::select(&inner.accounts, &HashSet::new(), now())?
    };
    // Preserve versions for unchanged pins, so concurrent same-account requests do not conflict.
    let new_keys = keys
        .iter()
        .filter(|key| !sessions.pins.contains_key(*key))
        .count();
    if sessions.pins.len() + new_keys > MAX_SESSIONS {
        return Err(CodexError::unavailable(
            "Codex session cache is full; wait for inactive sessions to expire",
        ));
    }
    let rebind = keys.iter().any(|key| {
        sessions
            .pins
            .get(key)
            .is_some_and(|pin| pin.account_id != id)
    });
    let keys = if rebind {
        sessions.aliases(&keys, &id)?
    } else {
        for key in &keys {
            sessions.bind(*key, &id)?;
            if migrated {
                sessions
                    .pins
                    .get_mut(key)
                    .expect("alias was just bound")
                    .migrated = true;
            }
        }
        keys.into_iter()
            .map(|key| (key, sessions.pins[&key].version))
            .collect()
    };
    Ok(Selection { id, keys, portable })
}

fn forwarding_headers(incoming: &HeaderMap, scope: &[u8; 32]) -> HeaderMap {
    let mut outgoing = HeaderMap::new();
    // Allowlist, not a denylist: no caller Authorization, ChatGPT identity, cookies, proxy credentials,
    // beta transport opt-ins, or arbitrary OpenAI authorization headers may reach the subscription.
    for name in ["session_id", "thread_id", "x-codex-session-id"] {
        if let Some(value) = incoming.get(name).and_then(|value| value.to_str().ok()) {
            if let Ok(value) = HeaderValue::from_str(&scoped_id(scope, "session", value)) {
                outgoing.insert(HeaderName::from_static(name), value);
            }
        }
    }
    for name in [
        "x-codex-turn-state",
        "x-codex-turn-metadata",
        "x-client-request-id",
        "x-openai-subagent",
    ] {
        if let Some(value) = incoming
            .get(name)
            .filter(|value| value.as_bytes().len() <= 8192)
        {
            outgoing.insert(HeaderName::from_static(name), value.clone());
        }
    }
    outgoing
}

fn response_headers(upstream: &HeaderMap) -> HeaderMap {
    let mut result = HeaderMap::new();
    for (name, value) in upstream {
        let name_str = name.as_str();
        let rate_window = name_str.starts_with("x-")
            && [
                "-primary-used-percent",
                "-primary-window-minutes",
                "-primary-reset-at",
                "-secondary-used-percent",
                "-secondary-window-minutes",
                "-secondary-reset-at",
                "-limit-name",
            ]
            .iter()
            .any(|suffix| name_str.ends_with(suffix));
        if rate_window
            || name_str.starts_with("x-ratelimit-")
            || name_str.starts_with("x-codex-credits-")
            || matches!(
                name_str,
                "retry-after"
                    | "request-id"
                    | "x-request-id"
                    | "openai-processing-ms"
                    | "openai-version"
                    | "openai-model"
                    | "x-reasoning-included"
                    | "x-codex-turn-state"
                    | "x-codex-models-etag"
                    | "x-codex-rate-limit-reached-type"
                    | "x-codex-promo-message"
                    | "content-type"
                    | "etag"
            )
        {
            result.insert(name.clone(), value.clone());
        }
    }
    result.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    result
}
pub(super) fn metadata(response: &mut Response, record: &Record, model: &str) {
    // Local monitor fields are never copied from caller/upstream identity headers.
    if let Ok(value) = HeaderValue::from_str(
        record
            .account
            .email
            .as_deref()
            .unwrap_or(&record.account.id),
    ) {
        response.headers_mut().insert("x-account-email", value);
    }
    if let Ok(value) = HeaderValue::from_str(model) {
        response.headers_mut().insert("x-mapped-model", value);
    }
}
pub(super) fn json_response(status: StatusCode, headers: HeaderMap, value: Value) -> Response {
    let mut response = (status, Json(value)).into_response();
    response.headers_mut().extend(headers);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

pub(super) async fn models(State(state): State<AppState>) -> Result<Json<Value>, CodexError> {
    let id = state.codex.preferred_account().await?;
    let catalog = state
        .codex
        .authorized_get(&id, auth::MODELS_URL, true)
        .await?;
    let models = catalog
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| CodexError::upstream("Codex returned an invalid model catalog"))?;
    let data: Vec<Value> = models
        .iter()
        .filter_map(|model| {
            model
                .get("slug")
                .and_then(Value::as_str)
                .map(|id| json!({"id": id, "object": "model", "owned_by": "openai"}))
        })
        .collect();
    Ok(Json(json!({"object": "list", "data": data})))
}

pub(super) async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    proxy(state.codex, headers, body, false).await
}
pub(super) async fn compact(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    proxy(state.codex, headers, body, true).await
}

async fn proxy(
    manager: Arc<CodexManager>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
    compact: bool,
) -> Response {
    let mut body = match parse_json(body) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    match body.get("model").and_then(Value::as_str).filter(|model| {
        !model.is_empty() && model.len() <= 256 && !model.chars().any(char::is_control)
    }) {
        Some(_) => {}
        None => {
            return CodexError::bad_request("A native Codex model ID is required").into_response()
        }
    };
    let caller_scope = scope(&headers);
    let id = match select_account(&manager, &headers, &body, &caller_scope).await {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    match forward(manager, id, headers, &mut body, caller_scope, compact, None).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

pub(super) async fn forward(
    manager: Arc<CodexManager>,
    mut selection: Selection,
    headers: HeaderMap,
    body: &mut Value,
    scope: [u8; 32],
    compact: bool,
    messages: Option<super::anthropic::ResponseOptions>,
) -> Result<Response, CodexError> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let is_messages = messages.is_some();
    let result = forward_inner(
        manager.clone(),
        &mut selection,
        headers,
        body,
        scope,
        compact,
        messages,
    )
    .await;
    let mut response = match result {
        Ok(response) => response,
        Err(error) if is_messages => super::anthropic::error_response(error),
        Err(error) => error.into_response(),
    };
    // Selection can change on 429. Both protocol wrappers must use the final account, including errors.
    if let Ok(record) = manager.record(&selection.id, false).await {
        metadata(&mut response, &record, &model);
    }
    Ok(response)
}

async fn forward_inner(
    manager: Arc<CodexManager>,
    selection: &mut Selection,
    headers: HeaderMap,
    body: &mut Value,
    scope: [u8; 32],
    compact: bool,
    messages: Option<super::anthropic::ResponseOptions>,
) -> Result<Response, CodexError> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| CodexError::bad_request("Responses request must be an object"))?;
    let streaming = match object.get("stream") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(CodexError::bad_request("stream must be a boolean")),
    };
    if object.get("background").and_then(Value::as_bool) == Some(true) {
        return Err(CodexError::bad_request(
            "Background Responses are not supported by the subscription gateway",
        ));
    }
    if let Some(cache_key) = object.get("prompt_cache_key").and_then(Value::as_str) {
        object.insert(
            "prompt_cache_key".into(),
            Value::String(scoped_id(&scope, "cache", cache_key)),
        );
    }
    if compact {
        let input = object
            .get_mut("input")
            .ok_or_else(|| CodexError::bad_request("Compact input is required"))?;
        if input.is_string() {
            *input = json!([{"role": "user", "content": input.take()}]);
        }
        input
            .as_array_mut()
            .ok_or_else(|| CodexError::bad_request("Compact input must be text or an array"))?
            .push(json!({"type": "compaction_trigger"}));
    }
    object.insert("store".into(), Value::Bool(false));
    object.insert("stream".into(), Value::Bool(true));
    let bytes = Bytes::from(
        serde_json::to_vec(&body).map_err(|_| CodexError::bad_request("Invalid Responses body"))?,
    );
    let forwarded = forwarding_headers(&headers, &scope);
    // Encode and scope exactly once: retries reuse identical bytes, including compaction injection.
    let mut visited = HashSet::new();
    // Bound even concurrent account imports/deletions; a changing pool cannot prolong this request.
    let mut remaining = manager.inner.lock().await.accounts.accounts.len();
    let (upstream, record) = loop {
        if remaining == 0 {
            let inner = manager.inner.lock().await;
            return Err(match scheduler::select(&inner.accounts, &visited, now()) {
                Err(error) => error,
                Ok(_) => CodexError::cooling(
                    now() + 1,
                    "Codex quota failover reached its per-request attempt limit; retry the request",
                ),
            });
        }
        remaining -= 1;
        selection.ready(&manager, &visited).await?;
        let mut record = manager.credentials(&selection.id, None, true).await?;
        selection.ready(&manager, &visited).await?;
        if record.account.id != selection.id {
            visited.insert(record.account.id.clone());
            continue;
        }
        visited.insert(selection.id.clone());
        let mut upstream = send(&manager, &record, &forwarded, &bytes).await?;
        if upstream.status() == StatusCode::UNAUTHORIZED {
            // Exactly one same-account refresh per attempted account. A second 401 is terminal.
            record = manager
                .credentials(&selection.id, Some(&record.tokens.access_token), true)
                .await?;
            selection.validate(&*manager.sessions.lock().await)?;
            upstream = send(&manager, &record, &forwarded, &bytes).await?;
        }
        let status = upstream.status();
        if status.is_success() {
            break (upstream, record);
        }
        let outbound_headers = response_headers(upstream.headers());
        let raw = match auth::read_bounded(upstream, auth::JSON_LIMIT).await {
            Ok(raw) => raw,
            Err(error) => {
                if status == StatusCode::TOO_MANY_REQUESTS {
                    manager
                        .note_cooldown(
                            &selection.id,
                            &scheduler::from_429(&outbound_headers, &Value::Null, now()),
                        )
                        .await?;
                }
                // Even a 429 whose body was interrupted is not replayed after a network error.
                return Err(error);
            }
        };
        let parsed = serde_json::from_slice::<Value>(&raw);
        if status == StatusCode::TOO_MANY_REQUESTS && !parsed.as_ref().is_ok_and(Value::is_object) {
            manager
                .note_cooldown(
                    &selection.id,
                    &scheduler::from_429(&outbound_headers, &Value::Null, now()),
                )
                .await?;
            return Err(CodexError::upstream(
                "Codex returned a malformed quota response; no retry was performed",
            ));
        }
        let mut error = parsed.unwrap_or_else(|_| json!({"error": {
            "message": format!("Codex upstream returned HTTP {} with a non-JSON body", status.as_u16()), "type": "upstream_error"
        }}));
        if status == StatusCode::TOO_MANY_REQUESTS {
            let cooldown = scheduler::from_429(&outbound_headers, &error, now());
            manager.note_cooldown(&selection.id, &cooldown).await?;
            if !selection.portable {
                return Err(bound_cooldown(cooldown.until));
            }
            continue;
        }
        record.tokens.redact(&mut error);
        manager
            .note_error(
                &selection.id,
                &CodexError::upstream_status(status, "Codex upstream request failed"),
            )
            .await?;
        if messages.is_some() {
            error = super::anthropic::upstream_error(status, &error);
        }
        return Ok(json_response(status, outbound_headers, error));
    };
    let id = selection.id.clone();
    let status = upstream.status();
    let mut outbound_headers = response_headers(upstream.headers());
    if let Some(turn) = upstream
        .headers()
        .get("x-codex-turn-state")
        .and_then(|value| value.to_str().ok())
    {
        manager
            .sessions
            .lock()
            .await
            .bind(session_key(&scope, "turn", turn), &id)?;
    }
    {
        let mut inner = manager.inner.lock().await;
        if let Some(record) = inner
            .accounts
            .accounts
            .iter_mut()
            .find(|record| record.account.id == id)
        {
            // Usage timestamp is operational metadata; credential rotations themselves are always durable.
            record.account.last_used_at = Some(now());
            record.account.last_error = None;
        }
    }
    if let Some(options) = messages {
        return super::anthropic::respond(
            upstream,
            manager,
            scope,
            id,
            record,
            outbound_headers,
            options,
        )
        .await;
    }
    let is_sse = is_sse_response(&upstream);
    if is_sse && !outbound_headers.contains_key(header::CONTENT_TYPE) {
        outbound_headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
    }
    if !is_sse {
        let mut value: Value =
            serde_json::from_slice(&auth::read_bounded(upstream, MAX_COLLECTED).await?)
                .map_err(|_| CodexError::upstream("Codex returned an invalid JSON response"))?;
        record.tokens.redact(&mut value);
        remember_response(&manager, &scope, &id, &value).await?;
        observe_terminal_event(&manager, &id, &value).await?;
        if compact {
            value["object"] = json!("response.compaction");
        }
        return Ok(json_response(status, outbound_headers, value));
    }
    if !streaming || compact {
        let (collected_status, mut value) =
            collect_response(upstream, &manager, &scope, &id, false).await?;
        record.tokens.redact(&mut value);
        if compact {
            value["object"] = json!("response.compaction");
        }
        return Ok(json_response(collected_status, outbound_headers, value));
    }
    let stream_manager = manager.clone();
    let stream = async_stream::try_stream! {
        let mut source = upstream.bytes_stream();
        let mut parser = SseParser::default();
        let mut terminal = false;
        while let Some(chunk) = tokio::time::timeout(STREAM_IDLE, source.next()).await
            .map_err(|_| std::io::Error::other("Codex upstream stream timed out"))? {
            let chunk = chunk.map_err(|_| std::io::Error::other("Codex upstream stream interrupted"))?;
            for event in parser.push(&chunk).map_err(|error| std::io::Error::other(error.message))? {
                observe_terminal_event(&stream_manager, &id, &event)
                    .await
                    .map_err(|error| std::io::Error::other(error.message))?;
                if let Some(response) = event.get("response") {
                    remember_response(&stream_manager, &scope, &id, response).await.map_err(|error| std::io::Error::other(error.message))?;
                }
                if let Some(item) = event.get("item") {
                    remember_item(&mut *stream_manager.sessions.lock().await, &scope, &id, item)
                        .map_err(|error| std::io::Error::other(error.message))?;
                }
                terminal |= matches!(event.get("type").and_then(Value::as_str), Some("response.completed" | "response.incomplete" | "response.failed" | "error"));
            }
            // Pull-based Body polling provides backpressure. Dropping the body cancels the upstream read.
            yield chunk;
        }
        if !terminal {
            Err(std::io::Error::other(
                "Codex upstream stream ended without a terminal response",
            ))?;
        }
    };
    let mut response = Response::new(Body::from_stream(
        stream.map(|chunk: Result<Bytes, std::io::Error>| chunk),
    ));
    *response.status_mut() = status;
    *response.headers_mut() = outbound_headers;
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}

async fn send(
    manager: &CodexManager,
    record: &Record,
    forwarded: &HeaderMap,
    bytes: &Bytes,
) -> Result<reqwest::Response, CodexError> {
    let client = manager.client();
    let request = auth::authorized(
        &client,
        reqwest::Method::POST,
        auth::RESPONSES_URL,
        &record.tokens,
    )
    .headers(forwarded.clone())
    .header(header::CONTENT_TYPE, "application/json")
    .header(header::ACCEPT, "text/event-stream")
    .timeout(Duration::from_secs(30 * 60))
    .body(bytes.clone())
    .build()
    .map_err(|_| CodexError::upstream("Unable to construct Codex upstream request"))?;
    #[cfg(test)]
    let request = {
        let mut request = request;
        if let Some(url) = &manager.responses_url {
            *request.url_mut() = url.clone();
        }
        request
    };
    tokio::time::timeout(Duration::from_secs(300), client.execute(request))
        .await
        .map_err(|_| CodexError::upstream("Codex upstream did not respond before the gateway deadline; no retry was performed"))?
        .map_err(|_| CodexError::upstream("Unable to reach Codex upstream; no retry was performed"))
}

pub(super) async fn remember_response(
    manager: &CodexManager,
    scope: &[u8; 32],
    account_id: &str,
    response: &Value,
) -> Result<(), CodexError> {
    let mut sessions = manager.sessions.lock().await;
    if let Some(id) = response
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 512)
    {
        sessions.bind(session_key(scope, "response", id), account_id)?;
    }
    if let Some(items) = response.get("output").and_then(Value::as_array) {
        for item in items {
            remember_item(&mut sessions, scope, account_id, item)?;
        }
    }
    Ok(())
}

fn remember_item(
    sessions: &mut SessionCache,
    scope: &[u8; 32],
    account_id: &str,
    item: &Value,
) -> Result<(), CodexError> {
    for (field, kind) in [
        ("id", "item"),
        ("call_id", "call"),
        ("encrypted_content", "encrypted"),
    ] {
        if let Some(value) = item
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            sessions.bind(session_key(scope, kind, value), account_id)?;
        }
    }
    Ok(())
}

// Only the observer/collector parses SSE. Streaming clients receive original chunks, including tools,
// encrypted reasoning, unknown event types, comments and [DONE], byte-for-byte.
#[derive(Default)]
pub(super) struct SseParser {
    line: Vec<u8>,
    data: Vec<u8>,
    after_cr: bool,
    strict: bool,
}
impl SseParser {
    pub(super) fn strict() -> Self {
        Self {
            strict: true,
            ..Self::default()
        }
    }
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, CodexError> {
        let mut events = Vec::new();
        for &byte in bytes {
            if self.after_cr && byte == b'\n' {
                self.after_cr = false;
                continue;
            }
            self.after_cr = false;
            if byte == b'\n' || byte == b'\r' {
                self.after_cr = byte == b'\r';
                if self.line.is_empty() {
                    if !self.data.is_empty() {
                        if self.data.last() == Some(&b'\n') {
                            self.data.pop();
                        }
                        if self.data != b"[DONE]" {
                            // SSE permits non-JSON extension/heartbeat data. This is an observer,
                            // not a wire validator; non-stream collection still requires a terminal JSON response.
                            match serde_json::from_slice(&self.data) {
                                Ok(event) => events.push(event),
                                Err(_) if self.strict => {
                                    events.push(json!({"type":"gateway.invalid_json"}))
                                }
                                Err(_) => {}
                            }
                        } else if self.strict {
                            events.push(json!({"type":"gateway.unexpected_done"}));
                        }
                        self.data.clear();
                    }
                } else if self.line.starts_with(b"data:") {
                    let value = &self.line[5..];
                    let value = value.strip_prefix(b" ").unwrap_or(value);
                    self.data.extend_from_slice(value);
                    self.data.push(b'\n');
                }
                self.line.clear();
            } else {
                self.line.push(byte);
            }
            if self.line.len() + self.data.len() > MAX_EVENT {
                return Err(CodexError::upstream(
                    "Codex SSE event exceeds the gateway size limit",
                ));
            }
        }
        Ok(events)
    }
}

pub(super) fn is_sse_response(response: &reqwest::Response) -> bool {
    // Responses is explicitly requested with stream=true. Some subscription edges omit
    // Content-Type; retain that negotiated contract.
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map_or(true, |value| {
            value.to_ascii_lowercase().starts_with("text/event-stream")
        })
}

pub(super) async fn observe_terminal_event(
    manager: &CodexManager,
    account_id: &str,
    event: &Value,
) -> Result<(), CodexError> {
    let error = match event.get("type").and_then(Value::as_str) {
        Some("response.failed" | "response.incomplete") => event
            .get("response")
            .and_then(|response| response.get("error")),
        Some("error") => event.get("error").or(Some(event)),
        _ if event.get("status").and_then(Value::as_str) == Some("failed") => event.get("error"),
        _ => None,
    };
    if let Some(error) = error.filter(|error| scheduler::is_cooldown_error(error, now())) {
        manager
            .note_cooldown(
                account_id,
                &scheduler::from_429(&HeaderMap::new(), error, now()),
            )
            .await?;
    }
    Ok(())
}

pub(super) async fn collect_response(
    upstream: reqwest::Response,
    manager: &CodexManager,
    scope: &[u8; 32],
    account_id: &str,
    strict: bool,
) -> Result<(StatusCode, Value), CodexError> {
    let mut source = upstream.bytes_stream();
    let mut parser = if strict {
        SseParser::strict()
    } else {
        SseParser::default()
    };
    let mut total = 0usize;
    let mut completed_items = BTreeMap::new();
    while let Some(chunk) = tokio::time::timeout(STREAM_IDLE, source.next())
        .await
        .map_err(|_| CodexError::upstream("Codex response collection timed out"))?
    {
        let chunk = chunk.map_err(|_| {
            CodexError::upstream("Codex upstream response interrupted before completion")
        })?;
        total = total.saturating_add(chunk.len());
        if total > MAX_COLLECTED {
            return Err(CodexError::upstream(
                "Non-stream Codex response exceeds 64 MiB; use stream=true",
            ));
        }
        for mut event in parser.push(&chunk)? {
            if let Some(response) = event.get("response") {
                remember_response(manager, scope, account_id, response).await?;
            }
            observe_terminal_event(manager, account_id, &event).await?;
            if let Some(item) = event.get("item") {
                remember_item(&mut *manager.sessions.lock().await, scope, account_id, item)?;
            }
            match event.get("type").and_then(Value::as_str) {
                Some("response.output_item.done") => {
                    if let Some(index) = event.get("output_index").and_then(Value::as_u64) {
                        if let Some(item) = event.get_mut("item") {
                            completed_items.insert(index, item.take());
                        }
                    }
                }
                Some("response.completed" | "response.incomplete" | "response.failed") => {
                    let mut response =
                        event.get_mut("response").map(Value::take).ok_or_else(|| {
                            CodexError::upstream("Codex terminal event contains no response")
                        })?;
                    // Subscription streams may deliver output only in item.done events.
                    // A populated terminal output remains authoritative; never duplicate it.
                    if response
                        .get("output")
                        .and_then(Value::as_array)
                        .is_none_or(Vec::is_empty)
                        && !completed_items.is_empty()
                    {
                        if let Some(object) = response.as_object_mut() {
                            object.insert(
                                "output".into(),
                                Value::Array(completed_items.into_values().collect()),
                            );
                        }
                    }
                    return Ok((StatusCode::OK, response));
                }
                Some("error") => return Ok((StatusCode::BAD_GATEWAY, event)),
                Some("gateway.invalid_json" | "gateway.unexpected_done") => return Err(CodexError::upstream("Codex stream contained malformed data or ended before its terminal response")),
                _ => {}
            }
        }
    }
    Err(CodexError::upstream(
        "Codex upstream stream ended without a terminal response",
    ))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::collections::VecDeque;

    pub(in crate::proxy::codex) struct Reply {
        status: StatusCode,
        headers: HeaderMap,
        body: String,
    }

    impl Reply {
        pub(in crate::proxy::codex) fn quota(until: i64) -> Self {
            Self {
                status: StatusCode::TOO_MANY_REQUESTS,
                headers: HeaderMap::new(),
                body: json!({"error":{"type":"usage_limit_reached","message":"quota exhausted","resets_at":until}}).to_string(),
            }
        }

        pub(in crate::proxy::codex) fn json(value: Value) -> Self {
            Self {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                body: value.to_string(),
            }
        }

        pub(in crate::proxy::codex) fn ok(id: &str) -> Self {
            Self {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                body: json!({"id":id,"object":"response","status":"completed",
                    "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"recovered"}]}],
                    "usage":{"input_tokens":5,"output_tokens":2,"input_tokens_details":{"cached_tokens":0}}}).to_string(),
            }
        }

        pub(in crate::proxy::codex) fn sse(wire: &str) -> Self {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            Self {
                status: StatusCode::OK,
                headers,
                body: wire.into(),
            }
        }
    }

    pub(in crate::proxy::codex) struct Fixture {
        pub manager: Arc<CodexManager>,
        pub first: String,
        pub second: String,
        pub calls: Arc<tokio::sync::Mutex<Vec<(String, Value)>>>,
        pub temp: tempfile::TempDir,
        server: tokio::task::JoinHandle<()>,
    }

    impl Fixture {
        pub(in crate::proxy::codex) async fn new(replies: Vec<Reply>) -> Self {
            let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
            let requests = calls.clone();
            let replies = Arc::new(tokio::sync::Mutex::new(VecDeque::from(replies)));
            let app = axum::Router::new().route(
                "/",
                axum::routing::post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let requests = requests.clone();
                    let replies = replies.clone();
                    async move {
                        requests.lock().await.push((
                            headers["chatgpt-account-id"].to_str().unwrap().to_string(),
                            body,
                        ));
                        let reply = replies
                            .lock()
                            .await
                            .pop_front()
                            .expect("unexpected inference replay");
                        let mut response = Response::new(Body::from(reply.body));
                        *response.status_mut() = reply.status;
                        *response.headers_mut() = reply.headers;
                        if !response.headers().contains_key(header::CONTENT_TYPE) {
                            response.headers_mut().insert(
                                header::CONTENT_TYPE,
                                HeaderValue::from_static("application/json"),
                            );
                        }
                        response
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let temp = tempfile::tempdir().unwrap();
            let mut manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
            manager.responses_url = Some(format!("http://{address}/").parse().unwrap());
            *manager.client.write() = reqwest::Client::builder().no_proxy().build().unwrap();
            let mut ids = Vec::new();
            for workspace in ["first-workspace", "second-workspace"] {
                let tokens = auth::Tokens::from_auth_json(&json!({"tokens":{
                    "access_token":format!("access-{workspace}"),"refresh_token":format!("refresh-{workspace}"),
                    "id_token":"identity","account_id":workspace
                }})).unwrap();
                let account = manager
                    .upsert_account(
                        &mut *manager.inner.lock().await,
                        tokens,
                        None,
                        &json!({}),
                        true,
                    )
                    .unwrap();
                ids.push(account.id);
            }
            Self {
                manager: Arc::new(manager),
                first: ids.remove(0),
                second: ids.remove(0),
                calls,
                temp,
                server,
            }
        }

        pub(in crate::proxy::codex) async fn responses(
            &self,
            headers: HeaderMap,
            body: Value,
            compact: bool,
        ) -> Response {
            proxy(self.manager.clone(), headers, Ok(Json(body)), compact).await
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    #[tokio::test]
    async fn quota_failover_rebinds_old_plain_aliases_but_retains_response_affinity_and_preference()
    {
        let until = now() + 120;
        let fixture = Fixture::new(vec![
            Reply::quota(until),
            Reply::ok("resp-recovered"),
            Reply::ok("resp-next-turn"),
        ])
        .await;
        let mut headers = HeaderMap::new();
        headers.insert("session_id", HeaderValue::from_static("old-session"));
        let scope = scope(&headers);
        let body = json!({"model":"native-model","prompt_cache_key":"old-cache","input":[
            {"role":"user","content":"Hello"},{"role":"assistant","content":"Hi"},{"role":"user","content":"Continue"}]});
        let old = select_account(&fixture.manager, &headers, &body, &scope)
            .await
            .unwrap();
        assert_eq!(old.id, fixture.first);
        remember_response(
            &fixture.manager,
            &scope,
            &fixture.first,
            &json!({"id":"resp-original"}),
        )
        .await
        .unwrap();
        let response = fixture.responses(headers.clone(), body.clone(), true).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["x-account-email"],
            fixture.second.as_str()
        );
        let value: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), MAX_COLLECTED)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["object"], "response.compaction");
        assert_eq!(value["output"][0]["content"][0]["text"], "recovered");
        let calls = fixture.calls.lock().await;
        assert_eq!(
            calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
            ["first-workspace", "second-workspace"]
        );
        assert_eq!(calls[0].1, calls[1].1);
        assert_eq!(
            calls[1].1["prompt_cache_key"],
            scoped_id(&scope, "cache", "old-cache")
        );
        assert_eq!(
            calls[1].1["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["type"] == "compaction_trigger")
                .count(),
            1
        );
        drop(calls);
        let current = select_account(&fixture.manager, &headers, &body, &scope)
            .await
            .unwrap();
        assert_eq!(current.id, fixture.second);
        let next = fixture
            .responses(headers.clone(), body.clone(), false)
            .await;
        assert_eq!(next.status(), StatusCode::OK);
        assert_eq!(next.headers()["x-account-email"], fixture.second.as_str());
        assert_eq!(
            fixture
                .calls
                .lock()
                .await
                .iter()
                .map(|call| call.0.as_str())
                .collect::<Vec<_>>(),
            ["first-workspace", "second-workspace", "second-workspace"]
        );
        let mut stale = old;
        assert_eq!(
            stale
                .ready(&fixture.manager, &HashSet::new())
                .await
                .unwrap_err()
                .status,
            StatusCode::CONFLICT
        );
        let error = select_account(
            &fixture.manager,
            &HeaderMap::new(),
            &json!({"previous_response_id":"resp-original"}),
            &scope,
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(error.retry_after.is_some());
        assert_eq!(
            fixture
                .manager
                .inner
                .lock()
                .await
                .accounts
                .active_account_id
                .as_deref(),
            Some(fixture.first.as_str())
        );
        let restarted = CodexManager::new(fixture.temp.path().to_path_buf(), None).unwrap();
        assert_eq!(restarted.preferred_account().await.unwrap(), fixture.second);
        assert_eq!(
            restarted
                .record(&fixture.first, true)
                .await
                .unwrap()
                .account
                .cooldown_until,
            Some(until)
        );
    }

    #[tokio::test]
    async fn all_accounts_exhausted_return_earliest_retry_after_without_replay() {
        let first_reset = now() + 180;
        let second_reset = now() + 90;
        let fixture =
            Fixture::new(vec![Reply::quota(first_reset), Reply::quota(second_reset)]).await;
        let response = fixture
            .responses(
                HeaderMap::new(),
                json!({"model":"native","input":"hello"}),
                false,
            )
            .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers()["x-account-email"],
            fixture.second.as_str()
        );
        let retry: i64 = response.headers()[header::RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=90).contains(&retry));
        assert!(retry >= second_reset - now());
        assert_eq!(fixture.calls.lock().await.len(), 2);
        let again = fixture
            .responses(
                HeaderMap::new(),
                json!({"model":"native","input":"again"}),
                false,
            )
            .await;
        assert_eq!(again.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(fixture.calls.lock().await.len(), 2);
        let mut inner = fixture.manager.inner.lock().await;
        for record in &mut inner.accounts.accounts {
            record.account.enabled = false;
        }
        drop(inner);
        assert_eq!(
            fixture
                .responses(
                    HeaderMap::new(),
                    json!({"model":"native","input":"hello"}),
                    false
                )
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn quota_migration_preserves_native_private_state_issuers() {
        let mut first = Reply::ok("resp-before");
        let mut value: Value = serde_json::from_str(&first.body).unwrap();
        value["output"] = json!([
            {"id":"item-A","type":"reasoning","encrypted_content":"encrypted-A"},
            {"type":"function_call","call_id":"call-A","name":"read","arguments":"{}"}
        ]);
        first.body = value.to_string();
        first
            .headers
            .insert("x-codex-turn-state", HeaderValue::from_static("turn-A"));
        // Observe item-level SSE provenance even when the terminal response omits output.
        let mut second = Reply::sse(&format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\n",
            json!({"type":"response.output_item.done","item":{"id":"item-B","type":"reasoning","encrypted_content":"encrypted-B"}}),
            json!({"type":"response.output_item.done","item":{"type":"function_call","call_id":"call-B","name":"read","arguments":"{}"}}),
            json!({"type":"response.completed","response":{"id":"resp-after","output":[]}})
        ));
        second
            .headers
            .insert("x-codex-turn-state", HeaderValue::from_static("turn-B"));
        let mut replies = vec![first, Reply::quota(now() + 120), second];
        replies.extend((0..5).map(|_| Reply::ok("resp-continuation")));
        let fixture = Fixture::new(replies).await;
        let mut headers = HeaderMap::new();
        headers.insert("session_id", HeaderValue::from_static("migrating-session"));
        for text in ["before quota", "after quota"] {
            let response = fixture
                .responses(
                    headers.clone(),
                    json!({"model":"native","input":text,"stream":true}),
                    false,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            axum::body::to_bytes(response.into_body(), MAX_COLLECTED)
                .await
                .unwrap();
        }
        for input in [
            json!([{"type":"reasoning","encrypted_content":"encrypted-A"}]),
            json!([{"type":"compaction","encrypted_content":"encrypted-A"}]),
            json!([{"type":"function_call_output","call_id":"call-A","output":"result"}]),
            json!([{"type":"item_reference","id":"item-A"}]),
            json!([{"type":"reasoning","encrypted_content":"unknown-private-state"}]),
            Value::Null,
        ] {
            let mut headers = headers.clone();
            if input.is_null() {
                headers.insert("x-codex-turn-state", HeaderValue::from_static("turn-A"));
            }
            let response = fixture.responses(headers,
                json!({"model":"native","input":if input.is_null() {json!("continue")} else {input}}), false).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
        }
        assert_eq!(fixture.calls.lock().await.len(), 3);
        // New state really issued by B remains usable; do not simply reject all private
        // continuations after failover, which would break the next native CLI turn.
        for input in [
            json!([{"type":"reasoning","encrypted_content":"encrypted-B"}]),
            json!([{"type":"compaction","encrypted_content":"encrypted-B"}]),
            json!([{"type":"function_call_output","call_id":"call-B","output":"result"}]),
            json!([{"type":"item_reference","id":"item-B"}]),
            Value::Null,
        ] {
            let mut headers = headers.clone();
            if input.is_null() {
                headers.insert("x-codex-turn-state", HeaderValue::from_static("turn-B"));
            }
            let response = fixture.responses(headers,
                json!({"model":"native","input":if input.is_null() {json!("continue")} else {input}}), false).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["x-account-email"],
                fixture.second.as_str()
            );
        }
        assert_eq!(fixture.calls.lock().await.len(), 8);
    }

    #[tokio::test]
    async fn malformed_quota_response_cools_account_without_replaying_inference() {
        let mut malformed = Reply::quota(now() + 120);
        malformed.body = "<html>invalid upstream response</html>".into();
        let fixture = Fixture::new(vec![malformed, Reply::ok("must-not-replay")]).await;
        let response = fixture
            .responses(
                HeaderMap::new(),
                json!({"model":"native","input":"hello"}),
                false,
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(fixture.calls.lock().await.len(), 1);
        assert_eq!(
            fixture.manager.preferred_account().await.unwrap(),
            fixture.second
        );
    }

    #[tokio::test]
    async fn account_bound_native_requests_never_migrate_on_quota_failure() {
        for (body, turn_state) in [
            (
                json!({"previous_response_id":"resp-bound","input":"continue"}),
                false,
            ),
            (
                json!({"input":[{"type":"reasoning","encrypted_content":"private"}]}),
                false,
            ),
            (
                json!({"input":[{"type":"compaction","encrypted_content":"private"}]}),
                false,
            ),
            (
                json!({"input":[{"type":"function_call_output","call_id":"native-call","output":"result"}]}),
                false,
            ),
            (json!({"input":"continue"}), true),
        ] {
            let fixture = Fixture::new(vec![Reply::quota(now() + 120)]).await;
            let mut headers = HeaderMap::new();
            headers.insert("session_id", HeaderValue::from_static("bound-session"));
            if turn_state {
                headers.insert("x-codex-turn-state", HeaderValue::from_static("private"));
            }
            let scope = scope(&headers);
            fixture
                .manager
                .sessions
                .lock()
                .await
                .bind(
                    session_key(&scope, "session", "bound-session"),
                    &fixture.first,
                )
                .unwrap();
            remember_response(
                &fixture.manager,
                &scope,
                &fixture.first,
                &json!({"id":"resp-bound"}),
            )
            .await
            .unwrap();
            let mut body = body;
            body["model"] = json!("native");
            let response = fixture
                .responses(headers.clone(), body.clone(), false)
                .await;
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(
                response.headers()["x-account-email"],
                fixture.first.as_str()
            );
            assert!(response.headers().contains_key(header::RETRY_AFTER));
            let second = fixture.responses(headers, body, false).await;
            assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(
                fixture
                    .calls
                    .lock()
                    .await
                    .iter()
                    .map(|call| call.0.as_str())
                    .collect::<Vec<_>>(),
                ["first-workspace"]
            );
            assert!(fixture
                .manager
                .record(&fixture.second, true)
                .await
                .unwrap()
                .account
                .last_used_at
                .is_none());
        }
    }

    #[tokio::test]
    async fn inference_never_replays_server_errors_network_failures_or_started_streams() {
        let fixture = Fixture::new(vec![Reply {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            headers: HeaderMap::new(),
            body: json!({"error":{"message":"upstream failed"}}).to_string(),
        }])
        .await;
        let response = fixture
            .responses(
                HeaderMap::new(),
                json!({"model":"native","input":"hello"}),
                false,
            )
            .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(fixture.calls.lock().await.len(), 1);

        let wire = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"started\"}\n\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"type\":\"usage_limit_reached\"}}}\n\n";
        let streamed = Fixture::new(vec![Reply::sse(wire)]).await;
        let response = streamed
            .responses(
                HeaderMap::new(),
                json!({"model":"native","input":"hello","stream":true}),
                false,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), MAX_COLLECTED)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), wire.as_bytes());
        assert_eq!(streamed.calls.lock().await.len(), 1);
        assert_eq!(
            streamed.manager.preferred_account().await.unwrap(),
            streamed.second
        );

        let interrupted = Fixture::new(vec![Reply::sse(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"unfinished\"}\n\n",
        )])
        .await;
        let response = interrupted
            .responses(
                HeaderMap::new(),
                json!({"model":"native","input":"hello","stream":true}),
                false,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(axum::body::to_bytes(response.into_body(), MAX_COLLECTED)
            .await
            .is_err());

        let disconnected = Fixture::new(Vec::new()).await;
        disconnected.server.abort();
        while !disconnected.server.is_finished() {
            tokio::task::yield_now().await;
        }
        let response = disconnected
            .responses(
                HeaderMap::new(),
                json!({"model":"native","input":"hello"}),
                false,
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            response.headers()["x-account-email"],
            disconnected.first.as_str()
        );
        assert!(disconnected
            .manager
            .record(&disconnected.second, true)
            .await
            .unwrap()
            .account
            .last_used_at
            .is_none());
    }

    #[test]
    fn sse_utf8_crlf_multiline_and_chunk_boundaries_preserve_terminal_items() {
        let wire = concat!(": heartbeat\r\n\r\n", "event: response.completed\r\ndata: {\"type\":\"response.completed\",\r\n", "data: \"response\":{\"id\":\"r1\",\"output\":[{\"type\":\"reasoning\",\"encrypted_content\":\"秘密\"},{\"type\":\"function_call\",\"arguments\":\"{}\"}]}}\r\n\r\n", "data: [DONE]\r\n\r\n");
        for width in [1, 2, 7, 127] {
            let mut parser = SseParser::default();
            let events: Vec<_> = wire
                .as_bytes()
                .chunks(width)
                .flat_map(|chunk| parser.push(chunk).unwrap())
                .collect();
            assert_eq!(
                events,
                vec![
                    json!({"type":"response.completed", "response":{"id":"r1", "output":[{"type":"reasoning","encrypted_content":"秘密"},{"type":"function_call","arguments":"{}"}]}})
                ]
            );
        }
    }

    #[tokio::test]
    async fn http_sse_collection_retains_tool_calls_and_rejects_truncated_responses() {
        use axum::routing::get;
        let terminal = json!({
            "id": "resp-tools",
            "object": "response",
            "status": "completed",
            "output": [
                {"type": "reasoning", "encrypted_content": "opaque-reasoning"},
                {"type": "function_call", "call_id": "call-1", "name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}"}
            ]
        });
        let event = format!(
            "data: {}\n\n",
            json!({"type": "response.completed", "response": terminal})
        );
        let sparse_event = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\n",
            json!({"type": "response.output_item.done", "output_index": 1, "item": terminal["output"][1]}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": terminal["output"][0]}),
            json!({"type": "response.completed", "response": {"id":"resp-tools", "object":"response", "status":"completed", "output":[]}})
        );
        let app = axum::Router::new()
            .route("/complete", get(move || {
                let event = event.clone();
                async move { Response::new(Body::from(event)) }
            }))
            .route("/sparse", get(move || {
                let event = sparse_event.clone();
                async move { Response::new(Body::from(event)) }
            }))
            .route("/truncated", get(|| async {
                ([(header::CONTENT_TYPE, "text/event-stream")], "data: {\"type\":\"response.output_text.delta\",\"delta\":\"unfinished\"}\n\n")
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for path in ["/complete", "/sparse"] {
            let response = client
                .get(format!("http://{address}{path}"))
                .send()
                .await
                .unwrap();
            assert!(!response.headers().contains_key(header::CONTENT_TYPE));
            assert!(is_sse_response(&response));
            let (status, output) =
                collect_response(response, &manager, &[0; 32], "account-1", false)
                    .await
                    .unwrap();
            assert_eq!(status, StatusCode::OK);
            assert_eq!(output, terminal);
        }
        let truncated = client
            .get(format!("http://{address}/truncated"))
            .send()
            .await
            .unwrap();
        let error = collect_response(truncated, &manager, &[0; 32], "account-1", false)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        server.abort();
    }

    #[tokio::test]
    async fn full_text_history_can_rebind_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let tokens = auth::Tokens::from_auth_json(&json!({"tokens": {
            "access_token": "access", "refresh_token": "refresh",
            "id_token": "identity", "account_id": "workspace"
        }}))
        .unwrap();
        let account = manager
            .upsert_account(
                &mut *manager.inner.lock().await,
                tokens,
                None,
                &json!({}),
                true,
            )
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "session_id",
            HeaderValue::from_static("existing-conversation"),
        );
        let caller = scope(&headers);
        select_account(
            &manager,
            &headers,
            &json!({"input": [
                {"role": "user", "content": "Hello"}
            ]}),
            &caller,
        )
        .await
        .unwrap();
        drop(manager);

        let restarted = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let selected = select_account(
            &restarted,
            &headers,
            &json!({"input": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hello!"},
                {"role": "user", "content": "Continue"}
            ]}),
            &caller,
        )
        .await
        .unwrap();
        assert_eq!(selected.id, account.id);
    }

    #[tokio::test]
    async fn unbound_account_dependent_state_still_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let mut headers = HeaderMap::new();
        for body in [
            json!({"previous_response_id": "lost-response"}),
            json!({"input": [{"role": "assistant", "encrypted_content": "opaque"}]}),
            json!({"input": [{"type": "item_reference", "id": "lost-item"}]}),
            json!({"input": [{"type": "compaction", "encrypted_content": "opaque"}]}),
            json!({"input": [{"type": "function_call_output", "call_id": "lost-call", "output": "result"}]}),
        ] {
            let error = select_account(&manager, &headers, &body, &scope(&headers))
                .await
                .unwrap_err();
            assert_eq!(error.status, StatusCode::CONFLICT);
        }
        headers.insert("x-codex-turn-state", HeaderValue::from_static("opaque"));
        let error = select_account(&manager, &headers, &json!({}), &scope(&headers))
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn sessions_are_key_scoped_and_unavailable_account_pins_never_rotate() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let mut inner = manager.inner.lock().await;
        let first_tokens = auth::Tokens::from_auth_json(&json!({"tokens": {"access_token":"first", "refresh_token":"first-refresh", "id_token":"first-id", "account_id":"first-workspace"}})).unwrap();
        let second_tokens = auth::Tokens::from_auth_json(&json!({"tokens": {"access_token":"second", "refresh_token":"second-refresh", "id_token":"second-id", "account_id":"second-workspace"}})).unwrap();
        let first = manager
            .upsert_account(&mut inner, first_tokens, None, &json!({}), true)
            .unwrap();
        let second = manager
            .upsert_account(&mut inner, second_tokens, None, &json!({}), true)
            .unwrap();
        inner.accounts.active_account_id = Some(second.id.clone());
        inner.accounts.accounts[0].account.enabled = false;
        drop(inner);
        let mut headers_a = HeaderMap::new();
        headers_a.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer key-a"),
        );
        headers_a.insert("session_id", HeaderValue::from_static("shared"));
        let mut headers_b = headers_a.clone();
        headers_b.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer key-b"),
        );
        let key = session_key(&scope(&headers_a), "session", "shared");
        manager.sessions.lock().await.bind(key, &first.id).unwrap();
        let disabled = select_account(&manager, &headers_a, &json!({}), &scope(&headers_a))
            .await
            .unwrap_err();
        assert_eq!(disabled.status, StatusCode::SERVICE_UNAVAILABLE);
        manager.inner.lock().await.accounts.accounts.remove(0);
        let deleted = select_account(&manager, &headers_a, &json!({}), &scope(&headers_a))
            .await
            .unwrap_err();
        assert_eq!(deleted.status, StatusCode::SERVICE_UNAVAILABLE);
        let independent = select_account(&manager, &headers_b, &json!({}), &scope(&headers_b))
            .await
            .unwrap();
        assert_eq!(independent.id, second.id);
        let forwarded = forwarding_headers(&headers_a, &scope(&headers_a));
        assert!(!forwarded.contains_key(header::AUTHORIZATION));
        assert_ne!(forwarded["session_id"], headers_a["session_id"]);
        assert_ne!(
            forwarded["session_id"],
            forwarding_headers(&headers_b, &scope(&headers_b))["session_id"]
        );
    }
}
