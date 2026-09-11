//! Anthropic wire compatibility over subscription Responses, not a Claude model implementation.
//! Codex owns the output budget and prompt cache. `max_tokens`, thinking budgets and cache hints
//! are advisory compatibility inputs; the response header and integration guide disclose this.
//! Tool IDs are random, credential-scope-bound handles. Encrypted reasoning never leaves the
//! gateway and can only be replayed on its original account while its in-memory handle is live.
use axum::{
    body::Body,
    extract::{rejection::JsonRejection, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    Json,
};
use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use super::{auth, parse_json, relay, store::Record, CodexError, CodexManager};
use crate::proxy::server::AppState;

mod search;

const TOOL_PREFIX: &str = "toolu_codex_";
const MAX_TOOLS: usize = 8192;
const COMPATIBILITY: &str = "max_tokens=upstream-managed; thinking_budget=effort-hint; cache_control=automatic; reasoning=server-side; tool_state=volatile-24h; web_search_max_uses=advisory; search_handles=gateway-opaque; source_quotes=unavailable";

#[derive(Clone)]
struct ToolPin {
    account: String,
    call_id: String,
    name: String,
    reasoning: Arc<BTreeMap<u64, Value>>,
    output_index: u64,
    touched: Instant,
    bytes: usize,
    search_bound: bool,
}

#[derive(Default)]
pub(super) struct ToolCache {
    pins: HashMap<[u8; 32], ToolPin>,
    bytes: usize,
    search: search::ReplayCache,
}

impl ToolCache {
    pub(super) fn prune(&mut self) {
        self.pins
            .retain(|_, pin| pin.touched.elapsed() < relay::SESSION_TTL);
        self.bytes = self.pins.values().map(|pin| pin.bytes).sum();
        self.search.prune();
    }

    fn lookup(
        &mut self,
        scope: &[u8; 32],
        id: &str,
        search_restored: bool,
    ) -> Result<ToolPin, CodexError> {
        let key = relay::session_key(scope, "anthropic-tool", id);
        let pin = self.pins.get_mut(&key).filter(|pin| pin.touched.elapsed() < relay::SESSION_TTL)
            .ok_or_else(|| CodexError::new(StatusCode::CONFLICT,
                "Unknown, expired, or differently scoped Codex tool ID; start a new conversation without old tool state"))?;
        if pin.search_bound && !search_restored {
            return Err(CodexError::new(StatusCode::CONFLICT,
                "This client tool belongs to gateway search history; replay the complete original assistant turn"));
        }
        pin.touched = Instant::now();
        Ok(pin.clone())
    }

    fn commit(
        &mut self,
        scope: &[u8; 32],
        account: &str,
        tools: &[OutputTool],
        reasoning: BTreeMap<u64, Value>,
    ) -> Result<(), CodexError> {
        if tools.is_empty() {
            return Ok(());
        }
        let mut calls = HashSet::new();
        if tools.iter().any(|tool| !calls.insert(&tool.call_id)) {
            return Err(CodexError::upstream(
                "Codex returned duplicate tool call IDs",
            ));
        }
        self.prune();
        let reasoning_bytes = serde_json::to_vec(&reasoning)
            .map_err(|_| CodexError::upstream("Invalid reasoning state"))?
            .len();
        let bytes = tools
            .iter()
            .try_fold(0usize, |sum, tool| {
                sum.checked_add(
                    reasoning_bytes + tool.call_id.len() + tool.name.len() + account.len(),
                )
            })
            .ok_or_else(|| CodexError::unavailable("Codex tool state is too large"))?;
        // Conservatively charge shared reasoning once per handle, bounding memory even after partial expiry.
        if self.pins.len() + self.search.len() + tools.len() > MAX_TOOLS
            || bytes
                > relay::MAX_COLLECTED
                    .saturating_sub(self.bytes)
                    .saturating_sub(self.search.retained_bytes())
        {
            return Err(CodexError::unavailable(
                "Codex tool state cache is full; wait for inactive conversations to expire",
            ));
        }
        let reasoning = Arc::new(reasoning);
        for tool in tools {
            self.pins.insert(
                relay::session_key(scope, "anthropic-tool", &tool.id),
                ToolPin {
                    account: account.to_string(),
                    call_id: tool.call_id.clone(),
                    name: tool.name.clone(),
                    reasoning: reasoning.clone(),
                    output_index: tool.output_index,
                    touched: Instant::now(),
                    bytes: reasoning_bytes + tool.call_id.len() + tool.name.len() + account.len(),
                    search_bound: false,
                },
            );
        }
        self.bytes += bytes;
        Ok(())
    }

    fn commit_output(
        &mut self,
        scope: &[u8; 32],
        account: &str,
        tools: &[OutputTool],
        reasoning: BTreeMap<u64, Value>,
        wire: Vec<Value>,
        native: Vec<Value>,
    ) -> Result<(), CodexError> {
        self.commit(scope, account, tools, reasoning)?;
        self.prune();
        let committed = self.search.commit(
            scope,
            account,
            wire,
            native,
            relay::MAX_COLLECTED.saturating_sub(self.bytes),
            MAX_TOOLS.saturating_sub(self.pins.len()),
        );
        match committed {
            Ok(search_bound) => {
                if search_bound {
                    for tool in tools {
                        if let Some(pin) = self.pins.get_mut(&relay::session_key(
                            scope,
                            "anthropic-tool",
                            &tool.id,
                        )) {
                            pin.search_bound = true;
                        }
                    }
                }
                Ok(())
            }
            Err(error) => {
                // These handles were freshly minted for this response. Admission
                // is atomic under the session lock; failed output retains no pins.
                for tool in tools {
                    if let Some(pin) =
                        self.pins
                            .remove(&relay::session_key(scope, "anthropic-tool", &tool.id))
                    {
                        self.bytes -= pin.bytes;
                    }
                }
                Err(error)
            }
        }
    }
}

pub(super) struct ResponseOptions {
    model: String,
    stream: bool,
}

struct MappedRequest {
    body: Value,
    options: ResponseOptions,
    tool_account: Option<String>,
}

fn object<'a>(value: &'a Value, location: &str) -> Result<&'a Map<String, Value>, CodexError> {
    value
        .as_object()
        .ok_or_else(|| CodexError::bad_request(format!("{location} must be an object")))
}
fn fields(value: &Value, allowed: &[&str], location: &str) -> Result<(), CodexError> {
    for key in object(value, location)?.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(CodexError::bad_request(format!(
                "Unsupported {location} parameter: {key}"
            )));
        }
    }
    Ok(())
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, CodexError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| CodexError::bad_request(format!("{key} must be a string")))
}
fn identifier<'a>(value: &'a Value, key: &str) -> Result<&'a str, CodexError> {
    let id = string(value, key)?;
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        Err(CodexError::bad_request(format!("Invalid {key}")))
    } else {
        Ok(id)
    }
}
fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>, CodexError> {
    value
        .get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| CodexError::bad_request(format!("{key} must be a boolean")))
        })
        .transpose()
}
fn cache_hint(value: &Value) -> Result<(), CodexError> {
    if let Some(hint) = value.get("cache_control").filter(|hint| !hint.is_null()) {
        fields(hint, &["type", "ttl"], "cache_control")?;
        if string(hint, "type")? != "ephemeral"
            || hint
                .get("ttl")
                .is_some_and(|ttl| !matches!(ttl.as_str(), Some("5m" | "1h")))
        {
            return Err(CodexError::bad_request("Only ephemeral cache hints are accepted; Codex manages cache lifetime automatically"));
        }
    }
    Ok(())
}
fn text_block(value: &Value) -> Result<&str, CodexError> {
    fields(
        value,
        &["type", "text", "cache_control", "citations"],
        "text block",
    )?;
    cache_hint(value)?;
    if value
        .get("citations")
        .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|v| !v.is_empty()))
    {
        return Err(CodexError::bad_request(
            "Anthropic citation replay is unsupported",
        ));
    }
    string(value, "text")
}
fn image(value: &Value) -> Result<Value, CodexError> {
    fields(value, &["type", "source", "cache_control"], "image block")?;
    cache_hint(value)?;
    let source = &value["source"];
    let url = match string(source, "type")? {
        "base64" => {
            fields(source, &["type", "media_type", "data"], "image source")?;
            let media = string(source, "media_type")?;
            if !matches!(
                media,
                "image/jpeg" | "image/png" | "image/gif" | "image/webp"
            ) {
                return Err(CodexError::bad_request("Unsupported image media_type"));
            }
            let data = string(source, "data")?;
            // Validate without allocating a decoded copy of potentially large image data.
            let mut decoder = base64::read::DecoderReader::new(
                data.as_bytes(),
                &base64::engine::general_purpose::STANDARD,
            );
            let count = std::io::copy(&mut decoder, &mut std::io::sink())
                .map_err(|_| CodexError::bad_request("Invalid base64 image data"))?;
            if count == 0 {
                return Err(CodexError::bad_request("Image data must not be empty"));
            }
            format!("data:{media};base64,{data}")
        }
        "url" => {
            fields(source, &["type", "url"], "image source")?;
            let raw = string(source, "url")?;
            let parsed =
                url::Url::parse(raw).map_err(|_| CodexError::bad_request("Invalid image URL"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Err(CodexError::bad_request(
                    "Image URL must be HTTP(S) without credentials",
                ));
            }
            raw.to_string()
        }
        _ => {
            return Err(CodexError::bad_request(
                "Only URL and base64 image sources are supported",
            ))
        }
    };
    Ok(json!({"type":"input_image", "image_url":url}))
}
fn flush_message(input: &mut Vec<Value>, role: &str, content: &mut Vec<Value>) {
    if !content.is_empty() {
        input.push(json!({"role":role, "content":std::mem::take(content)}));
    }
}

fn map_request(
    request: Value,
    scope: &[u8; 32],
    cache: &mut ToolCache,
) -> Result<MappedRequest, CodexError> {
    fields(
        &request,
        &[
            "model",
            "max_tokens",
            "messages",
            "system",
            "stream",
            "tools",
            "tool_choice",
            "metadata",
            "thinking",
            "output_config",
            "cache_control",
            "stop_sequences",
            "context_management",
        ],
        "Messages",
    )?;
    if let Some(context) = request.get("context_management") {
        fields(context, &["edits"], "context_management")?;
        let edits = context
            .get("edits")
            .and_then(Value::as_array)
            .ok_or_else(|| CodexError::bad_request("context_management.edits must be an array"))?;
        for edit in edits {
            fields(edit, &["type", "keep"], "context_management edit")?;
            if edit.get("type").and_then(Value::as_str) != Some("clear_thinking_20251015")
                || edit.get("keep").and_then(Value::as_str) != Some("all")
            {
                return Err(CodexError::bad_request("Only context_management clear_thinking_20251015 with keep=all is supported; Codex tool reasoning must be retained"));
            }
        }
    }
    let model = identifier(&request, "model")?.to_string();
    if model.len() > 256 {
        return Err(CodexError::bad_request(
            "A native Codex model ID is required",
        ));
    }
    if request
        .get("max_tokens")
        .and_then(Value::as_u64)
        .is_none_or(|v| v == 0)
    {
        return Err(CodexError::bad_request(
            "max_tokens must be a positive integer; Codex manages the actual output budget",
        ));
    }
    if let Some(stops) = request.get("stop_sequences") {
        if stops.as_array().is_none_or(|v| !v.is_empty()) {
            return Err(CodexError::bad_request(
                "Nonempty stop_sequences are unsupported by Codex; they cannot be silently ignored",
            ));
        }
    }
    cache_hint(&request)?;
    let stream = optional_bool(&request, "stream")?.unwrap_or(false);
    let mut body = json!({"model":model, "stream":stream, "instructions":"", "include":["reasoning.encrypted_content"]});
    if let Some(system) = request.get("system") {
        let text = if let Some(text) = system.as_str() {
            text.to_string()
        } else {
            let blocks = system
                .as_array()
                .ok_or_else(|| CodexError::bad_request("system must be text or text blocks"))?;
            let mut text = String::new();
            for block in blocks {
                if string(block, "type")? != "text" {
                    return Err(CodexError::bad_request(
                        "Only text system blocks are supported",
                    ));
                }
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(text_block(block)?);
            }
            text
        };
        body["instructions"] = json!(text);
    }
    if let Some(metadata) = request.get("metadata") {
        fields(metadata, &["user_id"], "metadata")?;
        if metadata.get("user_id").is_some_and(|v| !v.is_null()) {
            let user = identifier(metadata, "user_id")?;
            // Claude Code supplies a JSON user_id containing session_id; otherwise user_id is a
            // stable caller-selected cache identity, never an upstream identity or credential.
            let parsed = serde_json::from_str::<Value>(user).ok();
            let session = parsed
                .as_ref()
                .and_then(|v| v.get("session_id"))
                .and_then(Value::as_str)
                .unwrap_or(user);
            if session.is_empty() || session.len() > 512 {
                return Err(CodexError::bad_request(
                    "Invalid metadata session identifier",
                ));
            }
            body["prompt_cache_key"] = json!(session);
        }
    }
    let mut effort: Option<&str> = None;
    if let Some(thinking) = request.get("thinking") {
        fields(thinking, &["type", "budget_tokens", "display"], "thinking")?;
        if thinking
            .get("display")
            .is_some_and(|v| !matches!(v.as_str(), Some("summarized" | "omitted")))
        {
            return Err(CodexError::bad_request("Unsupported thinking display mode"));
        }
        effort = Some(match string(thinking, "type")? {
            "adaptive" => {
                if thinking.get("budget_tokens").is_some() {
                    return Err(CodexError::bad_request(
                        "adaptive thinking does not accept budget_tokens",
                    ));
                }
                "high"
            }
            "enabled" => {
                let budget = thinking.get("budget_tokens").and_then(Value::as_u64).filter(|v| *v >= 1024)
                    .ok_or_else(|| CodexError::bad_request("enabled thinking requires budget_tokens >= 1024 (mapped to effort, not an exact token budget)"))?;
                if budget < 4096 {
                    "low"
                } else if budget < 16384 {
                    "medium"
                } else {
                    "high"
                }
            }
            "disabled" => {
                if thinking.get("budget_tokens").is_some() {
                    return Err(CodexError::bad_request(
                        "disabled thinking does not accept budget_tokens",
                    ));
                }
                "none"
            }
            _ => return Err(CodexError::bad_request("Unsupported thinking type")),
        });
    }
    if let Some(config) = request.get("output_config") {
        fields(config, &["effort", "format"], "output_config")?;
        if let Some(format) = config.get("format") {
            fields(format, &["type", "schema"], "output_config.format")?;
            if string(format, "type")? != "json_schema" {
                return Err(CodexError::bad_request(
                    "Only json_schema output_config.format is supported",
                ));
            }
            let schema = format
                .get("schema")
                .filter(|schema| schema.is_object())
                .ok_or_else(|| {
                    CodexError::bad_request(
                        "output_config.format.schema must be a JSON Schema object",
                    )
                })?;
            // The subscription Responses endpoint enforces the schema; do not replace it
            // with a prompt hint or silently rewrite constraints for another dialect.
            body["text"] = json!({"format":{
                "type":"json_schema", "name":"anthropic_response", "strict":true, "schema":schema
            }});
        }
        if let Some(value) = config.get("effort") {
            if effort == Some("none") {
                return Err(CodexError::bad_request(
                    "output_config.effort conflicts with disabled thinking",
                ));
            }
            effort = Some(match value.as_str() {
                Some("low") => "low",
                Some("medium") => "medium",
                Some("high") => "high",
                Some("max") => "xhigh",
                _ => return Err(CodexError::bad_request("Unsupported output_config.effort")),
            });
        }
    }
    if let Some(effort) = effort {
        body["reasoning"] = json!({"effort":effort});
    }
    let mut names = HashSet::new();
    let mut tools = Vec::new();
    let mut native_search = false;
    if let Some(values) = request.get("tools") {
        for tool in values
            .as_array()
            .ok_or_else(|| CodexError::bad_request("tools must be an array"))?
        {
            let name = identifier(tool, "name")?;
            if !names.insert(name.to_string()) {
                return Err(CodexError::bad_request("Duplicate tool name"));
            }
            if tool.get("type").and_then(Value::as_str) == Some("web_search_20250305") {
                tools.push(search::map_tool(tool, &mut body)?);
                native_search = true;
                continue;
            }
            fields(
                tool,
                &[
                    "type",
                    "name",
                    "description",
                    "input_schema",
                    "cache_control",
                    "strict",
                ],
                "tool",
            )?;
            cache_hint(tool)?;
            if tool
                .get("type")
                .is_some_and(|v| !matches!(v.as_str(), Some("custom")))
            {
                return Err(CodexError::bad_request(
                    "Only custom client tools and web_search_20250305 are supported",
                ));
            }
            let schema = tool
                .get("input_schema")
                .filter(|v| v.is_object())
                .ok_or_else(|| CodexError::bad_request("Tool input_schema must be an object"))?;
            let mut mapped = json!({"type":"function", "name":name, "parameters":schema, "strict":optional_bool(tool, "strict")?.unwrap_or(false)});
            if tool.get("description").is_some() {
                mapped["description"] = json!(string(tool, "description")?);
            }
            tools.push(mapped);
        }
    }
    body["tools"] = json!(tools);
    if let Some(choice) = request.get("tool_choice") {
        fields(
            choice,
            &["type", "name", "disable_parallel_tool_use"],
            "tool_choice",
        )?;
        let kind = string(choice, "type")?;
        if kind != "tool" && choice.get("name").is_some() {
            return Err(CodexError::bad_request(
                "tool_choice.name requires type=tool",
            ));
        }
        body["tool_choice"] = match kind {
            "auto" | "none" => json!(kind),
            "any" if !names.is_empty() => json!("required"),
            "tool" => {
                let name = identifier(choice, "name")?;
                if !names.contains(name) {
                    return Err(CodexError::bad_request(
                        "tool_choice references an undefined tool",
                    ));
                }
                if native_search && name == "web_search" {
                    json!({"type":"web_search"})
                } else {
                    json!({"type":"function", "name":name})
                }
            }
            _ => {
                return Err(CodexError::bad_request(
                    "Invalid tool_choice or no tools available",
                ))
            }
        };
        if let Some(disable) = optional_bool(choice, "disable_parallel_tool_use")? {
            body["parallel_tool_calls"] = json!(!disable);
        }
    } else {
        body["tool_choice"] = json!("auto");
    }
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| CodexError::bad_request("messages must be a nonempty array"))?;
    if messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) != Some("system"))
        .and_then(|value| value.get("role"))
        .and_then(Value::as_str)
        != Some("user")
    {
        return Err(CodexError::bad_request("Assistant prefill is unsupported; the final conversational message must have role=user"));
    }
    let mut input = Vec::new();
    let mut pending: HashMap<String, String> = HashMap::new();
    let mut seen = HashSet::new();
    let mut reasoning_seen = HashSet::new();
    let mut tool_account: Option<String> = None;
    for message in messages {
        fields(message, &["role", "content"], "message")?;
        let role = string(message, "role")?;
        // Recent Claude Code versions emit system-role blocks in addition to top-level system.
        if !matches!(role, "user" | "assistant" | "system") {
            return Err(CodexError::bad_request(
                "Message role must be user, assistant, or system",
            ));
        }
        if role == "assistant" && !pending.is_empty() {
            return Err(CodexError::bad_request(
                "Every tool_use must receive its tool_result before the next assistant turn",
            ));
        }
        let raw = message
            .get("content")
            .ok_or_else(|| CodexError::bad_request("Message content is required"))?;
        let shorthand;
        let blocks = if let Some(text) = raw.as_str() {
            shorthand = vec![json!({"type":"text", "text":text})];
            &shorthand
        } else {
            raw.as_array().ok_or_else(|| {
                CodexError::bad_request("Message content must be text or an array")
            })?
        };
        if let Some((account, native)) = cache.search.replay(scope, blocks)? {
            if role != "assistant" {
                return Err(CodexError::bad_request(
                    "Gateway search history requires assistant role",
                ));
            }
            if tool_account
                .as_ref()
                .is_some_and(|origin| origin != &account)
            {
                return Err(CodexError::new(
                    StatusCode::CONFLICT,
                    "Tool history belongs to different Codex accounts",
                ));
            }
            tool_account = Some(account);
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("server_tool_use") {
                    if !seen.insert(identifier(block, "id")?.to_string()) {
                        return Err(CodexError::bad_request("Duplicate server_tool_use ID"));
                    }
                }
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let id = identifier(block, "id")?;
                    if !seen.insert(id.to_string()) {
                        return Err(CodexError::bad_request("Duplicate tool_use ID"));
                    }
                    let pin = cache.lookup(scope, id, true)?;
                    if tool_account
                        .as_ref()
                        .is_some_and(|account| account != &pin.account)
                    {
                        return Err(CodexError::new(
                            StatusCode::CONFLICT,
                            "Client tool and search history belong to different Codex accounts",
                        ));
                    }
                    pending.insert(id.to_string(), pin.call_id);
                }
            }
            for item in &native {
                if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                    reasoning_seen.insert(
                        serde_json::to_string(item)
                            .map_err(|_| CodexError::upstream("Invalid cached reasoning"))?,
                    );
                }
            }
            input.extend(native);
            continue;
        }
        let mut content = Vec::new();
        let mut tool_reasoning = Vec::new();
        for block in blocks {
            match string(block, "type")? {
                "text" => content.push(json!({"type":if role == "assistant" { "output_text" } else { "input_text" }, "text":text_block(block)?})),
                "image" if role == "user" => content.push(image(block)?),
                "tool_use" if role == "assistant" => {
                    fields(block, &["type", "id", "name", "input", "cache_control"], "tool_use")?;
                    cache_hint(block)?;
                    let id = identifier(block, "id")?;
                    let name = identifier(block, "name")?;
                    if !seen.insert(id.to_string()) { return Err(CodexError::bad_request("Duplicate tool_use ID")); }
                    let args = block.get("input").filter(|v| v.is_object()).ok_or_else(|| CodexError::bad_request("tool_use.input must be an object"))?;
                    flush_message(&mut input, role, &mut content);
                    let call_id = if id.starts_with(TOOL_PREFIX) {
                        let pin = cache.lookup(scope, id, false)?;
                        if pin.name != name { return Err(CodexError::bad_request("Tool name does not match its gateway-issued ID")); }
                        if tool_account.as_ref().is_some_and(|account| account != &pin.account) {
                            return Err(CodexError::new(StatusCode::CONFLICT, "Tool history belongs to different Codex accounts"));
                        }
                        tool_account = Some(pin.account);
                        for (_, reasoning) in pin.reasoning.range(..pin.output_index) {
                            let key = serde_json::to_string(reasoning).map_err(|_| CodexError::upstream("Invalid cached reasoning"))?;
                            if reasoning_seen.insert(key) { input.push(reasoning.clone()); }
                        }
                        tool_reasoning.push(pin.reasoning);
                        pin.call_id
                    } else { id.to_string() };
                    pending.insert(id.to_string(), call_id.clone());
                    input.push(json!({"type":"function_call", "call_id":call_id, "name":name, "arguments":serde_json::to_string(args).map_err(|_| CodexError::bad_request("Invalid tool input"))?}));
                }
                "tool_result" if role == "user" => {
                    fields(block, &["type", "tool_use_id", "content", "is_error", "cache_control"], "tool_result")?;
                    cache_hint(block)?;
                    let id = identifier(block, "tool_use_id")?;
                    let call_id = pending.remove(id).ok_or_else(|| CodexError::bad_request("tool_result requires a matching, preceding tool_use and may only occur once"))?;
                    flush_message(&mut input, role, &mut content);
                    let result = block.get("content");
                    let mut output = Vec::new();
                    if optional_bool(block, "is_error")?.unwrap_or(false) {
                        output.push(json!({"type":"input_text", "text":"Tool execution failed (is_error=true):"}));
                    }
                    match result {
                        None => output.push(json!({"type":"input_text", "text":""})),
                        Some(Value::String(text)) => output.push(json!({"type":"input_text", "text":text})),
                        Some(Value::Array(blocks)) => for part in blocks {
                            output.push(match string(part, "type")? {
                                "text" => json!({"type":"input_text", "text":text_block(part)?}),
                                "image" => image(part)?,
                                _ => return Err(CodexError::bad_request("Only text and image tool results are supported")),
                            });
                        },
                        _ => return Err(CodexError::bad_request("tool_result.content must be text or text/image blocks")),
                    }
                    input.push(json!({"type":"function_call_output", "call_id":call_id, "output":output}));
                }
                "thinking" | "redacted_thinking" => return Err(CodexError::bad_request("Anthropic thinking/signature replay is unsupported; Codex reasoning is retained privately via gateway tool IDs")),
                _ => return Err(CodexError::bad_request("Unsupported content block or content block role")),
            }
        }
        flush_message(&mut input, role, &mut content);
        // Preserve reasoning interleaved between tool calls, then any trailing reasoning from the
        // assistant turn. Parallel tools share their response state but never replay it twice.
        for reasoning in tool_reasoning {
            for reasoning in reasoning.values() {
                let key = serde_json::to_string(reasoning)
                    .map_err(|_| CodexError::upstream("Invalid cached reasoning"))?;
                if reasoning_seen.insert(key) {
                    input.push(reasoning.clone());
                }
            }
        }
    }
    if !pending.is_empty() {
        return Err(CodexError::bad_request(
            "Every tool_use requires a matching tool_result before requesting the next response",
        ));
    }
    body["input"] = json!(input);
    Ok(MappedRequest {
        body,
        options: ResponseOptions { model, stream },
        tool_account,
    })
}

