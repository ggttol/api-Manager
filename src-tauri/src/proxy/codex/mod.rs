//! Isolated ChatGPT subscription credentials and native Responses gateway.
//! OAuth and API contracts follow openai/codex rust-v0.154.0; no Google model mapping applies.
mod anthropic;
mod auth;
mod relay;
mod store;

use crate::proxy::{config::UpstreamProxyConfig, server::AppState};
use auth::Tokens;
use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, future::Future, path::PathBuf, sync::Arc, time::Duration};
use store::{Account, Accounts, Record, Vault};
use tokio::sync::{watch, Mutex};

pub(super) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Debug, Clone)]
struct CodexError {
    status: StatusCode,
    message: String,
    revoked: bool,
}

impl CodexError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            revoked: false,
        }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn upstream(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, message)
    }
    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }
    fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "Codex account or authorization not found",
        )
    }
    fn storage(message: String) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
    fn upstream_status(status: StatusCode, message: &str) -> Self {
        let downstream = if status.is_client_error() {
            status
        } else {
            StatusCode::BAD_GATEWAY
        };
        Self::new(
            downstream,
            format!("{message} (upstream HTTP {})", status.as_u16()),
        )
    }
    fn revoked() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "Codex refresh token expired, was reused, or was revoked; sign in again"
                .into(),
            revoked: true,
        }
    }
}

impl IntoResponse for CodexError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({"error": self.message}))).into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

// Pending rotated credentials stay under the same lock until their atomic persistence succeeds.
// They are never exposed to callers before that point, nor refreshed a second time after an I/O failure.
type RefreshLock = Arc<Mutex<Option<Tokens>>>;

#[derive(Clone, Serialize)]
struct DeviceStatus {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct DeviceLogin {
    state: DeviceStatus,
    expires_at: i64,
    cancel: watch::Sender<bool>,
}

struct Inner {
    accounts: Accounts,
    refresh_locks: HashMap<String, RefreshLock>,
    devices: HashMap<String, DeviceLogin>,
}

pub struct CodexManager {
    client: parking_lot::RwLock<Client>,
    vault: Vault,
    inner: Mutex<Inner>,
    imports: Mutex<()>,
    sessions: Mutex<relay::SessionCache>,
}

impl CodexManager {
    pub fn new(data_dir: PathBuf, proxy: Option<UpstreamProxyConfig>) -> Result<Self, String> {
        let client = Self::build_client(proxy)?;
        let (vault, accounts) = Vault::open(data_dir)?;
        let refresh_locks = accounts
            .accounts
            .iter()
            .map(|record| (record.account.id.clone(), Arc::new(Mutex::new(None))))
            .collect();
        Ok(Self {
            client: parking_lot::RwLock::new(client),
            vault,
            inner: Mutex::new(Inner {
                accounts,
                refresh_locks,
                devices: HashMap::new(),
            }),
            imports: Mutex::new(()),
            sessions: Mutex::new(relay::SessionCache::default()),
        })
    }

    fn build_client(proxy: Option<UpstreamProxyConfig>) -> Result<Client, String> {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(90));
        if let Some(proxy) = proxy {
            builder = builder.no_proxy();
            if proxy.enabled {
                builder = builder.proxy(
                    reqwest::Proxy::all(&proxy.url)
                        .map_err(|_| "Invalid Codex upstream proxy configuration")?,
                );
            }
        }
        builder
            .build()
            .map_err(|_| "Cannot initialize Codex HTTP client".into())
    }

    fn client(&self) -> Client {
        self.client.read().clone()
    }

    pub async fn update_proxy(&self, proxy: Option<UpstreamProxyConfig>) -> Result<(), String> {
        *self.client.write() = Self::build_client(proxy)?;
        Ok(())
    }

    fn commit_accounts(&self, inner: &mut Inner, accounts: Accounts) -> Result<(), CodexError> {
        self.vault.save(&accounts).map_err(CodexError::storage)?;
        inner.accounts = accounts;
        Ok(())
    }

