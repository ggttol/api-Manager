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
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use super::{auth, now, parse_json, store::Record, CodexError, CodexManager};
use crate::proxy::server::AppState;

const MAX_SESSIONS: usize = 8192;
const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_EVENT: usize = 32 * 1024 * 1024;
const MAX_COLLECTED: usize = 64 * 1024 * 1024;
const STREAM_IDLE: Duration = Duration::from_secs(300);

struct Pin {
    account_id: String,
    touched: Instant,
}
#[derive(Default)]
pub(super) struct SessionCache {
    pins: HashMap<[u8; 32], Pin>,
}

impl SessionCache {
    fn prune(&mut self) {
        self.pins
            .retain(|_, pin| pin.touched.elapsed() < SESSION_TTL);
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
        self.pins.insert(
            key,
            Pin {
                account_id: account_id.to_string(),
                touched: Instant::now(),
            },
        );
        Ok(())
    }
}

fn scope(headers: &HeaderMap) -> [u8; 32] {
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
fn session_key(scope: &[u8; 32], kind: &str, id: &str) -> [u8; 32] {
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
                    item.get("role")
                        .and_then(Value::as_str)
                        .is_some_and(|role| matches!(role, "assistant" | "tool"))
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

async fn select_account(
    manager: &CodexManager,
    headers: &HeaderMap,
    body: &Value,
    scope: &[u8; 32],
) -> Result<String, CodexError> {
    let keys = identifiers(headers, body, scope)?;
    let mut sessions = manager.sessions.lock().await;
    sessions.prune();
    let mut pinned: Option<String> = None;
    for key in &keys {
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
    if pinned.is_none() && continuation(headers, body) {
        return Err(CodexError::new(StatusCode::CONFLICT, "Codex continuation has no known account affinity; restart the conversation with a new session ID"));
    }
    let id = match pinned {
        Some(id) => {
            manager.record(&id, true).await.map_err(|_| CodexError::unavailable("The pinned Codex account is unavailable; this conversation cannot move to another account"))?;
            id
        }
        None => manager.preferred_account().await?,
    };
    // Check capacity before mutation so a request with several aliases is committed all-or-nothing.
    let new_keys = keys
        .iter()
        .filter(|key| !sessions.pins.contains_key(*key))
        .count();
    if sessions.pins.len() + new_keys > MAX_SESSIONS {
        return Err(CodexError::unavailable(
            "Codex session cache is full; wait for inactive sessions to expire",
        ));
    }
    for key in keys {
        sessions.bind(key, &id)?;
    }
    Ok(id)
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
fn metadata(response: &mut Response, record: &Record, model: &str) {
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
fn json_response(status: StatusCode, headers: HeaderMap, value: Value) -> Response {
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
    let catalog = state.codex.authorized_get(&id, auth::MODELS_URL).await?;
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
    let model = match body.get("model").and_then(Value::as_str).filter(|model| {
        !model.is_empty() && model.len() <= 256 && !model.chars().any(char::is_control)
    }) {
        Some(model) => model.to_string(),
        None => {
            return CodexError::bad_request("A native Codex model ID is required").into_response()
        }
    };
    let caller_scope = scope(&headers);
    let id = match select_account(&manager, &headers, &body, &caller_scope).await {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let selected = match manager.record(&id, true).await {
        Ok(record) => record,
        Err(error) => return error.into_response(),
    };
    let result = forward(manager, id, headers, &mut body, caller_scope, compact).await;
    let mut response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    metadata(&mut response, &selected, &model);
    response
}

async fn forward(
    manager: Arc<CodexManager>,
    id: String,
    headers: HeaderMap,
    body: &mut Value,
    scope: [u8; 32],
    compact: bool,
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
    let url = auth::RESPONSES_URL;
    let mut record = manager.credentials(&id, None, true).await?;
    let send = |record: &Record| {
        auth::authorized(
            &manager.client(),
            reqwest::Method::POST,
            url,
            &record.tokens,
        )
        .headers(forwarded.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "text/event-stream")
        .timeout(Duration::from_secs(30 * 60))
        .body(bytes.clone())
        .send()
    };
    let mut upstream = tokio::time::timeout(Duration::from_secs(300), send(&record))
        .await
        .map_err(|_| {
            CodexError::upstream("Codex upstream did not respond before the gateway deadline")
        })?
        .map_err(|_| {
            CodexError::upstream("Unable to reach Codex upstream; no retry was performed")
        })?;
    if upstream.status() == StatusCode::UNAUTHORIZED {
        // An HTTP 401 before any downstream bytes is the only inference retry. Same account only.
        record = manager
            .credentials(&id, Some(&record.tokens.access_token), true)
            .await?;
        upstream = tokio::time::timeout(Duration::from_secs(300), send(&record))
            .await
            .map_err(|_| {
                CodexError::upstream("Codex upstream did not respond after authorization refresh")
            })?
            .map_err(|_| {
                CodexError::upstream("Unable to reach Codex upstream after authorization refresh")
            })?;
    }
    let status = upstream.status();
    let mut outbound_headers = response_headers(upstream.headers());
    if !status.is_success() {
        let raw = auth::read_bounded(upstream, auth::JSON_LIMIT).await?;
        let mut error = serde_json::from_slice::<Value>(&raw).unwrap_or_else(|_| json!({"error": {
            "message": format!("Codex upstream returned HTTP {} with a non-JSON body", status.as_u16()), "type": "upstream_error"
        }}));
        record.tokens.redact(&mut error);
        manager
            .note_error(
                &id,
                &CodexError::upstream_status(status, "Codex upstream request failed"),
            )
            .await?;
        return Ok(json_response(status, outbound_headers, error));
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
        if compact {
            value["object"] = json!("response.compaction");
        }
        return Ok(json_response(status, outbound_headers, value));
    }
    if !streaming || compact {
        let (collected_status, mut value) =
            collect_response(upstream, &manager, &scope, &id).await?;
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
        while let Some(chunk) = tokio::time::timeout(STREAM_IDLE, source.next()).await
            .map_err(|_| std::io::Error::other("Codex upstream stream timed out"))? {
            let chunk = chunk.map_err(|_| std::io::Error::other("Codex upstream stream interrupted"))?;
            for event in parser.push(&chunk).map_err(|error| std::io::Error::other(error.message))? {
                if let Some(response) = event.get("response") {
                    remember_response(&stream_manager, &scope, &id, response).await.map_err(|error| std::io::Error::other(error.message))?;
                }
            }
            // Pull-based Body polling provides backpressure. Dropping the body cancels the upstream read.
            yield chunk;
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

async fn remember_response(
    manager: &CodexManager,
    scope: &[u8; 32],
    account_id: &str,
    response: &Value,
) -> Result<(), CodexError> {
    if let Some(id) = response
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 512)
    {
        manager
            .sessions
            .lock()
            .await
            .bind(session_key(scope, "response", id), account_id)?;
    }
    Ok(())
}

// Only the observer/collector parses SSE. Streaming clients receive original chunks, including tools,
// encrypted reasoning, unknown event types, comments and [DONE], byte-for-byte.
#[derive(Default)]
struct SseParser {
    line: Vec<u8>,
    data: Vec<u8>,
    after_cr: bool,
}
impl SseParser {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, CodexError> {
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
                            if let Ok(event) = serde_json::from_slice(&self.data) {
                                events.push(event);
                            }
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

fn is_sse_response(response: &reqwest::Response) -> bool {
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

async fn collect_response(
    upstream: reqwest::Response,
    manager: &CodexManager,
    scope: &[u8; 32],
    account_id: &str,
) -> Result<(StatusCode, Value), CodexError> {
    let mut source = upstream.bytes_stream();
    let mut parser = SseParser::default();
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
                _ => {}
            }
        }
    }
    Err(CodexError::upstream(
        "Codex upstream stream ended without a terminal response",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let (status, output) = collect_response(response, &manager, &[0; 32], "account-1")
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
        let error = collect_response(truncated, &manager, &[0; 32], "account-1")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        server.abort();
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
        assert_eq!(independent, second.id);
        let forwarded = forwarding_headers(&headers_a, &scope(&headers_a));
        assert!(!forwarded.contains_key(header::AUTHORIZATION));
        assert_ne!(forwarded["session_id"], headers_a["session_id"]);
        assert_ne!(
            forwarded["session_id"],
            forwarding_headers(&headers_b, &scope(&headers_b))["session_id"]
        );
    }
}
