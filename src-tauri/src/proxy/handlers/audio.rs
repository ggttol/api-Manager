use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use tracing::{debug, info};
use uuid::Uuid;

use crate::proxy::{audio::AudioProcessor, server::AppState};

const DEFAULT_TRANSCRIPTION_MODEL: &str = "gemini-2.0-flash";

fn resolve_transcription_model(
    requested_model: &str,
    custom_mapping: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        return Err("缺少转录模型".to_string());
    }

    let routed =
        crate::proxy::common::model_mapping::resolve_model_route(requested_model, custom_mapping);
    if routed != requested_model {
        return Ok(routed);
    }

    match requested_model {
        "whisper-1" => Ok(DEFAULT_TRANSCRIPTION_MODEL.to_string()),
        model if model.starts_with("gemini-") => Ok(model.to_string()),
        _ => Err(format!("不支持的转录模型: {requested_model}")),
    }
}

fn extract_transcript(response: &Value) -> Result<String, String> {
    let inner_response = response.get("response").unwrap_or(response);
    if let Some(error) = inner_response.get("error").filter(|error| !error.is_null()) {
        return Err(format!("Gemini API 错误: {error}"));
    }
    if let Some(reason) = inner_response
        .get("promptFeedback")
        .and_then(|feedback| feedback.get("blockReason"))
        .and_then(Value::as_str)
    {
        return Err(format!("转录请求被 Gemini 拦截: {reason}"));
    }

    let candidate = inner_response
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .ok_or_else(|| "Gemini 未返回转录候选结果".to_string())?;
    let finish_reason = candidate.get("finishReason").and_then(Value::as_str);
    if let Some(reason) = finish_reason.filter(|reason| *reason != "STOP") {
        return Err(format!("Gemini 转录未成功完成 (finish reason: {reason})"));
    }
    let parts = candidate
        .get("content")
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            let finish_reason = finish_reason.unwrap_or("unknown");
            format!("Gemini 未返回有效转录文本 (finish reason: {finish_reason})")
        })?;

    let transcript = parts
        .iter()
        .filter(|part| part.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    if transcript.is_empty() {
        return Err("Gemini 未返回有效转录文本".to_string());
    }
    Ok(transcript)
}

/// 处理音频转录请求 (OpenAI Whisper API 兼容)
pub async fn handle_audio_transcription(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut audio_data: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;
    let mut model = "whisper-1".to_string();
    let mut prompt = "Generate a transcript of the speech.".to_string();

    // 1. 解析 multipart/form-data
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("解析表单失败: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();

        match name.as_str() {
            "file" => {
                filename = field.file_name().map(|s| s.to_string());
                audio_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| (StatusCode::BAD_REQUEST, format!("读取文件失败: {}", e)))?
                        .to_vec(),
                );
            }
            "model" => {
                model = field.text().await.unwrap_or(model);
            }
            "prompt" => {
                prompt = field.text().await.unwrap_or(prompt);
            }
            _ => {}
        }
    }

    let audio_bytes = audio_data.ok_or((StatusCode::BAD_REQUEST, "缺少音频文件".to_string()))?;

    let file_name = filename.ok_or((StatusCode::BAD_REQUEST, "无法获取文件名".to_string()))?;

    info!(
        "收到音频转录请求: 文件={}, 大小={} bytes, 模型={}",
        file_name,
        audio_bytes.len(),
        model
    );

    // 2. 检测 MIME 类型
    let mime_type =
        AudioProcessor::detect_mime_type(&file_name).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // 3. 验证文件大小
    if AudioProcessor::exceeds_size_limit(audio_bytes.len()) {
        let size_mb = audio_bytes.len() as f64 / (1024.0 * 1024.0);
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "音频文件过大 ({:.1} MB)。最大支持 15 MB (约 16 分钟 MP3)。建议: 1) 压缩音频质量 2) 分段上传",
                size_mb
            ),
        ));
    }

    let mapped_model = {
        let custom_mapping = state.custom_mapping.read().await;
        resolve_transcription_model(&model, &custom_mapping)
    }
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // 4. 使用 Inline Data 方式
    debug!("使用 Inline Data 方式处理");
    let base64_audio = AudioProcessor::encode_to_base64(&audio_bytes);

    // 5. 构建 Gemini 请求
    let gemini_request = json!({
        "contents": [{
            "parts": [
                {"text": prompt},
                {
                    "inlineData": {
                        "mimeType": mime_type,
                        "data": base64_audio
                    }
                }
            ]
        }]
    });

    // 6. 获取 Token 和上游客户端
    let token_manager = state.token_manager;
    let (access_token, project_id, email, account_id, _wait_ms) = token_manager
        .get_token("text", false, None, &mapped_model)
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e))?;
    let mapped_model = token_manager
        .resolve_dynamic_model_for_account(&account_id, &mapped_model)
        .await;

    info!("使用账号: {}", email);

    // 7. 包装请求为 v1internal 格式
    let wrapped_body = json!({
        "project": project_id,
        "requestId": format!("audio-{}", Uuid::new_v4()),
        "request": gemini_request,
        "model": mapped_model,
        "userAgent": "antigravity",
        "requestType": "text"
    });

    // 8. 发送请求到 Gemini
    let upstream = state.upstream.clone();
    let response = upstream
        .call_v1_internal(
            "generateContent",
            &access_token,
            wrapped_body,
            None,
            Some(account_id.as_str()),
        )
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("上游请求失败: {}", e)))?
        .response;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Gemini API 错误: {}", error_text),
        ));
    }

    let result: Value = response
        .json()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("解析响应失败: {}", e)))?;

    // 9. 提取文本响应（解包 v1internal 响应）
    let text = extract_transcript(&result).map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

    info!("音频转录完成，返回 {} 字符", text.len());

    // 10. 返回标准格式响应
    Ok((
        StatusCode::OK,
        [("X-Account-Email", email.as_str())],
        Json(json!({
            "text": text
        })),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn transcription_alias_and_custom_mapping_route_to_gemini_models() {
        assert_eq!(
            resolve_transcription_model("whisper-1", &HashMap::new()).unwrap(),
            DEFAULT_TRANSCRIPTION_MODEL
        );

        let mapping = HashMap::from([("whisper-1".to_string(), "gemini-custom-audio".to_string())]);
        assert_eq!(
            resolve_transcription_model("whisper-1", &mapping).unwrap(),
            "gemini-custom-audio"
        );
        assert!(resolve_transcription_model("unsupported-whisper", &HashMap::new()).is_err());
    }

    #[test]
    fn transcript_accepts_only_successful_candidate_finish_reason() {
        let response = json!({
            "response": {
                "candidates": [{
                    "finishReason": "STOP",
                    "content": {"parts": [
                        {"text": "internal", "thought": true},
                        {"thoughtSignature": "opaque"},
                        {"text": "first "},
                        {"text": "second"}
                    ]}
                }]
            }
        });
        assert_eq!(extract_transcript(&response).unwrap(), "first second");

        for finish_reason in ["MAX_TOKENS", "SAFETY"] {
            let incomplete = json!({
                "candidates": [{
                    "finishReason": finish_reason,
                    "content": {"parts": [{"text": "partial"}]}
                }]
            });
            let error = extract_transcript(&incomplete).unwrap_err();
            assert!(error.contains(finish_reason), "{error}");
        }

        let no_transcript = json!({
            "candidates": [{"finishReason": "STOP", "content": {"parts": [
                {"text": "internal", "thought": true}
            ]}}]
        });
        assert!(extract_transcript(&no_transcript).is_err());
    }
}