    async fn record(&self, id: &str, require_enabled: bool) -> Result<Record, CodexError> {
        let inner = self.inner.lock().await;
        let record = inner
            .accounts
            .accounts
            .iter()
            .find(|record| record.account.id == id)
            .ok_or_else(CodexError::not_found)?;
        if require_enabled && (!record.account.enabled || !record.verified) {
            return Err(CodexError::unavailable("The pinned Codex account is disabled; explicitly start a new session or re-enable it"));
        }
        Ok(record.clone())
    }

    async fn preferred_account(&self) -> Result<String, CodexError> {
        let inner = self.inner.lock().await;
        let preferred =
            inner.accounts.active_account_id.as_deref().and_then(|id| {
                inner.accounts.accounts.iter().find(|record| {
                    record.account.id == id && record.account.enabled && record.verified
                })
            });
        preferred.or_else(|| inner.accounts.accounts.iter().find(|record| record.account.enabled && record.verified))
            .map(|record| record.account.id.clone())
            .ok_or_else(|| CodexError::unavailable("No enabled Codex subscription account; authorize one in the Codex administration page"))
    }

    async fn note_error(&self, id: &str, error: &CodexError) -> Result<(), CodexError> {
        let mut inner = self.inner.lock().await;
        let mut accounts = inner.accounts.clone();
        if let Some(record) = accounts
            .accounts
            .iter_mut()
            .find(|record| record.account.id == id)
        {
            record.account.last_error = Some(error.message.clone());
            if error.revoked {
                record.account.enabled = false;
            }
            self.commit_accounts(&mut inner, accounts)?;
        }
        Ok(())
    }

    async fn credentials(
        &self,
        id: &str,
        unauthorized_token: Option<&str>,
        require_enabled: bool,
    ) -> Result<Record, CodexError> {
        let client = self.client();
        self.credentials_with(
            id,
            unauthorized_token,
            require_enabled,
            |tokens| async move { auth::refresh(&client, &tokens).await },
        )
        .await
    }

    async fn credentials_with<F, Fut>(
        &self,
        id: &str,
        unauthorized_token: Option<&str>,
        require_enabled: bool,
        refresh: F,
    ) -> Result<Record, CodexError>
    where
        F: FnOnce(Tokens) -> Fut,
        Fut: Future<Output = Result<Tokens, CodexError>>,
    {
        let lock = self
            .inner
            .lock()
            .await
            .refresh_locks
            .get(id)
            .cloned()
            .ok_or_else(CodexError::not_found)?;
        let mut pending = lock.lock().await;
        let mut current = self.record(id, require_enabled).await?;
        if pending.is_none()
            && (current.tokens.needs_refresh()
                || unauthorized_token.is_some_and(|token| token == current.tokens.access_token))
        {
            match refresh(current.tokens.clone()).await {
                Ok(tokens) => *pending = Some(tokens),
                Err(error) => {
                    self.note_error(id, &error).await?;
                    return Err(error);
                }
            }
        }
        if let Some(tokens) = pending.as_ref() {
            let mut inner = self.inner.lock().await;
            let mut accounts = inner.accounts.clone();
            let record = accounts
                .accounts
                .iter_mut()
                .find(|record| record.account.id == id)
                .ok_or_else(CodexError::not_found)?;
            record.tokens = tokens.clone();
            record.account.expires_at = tokens.expires_at();
            record.account.email = tokens.email();
            record.account.plan_type = tokens.plan_type();
            record.account.last_error = if record.verified {
                None
            } else {
                Some("Unverified credentials; refresh to verify before enabling".into())
            };
            current = record.clone();
            self.commit_accounts(&mut inner, accounts)?;
            *pending = None;
        }
        // Recheck deletion/disable that may have happened while an OAuth request was in flight.
        if require_enabled {
            self.record(id, true).await?;
        }
        Ok(current)
    }

