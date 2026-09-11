use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::StreamExt;
use serde_json::Value;
use tokio::time::Duration;

use crate::proxy::server::AppState;

fn map_model_for_zai(original: &str, state: &crate::proxy::ZaiConfig) -> String {
    let m = original.to_lowercase();
    if let Some(mapped) = state.model_mapping.get(original) {
        return mapped.clone();
    }
    if let Some(mapped) = state.model_mapping.get(&m) {
        return mapped.clone();
    }
    if m.starts_with("zai:") {
        return original[4..].to_string();
    }
    if m.starts_with("glm-") {
        return original.to_string();
    }
    if !m.starts_with("claude-") {
        return original.to_string();
    }
    if m.contains("opus") {
        return state.models.opus.clone();
    }
    if m.contains("haiku") {
        return state.models.haiku.clone();
    }
    state.models.sonnet.clone()
}

fn join_base_url(base: &str, path: &str) -> Result<String, String> {
    let base = base.trim_end_matches('/');
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };
    Ok(format!("{}{}", base, path))
}

fn build_client(
    upstream_proxy: Option<crate::proxy::config::UpstreamProxyConfig>,
    timeout_secs: u64,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(timeout_secs.max(5)));

    if let Some(config) = upstream_proxy {
        if config.enabled && !config.url.is_empty() {
            let url = crate::proxy::config::normalize_proxy_url(&config.url);
            let proxy = reqwest::Proxy::all(&url)
                .map_err(|e| format!("Invalid upstream proxy url: {}", e))?;
            builder = builder.proxy(proxy);
        }
    }

    builder
        .tcp_nodelay(true) // [FIX #307] Disable Nagle's algorithm to improve latency for small requests
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))
}

fn copy_passthrough_headers(incoming: &HeaderMap) -> HeaderMap {
    // Only forward a conservative set of headers to avoid leaking the local proxy key or cookies.
    let mut out = HeaderMap::new();

    for (k, v) in incoming.iter() {
        let key = k.as_str().to_ascii_lowercase();
        match key.as_str() {
            "content-type" | "accept" | "anthropic-version" | "user-agent" | "cache-control" => {
                out.insert(k.clone(), v.clone());
            }
            _ => {}
        }
    }

    // Reqwest is not configured to decode compressed upstream bodies. Request an uncompressed
    // response rather than forwarding an encoding we cannot safely transform.
    out.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    out
}

fn set_zai_auth(headers: &mut HeaderMap, incoming: &HeaderMap, api_key: &str) {
    // Prefer to keep the same auth scheme as the incoming request:
    // - If the client used x-api-key (Anthropic style), replace it.
    // - Else if it used Authorization, replace it with Bearer.
    // - Else default to x-api-key.
    let has_x_api_key = incoming.contains_key("x-api-key");
    let has_auth = incoming.contains_key(header::AUTHORIZATION);

    if has_x_api_key || !has_auth {
        if let Ok(v) = HeaderValue::from_str(api_key) {
            headers.insert("x-api-key", v);
        }
    }

    if has_auth {
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", api_key)) {
            headers.insert(header::AUTHORIZATION, v);
        }
    }
}

/// Remove Anthropic cache-control metadata only where the protocol defines content blocks.
///
/// Tool inputs and JSON schemas are arbitrary application data, so recursively walking the
/// payload would corrupt valid fields named `cache_control`.
pub fn remove_content_block_cache_control(body: &mut Value) {
    if let Some(root) = body.as_object_mut() {
        root.remove("cache_control");

        if let Some(system) = root.get_mut("system").and_then(Value::as_array_mut) {
            clean_content_blocks(system);
        }
        if let Some(messages) = root.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages {
                if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
                    clean_content_blocks(content);
                }
            }
        }
    }
}

fn clean_content_blocks(blocks: &mut [Value]) {
    for block in blocks {
        let Some(object) = block.as_object_mut() else {
            continue;
        };
        object.remove("cache_control");

        // A tool result may itself contain Anthropic content blocks. Do not traverse any
        // other fields: in particular, tool_use.input and schema values are opaque payloads.
        if object.get("type").and_then(Value::as_str) == Some("tool_result") {
            if let Some(content) = object.get_mut("content").and_then(Value::as_array_mut) {
                clean_content_blocks(content);
            }
        }
    }
}

fn upstream_body<S, E>(stream: S) -> Body
where
    S: futures::Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    Body::from_stream(stream.map(|chunk| chunk.map_err(std::io::Error::other)))
}

fn copy_upstream_response_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in [header::CONTENT_TYPE, header::CONTENT_ENCODING] {
        if let Some(value) = headers.get(&name) {
            out.insert(name, value.clone());
        }
    }
    out
}

