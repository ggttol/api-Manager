// Gemini Stream Collector
// Used for auto-converting streaming responses to JSON for non-streaming requests

use std::collections::BTreeMap;

use bytes::Bytes;
use futures::StreamExt;
use serde_json::{Map, Value};
use tracing::debug;

use crate::proxy::SignatureCache;

/// Collects a Gemini SSE stream into a complete Gemini response.
///
/// A non-streaming caller cannot recover from an upstream error or a truncated
/// stream, so this deliberately requires an explicit terminal response frame.
pub async fn collect_stream_to_json<S, E>(mut stream: S, session_id: &str) -> Result<Value, String>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut response = Map::new();
    let mut candidates: BTreeMap<i64, Value> = BTreeMap::new();
    let mut completed_candidates = std::collections::BTreeSet::new();
    let mut saw_blocked_prompt = false;
    let fallback_response_id = format!("gemini-collector-{}", uuid::Uuid::new_v4());

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("Gemini upstream stream error: {e}"))?;
        let text = std::str::from_utf8(&chunk)
            .map_err(|e| format!("Gemini upstream stream contained invalid UTF-8: {e}"))?;

        for raw_line in text.lines() {
            let line = raw_line.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }

            let mut frame: Value = serde_json::from_str(data)
                .map_err(|e| format!("Invalid Gemini SSE data frame: {e}"))?;
            let actual = frame.get_mut("response").map(Value::take).unwrap_or(frame);

            if let Some(error) = actual.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Gemini upstream returned an error");
                return Err(format!("Gemini upstream error: {message}"));
            }
            let actual_object = actual
                .as_object()
                .ok_or_else(|| "Gemini SSE frame must be an object".to_string())?;

            // Preserve response-level metadata (responseId, modelVersion,
            // promptFeedback, usageMetadata, etc.) exactly as received.
            for (key, value) in actual_object {
                if key != "candidates" {
                    response.insert(key.clone(), value.clone());
                }
            }
            let blocked_prompt = actual_object
                .get("promptFeedback")
                .and_then(Value::as_object)
                .is_some_and(|feedback| feedback.get("blockReason").is_some());
            if blocked_prompt
                && candidates.is_empty()
                && actual_object
                    .get("candidates")
                    .and_then(Value::as_array)
                    .map_or(true, Vec::is_empty)
            {
                // A blocked prompt intentionally has no candidate; do not invent one.
                saw_blocked_prompt = true;
            }

            if let Some(frame_candidates) =
                actual_object.get("candidates").and_then(Value::as_array)
            {
                for (position, frame_candidate) in frame_candidates.iter().enumerate() {
                    let index = frame_candidate
                        .get("index")
                        .and_then(Value::as_i64)
                        .unwrap_or(position as i64);
                    let candidate = candidates.entry(index).or_insert_with(|| {
                        let mut candidate = frame_candidate.clone();
                        candidate["index"] = Value::from(index);
                        candidate["content"]["parts"] = Value::Array(Vec::new());
                        candidate
                    });

                    merge_candidate_metadata(candidate, frame_candidate);
                    if frame_candidate.get("finishReason").is_some() {
                        completed_candidates.insert(index);
                    }

                    if let Some(parts) = frame_candidate
                        .get("content")
                        .and_then(|content| content.get("parts"))
                        .and_then(Value::as_array)
                    {
                        let collected_parts = candidate["content"]["parts"]
                            .as_array_mut()
                            .expect("collector initializes candidate content parts as an array");
                        let response_id = actual_object
                            .get("responseId")
                            .and_then(Value::as_str)
                            .unwrap_or(&fallback_response_id);
                        for part in parts {
                            cache_signature(part, session_id, response_id);
                            append_part(collected_parts, part.clone());
                        }
                    }
                }
            }
        }
    }

    if !saw_blocked_prompt && candidates.is_empty() {
        return Err("Gemini upstream stream ended before a terminal response frame".to_string());
    }
    if candidates
        .keys()
        .any(|index| !completed_candidates.contains(index))
    {
        return Err(
            "Gemini upstream stream ended before every candidate reached a terminal response frame"
                .to_string(),
        );
    }

    if !candidates.is_empty() {
        response.insert(
            "candidates".to_string(),
            Value::Array(candidates.into_values().collect()),
        );
    }

    Ok(Value::Object(response))
}

