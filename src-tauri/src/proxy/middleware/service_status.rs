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
    let always_allowed = match allowed_while_stopped(&request) {
        Ok(allowed) => allowed,
        Err(status) => return status.into_response(),
    };
    // Management and verified internal traffic must not wait for service-state writes.
    let running = if always_allowed {
        true
    } else {
        *state.is_running.read().await
    };
    service_status_middleware_with_running(running, request, next).await
}

fn allowed_while_stopped(request: &Request) -> Result<bool, StatusCode> {
    crate::proxy::middleware::internal::require_validated_internal_request(request)?;
    let path = request.uri().path();
    Ok(path.starts_with("/api/")
        || path.starts_with("/internal/")
        || path == "/auth/callback"
        || path == "/health")
}

/// Share the production gate with router integration tests without recreating AppState.
pub(crate) async fn service_status_middleware_with_running(
    running: bool,
    request: Request,
    next: Next,
) -> Response {
    let always_allowed = match allowed_while_stopped(&request) {
        Ok(allowed) => allowed,
        Err(status) => return status.into_response(),
    };
    if !running && !always_allowed {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Proxy service is currently disabled".to_string(),
        )
            .into_response();
    }

    next.run(request).await
}
