use axum::{
    body::to_bytes,
    extract::{rejection::JsonRejection, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, read::DecoderReader};
use serde_json::{json, Map, Value};
use std::{io, sync::Arc};

use super::{parse_json, relay, CodexError, CodexManager};
use crate::proxy::server::AppState;

const IMAGE_MODEL: &str = "gpt-5.6-luna";
const MAX_PROMPT_BYTES: usize = 256 * 1024;
const MAX_IMAGE_BYTES: usize = 48 * 1024 * 1024;

pub(in crate::proxy::codex) async fn generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let request = match parse_json(body) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    match generate_request(state.codex, headers, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

pub(in crate::proxy::codex) async fn generate_request(
    manager: Arc<CodexManager>,
    headers: HeaderMap,
    request: Value,
) -> Result<Response, CodexError> {
    let (prompt, tool) = image_request(request)?;
    let mut native = json!({
        "model": IMAGE_MODEL,
        "instructions": "Generate the requested image with the image_generation tool. Do not respond with text.",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": prompt}]}],
        "tools": [tool],
    });
    let scope = relay::scope(&headers);
    let selection = relay::select_account(&manager, &headers, &native, &scope).await?;
    let response =
        relay::forward(manager, selection, headers, &mut native, scope, false, None).await?;
    if !response.status().is_success() {
        return Ok(response);
    }

    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, relay::MAX_COLLECTED)
        .await
        .map_err(|_| CodexError::upstream("Codex image response exceeds the gateway size limit"))?;
    let native: Value = serde_json::from_slice(&bytes)
        .map_err(|_| CodexError::upstream("Codex returned an invalid image generation response"))?;
    let data = image_data(&native)?;
    let mut payload = json!({"created": super::now(), "data": data});
    // Preserve only upstream-reported totals for existing gateway accounting.
    // These are Responses usage, not an invented public Images price estimate.
    if let Some(usage) = native.get("usage").filter(|value| value.is_object()) {
        payload["usage"] = usage.clone();
    }
    let mut output = (StatusCode::OK, Json(payload)).into_response();
    *output.headers_mut() = parts.headers;
    output.headers_mut().remove(header::CONTENT_LENGTH);
    output.headers_mut().remove(header::CONTENT_ENCODING);
    output.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    output.headers_mut().insert(
        "x-codex-image-compatibility",
        header::HeaderValue::from_static(
            "requested-model=compatibility-id; renderer=upstream-default; n=1; usage=responses",
        ),
    );
    Ok(output)
}

fn image_request(request: Value) -> Result<(String, Value), CodexError> {
    let object = request
        .as_object()
        .ok_or_else(|| CodexError::bad_request("Images request must be an object"))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "model"
                | "prompt"
                | "n"
                | "size"
                | "quality"
                | "background"
                | "output_format"
                | "output_compression"
                | "moderation"
                | "response_format"
        ) {
            return Err(CodexError::bad_request(format!(
                "Images option '{key}' is not supported by the Codex subscription gateway"
            )));
        }
    }
    if let Some(model) = object.get("model") {
        let model = model
            .as_str()
            .ok_or_else(|| CodexError::bad_request("model must be a string"))?;
        if model != "gpt-image-2" {
            return Err(CodexError::bad_request(
                "Only gpt-image-2 is supported for Codex image generation",
            ));
        }
    }
    let prompt = object
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|prompt| !prompt.is_empty() && prompt.len() <= MAX_PROMPT_BYTES)
        .ok_or_else(|| {
            CodexError::bad_request("prompt must be a non-empty string no larger than 256 KiB")
        })?
        .to_owned();
    if object
        .get("n")
        .is_some_and(|value| value.as_u64() != Some(1))
    {
        return Err(CodexError::bad_request(
            "Only n=1 is supported because native Codex image generation returns one image per request",
        ));
    }
    if let Some(response_format) = object.get("response_format") {
        if response_format.as_str() != Some("b64_json") {
            return Err(CodexError::bad_request(
                "Only response_format=b64_json is supported; Codex does not expose hosted image URLs",
            ));
        }
    }

    let mut tool = Map::new();
    tool.insert("type".into(), Value::String("image_generation".into()));
    copy_choice(
        object,
        &mut tool,
        "size",
        &["auto", "1024x1024", "1536x1024", "1024x1536"],
    )?;
    copy_choice(
        object,
        &mut tool,
        "quality",
        &["auto", "low", "medium", "high"],
    )?;
    copy_choice(
        object,
        &mut tool,
        "background",
        &["auto", "opaque", "transparent"],
    )?;
    copy_choice(object, &mut tool, "output_format", &["png", "jpeg", "webp"])?;
    copy_choice(object, &mut tool, "moderation", &["auto", "low"])?;
    if let Some(compression) = object.get("output_compression") {
        let compression = compression
            .as_u64()
            .filter(|value| *value <= 100)
            .ok_or_else(|| {
                CodexError::bad_request("output_compression must be an integer from 0 through 100")
            })?;
        tool.insert("output_compression".into(), Value::from(compression));
    }
    Ok((prompt, Value::Object(tool)))
}

