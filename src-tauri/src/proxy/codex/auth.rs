use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

use super::{now, CodexError};

// Official Codex OAuth public client, not an API key. Endpoints are deliberately not configurable.
pub(super) const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(super) const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub(super) const USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub(super) const POLL_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub(super) const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
pub(super) const MODELS_URL: &str =
    "https://chatgpt.com/backend-api/codex/models?client_version=0.154.0";
pub(super) const RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
pub(super) const COMPACT_URL: &str = "https://chatgpt.com/backend-api/codex/responses/compact";
pub(super) const JSON_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub account_id: String,
    pub refreshed_at: i64,
}

fn required_token(value: &Value, key: &str) -> Result<String, CodexError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|token| {
            !token.is_empty() && token.len() <= 32768 && !token.chars().any(char::is_whitespace)
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            CodexError::bad_request(
                "auth.json requires nonempty subscription access_token, refresh_token and id_token",
            )
        })
}

// Unverified decoding is metadata only. Every imported account is authorized against WHAM before use.
fn claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

pub(super) fn clean_metadata(value: Option<&str>, max: usize) -> Option<String> {
    value
        .filter(|value| {
            !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

impl Tokens {
    pub fn from_auth_json(value: &Value) -> Result<Self, CodexError> {
        if value
            .get("OPENAI_API_KEY")
            .and_then(Value::as_str)
            .is_some_and(|key| !key.is_empty())
            || value
                .get("auth_mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| mode != "chatgpt")
        {
            return Err(CodexError::bad_request(
                "Import ChatGPT subscription auth.json, not an API key or external credentials",
            ));
        }
        let tokens = value
            .get("tokens")
            .filter(|value| value.is_object())
            .ok_or_else(|| CodexError::bad_request("auth.json is missing subscription tokens"))?;
        let access_token = required_token(tokens, "access_token")?;
        let refresh_token = required_token(tokens, "refresh_token")?;
        let id_token = required_token(tokens, "id_token")?;
        let identity = claims(&id_token);
        let access = claims(&access_token);
        let account_id = tokens
            .get("account_id")
            .and_then(Value::as_str)
            .or_else(|| {
                identity
                    .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                access
                    .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                    .and_then(Value::as_str)
            });
        let account_id = clean_metadata(account_id, 256)
            .filter(|id| id.is_ascii() && !id.chars().any(char::is_whitespace))
            .ok_or_else(|| {
                CodexError::bad_request("auth.json is missing a valid ChatGPT account ID")
            })?;
        if identity
            .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_is_fedramp")
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Err(CodexError::bad_request("FedRAMP accounts require a separate approved deployment and are not supported by this gateway"));
        }
        Ok(Self {
            access_token,
            refresh_token,
            id_token,
            account_id,
            refreshed_at: now(),
        })
    }

    pub fn expires_at(&self) -> Option<i64> {
        claims(&self.access_token)
            .get("exp")
            .and_then(Value::as_i64)
    }

    pub fn needs_refresh(&self) -> bool {
        self.expires_at()
            .map(|expires| expires <= now() + 300)
            .unwrap_or_else(|| self.refreshed_at <= now() - 8 * 24 * 60 * 60)
    }

    pub fn email(&self) -> Option<String> {
        let identity = claims(&self.id_token);
        clean_metadata(
            identity.get("email").and_then(Value::as_str).or_else(|| {
                identity
                    .pointer("/https:~1~1api.openai.com~1profile/email")
                    .and_then(Value::as_str)
            }),
            254,
        )
    }

    pub fn plan_type(&self) -> Option<String> {
        let identity = claims(&self.id_token);
        clean_metadata(
            identity
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_plan_type")
                .and_then(Value::as_str),
            64,
        )
    }

    pub fn owner(&self) -> Option<String> {
        let identity = claims(&self.id_token);
        clean_metadata(
            identity
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_user_id")
                .and_then(Value::as_str)
                .or_else(|| identity.get("sub").and_then(Value::as_str)),
            256,
        )
        .or_else(|| self.email())
    }

    pub fn redact(&self, value: &mut Value) {
        match value {
            Value::String(text) => {
                for secret in [&self.access_token, &self.refresh_token, &self.id_token] {
                    if !secret.is_empty() && text.contains(secret) {
                        *text = text.replace(secret, "[REDACTED]");
                    }
                }
            }
            Value::Array(values) => values.iter_mut().for_each(|value| self.redact(value)),
            Value::Object(values) => {
                values.retain(|key, _| {
                    !matches!(
                        key.to_ascii_lowercase().as_str(),
                        "access_token" | "refresh_token" | "id_token" | "authorization" | "api_key"
                    )
                });
                values.values_mut().for_each(|value| self.redact(value));
            }
            _ => {}
        }
    }
}

pub(super) async fn read_bounded(response: Response, limit: usize) -> Result<Vec<u8>, CodexError> {
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(300), stream.next())
        .await
        .map_err(|_| CodexError::upstream("Codex upstream response timed out"))?
    {
        let chunk =
            chunk.map_err(|_| CodexError::upstream("Codex upstream response interrupted"))?;
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(CodexError::upstream(
                "Codex upstream response exceeds the gateway size limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) async fn json_body(response: Response) -> Result<Value, CodexError> {
    serde_json::from_slice(&read_bounded(response, JSON_LIMIT).await?)
        .map_err(|_| CodexError::upstream("Codex upstream returned invalid JSON"))
}

pub(super) fn authorized(
    client: &Client,
    method: reqwest::Method,
    url: &'static str,
    tokens: &Tokens,
) -> reqwest::RequestBuilder {
    client
        .request(method, url)
        .bearer_auth(&tokens.access_token)
        .header("ChatGPT-Account-Id", &tokens.account_id)
        .header("originator", "codex_cli_rs")
        .header("User-Agent", "codex_cli_rs/0.154.0 (API Manager gateway)")
}

pub(super) async fn refresh(client: &Client, old: &Tokens) -> Result<Tokens, CodexError> {
    let response = client.post(TOKEN_URL).timeout(Duration::from_secs(45))
        .json(&json!({"client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": old.refresh_token}))
        .send().await.map_err(|_| CodexError::upstream("Codex token refresh could not reach the authorization service"))?;
    let status = response.status();
    let value = json_body(response).await?;
    if !status.is_success() {
        let code = value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .or_else(|| value.get("error").and_then(Value::as_str))
            .or_else(|| value.get("code").and_then(Value::as_str))
            .unwrap_or("");
        let revoked = (status == reqwest::StatusCode::BAD_REQUEST && code == "invalid_grant")
            || ((status.is_client_error())
                && matches!(
                    code,
                    "refresh_token_expired" | "refresh_token_reused" | "refresh_token_invalidated"
                ));
        return Err(if revoked {
            CodexError::revoked()
        } else {
            CodexError::upstream_status(
                status,
                "Codex token refresh was rejected; the account was not disabled",
            )
        });
    }
    let access_token = required_token(&value, "access_token")
        .map_err(|_| CodexError::upstream("Codex token refresh returned no valid access token"))?;
    let id_token = value
        .get("id_token")
        .filter(|value| !value.is_null())
        .map(|_| required_token(&value, "id_token"))
        .transpose()?
        .unwrap_or_else(|| old.id_token.clone());
    let refresh_token = value
        .get("refresh_token")
        .filter(|value| !value.is_null())
        .map(|_| required_token(&value, "refresh_token"))
        .transpose()?
        .unwrap_or_else(|| old.refresh_token.clone());
    let next = Tokens::from_auth_json(&json!({"tokens": {
        "access_token": access_token, "refresh_token": refresh_token, "id_token": id_token, "account_id": old.account_id
    }}))?;
    let next_claims = claims(&next.id_token);
    let new_account = next_claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(Value::as_str);
    if new_account.is_some_and(|id| id != old.account_id)
        || (old.owner().is_some() && old.owner() != next.owner())
    {
        return Err(CodexError::upstream(
            "Refreshed Codex credentials belong to a different identity; sign in again",
        ));
    }
    Ok(next)
}

pub(super) async fn verify(client: &Client, tokens: &Tokens) -> Result<Value, CodexError> {
    let response = authorized(client, reqwest::Method::GET, USAGE_URL, tokens)
        .timeout(Duration::from_secs(45))
        .send()
        .await
        .map_err(|_| {
            CodexError::upstream("Unable to reach ChatGPT to verify subscription credentials")
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(CodexError::upstream_status(
            status,
            "ChatGPT did not authorize these subscription credentials",
        ));
    }
    let mut usage = json_body(response).await?;
    if !usage.is_object() {
        return Err(CodexError::upstream(
            "ChatGPT returned an invalid subscription usage response",
        ));
    }
    if usage
        .get("account_id")
        .and_then(Value::as_str)
        .is_some_and(|id| id != tokens.account_id)
    {
        return Err(CodexError::bad_request(
            "ChatGPT verified a different account; import the matching workspace credentials",
        ));
    }
    tokens.redact(&mut usage);
    Ok(usage)
}

#[derive(Deserialize)]
pub(super) struct DeviceCode {
    pub device_auth_id: String,
    #[serde(alias = "usercode")]
    pub user_code: String,
    #[serde(default)]
    pub interval: Value,
}

impl DeviceCode {
    pub fn interval(&self) -> u64 {
        self.interval
            .as_u64()
            .or_else(|| {
                self.interval
                    .as_str()
                    .and_then(|value| value.trim().parse().ok())
            })
            .unwrap_or(5)
            .max(1)
    }
}

pub(super) async fn start_device(client: &Client) -> Result<DeviceCode, CodexError> {
    let response = client
        .post(USER_CODE_URL)
        .timeout(Duration::from_secs(45))
        .json(&json!({"client_id": CLIENT_ID}))
        .send()
        .await
        .map_err(|_| CodexError::upstream("Unable to start Codex device authorization"))?;
    if !response.status().is_success() {
        return Err(CodexError::upstream_status(
            response.status(),
            "Codex device authorization is unavailable; enable device login in ChatGPT settings",
        ));
    }
    let code: DeviceCode = serde_json::from_value(json_body(response).await?)
        .map_err(|_| CodexError::upstream("Invalid Codex device authorization response"))?;
    if code.device_auth_id.is_empty()
        || code.device_auth_id.len() > 1024
        || clean_metadata(Some(&code.user_code), 128).is_none()
    {
        return Err(CodexError::upstream(
            "Invalid Codex device authorization response",
        ));
    }
    Ok(code)
}

pub(super) async fn poll_device(
    client: &Client,
    code: &DeviceCode,
) -> Result<Option<Tokens>, CodexError> {
    let response = client
        .post(POLL_URL)
        .timeout(Duration::from_secs(45))
        .json(&json!({"device_auth_id": code.device_auth_id, "user_code": code.user_code}))
        .send()
        .await
        .map_err(|_| CodexError::upstream("Codex device authorization polling failed"))?;
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(CodexError::upstream_status(
            status,
            "Codex device authorization was rejected",
        ));
    }
    let value = json_body(response).await?;
    let code = required_token(&value, "authorization_code")
        .map_err(|_| CodexError::upstream("Invalid device authorization code"))?;
    let verifier = required_token(&value, "code_verifier")
        .map_err(|_| CodexError::upstream("Invalid device authorization verifier"))?;
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", &code)
        .append_pair(
            "redirect_uri",
            "https://auth.openai.com/deviceauth/callback",
        )
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code_verifier", &verifier)
        .finish();
    let response = client
        .post(TOKEN_URL)
        .timeout(Duration::from_secs(45))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await
        .map_err(|_| {
            CodexError::upstream(
                "Codex device token exchange could not reach the authorization service",
            )
        })?;
    if !response.status().is_success() {
        return Err(CodexError::upstream_status(
            response.status(),
            "Codex device token exchange was rejected",
        ));
    }
    let value = json_body(response).await?;
    let tokens = Tokens::from_auth_json(&json!({"tokens": value}))?;
    Ok(Some(tokens))
}