pub async fn forward_anthropic_json(
    state: &AppState,
    method: Method,
    path: &str,
    incoming_headers: &HeaderMap,
    mut body: Value,
    message_count: usize, // [NEW v4.0.0] Pass message count for rewind detection
) -> Response {
    let zai = state.zai.read().await.clone();
    if !zai.enabled || zai.dispatch_mode == crate::proxy::ZaiDispatchMode::Off {
        return (StatusCode::BAD_REQUEST, "z.ai is disabled").into_response();
    }

    if zai.api_key.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "z.ai api_key is not set").into_response();
    }

    if let Some(model) = body.get("model").and_then(|v| v.as_str()) {
        let mapped = map_model_for_zai(model, &zai);
        body["model"] = Value::String(mapped.clone());

        // [FIX] Caching for z.ai (to support thinking-filter)
        if let Some(sig) = body
            .get("thinking")
            .and_then(|t| t.get("signature"))
            .and_then(|s| s.as_str())
        {
            crate::proxy::SignatureCache::global().cache_session_signature(
                "zai-session",
                sig.to_string(),
                message_count,
            );
            crate::proxy::SignatureCache::global().cache_thinking_family(sig.to_string(), mapped);
        }
    }

    let url = match join_base_url(&zai.base_url, path) {
        Ok(u) => u,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    let timeout_secs = state.request_timeout.max(5);
    let upstream_proxy = state.upstream_proxy.read().await.clone();
    let client = match build_client(Some(upstream_proxy), timeout_secs) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };

    let mut headers = copy_passthrough_headers(incoming_headers);
    set_zai_auth(&mut headers, incoming_headers, &zai.api_key);

    // Ensure JSON content type.
    headers
        .entry(header::CONTENT_TYPE)
        .or_insert(HeaderValue::from_static("application/json"));

    // z.ai rejects Anthropic cache-control metadata, but tool values and JSON schemas are
    // opaque application payloads and must remain byte-for-byte semantically intact.
    remove_content_block_cache_control(&mut body);

    // [FIX #307] Explicitly serialize body to Vec<u8> to ensure Content-Length is set correctly.
    // This avoids "Transfer-Encoding: chunked" for small bodies which caused connection errors.
    let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
    let body_len = body_bytes.len();

    tracing::debug!(
        "Forwarding request to z.ai (len: {} bytes): {}",
        body_len,
        url
    );

    let req = client
        .request(method, &url)
        .headers(headers)
        .body(body_bytes); // Use .body(Vec<u8>) instead of .json()

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("Upstream request failed: {}", e),
            )
                .into_response();
        }
    };

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    let mut out = Response::builder().status(status);
    for (name, value) in copy_upstream_response_headers(resp.headers()).iter() {
        out = out.header(name, value);
    }

    // Stream response body to the client (covers SSE and non-SSE). Body read failures must
    // remain failures; appending diagnostic bytes produces invalid JSON/SSE success responses.
    let stream = upstream_body(resp.bytes_stream());

    out.body(stream).unwrap_or_else(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to build response",
        )
            .into_response()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[test]
    fn content_metadata_cleanup_preserves_tool_payloads_and_schemas() {
        let mut request = serde_json::json!({
            "cache_control": { "type": "ephemeral" },
            "messages": [{
                "content": [{
                    "type": "tool_use",
                    "cache_control": { "type": "ephemeral" },
                    "input": { "cache_control": "application value", "thought": "application value" }
                }, {
                    "type": "tool_result",
                    "content": [{ "type": "text", "cache_control": { "type": "ephemeral" }, "text": "ok" }]
                }]
            }],
            "tools": [{
                "input_schema": {
                    "properties": { "cache_control": { "type": "string" } },
                    "required": ["cache_control"]
                }
            }]
        });

        remove_content_block_cache_control(&mut request);

        assert!(request.get("cache_control").is_none());
        assert!(request["messages"][0]["content"][0]
            .get("cache_control")
            .is_none());
        assert_eq!(
            request["messages"][0]["content"][0]["input"]["cache_control"],
            "application value"
        );
        assert!(request["messages"][0]["content"][1]["content"][0]
            .get("cache_control")
            .is_none());
        assert_eq!(
            request["tools"][0]["input_schema"]["properties"]["cache_control"]["type"],
            "string"
        );
        assert_eq!(
            request["tools"][0]["input_schema"]["required"][0],
            "cache_control"
        );
    }

    #[test]
    fn passthrough_negotiates_identity_and_retains_upstream_content_encoding() {
        let mut request = HeaderMap::new();
        request.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        let forwarded = copy_passthrough_headers(&request);
        assert_eq!(forwarded.get(header::ACCEPT_ENCODING).unwrap(), "identity");

        let mut upstream = HeaderMap::new();
        upstream.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        assert_eq!(
            copy_upstream_response_headers(&upstream)
                .get(header::CONTENT_ENCODING)
                .unwrap(),
            "gzip"
        );
    }

    #[tokio::test]
    async fn passthrough_body_failure_remains_a_body_failure() {
        let body = upstream_body(stream::iter(vec![Err::<Bytes, _>(std::io::Error::other(
            "truncated upstream",
        ))]));
        assert!(axum::body::to_bytes(body, 1024).await.is_err());
    }
}