fn error_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        503 | 529 => "overloaded_error",
        400..=499 => "invalid_request_error",
        _ => "api_error",
    }
}
fn error_value(error: &CodexError) -> Value {
    json!({"type":"error", "error":{"type":error_type(error.status), "message":error.message}})
}
pub(super) fn error_response(error: CodexError) -> Response {
    relay::json_response(error.status, error.headers(), error_value(&error))
}
pub(super) fn upstream_error(status: StatusCode, value: &Value) -> Value {
    let message = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
        .or_else(|| value.get("message").and_then(Value::as_str))
        .or_else(|| value.get("detail").and_then(Value::as_str))
        .unwrap_or("Codex upstream request failed");
    error_value(&CodexError::new(status, message))
}
fn compatibility_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-codex-compatibility",
        HeaderValue::from_static(COMPATIBILITY),
    );
}
pub(super) async fn count_tokens(body: Result<Json<Value>, JsonRejection>) -> Response {
    let error = match parse_json(body) {
        Ok(_) => CodexError::new(StatusCode::NOT_IMPLEMENTED,
            "Exact Messages token counting is unavailable for Codex subscription models; no estimated count is returned"),
        Err(error) => error,
    };
    let mut response = error_response(error);
    compatibility_headers(&mut response);
    response
}
pub(super) async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let mut response = messages_inner(state.codex, headers, body).await;
    compatibility_headers(&mut response);
    response
}
async fn messages_inner(
    manager: Arc<CodexManager>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let request = match parse_json(body) {
        Ok(value) => value,
        Err(error) => return error_response(error),
    };
    if headers.contains_key("x-codex-turn-state") {
        return error_response(CodexError::bad_request(
            "Native Codex turn-state headers cannot be replayed through Messages",
        ));
    }
    let scope = relay::scope(&headers);
    let mapped = {
        let mut sessions = manager.sessions.lock().await;
        map_request(request, &scope, &mut sessions.anthropic_tools)
    };
    let mut mapped = match mapped {
        Ok(mapped) => mapped,
        Err(error) => return error_response(error),
    };
    let id = match relay::select_messages_account(
        &manager,
        &headers,
        &mapped.body,
        &scope,
        mapped.tool_account,
    )
    .await
    {
        Ok(id) => id,
        Err(error) => return error_response(error),
    };
    match relay::forward(
        manager,
        id,
        headers,
        &mut mapped.body,
        scope,
        false,
        Some(mapped.options),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error_response(error),
    }
}