fn copy_choice(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    name: &str,
    supported: &[&str],
) -> Result<(), CodexError> {
    let Some(value) = source.get(name) else {
        return Ok(());
    };
    let value = value
        .as_str()
        .filter(|value| supported.contains(value))
        .ok_or_else(|| {
            CodexError::bad_request(format!("Unsupported {name} for Codex image generation"))
        })?;
    target.insert(name.into(), Value::String(value.into()));
    Ok(())
}

fn image_data(response: &Value) -> Result<Vec<Value>, CodexError> {
    if response.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(CodexError::upstream(
            "Codex image generation did not complete successfully",
        ));
    }
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| CodexError::upstream("Codex image response did not contain output"))?;
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("image_generation_call") {
            continue;
        }
        if item.get("status").is_some()
            && item.get("status").and_then(Value::as_str) != Some("completed")
        {
            return Err(CodexError::upstream(
                "Codex image generation did not complete successfully",
            ));
        }
        let result = item.get("result").and_then(Value::as_str).ok_or_else(|| {
            CodexError::upstream("Codex image generation completed without image bytes")
        })?;
        let encoded = result
            .strip_prefix("data:image/")
            .and_then(|value| value.split_once(";base64,").map(|(_, data)| data))
            .unwrap_or(result);
        if encoded.is_empty() || encoded.len() > MAX_IMAGE_BYTES.saturating_mul(4) / 3 + 4 {
            return Err(CodexError::upstream(
                "Codex returned an invalid or oversized generated image",
            ));
        }
        let mut decoder = DecoderReader::new(encoded.as_bytes(), &STANDARD);
        let mut limited = io::Read::take(&mut decoder, (MAX_IMAGE_BYTES + 1) as u64);
        let decoded = io::copy(&mut limited, &mut io::sink())
            .map_err(|_| CodexError::upstream("Codex returned invalid base64 image data"))?;
        if decoded == 0 || decoded > MAX_IMAGE_BYTES as u64 {
            return Err(CodexError::upstream(
                "Codex returned an invalid or oversized generated image",
            ));
        }
        return Ok(vec![json!({"b64_json": encoded})]);
    }
    Err(CodexError::upstream(
        "Codex completed without generating an image",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_failed_native_response_even_when_it_contains_bytes() {
        assert!(image_data(&json!({
            "status": "failed",
            "output": [{"type": "image_generation_call", "status": "completed", "result": "AQ=="}]
        }))
        .is_err());
    }

    #[test]
    fn rejects_incomplete_image_call() {
        assert!(image_data(&json!({
            "status": "completed",
            "output": [{"type": "image_generation_call", "status": "incomplete", "result": "AQ=="}]
        }))
        .is_err());
    }

    #[test]
    fn rejects_completed_response_without_image() {
        assert!(image_data(&json!({
            "status": "completed",
            "output": [{"type": "message", "status": "completed"}]
        }))
        .is_err());
    }

    #[test]
    fn rejects_malformed_native_image_bytes() {
        assert!(image_data(&json!({
            "status": "completed",
            "output": [{"type": "image_generation_call", "status": "completed", "result": "not base64"}]
        }))
        .is_err());
    }

    #[test]
    fn rejects_multi_image_requests() {
        assert!(image_request(json!({"model": "gpt-image-2", "prompt": "cat", "n": 2})).is_err());
    }
}
