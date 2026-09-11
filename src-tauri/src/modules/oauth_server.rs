use crate::modules::oauth;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use tauri::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::sync::watch;

const MAX_CALLBACK_HEADER_BYTES: usize = 16 * 1024;
const CALLBACK_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

struct OAuthFlowState {
    flow_id: String,
    auth_url: String,
    #[allow(dead_code)]
    redirect_uri: String,
    state: String,
    client_key: String,
    cancel_tx: watch::Sender<bool>,
    preparation_tx: watch::Sender<bool>,
    preparing: bool,
    code_tx: mpsc::Sender<Result<String, String>>,
    code_rx: Option<mpsc::Receiver<Result<String, String>>>,
}

static OAUTH_FLOW_STATE: OnceLock<Mutex<Option<OAuthFlowState>>> = OnceLock::new();

fn get_oauth_flow_state() -> &'static Mutex<Option<OAuthFlowState>> {
    OAUTH_FLOW_STATE.get_or_init(|| Mutex::new(None))
}

fn oauth_success_html() -> &'static str {
    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
    <html>\
    <body style='font-family: sans-serif; text-align: center; padding: 50px;'>\
    <h1 style='color: green;'>✅ Authorization Successful!</h1>\
    <p>You can close this window and return to the application.</p>\
    <script>setTimeout(function() { window.close(); }, 2000);</script>\
    </body>\
    </html>"
}

fn oauth_fail_html() -> &'static str {
    "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
    <html>\
    <body style='font-family: sans-serif; text-align: center; padding: 50px;'>\
    <h1 style='color: red;'>❌ Authorization Failed</h1>\
    <p>Failed to obtain Authorization Code. Please return to the app and try again.</p>\
    </body>\
    </html>"
}

