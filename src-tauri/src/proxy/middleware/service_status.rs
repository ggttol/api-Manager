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

    // Pause inference, not the management UI or its static assets.
    let inference = path.starts_with("/v1/")
        || path.starts_with("/v1beta/")
        || path.starts_with("/codex/v1/")
        || path == "/responses"
        || path.starts_with("/responses/")
        || path.starts_with("/mcp/");
    if !inference {
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
