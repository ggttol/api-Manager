// OpenAI Stream Collector
// Used for auto-converting streaming responses to JSON for non-streaming requests

use super::models::*;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::Value;
use std::collections::BTreeMap;

/// Collects an OpenAI SSE stream into a complete OpenAIResponse
pub async fn collect_stream_to_json<S, E>(mut stream: S) -> Result<OpenAIResponse, String>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut response = OpenAIResponse {
        id: "chatcmpl-unknown".to_string(),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp() as u64,
        model: "unknown".to_string(),
        choices: Vec::new(),
        usage: None,
    };

    #[derive(Default)]
    struct ChoiceAccumulator {
        role: Option<String>,
        content_parts: Vec<String>,
        reasoning_parts: Vec<String>,
        finish_reason: Option<String>,
        tool_calls: BTreeMap<u32, (String, String, String, Vec<String>)>,
    }

    let mut choices: BTreeMap<u32, ChoiceAccumulator> = BTreeMap::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("Stream error: {}", e))?;
        let text = String::from_utf8_lossy(&chunk);

        for line in text.lines() {
            let line = line.trim();
            if line.starts_with("data: ") {
                let data_str = line.trim_start_matches("data: ").trim();
                if data_str == "[DONE]" {
                    continue;
                }

                let json: Value =
                    serde_json::from_str(data_str).map_err(|e| format!("Invalid SSE JSON: {e}"))?;
                if let Some(error) = json.get("error").filter(|error| !error.is_null()) {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Upstream stream failed");
                    return Err(format!("Upstream stream error: {message}"));
                }

                if let Some(id) = json.get("id").and_then(|v| v.as_str()) {
                    response.id = id.to_string();
                }
                if let Some(model) = json.get("model").and_then(|v| v.as_str()) {
                    response.model = model.to_string();
                }
                if let Some(created) = json.get("created").and_then(|v| v.as_u64()) {
                    response.created = created;
                }
                if let Some(usage) = json.get("usage") {
                    if let Ok(usage) = serde_json::from_value::<OpenAIUsage>(usage.clone()) {
                        response.usage = Some(usage);
                    }
                }

                if let Some(chunk_choices) = json.get("choices").and_then(Value::as_array) {
                    for choice in chunk_choices {
                        let choice_index =
                            choice.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                        let accumulator = choices.entry(choice_index).or_default();
                        if let Some(delta) = choice.get("delta") {
                            if let Some(role) = delta.get("role").and_then(Value::as_str) {
                                accumulator.role = Some(role.to_string());
                            }
                            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                                accumulator.content_parts.push(content.to_string());
                            }
                            if let Some(reasoning) =
                                delta.get("reasoning_content").and_then(Value::as_str)
                            {
                                accumulator.reasoning_parts.push(reasoning.to_string());
                            }
                            if let Some(tool_calls) =
                                delta.get("tool_calls").and_then(Value::as_array)
                            {
                                for tool_call in tool_calls {
                                    let index =
                                        tool_call.get("index").and_then(Value::as_u64).unwrap_or(0)
                                            as u32;
                                    let entry =
                                        accumulator.tool_calls.entry(index).or_insert_with(|| {
                                            (
                                                String::new(),
                                                "function".to_string(),
                                                String::new(),
                                                Vec::new(),
                                            )
                                        });
                                    if let Some(id) = tool_call
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .filter(|id| !id.is_empty())
                                    {
                                        entry.0 = id.to_string();
                                    }
                                    if let Some(kind) = tool_call
                                        .get("type")
                                        .and_then(Value::as_str)
                                        .filter(|kind| !kind.is_empty())
                                    {
                                        entry.1 = kind.to_string();
                                    }
                                    if let Some(function) = tool_call.get("function") {
                                        if let Some(name) = function
                                            .get("name")
                                            .and_then(Value::as_str)
                                            .filter(|name| !name.is_empty())
                                        {
                                            entry.2 = name.to_string();
                                        }
                                        if let Some(arguments) =
                                            function.get("arguments").and_then(Value::as_str)
                                        {
                                            entry.3.push(arguments.to_string());
                                        }
                                    }
                                }
                            }
                        }
                        if choice.get("finish_reason").is_some()
                            && !choice["finish_reason"].is_null()
                        {
                            let reason = choice["finish_reason"]
                                .as_str()
                                .ok_or_else(|| "Invalid non-string finish_reason".to_string())?;
                            accumulator.finish_reason = Some(reason.to_string());
                        }
                    }
                }
            }
        }
    }

    if choices.is_empty()
        || choices
            .values()
            .any(|choice| choice.finish_reason.is_none())
    {
        return Err("Upstream stream ended without a terminal choice".to_string());
    }

    response.choices = choices
        .into_iter()
        .map(|(index, accumulator)| {
            let tool_calls = if accumulator.tool_calls.is_empty() {
                None
            } else {
                Some(
                    accumulator
                        .tool_calls
                        .into_iter()
                        .map(|(_, (id, kind, name, arguments))| ToolCall {
                            id,
                            r#type: kind,
                            function: Some(ToolFunction {
                                name,
                                arguments: arguments.join(""),
                            }),
                            status: None,
                            call_id: None,
                            operation: None,
                        })
                        .collect(),
                )
            };
            Choice {
                index,
                message: OpenAIMessage {
                    role: accumulator.role.unwrap_or_else(|| "assistant".to_string()),
                    content: Some(OpenAIContent::String(accumulator.content_parts.join(""))),
                    reasoning_content: (!accumulator.reasoning_parts.is_empty())
                        .then(|| accumulator.reasoning_parts.join("")),
                    tool_calls,
                    tool_call_id: None,
                    name: None,
                    refusal: None,
                },
                finish_reason: accumulator.finish_reason,
            }
        })
        .collect();

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[tokio::test]
    async fn collector_keeps_candidates_separate_and_consumes_trailing_usage() {
        let frames = [
            r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"A"}},{"index":1,"delta":{"role":"assistant","content":"B"}}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"},{"index":1,"delta":{},"finish_reason":"stop"}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
            "data: [DONE]",
        ];
        let response = collect_stream_to_json(stream::iter(
            frames
                .into_iter()
                .map(|frame| Ok::<_, String>(Bytes::from(format!("{frame}\n\n")))),
        ))
        .await
        .expect("complete stream");

        assert_eq!(response.choices.len(), 2);
        assert!(matches!(
            response.choices[0].message.content.as_ref(),
            Some(OpenAIContent::String(content)) if content == "A"
        ));
        assert!(matches!(
            response.choices[1].message.content.as_ref(),
            Some(OpenAIContent::String(content)) if content == "B"
        ));
        assert_eq!(response.usage.expect("usage").total_tokens, 5);
    }

    #[tokio::test]
    async fn collector_rejects_error_or_unfinished_streams() {
        let error = collect_stream_to_json(stream::iter([Ok::<_, String>(Bytes::from(
            "data: {\"choices\":[],\"error\":{\"message\":\"quota exhausted\"}}\n\n",
        ))]))
        .await
        .expect_err("error envelope must fail collection");
        assert!(error.contains("quota exhausted"));

        let incomplete = collect_stream_to_json(stream::iter([Ok::<_, String>(Bytes::from(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
        ))]))
        .await
        .expect_err("missing terminal choice must fail collection");
        assert!(incomplete.contains("terminal choice"));
    }

    #[tokio::test]
    async fn collector_rejects_when_any_candidate_lacks_a_finish() {
        let response = collect_stream_to_json(stream::iter([Ok::<_, String>(Bytes::from(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"complete\"},\"finish_reason\":\"stop\"},{\"index\":1,\"delta\":{\"content\":\"partial\"}}]}\n\n",
        ))]))
        .await;
        assert!(response.is_err());
    }

    #[tokio::test]
    async fn collector_preserves_tool_call_index_order() {
        let frame = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {"index": 2, "id": "third", "type": "function", "function": {"name": "third", "arguments": "{}"}},
                        {"index": 0, "id": "first", "type": "function", "function": {"name": "first", "arguments": "{}"}},
                        {"index": 1, "id": "second", "type": "function", "function": {"name": "second", "arguments": "{}"}}
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let response = collect_stream_to_json(stream::iter([Ok::<_, String>(Bytes::from(
            format!("data: {frame}\n\n"),
        ))]))
        .await
        .expect("complete stream");
        let names: Vec<_> = response.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("calls")
            .iter()
            .map(|call| call.function.as_ref().expect("function").name.as_str())
            .collect();
        assert_eq!(names, ["first", "second", "third"]);
    }
}
