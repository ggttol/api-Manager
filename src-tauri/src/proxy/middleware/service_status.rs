use crate::proxy::server::AppState;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

pub async fn service_status_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();

    // Pause inference routes, but leave management, internal support and auth callback available.
    let inference = path.starts_with("/v1/")
        || path.starts_with("/v1beta/")
        || path.starts_with("/codex/v1/")
        || path == "/responses"
        || path.starts_with("/responses/")
        || path.starts_with("/mcp/")
        || path == "/internal/warmup";
    let always_available = path.starts_with("/api/")
        || (path.starts_with("/internal/") && path != "/internal/warmup")
        || path == "/auth/callback"
        || path == "/health";
    if !inference || always_available {
        return next.run(request).await;
    }

    let running = {
        let r = state.is_running.read().await;
        *r
    };

    if !running {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Proxy service is currently disabled".to_string(),
        )
            .into_response();
    }

    next.run(request).await
}