async fn read_callback_header(
    stream: &mut TcpStream,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<String, ()> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if *cancel_rx.borrow() {
            return Err(());
        }
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(bytes).map_err(|_| ());
        }
        if bytes.len() >= MAX_CALLBACK_HEADER_BYTES {
            return Err(());
        }
        let read_limit = (MAX_CALLBACK_HEADER_BYTES - bytes.len()).min(chunk.len());
        let read = tokio::select! {
            _ = cancel_rx.changed() => return Err(()),
            result = tokio::time::timeout(
                CALLBACK_READ_TIMEOUT,
                stream.read(&mut chunk[..read_limit]),
            ) => result,
        }
        .map_err(|_| ())?
        .map_err(|_| ())?;
        if read == 0 {
            return Err(());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

fn callback_code(request: &str, expected_state: &str) -> Option<String> {
    let mut parts = request.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let url = Url::parse(&format!("http://localhost{}", parts.next()?)).ok()?;
    if url.path() != "/oauth-callback" {
        return None;
    }
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    if state.as_deref() == Some(expected_state) {
        code
    } else {
        None
    }
}

async fn serve_callback_listener(
    listener: TcpListener,
    expected_state: String,
    code_tx: mpsc::Sender<Result<String, String>>,
    mut cancel_rx: watch::Receiver<bool>,
    app_handle: Option<tauri::AppHandle>,
    accepted_tx: Option<mpsc::UnboundedSender<()>>,
) {
    let mut accepted_tx = accepted_tx;
    loop {
        if *cancel_rx.borrow() {
            return;
        }
        let accepted = tokio::select! {
            _ = cancel_rx.changed() => return,
            result = listener.accept() => result,
        };
        let Ok((mut stream, _)) = accepted else {
            return;
        };
        if *cancel_rx.borrow() {
            return;
        }
        if let Some(accepted_tx) = accepted_tx.take() {
            let _ = accepted_tx.send(());
        }
        let Some(code) = read_callback_header(&mut stream, &mut cancel_rx)
            .await
            .ok()
            .and_then(|request| callback_code(&request, &expected_state))
        else {
            if *cancel_rx.borrow() {
                return;
            }
            let _ = stream.write_all(oauth_fail_html().as_bytes()).await;
            let _ = stream.flush().await;
            continue;
        };
        if *cancel_rx.borrow() {
            return;
        }
        let _ = stream.write_all(oauth_success_html().as_bytes()).await;
        let _ = stream.flush().await;
        if *cancel_rx.borrow() {
            return;
        }
        if let Some(handle) = app_handle.as_ref() {
            use tauri::Emitter;
            let _ = handle.emit("oauth-callback-received", ());
        }
        tokio::select! {
            _ = cancel_rx.changed() => return,
            _ = code_tx.send(Ok(code)) => return,
        }
    }
}

fn clear_flow_if_current(flow_id: &str) {
    if let Ok(mut lock) = get_oauth_flow_state().lock() {
        if lock.as_ref().is_some_and(|state| state.flow_id == flow_id) {
            if let Some(state) = lock.take() {
                let _ = state.cancel_tx.send(true);
            }
        }
    }
}

fn flow_is_current(flow_id: &str, cancel_rx: &watch::Receiver<bool>) -> bool {
    !*cancel_rx.borrow()
        && get_oauth_flow_state()
            .lock()
            .ok()
            .and_then(|state| state.as_ref().map(|state| state.flow_id == flow_id))
            .unwrap_or(false)
}

async fn await_or_cancel<T>(
    future: impl Future<Output = Result<T, String>>,
    mut cancel_rx: watch::Receiver<bool>,
) -> Result<T, String> {
    if *cancel_rx.borrow() {
        return Err("OAuth cancelled".to_string());
    }
    let result = tokio::select! {
        biased;
        _ = cancel_rx.changed() => return Err("OAuth cancelled".to_string()),
        result = future => result,
    };
    if *cancel_rx.borrow() {
        Err("OAuth cancelled".to_string())
    } else {
        result
    }
}

async fn exchange_code_cancellable(
    code: &str,
    redirect_uri: &str,
    client_key: &str,
    cancel_rx: watch::Receiver<bool>,
) -> Result<oauth::TokenResponse, String> {
    await_or_cancel(
        oauth::exchange_code_with_client(code, redirect_uri, Some(client_key)),
        cancel_rx,
    )
    .await
}

fn take_flow_receiver() -> Result<
    (
        String,
        mpsc::Receiver<Result<String, String>>,
        String,
        String,
        watch::Receiver<bool>,
    ),
    String,
> {
    let mut lock = get_oauth_flow_state()
        .lock()
        .map_err(|_| "OAuth state lock corrupted".to_string())?;
    let state = lock
        .as_mut()
        .ok_or_else(|| "OAuth state does not exist".to_string())?;
    let receiver = state
        .code_rx
        .take()
        .ok_or_else(|| "OAuth authorization already in progress".to_string())?;
    Ok((
        state.flow_id.clone(),
        receiver,
        state.redirect_uri.clone(),
        state.client_key.clone(),
        state.cancel_tx.subscribe(),
    ))
}

async fn wait_for_code(
    mut receiver: mpsc::Receiver<Result<String, String>>,
    cancel_rx: watch::Receiver<bool>,
) -> Result<String, String> {
    await_or_cancel(
        async move {
            match receiver.recv().await {
                Some(result) => result,
                None => Err("OAuth flow channel closed unexpectedly".to_string()),
            }
        },
        cancel_rx,
    )
    .await
}

async fn ensure_oauth_flow_prepared(
    app_handle: Option<tauri::AppHandle>,
    requested_client_key: Option<String>,
) -> Result<String, String> {
    let requested_client_key = requested_client_key
        .as_deref()
        .map(|key| key.trim().to_ascii_lowercase())
        .filter(|key| !key.is_empty())
        .unwrap_or(oauth::get_active_oauth_client_key()?);

    let (flow_id, cancel_rx, code_tx) = loop {
        let wait_for_preparation = {
            let mut lock = get_oauth_flow_state()
                .lock()
                .map_err(|_| "OAuth state lock corrupted".to_string())?;
            match lock.as_mut() {
                Some(flow) if flow.preparing && flow.client_key == requested_client_key => Some((
                    flow.flow_id.clone(),
                    flow.cancel_tx.subscribe(),
                    flow.preparation_tx.subscribe(),
                )),
                Some(flow)
                    if !flow.preparing
                        && flow.client_key == requested_client_key
                        && flow.code_rx.is_some() =>
                {
                    return Ok(flow.auth_url.clone());
                }
                Some(_) => {
                    if let Some(previous) = lock.take() {
                        let _ = previous.cancel_tx.send(true);
                        let _ = previous.preparation_tx.send(true);
                    }
                    None
                }
                None => None,
            }
        };
        if let Some((waiting_flow_id, cancel_rx, mut prepared_rx)) = wait_for_preparation {
            let _ = prepared_rx.changed().await;
            if !flow_is_current(&waiting_flow_id, &cancel_rx) {
                return Err("OAuth cancelled".to_string());
            }
            continue;
        }

        let flow_id = uuid::Uuid::new_v4().to_string();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (preparation_tx, _) = watch::channel(false);
        let (code_tx, code_rx) = mpsc::channel::<Result<String, String>>(1);
        let mut lock = get_oauth_flow_state()
            .lock()
            .map_err(|_| "OAuth state lock corrupted".to_string())?;
        if lock.is_none() {
            *lock = Some(OAuthFlowState {
                flow_id: flow_id.clone(),
                auth_url: String::new(),
                redirect_uri: String::new(),
                state: String::new(),
                client_key: requested_client_key.clone(),
                cancel_tx: cancel_tx.clone(),
                preparation_tx: preparation_tx.clone(),
                preparing: true,
                code_tx: code_tx.clone(),
                code_rx: Some(code_rx),
            });
            break (flow_id, cancel_rx, code_tx);
        }
    };

    let preparation_result = async {
        let mut ipv4_listener = None;
        let mut ipv6_listener = None;
        let port;
        match TcpListener::bind("[::1]:0").await {
            Ok(listener) => {
                port = listener
                    .local_addr()
                    .map_err(|error| format!("failed_to_get_local_port: {}", error))?
                    .port();
                ipv6_listener = Some(listener);
                if let Ok(listener) = TcpListener::bind(format!("127.0.0.1:{port}")).await {
                    ipv4_listener = Some(listener);
                }
            }
            Err(_) => {
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .map_err(|error| format!("failed_to_bind_local_port: {}", error))?;
                port = listener
                    .local_addr()
                    .map_err(|error| format!("failed_to_get_local_port: {}", error))?
                    .port();
                ipv4_listener = Some(listener);
                if let Ok(listener) = TcpListener::bind(format!("[::1]:{port}")).await {
                    ipv6_listener = Some(listener);
                }
            }
        }

        if !flow_is_current(&flow_id, &cancel_rx) {
            return Err("OAuth cancelled".to_string());
        }
        let redirect_uri = match (ipv4_listener.is_some(), ipv6_listener.is_some()) {
            (true, true) => format!("http://localhost:{port}/oauth-callback"),
            (true, false) => format!("http://127.0.0.1:{port}/oauth-callback"),
            (false, true) => format!("http://[::1]:{port}/oauth-callback"),
            (false, false) => return Err("failed_to_bind_local_port".to_string()),
        };
        let state = uuid::Uuid::new_v4().to_string();
        let (auth_url, resolved_client_key) =
            oauth::get_auth_url_with_client(&redirect_uri, &state, Some(&requested_client_key))?;

        let mut lock = get_oauth_flow_state()
            .lock()
            .map_err(|_| "OAuth state lock corrupted".to_string())?;
        let flow = lock
            .as_mut()
            .filter(|flow| flow.flow_id == flow_id && flow.preparing && !*cancel_rx.borrow())
            .ok_or_else(|| "OAuth cancelled".to_string())?;
        flow.auth_url = auth_url.clone();
        flow.redirect_uri = redirect_uri;
        flow.state = state.clone();
        flow.client_key = resolved_client_key;
        flow.preparing = false;
        let _ = flow.preparation_tx.send(true);

        if let Some(listener) = ipv4_listener {
            tokio::spawn(serve_callback_listener(
                listener,
                state.clone(),
                code_tx.clone(),
                cancel_rx.clone(),
                app_handle.clone(),
                None,
            ));
        }
        if let Some(listener) = ipv6_listener {
            tokio::spawn(serve_callback_listener(
                listener,
                state,
                code_tx,
                cancel_rx,
                app_handle.clone(),
                None,
            ));
        }
        Ok(auth_url)
    }
    .await;

    if preparation_result.is_err() {
        clear_flow_if_current(&flow_id);
    }
    preparation_result
}

/// Pre-generate OAuth URL (does not open browser, does not block waiting for callback)
pub async fn prepare_oauth_url(
    app_handle: Option<tauri::AppHandle>,
    oauth_client_key: Option<String>,
) -> Result<String, String> {
    ensure_oauth_flow_prepared(app_handle, oauth_client_key).await
}

/// Cancel current OAuth flow
pub fn cancel_oauth_flow() {
    if let Ok(mut state) = get_oauth_flow_state().lock() {
        if let Some(s) = state.take() {
            let _ = s.cancel_tx.send(true);
            crate::modules::logger::log_info("Sent OAuth cancellation signal");
        }
    }
}

/// Start OAuth flow and wait for a callback before exchanging it for a token.
pub async fn start_oauth_flow(
    app_handle: Option<tauri::AppHandle>,
    oauth_client_key: Option<String>,
) -> Result<oauth::TokenResponse, String> {
    let auth_url = ensure_oauth_flow_prepared(app_handle.clone(), oauth_client_key).await?;
    if let Some(handle) = app_handle {
        use tauri_plugin_opener::OpenerExt;
        handle
            .opener()
            .open_url(&auth_url, None::<String>)
            .map_err(|error| format!("failed_to_open_browser: {}", error))?;
    }
    let (flow_id, receiver, redirect_uri, client_key, cancel_rx) = take_flow_receiver()?;
    let result = match wait_for_code(receiver, cancel_rx.clone()).await {
        Ok(code) => exchange_code_cancellable(&code, &redirect_uri, &client_key, cancel_rx).await,
        Err(error) => Err(error),
    };
    clear_flow_if_current(&flow_id);
    result
}

/// Complete a prepared desktop OAuth flow without opening a browser.
pub async fn complete_oauth_flow(
    app_handle: Option<tauri::AppHandle>,
) -> Result<oauth::TokenResponse, String> {
    ensure_oauth_flow_prepared(app_handle, None).await?;
    let (flow_id, receiver, redirect_uri, client_key, cancel_rx) = take_flow_receiver()?;
    let result = match wait_for_code(receiver, cancel_rx.clone()).await {
        Ok(code) => exchange_code_cancellable(&code, &redirect_uri, &client_key, cancel_rx).await,
        Err(error) => Err(error),
    };
    clear_flow_if_current(&flow_id);
    result
}
async fn persist_oauth_token(
    token: oauth::TokenResponse,
    flow_id: &str,
    cancel_rx: watch::Receiver<bool>,
) -> Result<crate::models::Account, String> {
    let refresh_token = token
        .refresh_token
        .ok_or_else(|| "未获取到 Refresh Token。请撤销权限后重试。".to_string())?;
    let temporary_account_id = uuid::Uuid::new_v4().to_string();
    let user_info = await_or_cancel(
        oauth::get_user_info(&token.access_token, Some(&temporary_account_id)),
        cancel_rx.clone(),
    )
    .await?;
    let project_id = await_or_cancel(
        async {
            Ok::<Option<String>, String>(
                crate::proxy::project_resolver::fetch_project_id(&token.access_token)
                    .await
                    .ok(),
            )
        },
        cancel_rx.clone(),
    )
    .await?;
    let display_name = user_info.get_display_name();
    let token_data = crate::models::TokenData::new(
        token.access_token,
        refresh_token,
        token.expires_in,
        Some(user_info.email.clone()),
        project_id,
        None,
        false,
        token.id_token,
    )
    .with_oauth_client_key(token.oauth_client_key);

    // Holding this short critical section makes ownership validation and the
    // synchronous account write one commit boundary: cancellation cannot land
    // between them and a replacement flow is never cleared by this completion.
    let lock = get_oauth_flow_state()
        .lock()
        .map_err(|_| "OAuth state lock corrupted".to_string())?;
    if *cancel_rx.borrow() || !lock.as_ref().is_some_and(|flow| flow.flow_id == flow_id) {
        return Err("OAuth cancelled".to_string());
    }
    crate::modules::upsert_account(user_info.email, display_name, token_data)
}

/// Create a state-bound web OAuth authorization URL. The matching callback must
/// be completed with [`complete_web_oauth`] exactly once.
pub async fn prepare_web_oauth_url(redirect_uri: String) -> Result<String, String> {
    let state = uuid::Uuid::new_v4().to_string();
    let (auth_url, client_key) = oauth::get_auth_url_with_client(&redirect_uri, &state, None)?;
    let (cancel_tx, _) = watch::channel(false);
    let (preparation_tx, _) = watch::channel(true);
    let (code_tx, code_rx) = mpsc::channel(1);
    let mut lock = get_oauth_flow_state()
        .lock()
        .map_err(|_| "OAuth state lock corrupted".to_string())?;
    if let Some(previous) = lock.take() {
        let _ = previous.cancel_tx.send(true);
    }
    *lock = Some(OAuthFlowState {
        flow_id: uuid::Uuid::new_v4().to_string(),
        auth_url: auth_url.clone(),
        redirect_uri,
        state,
        client_key,
        preparation_tx,
        preparing: false,
        cancel_tx,
        code_tx,
        code_rx: Some(code_rx),
    });
    Ok(auth_url)
}

/// Validate and consume one web OAuth callback, then persist the resulting account.
pub async fn complete_web_oauth(
    code: String,
    state: String,
) -> Result<crate::models::Account, String> {
    let (flow_id, redirect_uri, client_key, cancel_rx) = {
        let mut lock = get_oauth_flow_state()
            .lock()
            .map_err(|_| "OAuth state lock corrupted".to_string())?;
        let flow = lock
            .as_mut()
            .ok_or_else(|| "No active OAuth flow found".to_string())?;
        if flow.state != state {
            return Err("OAuth state mismatch (CSRF protection)".to_string());
        }
        if flow.code_rx.take().is_none() {
            return Err("OAuth state has already been consumed".to_string());
        }
        (
            flow.flow_id.clone(),
            flow.redirect_uri.clone(),
            flow.client_key.clone(),
            flow.cancel_tx.subscribe(),
        )
    };
    let result = async {
        let token =
            exchange_code_cancellable(&code, &redirect_uri, &client_key, cancel_rx.clone()).await?;
        persist_oauth_token(token, &flow_id, cancel_rx).await
    }
    .await;
    clear_flow_if_current(&flow_id);
    result
}

/// Manually submit an OAuth code to complete the flow.
/// This is used when the user manually copies the code/URL from the browser
/// because the localhost callback couldn't be reached (e.g. in Docker/remote).
pub async fn submit_oauth_code(
    code_input: String,
    state_input: Option<String>,
) -> Result<(), String> {
    let tx = {
        let lock = get_oauth_flow_state().lock().map_err(|e| e.to_string())?;
        if let Some(state) = lock.as_ref() {
            // Verify state if provided
            if let Some(provided_state) = state_input {
                if provided_state != state.state {
                    return Err("OAuth state mismatch (CSRF protection)".to_string());
                }
            }
            state.code_tx.clone()
        } else {
            return Err("No active OAuth flow found".to_string());
        }
    };

    // Extract code if it's a URL
    let code = if code_input.starts_with("http") {
        if let Ok(url) = Url::parse(&code_input) {
            url.query_pairs()
                .find(|(k, _)| k == "code")
                .map(|(_, v)| v.to_string())
                .unwrap_or(code_input)
        } else {
            code_input
        }
    } else {
        code_input
    };

    crate::modules::logger::log_info("Received manual OAuth code submission");

    // Send to the channel
    tx.send(Ok(code))
        .await
        .map_err(|_| "Failed to send code to OAuth flow (receiver dropped)".to_string())?;

    Ok(())
}
/// Manually prepare an OAuth flow without starting listeners.
/// Useful for Web/Docker environments where we only need manual code submission.
pub fn prepare_oauth_flow_manually(
    redirect_uri: String,
    state_str: String,
    oauth_client_key: Option<String>,
) -> Result<(String, mpsc::Receiver<Result<String, String>>), String> {
    let (auth_url, resolved_client_key) =
        oauth::get_auth_url_with_client(&redirect_uri, &state_str, oauth_client_key.as_deref())?;

    // Check if we can reuse existing state
    if let Ok(mut lock) = get_oauth_flow_state().lock() {
        if let Some(s) = lock.as_mut() {
            // If we already have a code_rx, we can't easily "steal" it again because it's already returned.
            // But if this is a NEW request (different state), we should overwrite.
            // For now, let's just clear and restart to be safe.
            let _ = s.cancel_tx.send(true);
            *lock = None;
        }
    }

    let (cancel_tx, _cancel_rx) = watch::channel(false);
    let (preparation_tx, _) = watch::channel(true);
    let (code_tx, code_rx) = mpsc::channel(1);

    if let Ok(mut state) = get_oauth_flow_state().lock() {
        *state = Some(OAuthFlowState {
            flow_id: uuid::Uuid::new_v4().to_string(),
            auth_url: auth_url.clone(),
            redirect_uri,
            state: state_str,
            client_key: resolved_client_key,
            cancel_tx,
            preparation_tx,
            preparing: false,
            code_tx,
            code_rx: None, // The legacy web caller owns this receiver.
        });
    }

    Ok((auth_url, code_rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    static FLOW_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn concurrent_prepare_returns_the_single_reserved_flow() {
        let _flow = FLOW_TEST_LOCK.lock().await;
        use std::sync::Arc;
        use tokio::sync::Barrier;

        cancel_oauth_flow();
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = barrier.clone();
        let first = tokio::spawn(async move {
            first_barrier.wait().await;
            prepare_oauth_url(None, None).await
        });
        let second_barrier = barrier.clone();
        let second = tokio::spawn(async move {
            second_barrier.wait().await;
            prepare_oauth_url(None, None).await
        });
        barrier.wait().await;

        let first_url = first.await.unwrap().unwrap();
        let second_url = second.await.unwrap().unwrap();
        assert_eq!(first_url, second_url);
        let state = get_oauth_flow_state().lock().unwrap();
        assert_eq!(state.as_ref().unwrap().auth_url, first_url);
        drop(state);
        cancel_oauth_flow();
    }

    #[tokio::test]
    async fn cancellation_wins_while_an_exchange_is_blocked() {
        use std::sync::Arc;
        use tokio::sync::{oneshot, Barrier};

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let barrier = Arc::new(Barrier::new(2));
        let exchange_barrier = barrier.clone();
        let (entered_tx, entered_rx) = oneshot::channel();
        let exchange = tokio::spawn(async move {
            await_or_cancel(
                async move {
                    let _ = entered_tx.send(());
                    exchange_barrier.wait().await;
                    Ok::<_, String>(())
                },
                cancel_rx,
            )
            .await
        });

        entered_rx.await.unwrap();
        cancel_tx.send(true).unwrap();
        barrier.wait().await;
        assert_eq!(exchange.await.unwrap(), Err("OAuth cancelled".to_string()));
    }

    #[tokio::test]
    async fn stale_completion_does_not_clear_a_replacement_flow() {
        let _flow = FLOW_TEST_LOCK.lock().await;
        cancel_oauth_flow();
        prepare_web_oauth_url("http://localhost/first".to_string())
            .await
            .unwrap();
        let first_id = get_oauth_flow_state()
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .flow_id
            .clone();
        let replacement_url = prepare_web_oauth_url("http://localhost/second".to_string())
            .await
            .unwrap();

        clear_flow_if_current(&first_id);
        let state = get_oauth_flow_state().lock().unwrap();
        assert_eq!(state.as_ref().unwrap().auth_url, replacement_url);
        drop(state);
        cancel_oauth_flow();
    }

    #[tokio::test]
    async fn callback_listener_ignores_unrelated_and_wrong_state_before_split_valid_callback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, mut receiver) = mpsc::channel(1);
        let (_cancel_sender, cancel_receiver) = watch::channel(false);
        let task = tokio::spawn(serve_callback_listener(
            listener,
            "expected".to_string(),
            sender,
            cancel_receiver,
            None,
            None,
        ));

        let mut unrelated = TcpStream::connect(address).await.unwrap();
        unrelated
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut wrong_state = TcpStream::connect(address).await.unwrap();
        wrong_state
            .write_all(b"GET /oauth-callback?code=wrong&state=nope HTTP/1.1\r\n")
            .await
            .unwrap();
        wrong_state
            .write_all(b"Host: localhost\r\n\r\n")
            .await
            .unwrap();

        let mut valid = TcpStream::connect(address).await.unwrap();
        valid
            .write_all(b"GET /oauth-callback?code=valid&state=expected HTTP/1.1\r\n")
            .await
            .unwrap();
        valid.write_all(b"Host: localhost\r\n\r\n").await.unwrap();
        assert_eq!(receiver.recv().await.unwrap().unwrap(), "valid");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_accepted_stalled_callback_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, _receiver) = mpsc::channel(1);
        let (cancel_sender, cancel_receiver) = watch::channel(false);
        let (accepted_sender, mut accepted_receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(serve_callback_listener(
            listener,
            "expected".to_string(),
            sender,
            cancel_receiver,
            None,
            Some(accepted_sender),
        ));
        let _stalled = TcpStream::connect(address).await.unwrap();
        accepted_receiver.recv().await.unwrap();
        cancel_sender.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("listener should stop after cancellation")
            .unwrap();
    }
}
