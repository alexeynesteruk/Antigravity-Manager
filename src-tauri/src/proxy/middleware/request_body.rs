//! Validate the decoded body before request monitoring consumes it.

use axum::{
    body::{Body, Bytes},
    extract::{FromRequest, Request},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Run after decompression and authentication, before monitoring. `Bytes` honors
/// the server's configured `DefaultBodyLimit`, so the limit applies to decoded
/// bytes rather than the compressed transfer size. Axum maps malformed body
/// streams to 400 and decoded size overflow to 413.
pub async fn validate_decoded_body(request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let mut body_request = Request::new(body);
    // The body-limit extension must reach the extractor. Preserve the original
    // parts, including the authenticated identity, for monitoring and handlers.
    *body_request.extensions_mut() = parts.extensions.clone();
    match Bytes::from_request(body_request, &()).await {
        Ok(bytes) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Err(rejection) => rejection.into_response(),
    }
}