fn merge_candidate_metadata(collected: &mut Value, frame: &Value) {
    let (Some(collected), Some(frame)) = (collected.as_object_mut(), frame.as_object()) else {
        return;
    };
    for (key, value) in frame {
        if key != "content" {
            collected.insert(key.clone(), value.clone());
        } else if let Some(content) = value.as_object() {
            let target = collected
                .entry("content".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(target) = target.as_object_mut() {
                for (content_key, content_value) in content {
                    if content_key != "parts" {
                        target.insert(content_key.clone(), content_value.clone());
                    }
                }
            }
        }
    }
}

fn cache_signature(part: &Value, session_id: &str, response_id: &str) {
    if let Some(signature) = part.get("thoughtSignature").and_then(Value::as_str) {
        SignatureCache::global().cache_gemini_session_signature(
            session_id,
            signature.to_owned(),
            response_id,
        );
        debug!(
            "[Gemini-AutoConverter] Cached signature (len: {}) for session: {}",
            signature.len(),
            session_id
        );
    }
}

fn append_part(parts: &mut Vec<Value>, part: Value) {
    // Text parts are only safely mergeable when both are plain text. In
    // particular, thoughtSignature is opaque protocol data and must survive as
    // its own part instead of being discarded by a text-only replacement.
    let is_plain_text = |value: &Value| {
        value.get("text").is_some() && value.as_object().is_some_and(|object| object.len() == 1)
    };
    if is_plain_text(&part) {
        if let Some(last) = parts.last_mut().filter(|last| is_plain_text(last)) {
            let text = format!(
                "{}{}",
                last.get("text").and_then(Value::as_str).unwrap_or_default(),
                part.get("text").and_then(Value::as_str).unwrap_or_default()
            );
            *last = serde_json::json!({ "text": text });
            return;
        }
    }
    parts.push(part);
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures::stream;
    use serde_json::json;

    use super::collect_stream_to_json;

    #[tokio::test]
    async fn collects_candidates_metadata_and_signature_without_merging_it_away() {
        let frames = vec![Ok::<_, String>(Bytes::from(format!(
            "data: {}\n\n",
            json!({"responseId":"response-1","modelVersion":"gemini-test","usageMetadata":{"totalTokenCount":4},"candidates":[
                {"index":1,"content":{"role":"model","parts":[{"text":"B"}]},"finishReason":"STOP","groundingMetadata":{"groundingChunks":[{"web":{"uri":"https://example.test"}}]}},
                {"index":0,"content":{"role":"model","parts":[{"text":"A"},{"text":"","thoughtSignature":"opaque-signature"}]},"finishReason":"MAX_TOKENS","safetyRatings":[{"category":"HARM_CATEGORY_DANGEROUS_CONTENT","probability":"NEGLIGIBLE"}]}
            ]})
        )))];

        let collected = collect_stream_to_json(stream::iter(frames), "collector-test")
            .await
            .expect("terminal candidates collect");
        assert_eq!(collected["responseId"], "response-1");
        assert_eq!(collected["modelVersion"], "gemini-test");
        assert_eq!(collected["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(collected["candidates"][0]["index"], 0);
        assert_eq!(
            collected["candidates"][0]["content"]["parts"][1]["thoughtSignature"],
            "opaque-signature"
        );
        assert_eq!(
            collected["candidates"][1]["groundingMetadata"]["groundingChunks"][0]["web"]["uri"],
            "https://example.test"
        );
    }

    #[tokio::test]
    async fn keeps_prompt_feedback_blocked_shape() {
        let frames = vec![Ok::<_, String>(Bytes::from(format!(
            "data: {}\n\n",
            json!({"response":{"promptFeedback":{"blockReason":"SAFETY","safetyRatings":[]}}})
        )))];
        let collected = collect_stream_to_json(stream::iter(frames), "collector-test")
            .await
            .expect("blocked response is terminal");
        assert_eq!(collected["promptFeedback"]["blockReason"], "SAFETY");
        assert!(collected.get("candidates").is_none());
    }

    #[tokio::test]
    async fn rejects_error_and_truncated_streams() {
        let error = stream::iter(vec![Ok::<_, String>(Bytes::from(
            "data: {\"error\":{\"message\":\"overloaded\"}}\n\n",
        ))]);
        assert!(collect_stream_to_json(error, "collector-test")
            .await
            .is_err());

        let truncated = stream::iter(vec![Ok::<_, String>(Bytes::from(
            "data: {\"candidates\":[{\"index\":0,\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}\n\n",
        ))]);
        assert!(collect_stream_to_json(truncated, "collector-test")
            .await
            .is_err());
    }
}
