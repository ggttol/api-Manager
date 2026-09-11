//! Native search protocol mapping and immutable, credential-scoped replay.
use super::*;

const SEARCH_PREFIX: &str = "srvtoolu_codex_";
const HANDLE_PREFIX: &str = "gateway_search_";

pub(super) fn map_tool(tool: &Value, body: &mut Value) -> Result<Value, CodexError> {
    fields(
        tool,
        &[
            "type",
            "name",
            "max_uses",
            "user_location",
            "cache_control",
            "allowed_callers",
        ],
        "web search tool",
    )?;
    cache_hint(tool)?;
    if identifier(tool, "name")? != "web_search" {
        return Err(CodexError::bad_request(
            "web_search_20250305 requires name=web_search",
        ));
    }
    if tool
        .get("allowed_callers")
        .is_some_and(|value| value != &json!(["direct"]))
    {
        return Err(CodexError::bad_request(
            "Native web search supports only allowed_callers=[direct]",
        ));
    }
    let mut mapped = json!({"type":"web_search", "search_context_size":"low"});
    if let Some(location) = tool.get("user_location").filter(|value| !value.is_null()) {
        fields(
            location,
            &["type", "city", "region", "country", "timezone"],
            "web search user_location",
        )?;
        if string(location, "type")? != "approximate" {
            return Err(CodexError::bad_request(
                "Web search user_location must be approximate",
            ));
        }
        let mut count = 0;
        for field in ["city", "region", "country", "timezone"] {
            if let Some(value) = location.get(field) {
                let text = value
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| {
                        CodexError::bad_request("Search location hints must be nonempty strings")
                    })?;
                if field == "country"
                    && (text.len() != 2 || !text.bytes().all(|byte| byte.is_ascii_uppercase()))
                {
                    return Err(CodexError::bad_request(
                        "Search country must be a two-letter uppercase country code",
                    ));
                }
                count += 1;
            }
        }
        if count == 0 {
            return Err(CodexError::bad_request(
                "Search location needs a city, region, country, or timezone",
            ));
        }
        mapped["user_location"] = location.clone();
    }
    if let Some(limit) = tool.get("max_uses").filter(|value| !value.is_null()) {
        let limit = limit.as_u64().filter(|limit| *limit > 0).ok_or_else(|| {
            CodexError::bad_request("web_search.max_uses must be a positive integer")
        })?;
        let instructions = body["instructions"].as_str().unwrap_or("");
        body["instructions"] = json!(format!("{instructions}\n\nWeb search budget guidance: use at most {limit} native web search requests in this response. This is an advisory limit, not an enforced cap."));
    }
    body["include"]
        .as_array_mut()
        .ok_or_else(|| CodexError::upstream("Missing native include list"))?
        .push(json!("web_search_call.action.sources"));
    Ok(mapped)
}

pub(super) fn server_id() -> String {
    format!("{SEARCH_PREFIX}{}", uuid::Uuid::new_v4().simple())
}
fn handle() -> String {
    format!("{HANDLE_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

pub(super) fn citation(annotation: &Value) -> Result<Value, CodexError> {
    if upstream_string(annotation, "type")? != "url_citation" {
        return Err(CodexError::upstream("Unsupported native output annotation"));
    }
    let url = upstream_string(annotation, "url")?;
    if url.is_empty() {
        return Err(CodexError::upstream("Native citation has an empty URL"));
    }
    if annotation
        .get("title")
        .is_some_and(|title| !title.is_null() && !title.is_string())
    {
        return Err(CodexError::upstream("Native citation title is invalid"));
    }
    // Native indices identify answer text, NOT source quotations. Never pass that text off as a quote.
    Ok(
        json!({"type":"web_search_result_location", "url":url, "title":annotation.get("title").cloned().unwrap_or(Value::Null), "cited_text":"", "encrypted_index":handle()}),
    )
}

pub(super) fn annotations(part: &Value) -> Result<&[Value], CodexError> {
    match part.get("annotations") {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(values)) => Ok(values),
        _ => Err(CodexError::upstream(
            "Native output annotations must be an array",
        )),
    }
}

pub(super) fn result(item: &Value, id: &str) -> Result<(Value, Value), CodexError> {
    if item.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(CodexError::upstream(
            "Native web search did not complete successfully",
        ));
    }
    let action = item
        .get("action")
        .filter(|action| action.is_object())
        .ok_or_else(|| CodexError::upstream("Native web search has no action"))?;
    let mut input = action.clone();
    input.as_object_mut().unwrap().remove("sources");
    // Keep query/queries and open_page/find action data faithfully; no fabricated search query.
    match upstream_string(action, "type")? {
        "search" | "open_page" | "find" => {}
        _ => return Err(CodexError::upstream("Unsupported native web search action")),
    }
    let mut sources = Vec::new();
    if let Some(values) = action.get("sources") {
        for source in values
            .as_array()
            .ok_or_else(|| CodexError::upstream("Native web search sources must be an array"))?
        {
            if upstream_string(source, "type")? != "url" {
                return Err(CodexError::upstream("Unsupported native web search source"));
            }
            let url = upstream_string(source, "url")?;
            let title = match source.get("title") {
                None | Some(Value::Null) => "",
                Some(Value::String(title)) => title,
                _ => {
                    return Err(CodexError::upstream(
                        "Invalid native web search source title",
                    ))
                }
            };
            sources.push(json!({"type":"web_search_result", "url":url, "title":title, "page_age":null, "encrypted_content":handle()}));
        }
    }
    Ok((
        json!({"type":"server_tool_use", "id":id, "name":"web_search", "input":input, "caller":{"type":"direct"}}),
        json!({"type":"web_search_tool_result", "tool_use_id":id, "content":sources, "caller":{"type":"direct"}}),
    ))
}