    async fn authorized_get(&self, id: &str, url: &'static str) -> Result<Value, CodexError> {
        let mut record = self.credentials(id, None, true).await?;
        let mut response =
            auth::authorized(&self.client(), reqwest::Method::GET, url, &record.tokens)
                .timeout(Duration::from_secs(45))
                .send()
                .await
                .map_err(|_| {
                    CodexError::upstream("Unable to reach the Codex subscription backend")
                })?;
        if response.status() == StatusCode::UNAUTHORIZED {
            record = self
                .credentials(id, Some(&record.tokens.access_token), true)
                .await?;
            response = auth::authorized(&self.client(), reqwest::Method::GET, url, &record.tokens)
                .timeout(Duration::from_secs(45))
                .send()
                .await
                .map_err(|_| {
                    CodexError::upstream("Unable to reach the Codex subscription backend")
                })?;
        }
        if !response.status().is_success() {
            let error = CodexError::upstream_status(
                response.status(),
                "Codex subscription backend rejected the request",
            );
            self.note_error(id, &error).await?;
            return Err(error);
        }
        let mut value = auth::json_body(response).await?;
        record.tokens.redact(&mut value);
        Ok(value)
    }

    fn upsert_account(
        &self,
        inner: &mut Inner,
        tokens: Tokens,
        label: Option<String>,
        usage: &Value,
        verified: bool,
    ) -> Result<Account, CodexError> {
        let mut accounts = inner.accounts.clone();
        let existing = accounts.accounts.iter_mut().find(|record| {
            record.tokens.account_id == tokens.account_id && record.tokens.owner() == tokens.owner()
        });
        let account = if let Some(record) = existing {
            record.tokens = tokens.clone();
            if verified && !record.verified {
                record.account.enabled = true;
            }
            record.verified = verified;
            if !verified {
                record.account.enabled = false;
            }
            record.account.email = tokens.email();
            record.account.plan_type =
                auth::clean_metadata(usage.get("plan_type").and_then(Value::as_str), 64)
                    .or_else(|| tokens.plan_type());
            record.account.expires_at = tokens.expires_at();
            record.account.last_error = if verified {
                None
            } else {
                Some("Unverified credentials; refresh to verify before enabling".into())
            };
            if let Some(label) = label {
                record.account.label = label;
            }
            record.account.clone()
        } else {
            if accounts.accounts.len() >= 128 {
                return Err(CodexError::bad_request("Codex account limit reached (128)"));
            }
            let account = Account {
                id: uuid::Uuid::new_v4().to_string(),
                email: tokens.email(),
                label: label.unwrap_or_else(|| "ChatGPT subscription".into()),
                plan_type: auth::clean_metadata(usage.get("plan_type").and_then(Value::as_str), 64)
                    .or_else(|| tokens.plan_type()),
                enabled: verified,
                expires_at: tokens.expires_at(),
                last_used_at: None,
                last_error: if verified {
                    None
                } else {
                    Some("Unverified credentials; refresh to verify before enabling".into())
                },
            };
            accounts.accounts.push(Record {
                account: account.clone(),
                tokens,
                verified,
            });
            account
        };
        if accounts.active_account_id.is_none() {
            accounts.active_account_id = Some(account.id.clone());
        }
        self.commit_accounts(inner, accounts)?;
        inner
            .refresh_locks
            .entry(account.id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(None)));
        Ok(account)
    }

    async fn import(&self, tokens: Tokens, label: Option<String>) -> Result<Account, CodexError> {
        let _import = self.imports.lock().await;
        let existing = {
            let inner = self.inner.lock().await;
            inner
                .accounts
                .accounts
                .iter()
                .find(|record| {
                    record.tokens.account_id == tokens.account_id
                        && record.tokens.owner() == tokens.owner()
                })
                .map(|record| {
                    (
                        record.account.id.clone(),
                        inner.refresh_locks[&record.account.id].clone(),
                    )
                })
        };
        let mut refresh_guard = match existing.as_ref() {
            Some((_, lock)) => Some(lock.lock().await),
            None => None,
        };
        let staged = {
            let mut inner = self.inner.lock().await;
            if existing
                .as_ref()
                .is_some_and(|(id, _)| !inner.refresh_locks.contains_key(id))
            {
                return Err(CodexError::new(
                    StatusCode::CONFLICT,
                    "Codex account was deleted while import was in progress",
                ));
            }
            // Save imported tokens disabled first. If a refresh rotates credentials, credentials()
            // durably saves the rotation before WHAM verification can use the new access token.
            self.upsert_account(&mut inner, tokens, label, &Value::Null, false)?
        };
        if let Some(guard) = refresh_guard.as_mut() {
            **guard = None;
        }
        drop(refresh_guard);
        let mut record = self.credentials(&staged.id, None, false).await?;
        let usage = match auth::verify(&self.client(), &record.tokens).await {
            Err(error) if error.status == StatusCode::UNAUTHORIZED => {
                record = self
                    .credentials(&staged.id, Some(&record.tokens.access_token), false)
                    .await?;
                auth::verify(&self.client(), &record.tokens).await
            }
            result => result,
        };
        match usage {
            Ok(usage) => self.mark_verified(&record, &usage).await,
            Err(error) => {
                self.note_error(&staged.id, &error).await?;
                Err(error)
            }
        }
    }

    async fn mark_verified(
        &self,
        verified_record: &Record,
        usage: &Value,
    ) -> Result<Account, CodexError> {
        let mut inner = self.inner.lock().await;
        let mut accounts = inner.accounts.clone();
        let record = accounts
            .accounts
            .iter_mut()
            .find(|record| record.account.id == verified_record.account.id)
            .ok_or_else(CodexError::not_found)?;
        if record.tokens.access_token != verified_record.tokens.access_token {
            return Err(CodexError::new(
                StatusCode::CONFLICT,
                "Codex credentials changed during verification; refresh again",
            ));
        }
        if !record.verified {
            record.account.enabled = true;
        }
        record.verified = true;
        record.account.last_error = None;
        record.account.plan_type =
            auth::clean_metadata(usage.get("plan_type").and_then(Value::as_str), 64)
                .or_else(|| record.tokens.plan_type());
        let result = record.account.clone();
        self.commit_accounts(&mut inner, accounts)?;
        Ok(result)
    }

    async fn finish_device(
        &self,
        id: &str,
        tokens: Tokens,
        usage: Value,
    ) -> Result<(), CodexError> {
        let _import = self.imports.lock().await;
        let existing_lock = {
            let inner = self.inner.lock().await;
            inner
                .accounts
                .accounts
                .iter()
                .find(|record| {
                    record.tokens.account_id == tokens.account_id
                        && record.tokens.owner() == tokens.owner()
                })
                .and_then(|record| inner.refresh_locks.get(&record.account.id))
                .cloned()
        };
        let mut refresh_guard = match existing_lock.as_ref() {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        let mut inner = self.inner.lock().await;
        // Cancellation and the final credential commit share this lock: DELETE cannot race a late import.
        let login = inner.devices.get(id).ok_or_else(CodexError::not_found)?;
        if login.state.status != "pending" || login.expires_at <= now() || *login.cancel.borrow() {
            return Err(CodexError::new(
                StatusCode::CONFLICT,
                "Codex device authorization is no longer pending",
            ));
        }
        let account = self.upsert_account(&mut inner, tokens, None, &usage, true)?;
        if let Some(guard) = refresh_guard.as_mut() {
            **guard = None;
        }
        if let Some(login) = inner.devices.get_mut(id) {
            login.state = DeviceStatus {
                status: "completed",
                account_id: Some(account.id),
                error: None,
            };
        }
        Ok(())
    }

    async fn run_device(
        self: Arc<Self>,
        id: String,
        code: auth::DeviceCode,
        mut cancelled: watch::Receiver<bool>,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
        let work = async {
            loop {
                // Never poll faster than the interval returned by the issuer.
                tokio::time::sleep(Duration::from_secs(code.interval().min(15 * 60))).await;
                if let Some(tokens) = auth::poll_device(&self.client(), &code).await? {
                    let usage = auth::verify(&self.client(), &tokens).await?;
                    self.finish_device(&id, tokens, usage).await?;
                    return Ok::<(), CodexError>(());
                }
            }
        };
        let outcome = tokio::select! {
            biased;
            _ = cancelled.changed() => return,
            _ = tokio::time::sleep_until(deadline) => ("expired", Some("Codex device authorization expired after 15 minutes".to_string())),
            result = work => match result {
                Ok(()) => return,
                Err(error) => ("failed", Some(error.message)),
            }
        };
        let mut inner = self.inner.lock().await;
        if let Some(login) = inner.devices.get_mut(&id) {
            if login.state.status == "pending" {
                login.state = DeviceStatus {
                    status: outcome.0,
                    account_id: None,
                    error: outcome.1,
                };
            }
        }
    }
}