fn upstream_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, CodexError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| CodexError::upstream(format!("Codex response is missing {key}")))
}
fn usage(response: &Value, provisional: bool) -> Result<Value, CodexError> {
    let value = &response["usage"];
    if provisional && value.is_null() {
        return Ok(
            json!({"input_tokens":0, "output_tokens":0, "cache_creation_input_tokens":0, "cache_read_input_tokens":0}),
        );
    }
    let input = value
        .get("input_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| CodexError::upstream("Codex response is missing input token usage"))?;
    let output = value
        .get("output_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| CodexError::upstream("Codex response is missing output token usage"))?;
    let cached = value
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if cached > input {
        return Err(CodexError::upstream(
            "Codex cached token usage exceeds input tokens",
        ));
    }
    // Responses input_tokens INCLUDES cache reads; Anthropic input_tokens EXCLUDES them.
    // Output tokens already include reasoning; never add reasoning_tokens again.
    let mut mapped = json!({"input_tokens":input-cached, "output_tokens":output, "cache_creation_input_tokens":0, "cache_read_input_tokens":cached});
    if let Some(count) = response.pointer("/tool_usage/web_search/num_requests") {
        let count = count
            .as_u64()
            .ok_or_else(|| CodexError::upstream("Invalid native web search usage"))?;
        mapped["server_tool_use"] = json!({"web_search_requests":count});
    } else if response
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item["type"] == "web_search_call"))
        && !provisional
    {
        return Err(CodexError::upstream(
            "Native web search response is missing actual search request usage",
        ));
    }
    Ok(mapped)
}
fn stop_reason(response: &Value, tools: bool) -> Result<&'static str, CodexError> {
    match response.get("status").and_then(Value::as_str) {
        Some("completed") => Ok(if tools { "tool_use" } else { "end_turn" }),
        Some("incomplete")
            if response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
                == Some("max_output_tokens") =>
        {
            Ok("max_tokens")
        }
        Some("failed") => Err(CodexError::upstream(
            response
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Codex response failed"),
        )),
        Some("incomplete") => Err(CodexError::upstream(
            "Codex response was incomplete for a reason other than the output limit",
        )),
        _ => Err(CodexError::upstream(
            "Codex response has no successful terminal status",
        )),
    }
}
fn message(
    response: &Value,
    model: &str,
    content: Vec<Value>,
    stop: Option<&str>,
    provisional: bool,
) -> Result<Value, CodexError> {
    Ok(json!({
        "id":upstream_string(response, "id")?, "type":"message", "role":"assistant", "model":model,
        "content":content, "stop_reason":stop, "stop_sequence":null, "usage":usage(response, provisional)?
    }))
}

