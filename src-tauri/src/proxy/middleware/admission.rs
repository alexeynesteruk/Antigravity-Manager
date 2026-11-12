use crate::proxy::{
    pipeline::{
        admission::{AdmissionError, SafetyController},
        budget,
    },
    server::AppState,
};
use axum::{
    body::{Body, HttpBody},
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use futures::StreamExt;

/// IDs come from authentication, never a caller-supplied device header or forwarded IP.
pub fn client_id(identity: Option<&super::auth::UserTokenIdentity>, internal: bool) -> String {
    if internal {
        "system".into()
    } else {
        identity
            .map(|identity| format!("device:{}", identity.token_id))
            .unwrap_or_else(|| "shared-gateway".into())
    }
}

pub fn requires_admission(method: &axum::http::Method, path: &str) -> bool {
    *method == axum::http::Method::POST
        && !path.contains("event_logging")
        && !path.starts_with("/v1/thinking/")
}

pub async fn admission_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !requires_admission(request.method(), request.uri().path()) {
        return next.run(request).await;
    }
    let safety = match state.token_manager.safety_controller() {
        Ok(safety) => safety,
        Err(_) => return AdmissionError::unavailable().response(),
    };
    admit_with_controller(safety, request, next).await
}

pub(crate) async fn admit_with_controller(
    safety: std::sync::Arc<SafetyController>,
    request: Request,
    next: Next,
) -> Response {
    let identity = client_id(
        request.extensions().get::<super::auth::UserTokenIdentity>(),
        request.uri().path().starts_with("/internal/"),
    );
    let permit = match tokio::task::spawn_blocking(move || {
        safety.admit(&identity, chrono::Utc::now().timestamp())
    })
    .await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(error)) => return error.response(),
        Err(_) => return AdmissionError::unavailable().response(),
    };
    let context =
        budget::current().unwrap_or_else(|| budget::RequestContext::new(budget::deadline_now()));
    context.set_safety(permit.clone());
    let response = budget::scope(context, next.run(request)).await;
    // Legacy adapters return typed errors as strings. Normalize small complete
    // error bodies here so image/edit/audio responses also retain Retry-After.
    let response = if matches!(response.status().as_u16(), 400 | 429)
        && response
            .body()
            .size_hint()
            .upper()
            .is_some_and(|length| length <= 65536)
    {
        let (parts, body) = response.into_parts();
        match axum::body::to_bytes(body, 65536).await {
            Ok(bytes) => match std::str::from_utf8(&bytes)
                .ok()
                .and_then(crate::proxy::pipeline::admission::response_from_encoded)
            {
                Some(response) => response,
                None => Response::from_parts(parts, Body::from(bytes)),
            },
            Err(_) => return AdmissionError::unavailable().response(),
        }
    } else {
        response
    };
    let (parts, body) = response.into_parts();
    // Keep admission context alive until the stream finishes or the receiver drops it.
    let mut stream = body.into_data_stream();
    let wrapped = async_stream::stream! { let _permit = permit; while let Some(chunk) = stream.next().await { yield chunk; } };
    Response::from_parts(parts, Body::from_stream(wrapped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::{Method, StatusCode},
        routing::{get, post},
        Router,
    };
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_gate(
        State(safety): State<Arc<SafetyController>>,
        request: Request,
        next: Next,
    ) -> Response {
        if requires_admission(request.method(), request.uri().path()) {
            admit_with_controller(safety, request, next).await
        } else {
            next.run(request).await
        }
    }

    #[tokio::test]
    async fn every_protocol_and_images_share_master_budget_without_health_or_admin_lockout() {
        let dir = tempfile::tempdir().unwrap();
        let safety = SafetyController::for_dir(dir.path()).unwrap();
        safety.configure(crate::proxy::pipeline::admission::AccountSafetyConfig {
            client_requests_per_minute: 1,
            ..Default::default()
        });
        let mut app = Router::new().route("/healthz", get(|| async { "ok" }));
        for path in [
            "/v1/chat/completions",
            "/v1/responses",
            "/v1/messages",
            "/v1beta/models/gemini:generateContent",
            "/v1/images/generations",
            "/v1/images/edits",
            "/internal/warmup",
        ] {
            app = app.route(path, post(|| async { "ok" }));
        }
        let app = app.layer(axum::middleware::from_fn_with_state(
            safety.clone(),
            test_gate,
        ));
        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        for path in [
            "/v1/responses",
            "/v1/messages",
            "/v1beta/models/gemini:generateContent",
            "/v1/images/generations",
            "/v1/images/edits",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(path)
                        .header("x-device-id", "spoofed-new-device")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS, "{path}");
            assert!(response.headers().contains_key("retry-after"));
        }
        safety.halt("OPERATOR_STOP").unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/internal/warmup")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            app.oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        assert!(!requires_admission(&Method::OPTIONS, "/v1/responses"));
    }
}