#[derive(Deserialize)]
struct ImportBody {
    auth_json: Value,
    label: Option<String>,
}
#[derive(Deserialize)]
struct PatchBody {
    label: Option<String>,
    enabled: Option<bool>,
}

fn parse_json<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, CodexError> {
    body.map(|Json(body)| body)
        .map_err(|error| CodexError::new(error.status(), "Invalid JSON request body"))
}
fn label(value: Option<String>) -> Result<Option<String>, CodexError> {
    value
        .map(|value| {
            let value = value.trim();
            if value.is_empty() {
                return Ok(String::new());
            }
            auth::clean_metadata(Some(value), 800)
                .filter(|value| value.chars().count() <= 200)
                .ok_or_else(|| {
                    CodexError::bad_request(
                        "Label must contain at most 200 characters without control characters",
                    )
                })
        })
        .transpose()
}

async fn accounts(State(state): State<AppState>) -> Json<Value> {
    let inner = state.codex.inner.lock().await;
    Json(
        json!({"accounts": inner.accounts.accounts.iter().map(|record| &record.account).collect::<Vec<_>>(), "active_account_id": inner.accounts.active_account_id}),
    )
}
async fn import(
    State(state): State<AppState>,
    body: Result<Json<ImportBody>, JsonRejection>,
) -> Result<Json<Account>, CodexError> {
    let body = parse_json(body)?;
    let tokens = Tokens::from_auth_json(&body.auth_json)?;
    Ok(Json(state.codex.import(tokens, label(body.label)?).await?))
}
async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<PatchBody>, JsonRejection>,
) -> Result<Json<Account>, CodexError> {
    let body = parse_json(body)?;
    let label = label(body.label)?;
    let mut inner = state.codex.inner.lock().await;
    let mut accounts = inner.accounts.clone();
    let record = accounts
        .accounts
        .iter_mut()
        .find(|record| record.account.id == id)
        .ok_or_else(CodexError::not_found)?;
    if body.enabled == Some(true) && !record.verified {
        return Err(CodexError::bad_request(
            "Refresh this account to verify subscription authorization before enabling it",
        ));
    }
    if let Some(label) = label {
        record.account.label = label;
    }
    if let Some(enabled) = body.enabled {
        record.account.enabled = enabled;
    }
    let result = record.account.clone();
    state.codex.commit_accounts(&mut inner, accounts)?;
    Ok(Json(result))
}
async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, CodexError> {
    let mut inner = state.codex.inner.lock().await;
    let mut accounts = inner.accounts.clone();
    let index = accounts
        .accounts
        .iter()
        .position(|record| record.account.id == id)
        .ok_or_else(CodexError::not_found)?;
    accounts.accounts.remove(index);
    if accounts.active_account_id.as_deref() == Some(&id) {
        accounts.active_account_id = None;
    }
    state.codex.commit_accounts(&mut inner, accounts)?;
    inner.refresh_locks.remove(&id);
    // Pending logins do not yet reveal their identity. Cancel them to prevent resurrection after deletion.
    for login in inner
        .devices
        .values_mut()
        .filter(|login| login.state.status == "pending")
    {
        login.state = DeviceStatus {
            status: "cancelled",
            account_id: None,
            error: None,
        };
        login.cancel.send_replace(true);
    }
    // Keep session tombstones; requests pinned to the deleted ID must fail, never switch accounts.
    Ok(Json(json!({"ok": true})))
}
async fn activate(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, CodexError> {
    let mut inner = state.codex.inner.lock().await;
    let record = inner
        .accounts
        .accounts
        .iter()
        .find(|record| record.account.id == id)
        .ok_or_else(CodexError::not_found)?;
    if !record.account.enabled {
        return Err(CodexError::bad_request(
            "Enable the Codex account before activating it",
        ));
    }
    let mut accounts = inner.accounts.clone();
    accounts.active_account_id = Some(id);
    state.codex.commit_accounts(&mut inner, accounts)?;
    Ok(Json(json!({"ok": true})))
}
async fn refresh(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Account>, CodexError> {
    let before = state.codex.record(&id, false).await?;
    let record = state
        .codex
        .credentials(&id, Some(&before.tokens.access_token), false)
        .await?;
    match auth::verify(&state.codex.client(), &record.tokens).await {
        Ok(usage) => Ok(Json(state.codex.mark_verified(&record, &usage).await?)),
        Err(error) => {
            state.codex.note_error(&id, &error).await?;
            Err(error)
        }
    }
}
async fn usage(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, CodexError> {
    Ok(Json(
        state.codex.authorized_get(&id, auth::USAGE_URL).await?,
    ))
}
async fn models(State(state): State<AppState>) -> Result<Json<Value>, CodexError> {
    let id = state.codex.preferred_account().await?;
    let value = state.codex.authorized_get(&id, auth::MODELS_URL).await?;
    if !value.get("models").is_some_and(Value::is_array) {
        return Err(CodexError::upstream(
            "Codex returned an invalid model catalog",
        ));
    }
    Ok(Json(value))
}
async fn start_device(State(state): State<AppState>) -> Result<Json<Value>, CodexError> {
    {
        let mut inner = state.codex.inner.lock().await;
        inner
            .devices
            .retain(|_, login| login.expires_at > now() - 15 * 60);
        if inner.devices.len() >= 64 {
            return Err(CodexError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many recent Codex authorization attempts; cancel or wait for expiration",
            ));
        }
    }
    let code = auth::start_device(&state.codex.client()).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let expires_at = now() + 15 * 60;
    let result = json!({"id": id, "verification_url": "https://auth.openai.com/codex/device", "user_code": code.user_code, "interval": code.interval(), "expires_at": expires_at});
    let (cancel, receiver) = watch::channel(false);
    let mut inner = state.codex.inner.lock().await;
    if inner.devices.len() >= 64 {
        return Err(CodexError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many concurrent Codex authorizations",
        ));
    }
    inner.devices.insert(
        id.clone(),
        DeviceLogin {
            state: DeviceStatus {
                status: "pending",
                account_id: None,
                error: None,
            },
            expires_at,
            cancel,
        },
    );
    drop(inner);
    tokio::spawn(state.codex.clone().run_device(id, code, receiver));
    Ok(Json(result))
}
async fn device_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DeviceStatus>, CodexError> {
    let inner = state.codex.inner.lock().await;
    let login = inner.devices.get(&id).ok_or_else(CodexError::not_found)?;
    Ok(Json(login.state.clone()))
}
async fn cancel_device(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DeviceStatus>, CodexError> {
    let mut inner = state.codex.inner.lock().await;
    let login = inner
        .devices
        .get_mut(&id)
        .ok_or_else(CodexError::not_found)?;
    if login.state.status == "pending" {
        login.state = DeviceStatus {
            status: "cancelled",
            account_id: None,
            error: None,
        };
        login.cancel.send_replace(true);
    }
    Ok(Json(login.state.clone()))
}
async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    // HTTP 401 here is subscription auth, not the outer admin middleware's gateway credentials.
    if response.status() == StatusCode::UNAUTHORIZED {
        *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route("/accounts", get(accounts))
        .route("/accounts/import", post(import))
        .route("/accounts/:id", axum::routing::patch(patch).delete(delete))
        .route("/accounts/:id/activate", post(activate))
        .route("/accounts/:id/refresh", post(refresh))
        .route("/accounts/:id/usage", get(usage))
        .route("/models", get(models))
        .route("/auth/device", post(start_device))
        .route("/auth/device/:id", get(device_status).delete(cancel_device))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(middleware::from_fn(no_store))
}

pub fn proxy_routes() -> Router<AppState> {
    Router::new()
        .route("/models", get(relay::models))
        .route("/responses", post(relay::responses))
        .route("/responses/compact", post(relay::compact))
        .route("/messages", post(anthropic::messages))
        .route("/messages/count_tokens", post(anthropic::count_tokens))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_device_cannot_commit_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CodexManager::new(temp.path().to_path_buf(), None).unwrap();
        let (cancel, _) = watch::channel(true);
        manager.inner.lock().await.devices.insert(
            "cancelled".into(),
            DeviceLogin {
                state: DeviceStatus {
                    status: "cancelled",
                    account_id: None,
                    error: None,
                },
                expires_at: now() + 900,
                cancel,
            },
        );
        let tokens = Tokens::from_auth_json(&json!({"tokens": {"access_token":"access", "refresh_token":"refresh", "id_token":"id", "account_id":"workspace"}})).unwrap();
        assert!(manager
            .finish_device("cancelled", tokens, json!({}))
            .await
            .is_err());
        assert!(manager.inner.lock().await.accounts.accounts.is_empty());
        let (_, stored) = Vault::open(temp.path().to_path_buf()).unwrap();
        assert!(stored.accounts.is_empty());
    }

    #[tokio::test]
    async fn concurrent_unauthorized_requests_rotate_once_and_publish_only_durable_tokens() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let temp = tempfile::tempdir().unwrap();
        let manager = Arc::new(CodexManager::new(temp.path().to_path_buf(), None).unwrap());
        let old = Tokens::from_auth_json(&json!({"tokens": {"access_token":"old-access", "refresh_token":"old-refresh", "id_token":"id", "account_id":"workspace"}})).unwrap();
        let account = manager
            .upsert_account(
                &mut *manager.inner.lock().await,
                old.clone(),
                None,
                &json!({}),
                true,
            )
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(tokio::sync::Barrier::new(9));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let manager = manager.clone();
            let calls = calls.clone();
            let start = start.clone();
            let id = account.id.clone();
            tasks.push(tokio::spawn(async move {
                start.wait().await;
                let record = manager
                    .credentials_with(&id, Some("old-access"), true, |mut tokens| async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        tokens.access_token = "rotated-access".into();
                        tokens.refresh_token = "rotated-refresh".into();
                        tokens.refreshed_at = now();
                        Ok(tokens)
                    })
                    .await
                    .unwrap();
                record.tokens.refresh_token
            }));
        }
        start.wait().await;
        for task in tasks {
            assert_eq!(task.await.unwrap(), "rotated-refresh");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let (_, durable) = Vault::open(temp.path().to_path_buf()).unwrap();
        assert_eq!(durable.accounts[0].tokens.refresh_token, "rotated-refresh");
        let transient = CodexError::upstream("Authorization service temporarily unavailable");
        manager.note_error(&account.id, &transient).await.unwrap();
        assert!(
            manager
                .record(&account.id, true)
                .await
                .unwrap()
                .account
                .enabled
        );
        manager
            .note_error(&account.id, &CodexError::revoked())
            .await
            .unwrap();
        assert!(manager.record(&account.id, true).await.is_err());
    }
}