struct OutputTool {
    id: String,
    call_id: String,
    name: String,
    output_index: u64,
}
fn output_tool(item: &Value, output_index: u64) -> Result<OutputTool, CodexError> {
    let call_id = upstream_string(item, "call_id")?;
    let name = upstream_string(item, "name")?;
    if call_id.is_empty() || name.is_empty() || call_id.len() > 512 || name.len() > 512 {
        return Err(CodexError::upstream("Invalid Codex tool call identity"));
    }
    Ok(OutputTool {
        id: format!("{TOOL_PREFIX}{}", uuid::Uuid::new_v4().simple()),
        call_id: call_id.to_string(),
        name: name.to_string(),
        output_index,
    })
}
fn tool_input(arguments: &str) -> Result<Value, CodexError> {
    let input: Value = serde_json::from_str(arguments)
        .map_err(|_| CodexError::upstream("Codex returned invalid tool argument JSON"))?;
    if !input.is_object() {
        return Err(CodexError::upstream(
            "Codex tool arguments must be a JSON object",
        ));
    }
    Ok(input)
}
fn output_text(part: &Value) -> Result<&str, CodexError> {
    match upstream_string(part, "type")? {
        "output_text" => upstream_string(part, "text"),
        "refusal" => upstream_string(part, "refusal"),
        _ => Err(CodexError::upstream(
            "Unsupported Codex output content type",
        )),
    }
}

async fn convert_response(
    response: Value,
    manager: &CodexManager,
    scope: &[u8; 32],
    account: &str,
    model: &str,
) -> Result<Value, CodexError> {
    // Check failure before looking at output, so upstream errors retain their meaningful message.
    stop_reason(&response, false)?;
    let items = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| CodexError::upstream("Codex response has no output array"))?;
    let mut content = Vec::new();
    let mut tools = Vec::new();
    let mut reasoning = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        match upstream_string(item, "type")? {
            "reasoning" => {
                reasoning.insert(index as u64, item.clone());
            }
            "message" => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(|| CodexError::upstream("Codex message has no content array"))?
                {
                    let mut block = json!({"type":"text", "text":output_text(part)?});
                    let citations = search::annotations(part)?
                        .iter()
                        .map(search::citation)
                        .collect::<Result<Vec<_>, _>>()?;
                    if !citations.is_empty() {
                        block["citations"] = json!(citations);
                    }
                    content.push(block);
                }
            }
            "web_search_call" => {
                let id = search::server_id();
                let (tool, result) = search::result(item, &id)?;
                content.push(tool);
                content.push(result);
            }
            "function_call" => {
                let tool = output_tool(item, index as u64)?;
                let input = tool_input(upstream_string(item, "arguments")?)?;
                content.push(
                    json!({"type":"tool_use", "id":tool.id, "name":tool.name, "input":input}),
                );
                tools.push(tool);
            }
            _ => return Err(CodexError::upstream("Unsupported Codex output item")),
        }
    }
    let stop = stop_reason(&response, !tools.is_empty())?;
    let message = message(&response, model, content.clone(), Some(stop), false)?;
    manager
        .sessions
        .lock()
        .await
        .anthropic_tools
        .commit_output(scope, account, &tools, reasoning, content, items.clone())?;
    Ok(message)
}

