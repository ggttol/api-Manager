use crate::{
    modules::security_db,
    proxy::{middleware::client_ip::resolve_client_ip, server::AppState},
};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Enforces configured IP policy against the canonical transport identity.
pub async fn ip_filter_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let security = { state.security.read().await.clone() };
    let Some(client_ip) = resolve_client_ip(&request, &security.trusted_proxies) else {
        tracing::warn!("[IP Filter] Unable to extract transport peer");
        return policy_error_response();
    };
    let ip = client_ip.to_string();
    let monitor = security.security_monitor;

    if monitor.whitelist.enabled {
        match security_db::is_ip_in_whitelist(&ip) {
            Ok(true) => return next.run(request).await,
            Ok(false) => {
                return create_blocked_response(
                    &ip,
                    "Access denied. Your IP is not in the whitelist.",
                )
            }
            Err(error) => {
                tracing::error!("[IP Filter] Failed to check whitelist: {error}");
                return policy_error_response();
            }
        }
    }

    if monitor.whitelist.whitelist_priority {
        match security_db::is_ip_in_whitelist(&ip) {
            Ok(true) => return next.run(request).await,
            Ok(false) => {}
            Err(error) => {
                tracing::error!("[IP Filter] Failed to check whitelist priority: {error}");
                return policy_error_response();
            }
        }
    }

    if monitor.blacklist.enabled {
        match security_db::get_blacklist_entry_for_ip(&ip) {
            Ok(Some(entry)) => {
                let reason = entry
                    .reason
                    .as_deref()
                    .unwrap_or("Malicious activity detected");
                save_blocked_log(&request, &ip, reason);
                return create_blocked_response(&ip, &format!("Access denied. Reason: {reason}."));
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!("[IP Filter] Failed to check blacklist: {error}");
                return policy_error_response();
            }
        }
    }

    next.run(request).await
}

fn save_blocked_log(request: &Request, ip: &str, reason: &str) {
    let log = security_db::IpAccessLog {
        id: uuid::Uuid::new_v4().to_string(),
        client_ip: ip.to_owned(),
        timestamp: chrono::Utc::now().timestamp(),
        method: Some(request.method().to_string()),
        path: Some(request.uri().to_string()),
        user_agent: request
            .headers()
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        status: Some(StatusCode::FORBIDDEN.as_u16() as i32),
        duration: Some(0),
        api_key_hash: None,
        blocked: true,
        block_reason: Some(format!("IP in blacklist: {reason}")),
        username: None,
    };
    tokio::spawn(async move {
        if let Err(error) = security_db::save_ip_access_log(&log) {
            tracing::error!("[IP Filter] Failed to save blocked access log: {error}");
        }
    });
}

fn policy_error_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "IP access policy is temporarily unavailable",
    )
        .into_response()
}

fn create_blocked_response(ip: &str, message: &str) -> Response {
    let body = serde_json::json!({
        "error": {"message": message, "type": "ip_blocked", "code": "ip_blocked", "ip": ip}
    });
    (
        StatusCode::FORBIDDEN,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&body).unwrap_or_else(|_| message.to_owned()),
    )
        .into_response()
}
