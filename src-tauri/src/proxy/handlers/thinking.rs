use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::proxy::middleware::auth::UserTokenIdentity;
use crate::proxy::server::AppState;
use crate::proxy::thinking_store::{SessionScope, ThinkingStore};

#[derive(Debug, Deserialize, Default)]
pub struct EndSessionBody {
    #[serde(default)]
    pub session_id: Option<String>,
}

fn json_with_session(
    status: StatusCode,
    client_id: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(client_id) {
        headers.insert("X-Session-Id", v);
    }
    (status, headers, Json(body)).into_response()
}

fn resolve_end_key(
    headers: &HeaderMap,
    body_sid: Option<&str>,
    tenant_id: Option<&str>,
) -> Result<SessionScope, (StatusCode, String)> {
    let supplied = body_sid
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .or_else(|| crate::proxy::thinking_store::explicit_session_id(headers, None))
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "session_id is required".to_string(),
            )
        })?;
    if supplied.starts_with("sess-")
        && supplied.len() == 21
        && supplied[5..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        let tenant = crate::proxy::thinking_store::tenant_component(headers, tenant_id);
        let key = format!("{tenant}:{supplied}");
        return Ok(SessionScope {
            client_id: supplied.clone(),
            store_key: key.clone(),
            affinity_id: supplied,
            affinity_key: key,
        });
    }
    let hint = json!({"session_id":supplied});
    Ok(SessionScope::resolve(
        headers,
        Some(&hint),
        None,
        "",
        tenant_id,
    ))
}

/// POST /v1/thinking/end
/// Body: { "session_id": "..." }  (or X-Session-Id header)
pub async fn handle_end_session(
    State(_state): State<AppState>,
    headers: HeaderMap,
    identity: Option<axum::extract::Extension<UserTokenIdentity>>,
    Json(body): Json<EndSessionBody>,
) -> impl IntoResponse {
    match resolve_end_key(
        &headers,
        body.session_id.as_deref(),
        identity.as_ref().map(|identity| identity.token_id.as_str()),
    ) {
        Ok(scope) => {
            let result = ThinkingStore::global().end_session(&scope.store_key);
            tracing::info!(
                "[ThinkingStore] Ended session {} (deleted {} turns, {} bytes)",
                scope.client_id,
                result.deleted_turns,
                result.deleted_bytes
            );
            json_with_session(
                StatusCode::OK,
                &scope.client_id,
                json!({
                    "ok": true,
                    "session_id": result.session_id,
                    "deleted_turns": result.deleted_turns,
                    "deleted_bytes": result.deleted_bytes
                }),
            )
        }
        Err((status, msg)) => (
            status,
            Json(json!({
                "ok": false,
                "error": { "message": msg }
            })),
        )
            .into_response(),
    }
}

/// DELETE /v1/thinking/sessions/:session_id
pub async fn handle_delete_session(
    State(_state): State<AppState>,
    headers: HeaderMap,
    identity: Option<axum::extract::Extension<UserTokenIdentity>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    match resolve_end_key(
        &headers,
        Some(&session_id),
        identity.as_ref().map(|identity| identity.token_id.as_str()),
    ) {
        Ok(scope) => {
            let result = ThinkingStore::global().end_session(&scope.store_key);
            json_with_session(
                StatusCode::OK,
                &scope.client_id,
                json!({
                    "ok": true,
                    "session_id": result.session_id,
                    "deleted_turns": result.deleted_turns,
                    "deleted_bytes": result.deleted_bytes
                }),
            )
        }
        Err((status, msg)) => (
            status,
            Json(json!({
                "ok": false,
                "error": { "message": msg }
            })),
        )
            .into_response(),
    }
}

/// GET /v1/thinking/sessions/:session_id
pub async fn handle_session_stats(
    State(_state): State<AppState>,
    headers: HeaderMap,
    identity: Option<axum::extract::Extension<UserTokenIdentity>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    match resolve_end_key(
        &headers,
        Some(&session_id),
        identity.as_ref().map(|identity| identity.token_id.as_str()),
    ) {
        Ok(scope) => {
            let (turns, bytes) = ThinkingStore::global()
                .session_stats(&scope.store_key)
                .unwrap_or((0, 0));
            json_with_session(
                StatusCode::OK,
                &scope.client_id,
                json!({
                    "ok": true,
                    "session_id": scope.client_id,
                    "turns": turns,
                    "bytes": bytes
                }),
            )
        }
        Err((status, msg)) => (
            status,
            Json(json!({
                "ok": false,
                "error": { "message": msg }
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod cleanup_identity_tests {
    use super::*;
    #[test]
    fn opaque_cleanup_id_is_exact_and_tenant_scoped() {
        let headers = HeaderMap::new();
        let opaque = "sess-0123456789abcdef";
        let first = resolve_end_key(&headers, Some(opaque), Some("tenant-a")).unwrap();
        let second = resolve_end_key(&headers, Some(opaque), Some("tenant-b")).unwrap();
        assert_eq!(first.client_id, opaque);
        assert_eq!(
            first.store_key,
            format!(
                "{}:{opaque}",
                crate::proxy::thinking_store::tenant_component(&headers, Some("tenant-a"))
            )
        );
        assert_ne!(first.store_key, second.store_key);
    }
}