struct Block {
    index: usize,
    emitted: usize,
    closed: bool,
    tool: Option<usize>,
    arguments: String,
    complete: bool,
    citations: BTreeMap<u64, (Value, Value, bool)>,
}
struct StreamMapper {
    model: String,
    started: bool,
    terminal: bool,
    active: Option<(u64, u64)>,
    blocks: BTreeMap<(u64, u64), Block>,
    tools: Vec<OutputTool>,
    reasoning: BTreeMap<u64, Value>,
    retained_bytes: usize,
    next_index: usize,
    search: search::StreamState,
    native: BTreeMap<u64, Value>,
}
impl StreamMapper {
    fn new(model: String) -> Self {
        Self {
            model,
            started: false,
            terminal: false,
            active: None,
            blocks: BTreeMap::new(),
            tools: Vec::new(),
            reasoning: BTreeMap::new(),
            retained_bytes: 0,
            next_index: 0,
            search: search::StreamState::default(),
            native: BTreeMap::new(),
        }
    }
    fn start(&mut self, response: &Value, events: &mut Vec<Value>) -> Result<(), CodexError> {
        if !self.started {
            events.push(json!({"type":"message_start", "message":message(response, &self.model, Vec::new(), None, true)?}));
            self.started = true;
        }
        Ok(())
    }
    fn open(
        &mut self,
        key: (u64, u64),
        item: Option<&Value>,
        events: &mut Vec<Value>,
    ) -> Result<(), CodexError> {
        if !self.started {
            return Err(CodexError::upstream(
                "Codex emitted content before response.created",
            ));
        }
        if let Some(block) = self.blocks.get(&key) {
            if block.tool.is_some() != item.is_some() {
                return Err(CodexError::upstream("Codex changed an output block type"));
            }
            return Ok(());
        }
        self.prepare_block(events)?;
        let index = self.allocate_index()?;
        let tool = if let Some(item) = item {
            let tool = output_tool(item, key.0)?;
            let index = self.tools.len();
            self.tools.push(tool);
            Some(index)
        } else {
            None
        };
        let content = match tool {
            Some(index) => {
                json!({"type":"tool_use", "id":self.tools[index].id, "name":self.tools[index].name, "input":{}})
            }
            None => json!({"type":"text", "text":""}),
        };
        events.push(json!({"type":"content_block_start", "index":index, "content_block":content}));
        self.blocks.insert(
            key,
            Block {
                index,
                emitted: 0,
                closed: false,
                tool,
                arguments: String::new(),
                complete: false,
                citations: BTreeMap::new(),
            },
        );
        self.active = Some(key);
        Ok(())
    }
    fn delta(
        &mut self,
        key: (u64, u64),
        text: &str,
        tool: bool,
        events: &mut Vec<Value>,
    ) -> Result<(), CodexError> {
        let block = self
            .blocks
            .get_mut(&key)
            .ok_or_else(|| CodexError::upstream("Codex delta has no content block"))?;
        if block.closed || block.tool.is_some() != tool {
            return Err(CodexError::upstream(
                "Codex delta targets a closed or incompatible content block",
            ));
        }
        if text.is_empty() {
            return Ok(());
        }
        if self.retained_bytes.saturating_add(text.len()) > relay::MAX_COLLECTED {
            return Err(CodexError::upstream(
                "Codex replay state exceeds the gateway limit",
            ));
        }
        self.retained_bytes += text.len();
        block.arguments.push_str(text);
        block.emitted = block.emitted.saturating_add(text.len());
        let delta = if tool {
            json!({"type":"input_json_delta", "partial_json":text})
        } else {
            json!({"type":"text_delta", "text":text})
        };
        events.push(json!({"type":"content_block_delta", "index":block.index, "delta":delta}));
        Ok(())
    }
    fn complete(
        &mut self,
        key: (u64, u64),
        text: &str,
        tool: bool,
        events: &mut Vec<Value>,
    ) -> Result<(), CodexError> {
        let block = self
            .blocks
            .get(&key)
            .ok_or_else(|| CodexError::upstream("Codex completion has no content block"))?;
        if !text.starts_with(&block.arguments) {
            return Err(CodexError::upstream(
                "Codex final output conflicts with streamed content",
            ));
        }
        let suffix = text.get(block.emitted..).ok_or_else(|| {
            CodexError::upstream("Codex final output conflicts with streamed content")
        })?;
        if block.closed {
            if !suffix.is_empty() {
                return Err(CodexError::upstream(
                    "Codex added output after closing a content block",
                ));
            }
            return Ok(());
        }
        self.delta(key, suffix, tool, events)?;
        self.blocks.get_mut(&key).unwrap().complete = true;
        if tool {
            self.close(key, events)?;
        }
        Ok(())
    }
    fn close(&mut self, key: (u64, u64), events: &mut Vec<Value>) -> Result<(), CodexError> {
        let block = self
            .blocks
            .get_mut(&key)
            .ok_or_else(|| CodexError::upstream("Codex closed an unknown block"))?;
        if !block.closed {
            if block.tool.is_some() {
                tool_input(&block.arguments)?;
            }
            block.closed = true;
            events.push(json!({"type":"content_block_stop", "index":block.index}));
            self.active = None;
        }
        Ok(())
    }
    fn item_done(
        &mut self,
        index: u64,
        item: &Value,
        events: &mut Vec<Value>,
    ) -> Result<(), CodexError> {
        self.remember_item(index, item)?;
        match upstream_string(item, "type")? {
            "reasoning" => {
                if let Some(previous) = self.reasoning.get(&index) {
                    if previous != item {
                        return Err(CodexError::upstream(
                            "Codex changed a completed reasoning item",
                        ));
                    }
                } else {
                    let bytes = serde_json::to_vec(item)
                        .map_err(|_| CodexError::upstream("Invalid Codex reasoning item"))?
                        .len();
                    if self.retained_bytes.saturating_add(bytes) > relay::MAX_COLLECTED {
                        return Err(CodexError::upstream(
                            "Codex reasoning state exceeds the gateway limit",
                        ));
                    }
                    self.retained_bytes += bytes;
                    self.reasoning.insert(index, item.clone());
                }
            }
            "message" => {
                for (content_index, part) in item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(|| CodexError::upstream("Codex message has no content"))?
                    .iter()
                    .enumerate()
                {
                    let key = (index, content_index as u64);
                    self.open(key, None, events)?;
                    self.complete(key, output_text(part)?, false, events)?;
                    self.part_annotations(key, part, events)?;
                }
            }
            "web_search_call" => self.search_done(index, item, false, events)?,
            "function_call" => {
                let key = (index, 0);
                self.open(key, Some(item), events)?;
                let tool = &self.tools[self.blocks[&key]
                    .tool
                    .ok_or_else(|| CodexError::upstream("Codex tool block changed type"))?];
                if tool.call_id != upstream_string(item, "call_id")?
                    || tool.name != upstream_string(item, "name")?
                {
                    return Err(CodexError::upstream(
                        "Codex tool identity changed during streaming",
                    ));
                }
                self.complete(key, upstream_string(item, "arguments")?, true, events)?;
            }
            _ => return Err(CodexError::upstream("Unsupported Codex output item")),
        }
        Ok(())
    }
    fn event(&mut self, event: &Value) -> Result<Vec<Value>, CodexError> {
        if self.terminal {
            return Ok(Vec::new());
        }
        let mut events = Vec::new();
        let kind = upstream_string(event, "type")?;
        let output_index = || {
            event
                .get("output_index")
                .and_then(Value::as_u64)
                .ok_or_else(|| CodexError::upstream("Codex event has no output_index"))
        };
        let content_index = || {
            event
                .get("content_index")
                .and_then(Value::as_u64)
                .ok_or_else(|| CodexError::upstream("Codex event has no content_index"))
        };
        match kind {
            "response.created" | "response.in_progress" => {
                self.start(&event["response"], &mut events)?
            }
            "response.output_item.added" => {
                let item = &event["item"];
                match upstream_string(item, "type")? {
                    "function_call" => self.open((output_index()?, 0), Some(item), &mut events)?,
                    "web_search_call" => self.search_open(output_index()?, item, &mut events)?,
                    "message" | "reasoning" => {}
                    _ => {
                        return Err(CodexError::upstream(
                            "Unsupported Codex streaming output item",
                        ))
                    }
                }
            }
            "response.content_part.added" => {
                let part = &event["part"];
                output_text(part)?;
                let key = (output_index()?, content_index()?);
                self.open(key, None, &mut events)?;
                self.delta(key, output_text(part)?, false, &mut events)?;
                self.part_annotations(key, part, &mut events)?;
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let key = (output_index()?, content_index()?);
                self.open(key, None, &mut events)?;
                self.delta(key, upstream_string(event, "delta")?, false, &mut events)?;
            }
            "response.output_text.annotation.added" => {
                let key = (output_index()?, content_index()?);
                self.open(key, None, &mut events)?;
                let annotation_index = event
                    .get("annotation_index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        CodexError::upstream("Native citation event has no annotation_index")
                    })?;
                self.annotation(key, annotation_index, &event["annotation"], &mut events)?;
            }
            "response.web_search_call.in_progress"
            | "response.web_search_call.searching"
            | "response.web_search_call.completed" => {
                if !self.search.calls.contains_key(&output_index()?) {
                    return Err(CodexError::upstream(
                        "Native search progress has no search call",
                    ));
                }
            }
            "response.function_call_arguments.delta" => {
                self.delta(
                    (output_index()?, 0),
                    upstream_string(event, "delta")?,
                    true,
                    &mut events,
                )?;
            }
            "response.output_text.done" | "response.refusal.done" => {
                let key = (output_index()?, content_index()?);
                self.open(key, None, &mut events)?;
                let field = if kind == "response.refusal.done" {
                    "refusal"
                } else {
                    "text"
                };
                self.complete(key, upstream_string(event, field)?, false, &mut events)?;
            }
            "response.content_part.done" => {
                let key = (output_index()?, content_index()?);
                self.open(key, None, &mut events)?;
                self.complete(key, output_text(&event["part"])?, false, &mut events)?;
                self.part_annotations(key, &event["part"], &mut events)?;
            }
            "response.function_call_arguments.done" => {
                self.complete(
                    (output_index()?, 0),
                    upstream_string(event, "arguments")?,
                    true,
                    &mut events,
                )?;
            }
            "response.output_item.done" => {
                self.item_done(output_index()?, &event["item"], &mut events)?
            }
            "response.completed" | "response.incomplete" => {
                let response = &event["response"];
                stop_reason(response, false)?;
                self.start(response, &mut events)?;
                if let Some(items) = response.get("output").and_then(Value::as_array) {
                    for (index, item) in items.iter().enumerate() {
                        self.item_done(index as u64, item, &mut events)?;
                    }
                }
                self.finish_search(&mut events)?;
                if let Some(key) = self.active {
                    if !self.blocks.get(&key).is_some_and(|block| block.complete) {
                        return Err(CodexError::upstream(
                            "Codex terminal response left unfinished content",
                        ));
                    }
                    self.close(key, &mut events)?;
                }
                if !self.search.calls.is_empty()
                    && response
                        .pointer("/tool_usage/web_search/num_requests")
                        .and_then(Value::as_u64)
                        .is_none()
                {
                    return Err(CodexError::upstream(
                        "Native web search response is missing actual search request usage",
                    ));
                }
                let stop = stop_reason(response, !self.tools.is_empty())?;
                events.push(json!({"type":"message_delta", "delta":{"stop_reason":stop, "stop_sequence":null}, "usage":usage(response, false)?}));
                events.push(json!({"type":"message_stop"}));
                self.terminal = true;
            }
            "response.failed" => {
                stop_reason(&event["response"], false)?;
                return Err(CodexError::upstream("Codex response failed"));
            }
            "error" => {
                return Err(CodexError::upstream(
                    event
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| event.pointer("/error/message").and_then(Value::as_str))
                        .unwrap_or("Codex upstream stream failed"),
                ))
            }
            // Codex's private reasoning is not Anthropic signed thinking. The complete reasoning
            // item is retained at item.done/terminal, never emitted or accepted from the caller.
            kind if kind.starts_with("response.reasoning") => {}
            "response.queued" | "response.metadata" | "codex.response.metadata" | "ping" => {}
            _ => return Err(CodexError::upstream("Unsupported Codex stream event")),
        }
        Ok(events)
    }
    fn eof(&self) -> Result<(), CodexError> {
        if self.terminal {
            Ok(())
        } else {
            Err(CodexError::upstream(
                "Codex upstream stream ended without a terminal response",
            ))
        }
    }
}
fn sse(event: &Value) -> Bytes {
    Bytes::from(format!(
        "event: {}\ndata: {event}\n\n",
        event["type"].as_str().unwrap_or("error")
    ))
}