struct Turn {
    account: String,
    wire: Vec<Value>,
    native: Vec<Value>,
    issued: Instant,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct ReplayCache {
    handles: HashMap<[u8; 32], Arc<Turn>>,
    bytes: usize,
}
impl ReplayCache {
    pub(super) fn prune(&mut self) {
        self.handles
            .retain(|_, turn| turn.issued.elapsed() < relay::SESSION_TTL);
        // Charge once per handle, conservatively bounding shared state through partial eviction.
        self.bytes = self.handles.values().map(|turn| turn.bytes).sum();
    }
    pub(super) fn commit(
        &mut self,
        scope: &[u8; 32],
        account: &str,
        wire: Vec<Value>,
        native: Vec<Value>,
        available: usize,
        slots: usize,
    ) -> Result<bool, CodexError> {
        let ids = handles(&wire)?;
        if ids.iter().all(|id| id.starts_with(TOOL_PREFIX)) {
            return Ok(false);
        }
        self.prune();
        let bytes = serde_json::to_vec(&(&wire, &native))
            .map_err(|_| CodexError::upstream("Invalid native search state"))?
            .len()
            + account.len();
        let charged = bytes
            .checked_mul(ids.len())
            .ok_or_else(|| CodexError::unavailable("Native search state is too large"))?;
        if ids.len() > slots.saturating_sub(self.handles.len())
            || charged > available.saturating_sub(self.bytes)
        {
            return Err(CodexError::unavailable(
                "Codex search state cache is full; wait for conversations to expire",
            ));
        }
        let turn = Arc::new(Turn {
            account: account.to_string(),
            wire,
            native,
            issued: Instant::now(),
            bytes,
        });
        for id in ids {
            self.handles.insert(
                relay::session_key(scope, "anthropic-search", &id),
                turn.clone(),
            );
        }
        self.bytes += charged;
        Ok(true)
    }
    pub(super) fn retained_bytes(&self) -> usize {
        self.bytes
    }
    pub(super) fn len(&self) -> usize {
        self.handles.len()
    }
    pub(super) fn replay(
        &self,
        scope: &[u8; 32],
        blocks: &[Value],
    ) -> Result<Option<(String, Vec<Value>)>, CodexError> {
        let ids = handles(blocks)?;
        if ids.is_empty() {
            return Ok(None);
        }
        let mut origin: Option<&Arc<Turn>> = None;
        for id in ids {
            let turn = self
                .handles
                .get(&relay::session_key(scope, "anthropic-search", &id))
                .filter(|turn| turn.issued.elapsed() < relay::SESSION_TTL);
            if turn.is_none() && id.starts_with(TOOL_PREFIX) {
                // Ordinary client tools use the legacy cache. A mixed-turn pin
                // remains search-bound there even after this replay state expires.
                continue;
            }
            let turn = turn.ok_or_else(|| CodexError::new(StatusCode::CONFLICT, "Unknown, foreign, expired, or differently scoped gateway search handle; replay the original assistant turn or start a new conversation"))?;
            if origin.is_some_and(|origin| !Arc::ptr_eq(origin, turn)) {
                return Err(CodexError::new(
                    StatusCode::CONFLICT,
                    "Search history mixes different gateway-issued turns",
                ));
            }
            origin = Some(turn);
        }
        let Some(turn) = origin else {
            return Ok(None);
        };
        let mut canonical = blocks.to_vec();
        for block in &mut canonical {
            cache_hint(block)?;
            if let Some(object) = block.as_object_mut() {
                object.remove("cache_control");
            }
            if block.get("type").and_then(Value::as_str) == Some("text")
                && block.get("citations").is_some_and(|value| {
                    value.is_null() || value.as_array().is_some_and(Vec::is_empty)
                })
            {
                block.as_object_mut().unwrap().remove("citations");
            }
        }
        if canonical != turn.wire {
            return Err(CodexError::bad_request(
                "Gateway search history must preserve the complete, unmodified assistant content",
            ));
        }
        Ok(Some((turn.account.clone(), turn.native.clone())))
    }
}

fn handles(blocks: &[Value]) -> Result<Vec<String>, CodexError> {
    let mut ids = HashSet::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("server_tool_use") => {
                ids.insert(identifier(block, "id")?.to_string());
            }
            Some("tool_use") => {
                let id = identifier(block, "id")?;
                if id.starts_with(SEARCH_PREFIX)
                    || id.starts_with(HANDLE_PREFIX)
                    || id.starts_with(TOOL_PREFIX)
                {
                    ids.insert(id.to_string());
                }
            }
            Some("web_search_tool_result") => {
                ids.insert(identifier(block, "tool_use_id")?.to_string());
                if let Some(results) = block.get("content").and_then(Value::as_array) {
                    for result in results {
                        ids.insert(identifier(result, "encrypted_content")?.to_string());
                    }
                }
            }
            Some("text") => {
                if let Some(citations) = block.get("citations").filter(|value| !value.is_null()) {
                    for citation in citations
                        .as_array()
                        .ok_or_else(|| CodexError::bad_request("Text citations must be an array"))?
                    {
                        if string(citation, "type")? != "web_search_result_location" {
                            return Err(CodexError::bad_request(
                                "Only gateway web search citations can be replayed",
                            ));
                        }
                        ids.insert(identifier(citation, "encrypted_index")?.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(ids.into_iter().collect())
}

pub(super) struct SearchCall {
    id: String,
    native_id: String,
    index: usize,
    tool: Option<Value>,
    result: Option<(usize, Value)>,
    action: Option<Value>,
}

#[derive(Default)]
pub(super) struct StreamState {
    pub(super) calls: BTreeMap<u64, SearchCall>,
    late: Vec<Value>,
    extra: BTreeMap<usize, Value>,
}

impl StreamMapper {
    pub(super) fn allocate_index(&mut self) -> Result<usize, CodexError> {
        if self.next_index >= MAX_TOOLS {
            return Err(CodexError::upstream("Too many native output blocks"));
        }
        let index = self.next_index;
        self.next_index += 1;
        Ok(index)
    }

    pub(super) fn prepare_block(&mut self, events: &mut Vec<Value>) -> Result<(), CodexError> {
        if let Some(key) = self.active {
            if !self.blocks.get(&key).is_some_and(|block| block.complete) {
                return Err(CodexError::upstream(
                    "Codex interleaved unfinished content blocks",
                ));
            }
            self.close(key, events)?;
        }
        if self.search.calls.values().any(|call| call.tool.is_none()) {
            return Err(CodexError::upstream(
                "Codex emitted output before finishing a native search call",
            ));
        }
        Ok(())
    }

    pub(super) fn remember_item(&mut self, index: u64, item: &Value) -> Result<(), CodexError> {
        if let Some(previous) = self.native.get(&index) {
            if previous == item {
                return Ok(());
            }
            if previous.get("type") != item.get("type") || previous.get("id") != item.get("id") {
                return Err(CodexError::upstream(
                    "Codex changed an output item identity",
                ));
            }
        }
        let size = serde_json::to_vec(item)
            .map_err(|_| CodexError::upstream("Invalid native output item"))?
            .len();
        let old_size = self
            .native
            .get(&index)
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| CodexError::upstream("Invalid retained output item"))?
            .map_or(0, |value| value.len());
        let retained = self
            .retained_bytes
            .saturating_sub(old_size)
            .saturating_add(size);
        if retained > relay::MAX_COLLECTED {
            return Err(CodexError::upstream(
                "Native replay state exceeds the gateway limit",
            ));
        }
        self.retained_bytes = retained;
        self.native.insert(index, item.clone());
        Ok(())
    }

    pub(super) fn search_open(
        &mut self,
        index: u64,
        item: &Value,
        events: &mut Vec<Value>,
    ) -> Result<(), CodexError> {
        let native_id = upstream_string(item, "id")?;
        if let Some(call) = self.search.calls.get(&index) {
            if call.native_id != native_id {
                return Err(CodexError::upstream("Native search identity changed"));
            }
            return Ok(());
        }
        if !self.started {
            return Err(CodexError::upstream(
                "Native search preceded response.created",
            ));
        }
        self.prepare_block(events)?;
        let block_index = self.allocate_index()?;
        let id = server_id();
        events.push(json!({"type":"content_block_start", "index":block_index, "content_block":{"type":"server_tool_use", "id":id, "name":"web_search", "input":{}, "caller":{"type":"direct"}}}));
        self.search.calls.insert(
            index,
            SearchCall {
                id,
                native_id: native_id.to_string(),
                index: block_index,
                tool: None,
                result: None,
                action: None,
            },
        );
        Ok(())
    }

    pub(super) fn search_done(
        &mut self,
        index: u64,
        item: &Value,
        terminal: bool,
        events: &mut Vec<Value>,
    ) -> Result<(), CodexError> {
        self.search_open(index, item, events)?;
        if let Some(action) = &self.search.calls[&index].action {
            if item.get("status").and_then(Value::as_str) != Some("completed")
                || item.get("action") != Some(action)
            {
                return Err(CodexError::upstream("Native search changed after emission"));
            }
            return Ok(());
        }
        let id = &self.search.calls[&index].id;
        let (tool, result) = result(item, id)?;
        let call = self.search.calls.get_mut(&index).unwrap();
        if let Some(previous) = &call.tool {
            if previous != &tool {
                return Err(CodexError::upstream(
                    "Native search action changed after completion",
                ));
            }
        } else {
            let arguments = serde_json::to_string(&tool["input"])
                .map_err(|_| CodexError::upstream("Invalid native search input"))?;
            events.push(json!({"type":"content_block_delta", "index":call.index, "delta":{"type":"input_json_delta", "partial_json":arguments}}));
            events.push(json!({"type":"content_block_stop", "index":call.index}));
            call.tool = Some(tool);
        }
        // Missing or empty sources can be enriched only in terminal metadata. Never invent results.
        if !terminal
            && item
                .pointer("/action/sources")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        {
            return Ok(());
        }
        self.prepare_block(events)?;
        let result_index = self.allocate_index()?;
        events.push(
            json!({"type":"content_block_start", "index":result_index, "content_block":result}),
        );
        events.push(json!({"type":"content_block_stop", "index":result_index}));
        let call = self.search.calls.get_mut(&index).unwrap();
        call.action = Some(item["action"].clone());
        call.result = Some((result_index, result));
        Ok(())
    }

    pub(super) fn annotation(
        &mut self,
        key: (u64, u64),
        index: u64,
        annotation: &Value,
        events: &mut Vec<Value>,
    ) -> Result<(), CodexError> {
        let block = self
            .blocks
            .get(&key)
            .ok_or_else(|| CodexError::upstream("Native citation has no text block"))?;
        if block.tool.is_some() {
            return Err(CodexError::upstream(
                "Native citation targets a client tool",
            ));
        }
        if let Some((previous, _, _)) = block.citations.get(&index) {
            if previous != annotation {
                return Err(CodexError::upstream(
                    "Native citation changed after emission",
                ));
            }
            return Ok(());
        }
        let mapped = citation(annotation)?;
        let size = serde_json::to_vec(&(annotation, &mapped))
            .map_err(|_| CodexError::upstream("Invalid native citation"))?
            .len();
        if self.retained_bytes.saturating_add(size) > relay::MAX_COLLECTED {
            return Err(CodexError::upstream(
                "Native citation state exceeds gateway limit",
            ));
        }
        self.retained_bytes += size;
        let block = self.blocks.get_mut(&key).unwrap();
        let late = block.closed;
        block
            .citations
            .insert(index, (annotation.clone(), mapped.clone(), late));
        if late {
            // Never reopen a closed block or repeat its text. Emit newly arrived citations in a
            // separate, empty text block at the next terminal boundary, preserving sequential SSE.
            self.search.late.push(mapped);
        } else {
            events.push(json!({"type":"content_block_delta", "index":block.index, "delta":{"type":"citations_delta", "citation":mapped}}));
        }
        Ok(())
    }

    pub(super) fn part_annotations(
        &mut self,
        key: (u64, u64),
        part: &Value,
        events: &mut Vec<Value>,
    ) -> Result<(), CodexError> {
        for (index, annotation) in annotations(part)?.iter().enumerate() {
            self.annotation(key, index as u64, annotation, events)?;
        }
        Ok(())
    }

    pub(super) fn finish_search(&mut self, events: &mut Vec<Value>) -> Result<(), CodexError> {
        let pending: Vec<_> = self
            .search
            .calls
            .iter()
            .filter(|(_, call)| call.result.is_none())
            .map(|(index, _)| *index)
            .collect();
        for index in pending {
            let item = self
                .native
                .get(&index)
                .cloned()
                .ok_or_else(|| CodexError::upstream("Native search never completed"))?;
            self.search_done(index, &item, true, events)?;
        }
        if !self.search.late.is_empty() {
            self.prepare_block(events)?;
            let index = self.allocate_index()?;
            let citations = std::mem::take(&mut self.search.late);
            events.push(json!({"type":"content_block_start", "index":index, "content_block":{"type":"text", "text":""}}));
            for citation in &citations {
                events.push(json!({"type":"content_block_delta", "index":index, "delta":{"type":"citations_delta", "citation":citation}}));
            }
            events.push(json!({"type":"content_block_stop", "index":index}));
            self.search.extra.insert(
                index,
                json!({"type":"text", "text":"", "citations":citations}),
            );
        }
        Ok(())
    }

    pub(super) fn replay_wire(&self) -> Result<Vec<Value>, CodexError> {
        let mut wire = self.search.extra.clone();
        for block in self.blocks.values() {
            let content = if let Some(tool) = block.tool {
                json!({"type":"tool_use", "id":self.tools[tool].id, "name":self.tools[tool].name, "input":tool_input(&block.arguments)?})
            } else {
                let mut text = json!({"type":"text", "text":block.arguments});
                let citations: Vec<_> = block
                    .citations
                    .values()
                    .filter(|(_, _, late)| !late)
                    .map(|(_, citation, _)| citation)
                    .collect();
                if !citations.is_empty() {
                    text["citations"] = json!(citations);
                }
                text
            };
            wire.insert(block.index, content);
        }
        for call in self.search.calls.values() {
            wire.insert(
                call.index,
                call.tool
                    .clone()
                    .ok_or_else(|| CodexError::upstream("Unfinished native search"))?,
            );
            let (index, result) = call
                .result
                .as_ref()
                .ok_or_else(|| CodexError::upstream("Unfinished native search result"))?;
            wire.insert(*index, result.clone());
        }
        Ok(wire.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(content: Value) -> Value {
        json!({"model":"native", "max_tokens":1024, "messages":[{"role":"user", "content":"Search current information"}, {"role":"assistant", "content":content}, {"role":"user", "content":"Explain those sources"}]})
    }
    fn search_request() -> Value {
        json!({"model":"native", "max_tokens":1024, "messages":[{"role":"user", "content":"Search current information"}],
            "tools":[{"type":"web_search_20250305", "name":"web_search", "max_uses":8}],
            "tool_choice":{"type":"auto"}, "output_config":{"effort":"high"}})
    }
    fn native_call(sources: bool) -> Value {
        let mut item = json!({"type":"web_search_call", "id":"ws-real", "status":"completed", "action":{"type":"search", "query":"current information", "queries":["current information"]}});
        if sources {
            item["action"]["sources"] = json!([{"type":"url", "url":"https://example.com/facts"}]);
        }
        item
    }
    fn annotation() -> Value {
        json!({"type":"url_citation", "start_index":0, "end_index":6, "url":"https://example.com/facts?utm_source=openai", "title":"Actual source title"})
    }
    fn native_message(cited: bool) -> Value {
        json!({"type":"message", "id":"msg-native", "role":"assistant", "status":"completed", "content":[{"type":"output_text", "text":"Answer", "annotations":if cited { vec![annotation()] } else { vec![] }}]})
    }
    fn terminal(output: Value) -> Value {
        json!({"id":"response-native", "status":"completed", "output":output, "usage":{"input_tokens":24, "output_tokens":10}, "tool_usage":{"web_search":{"num_requests":3}}})
    }

    #[test]
    fn native_search_rejects_ambiguous_tools_and_unsupported_constraints() {
        let mut request = search_request();
        request["tools"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"web_search", "input_schema":{"type":"object"}}));
        assert_eq!(
            map_request(request, &[0; 32], &mut ToolCache::default())
                .err()
                .unwrap()
                .status,
            StatusCode::BAD_REQUEST
        );
        for (field, value) in [
            ("max_uses", json!(0)),
            ("allowed_domains", json!(["example.com/path"])),
            ("blocked_domains", json!(["example.com"])),
            ("allowed_callers", json!(["code_execution_20260120"])),
        ] {
            let mut request = search_request();
            request["tools"][0][field] = value;
            assert_eq!(
                map_request(request, &[0; 32], &mut ToolCache::default())
                    .err()
                    .unwrap()
                    .status,
                StatusCode::BAD_REQUEST
            );
        }
        for format in [
            json!({"type":"text"}),
            json!({"type":"json_schema", "schema":[]}),
        ] {
            let mut request = search_request();
            request["output_config"]["format"] = format;
            assert_eq!(
                map_request(request, &[0; 32], &mut ToolCache::default())
                    .err()
                    .unwrap()
                    .status,
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[tokio::test]
    async fn native_search_unary_history_is_immutable_scoped_expiring_and_account_pinned() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let scope = [9; 32];
        let output = json!([
            {"type":"reasoning", "id":"rs-private", "encrypted_content":"private-never-exposed", "summary":[]},
            native_call(true), native_message(true)
        ]);
        let response = convert_response(
            terminal(output.clone()),
            &manager,
            &scope,
            "origin-account",
            "native",
        )
        .await
        .unwrap();
        assert_eq!(response["stop_reason"], "end_turn");
        assert_eq!(
            response["usage"]["server_tool_use"]["web_search_requests"],
            3
        );
        let wire = response["content"].clone();
        assert_eq!(wire[0]["type"], "server_tool_use");
        assert_eq!(wire[1]["tool_use_id"], wire[0]["id"]);
        assert_eq!(wire[1]["content"][0]["title"], "");
        assert_eq!(wire[2]["citations"][0]["cited_text"], "");
        assert_eq!(wire[2]["citations"][0]["title"], "Actual source title");
        assert_eq!(wire[2]["citations"][0]["url"], annotation()["url"]);
        assert!(!response.to_string().contains("private-never-exposed"));
        let mut sessions = manager.sessions.lock().await;
        let cache = &mut sessions.anthropic_tools;
        let mapped = map_request(request(wire.clone()), &scope, cache).unwrap();
        assert_eq!(mapped.tool_account.as_deref(), Some("origin-account"));
        assert_eq!(
            &mapped.body["input"].as_array().unwrap()[1..4],
            output.as_array().unwrap().as_slice()
        );
        assert_eq!(
            map_request(request(wire.clone()), &[7; 32], cache)
                .err()
                .unwrap()
                .status,
            StatusCode::CONFLICT
        );
        for pointer in [
            "/0/input/query",
            "/1/content/0/url",
            "/1/content/0/encrypted_content",
            "/2/text",
            "/2/citations/0/title",
            "/2/citations/0/encrypted_index",
        ] {
            let mut modified = wire.clone();
            *modified.pointer_mut(pointer).unwrap() = json!("modified");
            assert!(map_request(request(modified), &scope, cache).is_err());
        }
        let mut partial = wire.clone();
        partial.as_array_mut().unwrap().remove(1);
        assert!(map_request(request(partial), &scope, cache).is_err());
        let disguised =
            json!([{"type":"tool_use", "id":wire[0]["id"], "name":"web_search", "input":{}}]);
        assert!(map_request(request(disguised), &scope, cache).is_err());
        let original = cache.search.handles.values().next().unwrap();
        let expired = Arc::new(Turn {
            account: original.account.clone(),
            wire: original.wire.clone(),
            native: original.native.clone(),
            issued: Instant::now() - relay::SESSION_TTL,
            bytes: original.bytes,
        });
        for turn in cache.search.handles.values_mut() {
            *turn = expired.clone();
        }
        assert_eq!(
            map_request(request(wire), &scope, cache)
                .err()
                .unwrap()
                .status,
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn native_search_mixed_client_tools_cannot_bypass_whole_turn_replay() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let scope = [11; 32];
        let mixed = terminal(json!([native_call(true), native_message(true),
            {"type":"function_call", "call_id":"call-client", "name":"read_file", "arguments":"{\"path\":\"original\"}"}]));
        let response = convert_response(mixed, &manager, &scope, "origin", "native")
            .await
            .unwrap();
        let wire = response["content"].clone();
        let client = wire
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["type"] == "tool_use")
            .unwrap()
            .clone();
        let mut history = request(wire);
        history["messages"][2]["content"] =
            json!([{"type":"tool_result", "tool_use_id":client["id"], "content":"result"}]);
        let mut sessions = manager.sessions.lock().await;
        let cache = &mut sessions.anthropic_tools;
        assert!(map_request(history.clone(), &scope, cache).is_ok());
        history["messages"][1]["content"] = json!([client]);
        history["messages"][1]["content"][0]["input"]["path"] = json!("modified");
        assert!(
            map_request(history.clone(), &scope, cache).is_err(),
            "a mixed-turn client handle must not bypass missing search state"
        );
        // Client tool access can outlive the search turn's absolute expiry. Even after
        // search state is gone, that handle must never become a standalone client tool.
        cache.search.handles.clear();
        cache.search.bytes = 0;
        assert!(map_request(history, &scope, cache).is_err());
    }

    #[tokio::test]
    async fn native_search_failed_admission_does_not_consume_client_tool_capacity() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let scope = [12; 32];
        {
            let mut sessions = manager.sessions.lock().await;
            let turn = Arc::new(Turn {
                account: "existing".into(),
                wire: Vec::new(),
                native: Vec::new(),
                issued: Instant::now(),
                bytes: 0,
            });
            for index in 0..MAX_TOOLS - 1 {
                let mut key = [0; 32];
                key[..8].copy_from_slice(&(index as u64).to_le_bytes());
                sessions
                    .anthropic_tools
                    .search
                    .handles
                    .insert(key, turn.clone());
            }
        }
        let function = json!({"type":"function_call", "call_id":"call-new", "name":"read_file", "arguments":"{}"});
        let rejected = convert_response(
            terminal(json!([native_call(true), function.clone()])),
            &manager,
            &scope,
            "origin",
            "native",
        )
        .await;
        assert_eq!(
            rejected.unwrap_err().status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let available = convert_response(
            terminal(json!([function])),
            &manager,
            &scope,
            "origin",
            "native",
        )
        .await
        .unwrap();
        assert_eq!(available["stop_reason"], "tool_use");
        assert_eq!(available["content"][0]["name"], "read_file");
    }

    fn accumulate(events: &[Value]) -> Vec<Value> {
        let mut blocks = Vec::<Value>::new();
        let mut active = None;
        let mut arguments = String::new();
        for event in events {
            let index = event
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index as usize);
            match event["type"].as_str().unwrap() {
                "content_block_start" => {
                    assert!(active.is_none(), "blocks must remain sequential");
                    assert_eq!(index, Some(blocks.len()), "never reopen an old block");
                    blocks.push(event["content_block"].clone());
                    active = index;
                    arguments.clear();
                }
                "content_block_delta" => {
                    assert_eq!(active, index, "no delta after a block stop");
                    let block = &mut blocks[index.unwrap()];
                    match event["delta"]["type"].as_str().unwrap() {
                        "text_delta" => {
                            let text = block["text"].as_str().unwrap().to_string()
                                + event["delta"]["text"].as_str().unwrap();
                            block["text"] = json!(text);
                        }
                        "input_json_delta" => {
                            arguments.push_str(event["delta"]["partial_json"].as_str().unwrap())
                        }
                        "citations_delta" => {
                            if block.get("citations").is_none() {
                                block["citations"] = json!([]);
                            }
                            block["citations"]
                                .as_array_mut()
                                .unwrap()
                                .push(event["delta"]["citation"].clone());
                        }
                        _ => panic!("unexpected delta"),
                    }
                }
                "content_block_stop" => {
                    assert_eq!(active, index);
                    if !arguments.is_empty() {
                        blocks[index.unwrap()]["input"] = serde_json::from_str(&arguments).unwrap();
                    }
                    active = None;
                }
                _ => {}
            }
        }
        assert!(active.is_none());
        blocks
    }

    #[test]
    fn native_search_stream_emits_incrementally_and_deduplicates_late_annotations() {
        let mut mapper = StreamMapper::new("native".into());
        let mut events = Vec::new();
        for event in [
            json!({"type":"response.created", "response":{"id":"response-native"}}),
            json!({"type":"response.output_item.added", "output_index":0, "item":{"id":"ws-real", "type":"web_search_call"}}),
            json!({"type":"response.web_search_call.searching", "output_index":0}),
            json!({"type":"response.output_item.done", "output_index":0, "item":native_call(true)}),
            json!({"type":"response.output_text.delta", "output_index":1, "content_index":0, "delta":"Ans"}),
            json!({"type":"response.output_text.delta", "output_index":1, "content_index":0, "delta":"wer"}),
            json!({"type":"response.output_text.done", "output_index":1, "content_index":0, "text":"Answer"}),
            json!({"type":"response.output_text.annotation.added", "output_index":1, "content_index":0, "annotation_index":0, "annotation":annotation()}),
            json!({"type":"response.content_part.done", "output_index":1, "content_index":0, "part":native_message(true)["content"][0]}),
            json!({"type":"response.output_item.done", "output_index":1, "item":native_message(true)}),
        ] {
            events.extend(mapper.event(&event).unwrap());
        }
        assert_eq!(
            events
                .iter()
                .filter_map(|event| event.pointer("/delta/text").and_then(Value::as_str))
                .collect::<String>(),
            "Answer"
        );
        assert!(!events.iter().any(|event| event["type"] == "message_stop"));
        events.extend(mapper.event(&json!({"type":"response.completed", "response":terminal(json!([native_call(true), native_message(true)]))})).unwrap());
        let blocks = accumulate(&events);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[2]["citations"].as_array().unwrap().len(), 1);
        assert_eq!(blocks, mapper.replay_wire().unwrap());
        assert_eq!(events[events.len() - 2]["delta"]["stop_reason"], "end_turn");
        assert_eq!(
            events[events.len() - 2]["usage"]["server_tool_use"]["web_search_requests"],
            3
        );
        let mut cache = ToolCache::default();
        cache
            .commit_output(
                &[5; 32],
                "issuer",
                &mapper.tools,
                mapper.reasoning.clone(),
                blocks.clone(),
                mapper.native.values().cloned().collect(),
            )
            .unwrap();
        let mapped = map_request(request(json!(blocks)), &[5; 32], &mut cache).unwrap();
        assert_eq!(mapped.tool_account.as_deref(), Some("issuer"));
        assert_eq!(mapped.body["input"][1], native_call(true));
    }

    #[test]
    fn native_search_terminal_metadata_never_reopens_or_repeats_streamed_text() {
        let mut mapper = StreamMapper::new("native".into());
        let mut events = Vec::new();
        for event in [
            json!({"type":"response.created", "response":{"id":"response-native"}}),
            json!({"type":"response.output_item.done", "output_index":0, "item":native_call(false)}),
            json!({"type":"response.output_item.done", "output_index":1, "item":native_message(false)}),
            json!({"type":"response.output_text.delta", "output_index":2, "content_index":0, "delta":"More"}),
            json!({"type":"response.output_text.done", "output_index":2, "content_index":0, "text":"More"}),
        ] {
            events.extend(mapper.event(&event).unwrap());
        }
        let more = json!({"type":"message", "id":"msg-more", "role":"assistant", "content":[{"type":"output_text", "text":"More", "annotations":[]}]});
        events.extend(mapper.event(&json!({"type":"response.completed", "response":terminal(json!([native_call(true), native_message(true), more]))})).unwrap());
        let blocks = accumulate(&events);
        assert_eq!(
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<String>(),
            "AnswerMore"
        );
        assert_eq!(
            blocks
                .iter()
                .filter(|block| block["type"] == "server_tool_use")
                .count(),
            1
        );
        assert_eq!(
            blocks
                .iter()
                .filter(|block| block["type"] == "web_search_tool_result")
                .count(),
            1
        );
        assert_eq!(
            blocks
                .iter()
                .filter_map(|block| block.get("citations").and_then(Value::as_array))
                .map(Vec::len)
                .sum::<usize>(),
            1
        );
        assert_eq!(blocks, mapper.replay_wire().unwrap());
        let mut empty = StreamMapper::new("native".into());
        let events = empty.event(&json!({"type":"response.completed", "response":terminal(json!([native_call(false), native_message(false)]))})).unwrap();
        let blocks = accumulate(&events);
        assert!(blocks
            .iter()
            .find(|block| block["type"] == "web_search_tool_result")
            .unwrap()["content"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn native_search_history_never_crosses_accounts_or_fails_over_on_quota() {
        use relay::tests::{Fixture, Reply};
        let fixture = Fixture::new(vec![Reply::quota(crate::proxy::codex::now() + 120)]).await;
        let scope = relay::scope(&HeaderMap::new());
        let first = convert_response(
            terminal(json!([native_call(true), native_message(true)])),
            &fixture.manager,
            &scope,
            &fixture.first,
            "native",
        )
        .await
        .unwrap();
        let second = convert_response(
            terminal(json!([native_call(true), native_message(true)])),
            &fixture.manager,
            &scope,
            &fixture.second,
            "native",
        )
        .await
        .unwrap();
        let mut mixed = request(first["content"].clone());
        mixed["messages"].as_array_mut().unwrap().extend([
            json!({"role":"assistant", "content":second["content"]}),
            json!({"role":"user", "content":"Continue"}),
        ]);
        {
            let mut sessions = fixture.manager.sessions.lock().await;
            assert_eq!(
                map_request(mixed, &scope, &mut sessions.anthropic_tools)
                    .err()
                    .unwrap()
                    .status,
                StatusCode::CONFLICT
            );
        }
        let response = messages_inner(
            fixture.manager.clone(),
            HeaderMap::new(),
            Ok(Json(request(first["content"].clone()))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers()["x-account-email"],
            fixture.first.as_str()
        );
        assert!(response.headers().contains_key(header::RETRY_AFTER));
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
    }
}