pub(super) async fn respond(
    upstream: reqwest::Response,
    manager: Arc<CodexManager>,
    scope: [u8; 32],
    account: String,
    record: Record,
    mut headers: HeaderMap,
    options: ResponseOptions,
) -> Result<Response, CodexError> {
    let status = upstream.status();
    let is_sse = relay::is_sse_response(&upstream);
    if !options.stream || !is_sse {
        let (status, mut value) = if is_sse {
            relay::collect_response(upstream, &manager, &scope, &account, true).await?
        } else {
            let value =
                serde_json::from_slice(&auth::read_bounded(upstream, relay::MAX_COLLECTED).await?)
                    .map_err(|_| CodexError::upstream("Codex returned invalid JSON"))?;
            (status, value)
        };
        record.tokens.redact(&mut value);
        if !status.is_success() {
            return Ok(relay::json_response(
                status,
                headers,
                upstream_error(status, &value),
            ));
        }
        relay::remember_response(&manager, &scope, &account, &value).await?;
        if !options.stream {
            let mapped =
                convert_response(value, &manager, &scope, &account, &options.model).await?;
            return Ok(relay::json_response(status, headers, mapped));
        }
        // An actual unary upstream reply has no incremental data. Do not buffer an SSE upstream
        // to simulate a stream: this fallback is only for an upstream JSON content type.
        let mut mapper = StreamMapper::new(options.model);
        let events = mapper.event(&json!({"type":"response.completed", "response":value}))?;
        let wire = mapper.replay_wire()?;
        manager
            .sessions
            .lock()
            .await
            .anthropic_tools
            .commit_output(
                &scope,
                &account,
                &mapper.tools,
                mapper.reasoning,
                wire,
                mapper.native.into_values().collect(),
            )?;
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        let stream = futures::stream::iter(
            events
                .into_iter()
                .map(|event| Ok::<_, std::io::Error>(sse(&event))),
        );
        let mut response = Response::new(Body::from_stream(stream));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        return Ok(response);
    }
    let stream = async_stream::stream! {
        let mut source = upstream.bytes_stream();
        let mut parser = relay::SseParser::strict();
        let mut mapper = StreamMapper::new(options.model);
        'upstream: loop {
            let chunk = match tokio::time::timeout(relay::STREAM_IDLE, source.next()).await {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(None) => {
                    if let Err(error) = mapper.eof() { yield Ok::<Bytes, std::io::Error>(sse(&error_value(&error))); }
                    break;
                }
                Ok(Some(Err(_))) => {
                    yield Ok(sse(&error_value(&CodexError::upstream("Codex upstream stream interrupted")))); break;
                }
                Err(_) => {
                    yield Ok(sse(&error_value(&CodexError::upstream("Codex upstream stream timed out")))); break;
                }
            };
            let events = match parser.push(&chunk) {
                Ok(events) => events,
                Err(error) => { yield Ok(sse(&error_value(&error))); break; }
            };
            for mut event in events {
                record.tokens.redact(&mut event);
                let result = async {
                    if let Some(response) = event.get("response") { relay::remember_response(&manager, &scope, &account, response).await?; }
                    let events = mapper.event(&event)?;
                    if mapper.terminal {
                        let reasoning = std::mem::take(&mut mapper.reasoning);
                        let wire = mapper.replay_wire()?;
                        let native = std::mem::take(&mut mapper.native).into_values().collect();
                        manager.sessions.lock().await.anthropic_tools.commit_output(&scope, &account, &mapper.tools, reasoning, wire, native)?;
                    }
                    Ok::<_, CodexError>(events)
                }.await;
                match result {
                    Ok(events) => for event in events { yield Ok(sse(&event)); },
                    Err(error) => { yield Ok(sse(&error_value(&error))); break 'upstream; }
                }
                if mapper.terminal { break 'upstream; }
            }
        }
        // Pull-based Body polling preserves backpressure; dropping this generator drops upstream.
    };
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::super::relay::tests::{Fixture, Reply};
    use super::*;

    #[tokio::test]
    async fn messages_quota_failover_preserves_final_metadata_in_unary_and_streaming_modes() {
        for stream in [false, true] {
            let fixture = Fixture::new(vec![
                Reply::quota(super::super::now() + 120),
                Reply::ok("resp-messages"),
                Reply::ok("resp-messages-next"),
            ])
            .await;
            let mut headers = HeaderMap::new();
            headers.insert(
                "session_id",
                HeaderValue::from_static("old-messages-session"),
            );
            let scope = relay::scope(&headers);
            let mut body = request(json!([{"role":"user","content":"Hello"}]));
            body["stream"] = json!(stream);
            // Simulate an existing ordinary conversation, rather than only a new unpinned request.
            let mapped = map_request(body.clone(), &scope, &mut ToolCache::default()).unwrap();
            let old = relay::select_messages_account(
                &fixture.manager,
                &headers,
                &mapped.body,
                &scope,
                None,
            )
            .await
            .unwrap();
            assert_eq!(old.id, fixture.first);
            let response = messages_inner(
                fixture.manager.clone(),
                headers.clone(),
                Ok(Json(body.clone())),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["x-account-email"],
                fixture.second.as_str()
            );
            let bytes = axum::body::to_bytes(response.into_body(), relay::MAX_COLLECTED)
                .await
                .unwrap();
            if stream {
                let text = std::str::from_utf8(&bytes).unwrap();
                assert!(text.contains("\"text\":\"recovered\""));
                assert!(text.contains("message_stop"));
            } else {
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    value["content"],
                    json!([{"type":"text","text":"recovered"}])
                );
                assert_eq!(value["stop_reason"], "end_turn");
            }
            let next = messages_inner(fixture.manager.clone(), headers, Ok(Json(body))).await;
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
        }
    }

    #[tokio::test]
    async fn messages_gateway_tool_handles_never_fail_over_even_without_encrypted_reasoning() {
        for reasoning in [false, true] {
            let fixture = Fixture::new(vec![Reply::quota(super::super::now() + 120)]).await;
            let scope = relay::scope(&HeaderMap::new());
            let mut output = Vec::new();
            if reasoning {
                output.push(json!({"type":"reasoning","encrypted_content":"private"}));
            }
            output.push(json!({"type":"function_call","call_id":"original-call","name":"read_file","arguments":"{}"}));
            let response = convert_response(
                terminal(json!(output)),
                &fixture.manager,
                &scope,
                &fixture.first,
                "native",
            )
            .await
            .unwrap();
            let id = response["content"][0]["id"].as_str().unwrap();
            let history = tool_history(id);
            let response = messages_inner(
                fixture.manager.clone(),
                HeaderMap::new(),
                Ok(Json(history.clone())),
            )
            .await;
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(
                response.headers()["x-account-email"],
                fixture.first.as_str()
            );
            assert!(response.headers().contains_key(header::RETRY_AFTER));
            let value: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), relay::MAX_COLLECTED)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(value["error"]["type"], "rate_limit_error");
            let again = messages_inner(
                fixture.manager.clone(),
                HeaderMap::new(),
                Ok(Json(history.clone())),
            )
            .await;
            assert_eq!(again.status(), StatusCode::TOO_MANY_REQUESTS);
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
            let mapped = {
                let mut sessions = fixture.manager.sessions.lock().await;
                map_request(history, &scope, &mut sessions.anthropic_tools).unwrap()
            };
            assert_eq!(mapped.tool_account.as_deref(), Some(fixture.first.as_str()));
        }
    }

    fn request(messages: Value) -> Value {
        json!({"model":"gpt-5.6-sol", "max_tokens":1024, "messages":messages})
    }
    fn terminal(output: Value) -> Value {
        json!({"id":"resp-test", "status":"completed", "output":output,
            "usage":{"input_tokens":100, "output_tokens":12,
                "input_tokens_details":{"cached_tokens":80}, "output_tokens_details":{"reasoning_tokens":7}}})
    }
    fn tool_history(id: &str) -> Value {
        request(json!([
            {"role":"user", "content":"Read a file"},
            {"role":"assistant", "content":[{"type":"tool_use", "id":id, "name":"read_file", "input":{"path":"a.txt"}}]},
            {"role":"user", "content":[{"type":"tool_result", "tool_use_id":id, "is_error":true, "content":[
                {"type":"text", "text":"Permission denied"}, {"type":"image", "source":{"type":"url", "url":"https://example.com/error.png"}}
            ]}]}
        ]))
    }

    #[test]
    fn tool_history_preserves_pairing_and_error_content_and_rejects_orphans() {
        let mapped = map_request(
            tool_history("portable-call"),
            &[1; 32],
            &mut ToolCache::default(),
        )
        .unwrap();
        assert_eq!(
            mapped.body["input"][1],
            json!({"type":"function_call", "call_id":"portable-call", "name":"read_file", "arguments":"{\"path\":\"a.txt\"}"})
        );
        assert_eq!(
            mapped.body["input"][2],
            json!({"type":"function_call_output", "call_id":"portable-call", "output":[
                {"type":"input_text", "text":"Tool execution failed (is_error=true):"},
                {"type":"input_text", "text":"Permission denied"}, {"type":"input_image", "image_url":"https://example.com/error.png"}
            ]})
        );
        let mut orphan = tool_history("portable-call");
        orphan["messages"].as_array_mut().unwrap().remove(1);
        let error = map_request(orphan, &[1; 32], &mut ToolCache::default())
            .err()
            .unwrap();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        let mut duplicate = tool_history("portable-call");
        let result = duplicate["messages"][2]["content"][0].clone();
        duplicate["messages"][2]["content"]
            .as_array_mut()
            .unwrap()
            .push(result);
        assert_eq!(
            map_request(duplicate, &[1; 32], &mut ToolCache::default())
                .err()
                .unwrap()
                .status,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn gateway_tool_roundtrip_keeps_account_and_reasoning_and_rejects_other_scopes() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let first_tokens = auth::Tokens::from_auth_json(&json!({"tokens":{
            "access_token":"first", "refresh_token":"first-refresh", "id_token":"first-id", "account_id":"first-workspace"
        }})).unwrap();
        let second_tokens = auth::Tokens::from_auth_json(&json!({"tokens":{
            "access_token":"second", "refresh_token":"second-refresh", "id_token":"second-id", "account_id":"second-workspace"
        }})).unwrap();
        let mut inner = manager.inner.lock().await;
        let first = manager
            .upsert_account(&mut inner, first_tokens, None, &json!({}), true)
            .unwrap();
        let second = manager
            .upsert_account(&mut inner, second_tokens, None, &json!({}), true)
            .unwrap();
        inner.accounts.active_account_id = Some(second.id.clone());
        drop(inner);
        let scope = relay::scope(&HeaderMap::new());
        let reasoning =
            json!({"type":"reasoning", "id":"rs-test", "encrypted_content":"opaque", "summary":[]});
        let response = convert_response(terminal(json!([
            reasoning,
            {"type":"function_call", "call_id":"original-call", "name":"read_file", "arguments":"{\"path\":\"a.txt\"}"}
        ])), &manager, &scope, &first.id, "gpt-5.6-sol").await.unwrap();
        assert_eq!(response["stop_reason"], "tool_use");
        assert_eq!(response["content"][0]["type"], "tool_use");
        let id = response["content"][0]["id"].as_str().unwrap();
        let mapped = {
            let mut sessions = manager.sessions.lock().await;
            let mapped =
                map_request(tool_history(id), &scope, &mut sessions.anthropic_tools).unwrap();
            assert_eq!(
                map_request(tool_history(id), &[99; 32], &mut sessions.anthropic_tools)
                    .err()
                    .unwrap()
                    .status,
                StatusCode::CONFLICT
            );
            mapped
        };
        assert_eq!(mapped.body["input"][1], reasoning);
        assert_eq!(mapped.body["input"][2]["call_id"], "original-call");
        assert_eq!(mapped.body["input"][3]["call_id"], "original-call");
        let selected = relay::select_messages_account(
            &manager,
            &HeaderMap::new(),
            &mapped.body,
            &scope,
            mapped.tool_account.clone(),
        )
        .await
        .unwrap();
        assert_eq!(selected.id, first.id);
        manager
            .inner
            .lock()
            .await
            .accounts
            .accounts
            .iter_mut()
            .find(|record| record.account.id == first.id)
            .unwrap()
            .account
            .enabled = false;
        assert_eq!(
            relay::select_messages_account(
                &manager,
                &HeaderMap::new(),
                &mapped.body,
                &scope,
                mapped.tool_account
            )
            .await
            .unwrap_err()
            .status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let portable = map_request(
            tool_history("external-self-contained"),
            &scope,
            &mut ToolCache::default(),
        )
        .unwrap();
        assert_eq!(
            relay::select_messages_account(
                &manager,
                &HeaderMap::new(),
                &portable.body,
                &scope,
                portable.tool_account
            )
            .await
            .unwrap()
            .id,
            second.id
        );
        let mut sessions = manager.sessions.lock().await;
        sessions
            .anthropic_tools
            .pins
            .get_mut(&relay::session_key(&scope, "anthropic-tool", id))
            .unwrap()
            .touched = Instant::now() - relay::SESSION_TTL;
        assert_eq!(
            map_request(tool_history(id), &scope, &mut sessions.anthropic_tools)
                .err()
                .unwrap()
                .status,
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn fragmented_stream_emits_text_and_tool_json_before_terminal_without_usage_double_counting() {
        let mut mapper = StreamMapper::new("gpt-5.6-sol".to_string());
        let mut parser = relay::SseParser::strict();
        let events = [
            json!({"type":"response.created", "response":{"id":"resp-test", "usage":null}}),
            json!({"type":"response.metadata", "metadata":{"model":"gpt-5.6-sol"}}),
            json!({"type":"codex.response.metadata", "metadata":{"turn_id":"turn-test"}}),
            json!({"type":"response.content_part.added", "output_index":0, "content_index":0, "part":{"type":"output_text", "text":""}}),
            json!({"type":"response.output_text.delta", "output_index":0, "content_index":0, "delta":"你好"}),
            json!({"type":"response.output_text.done", "output_index":0, "content_index":0, "text":"你好"}),
            json!({"type":"response.output_item.added", "output_index":1, "item":{"type":"function_call", "call_id":"call-1", "name":"read_file"}}),
            json!({"type":"response.function_call_arguments.delta", "output_index":1, "delta":"{\"path\":"}),
            json!({"type":"response.function_call_arguments.delta", "output_index":1, "delta":"\"a.txt\"}"}),
            json!({"type":"response.function_call_arguments.done", "output_index":1, "arguments":"{\"path\":\"a.txt\"}"}),
        ];
        let wire: String = events
            .iter()
            .map(|event| format!("data: {event}\r\n\r\n"))
            .collect();
        let mut observed = Vec::new();
        for byte in wire.as_bytes() {
            for event in parser.push(&[*byte]).unwrap() {
                observed.extend(mapper.event(&event).unwrap());
            }
        }
        assert_eq!(observed[0]["type"], "message_start");
        assert_eq!(
            observed
                .iter()
                .filter(|event| event["type"] == "message_stop")
                .count(),
            0
        );
        assert_eq!(
            observed
                .iter()
                .find(|event| event.pointer("/delta/type") == Some(&json!("text_delta")))
                .unwrap()["delta"]["text"],
            "你好"
        );
        let arguments: String = observed
            .iter()
            .filter_map(|event| event.pointer("/delta/partial_json").and_then(Value::as_str))
            .collect();
        assert_eq!(
            serde_json::from_str::<Value>(&arguments).unwrap(),
            json!({"path":"a.txt"})
        );
        let tool_start = observed
            .iter()
            .find(|event| event.pointer("/content_block/type") == Some(&json!("tool_use")))
            .unwrap();
        assert_eq!(tool_start["content_block"]["name"], "read_file");
        assert_eq!(tool_start["index"], 1);
        assert_eq!(
            observed
                .iter()
                .filter(|event| event["type"] == "content_block_stop")
                .map(|event| event["index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        let end = mapper
            .event(&json!({"type":"response.completed", "response":terminal(json!([]))}))
            .unwrap();
        assert_eq!(
            end,
            vec![
                json!({"type":"message_delta", "delta":{"stop_reason":"tool_use", "stop_sequence":null},
                "usage":{"input_tokens":20, "output_tokens":12, "cache_creation_input_tokens":0, "cache_read_input_tokens":80}}),
                json!({"type":"message_stop"})
            ]
        );
        assert!(mapper
            .event(&json!({"type":"error", "message":"after terminal"}))
            .unwrap()
            .is_empty());
        mapper.eof().unwrap();
    }

    #[test]
    fn truncated_and_malformed_streams_never_manufacture_success() {
        let mut mapper = StreamMapper::new("gpt-5.6-sol".to_string());
        mapper
            .event(&json!({"type":"response.created", "response":{"id":"resp-test"}}))
            .unwrap();
        mapper.event(&json!({"type":"response.output_text.delta", "output_index":0, "content_index":0, "delta":"unfinished"})).unwrap();
        assert_eq!(mapper.eof().unwrap_err().status, StatusCode::BAD_GATEWAY);
        let failed = mapper.event(&json!({"type":"response.failed", "response":{"status":"failed", "error":{"message":"failure"}}})).unwrap_err();
        assert_eq!(error_value(&failed)["type"], "error");
        assert!(!mapper.terminal);
        for wire in ["data: not-json\n\n", "data: [DONE]\n\n"] {
            let mut parser = relay::SseParser::strict();
            let event = parser.push(wire.as_bytes()).unwrap().remove(0);
            assert!(mapper.event(&event).is_err());
            assert!(!mapper.terminal);
        }
        let mut malformed_tool = StreamMapper::new("gpt-5.6-sol".to_string());
        malformed_tool
            .event(&json!({"type":"response.created", "response":{"id":"resp-test"}}))
            .unwrap();
        malformed_tool.event(&json!({"type":"response.output_item.added", "output_index":0, "item":{"type":"function_call", "call_id":"c1", "name":"read_file"}})).unwrap();
        let error = malformed_tool.event(&json!({"type":"response.function_call_arguments.done", "output_index":0, "arguments":"{"})).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert!(!malformed_tool.terminal);
    }

    #[test]
    fn incomplete_output_limit_is_distinct_from_upstream_failure() {
        let mut response = terminal(json!([]));
        response["status"] = json!("incomplete");
        response["incomplete_details"] = json!({"reason":"max_output_tokens"});
        assert_eq!(stop_reason(&response, true).unwrap(), "max_tokens");
        response["incomplete_details"]["reason"] = json!("content_filter");
        assert!(stop_reason(&response, false).is_err());
        response["usage"]["input_tokens_details"]["cached_tokens"] = json!(101);
        assert!(usage(&response, false).is_err());
    }

    #[test]
    fn claude_code_retention_extension_maps_but_semantic_edits_and_stop_sequences_reject() {
        let mut value = request(json!([
            {"role":"user", "content":"hello"},
            {"role":"system", "content":[{"type":"text", "text":"remember this", "cache_control":{"type":"ephemeral"}}]}
        ]));
        value["thinking"] = json!({"type":"adaptive", "display":"omitted"});
        value["context_management"] =
            json!({"edits":[{"type":"clear_thinking_20251015", "keep":"all"}]});
        value["output_config"] = json!({"effort":"high"});
        let mapped = map_request(value.clone(), &[0; 32], &mut ToolCache::default()).unwrap();
        assert_eq!(
            mapped.body["input"][1],
            json!({"role":"system", "content":[{"type":"input_text", "text":"remember this"}]})
        );
        assert_eq!(mapped.body["reasoning"], json!({"effort":"high"}));
        value["context_management"]["edits"][0]["keep"] = json!("none");
        assert_eq!(
            map_request(value, &[0; 32], &mut ToolCache::default())
                .err()
                .unwrap()
                .status,
            StatusCode::BAD_REQUEST
        );
        let mut stops = request(json!([{"role":"user", "content":"hello"}]));
        stops["stop_sequences"] = json!(["stop"]);
        let error = map_request(stops, &[0; 32], &mut ToolCache::default())
            .err()
            .unwrap();
        assert_eq!(
            error_value(&error)["error"]["type"],
            "invalid_request_error"
        );
    }

    #[test]
    fn parallel_tool_replay_preserves_interleaved_private_reasoning_order() {
        let mut cache = ToolCache::default();
        let scope = [1; 32];
        let first = OutputTool {
            id: "toolu_codex_first".into(),
            call_id: "call-first".into(),
            name: "read_file".into(),
            output_index: 1,
        };
        let second = OutputTool {
            id: "toolu_codex_second".into(),
            call_id: "call-second".into(),
            name: "read_file".into(),
            output_index: 3,
        };
        let reasoning = BTreeMap::from([
            (
                0,
                json!({"type":"reasoning", "id":"reason-first", "encrypted_content":"first"}),
            ),
            (
                2,
                json!({"type":"reasoning", "id":"reason-second", "encrypted_content":"second"}),
            ),
        ]);
        cache
            .commit(&scope, "account", &[first, second], reasoning)
            .unwrap();
        let value = request(json!([
            {"role":"assistant", "content":[
                {"type":"tool_use", "id":"toolu_codex_first", "name":"read_file", "input":{}},
                {"type":"tool_use", "id":"toolu_codex_second", "name":"read_file", "input":{}}
            ]},
            {"role":"user", "content":[
                {"type":"tool_result", "tool_use_id":"toolu_codex_first", "content":"one"},
                {"type":"tool_result", "tool_use_id":"toolu_codex_second", "content":"two"}
            ]}
        ]));
        let mapped = map_request(value, &scope, &mut cache).unwrap();
        let input = mapped.body["input"].as_array().unwrap();
        assert_eq!(
            input
                .iter()
                .map(|item| item["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "reasoning",
                "function_call",
                "reasoning",
                "function_call",
                "function_call_output",
                "function_call_output"
            ]
        );
        assert_eq!(input[0]["id"], "reason-first");
        assert_eq!(input[2]["id"], "reason-second");
    }
}
