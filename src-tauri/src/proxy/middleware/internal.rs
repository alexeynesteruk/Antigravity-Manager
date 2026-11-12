use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use once_cell::sync::Lazy;

pub const INTERNAL_REQUEST_CREDENTIAL_HEADER: &str = "x-antigravity-internal-credential";
static INTERNAL_REQUEST_CREDENTIAL: Lazy<String> = Lazy::new(|| {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
});

pub(crate) fn internal_request_credential() -> &'static str {
    INTERNAL_REQUEST_CREDENTIAL.as_str()
}

/// Validate the destination before attaching process credentials or account tokens.
/// Internal routes accept direct loopback peers, so a specific LAN listener
/// cannot serve these calls and must fail before making a network request.
pub(crate) fn warmup_url(base_url: &str) -> Result<String, String> {
    let origin = url::Url::parse(base_url).map_err(|_| "Invalid local proxy origin".to_string())?;
    let loopback = match origin.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|address| address.is_loopback())
        }
        _ => false,
    };
    if origin.scheme() != "http" || !loopback {
        return Err("Internal warmup requires a loopback or wildcard listener; restart with a compatible bind address".into());
    }
    Ok(format!(
        "{}/internal/warmup",
        base_url.trim_end_matches('/')
    ))
}

#[derive(Clone)]
struct ValidatedInternalRequest;

fn is_internal(request: &Request) -> bool {
    request.uri().path().starts_with("/internal/")
}

/// Only the outer guard can add this process-local marker.
pub fn require_validated_internal_request(request: &Request) -> Result<(), StatusCode> {
    if is_internal(request)
        && request
            .extensions()
            .get::<ValidatedInternalRequest>()
            .is_none()
    {
        Err(StatusCode::FORBIDDEN)
    } else {
        Ok(())
    }
}

/// Must run outside CORS and service-status middleware, including for OPTIONS requests.
pub async fn internal_loopback_middleware(mut request: Request, next: Next) -> Response {
    if is_internal(&request) {
        if !crate::proxy::middleware::ip_filter::is_direct_loopback(&request) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let mut values = request
            .headers()
            .get_all(INTERNAL_REQUEST_CREDENTIAL_HEADER)
            .iter();
        let valid = matches!((values.next(), values.next()), (Some(value), None)
            if value.as_bytes() == internal_request_credential().as_bytes());
        if !valid {
            return StatusCode::FORBIDDEN.into_response();
        }
        request.extensions_mut().insert(ValidatedInternalRequest);
    }
    // Never forward an accidentally supplied private credential to any handler or log.
    request
        .headers_mut()
        .remove(INTERNAL_REQUEST_CREDENTIAL_HEADER);
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{HeaderMap, Method},
        routing::any,
        Router,
    };
    use std::net::SocketAddr;
    use tower::ServiceExt;

    #[test]
    fn warmup_destinations_are_validated_before_credentials_are_attached() {
        for origin in ["http://127.0.0.1:8045", "http://[::1]:8045"] {
            assert_eq!(
                warmup_url(origin).unwrap(),
                format!("{origin}/internal/warmup")
            );
        }
        for origin in [
            "http://192.168.1.10:8045",
            "http://203.0.113.10:8045",
            "http://example.com",
            "not-a-url",
        ] {
            assert!(warmup_url(origin).is_err(), "{origin}");
        }
    }

    fn app() -> Router {
        Router::new()
            .route(
                "/internal/warmup",
                any(|headers: HeaderMap| async move {
                    assert!(!headers.contains_key(INTERNAL_REQUEST_CREDENTIAL_HEADER));
                    StatusCode::NO_CONTENT
                }),
            )
            .layer(crate::proxy::middleware::cors::cors_layer())
            .layer(axum::middleware::from_fn(internal_loopback_middleware))
    }

    fn request(peer: Option<&str>, credential: Option<&str>, method: Method) -> Request {
        let mut request = Request::builder()
            .uri("/internal/warmup")
            .method(method)
            .header("x-forwarded-for", "127.0.0.1")
            .body(Body::empty())
            .unwrap();
        if let Some(peer) = peer {
            request
                .extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        }
        if let Some(value) = credential {
            request
                .headers_mut()
                .insert(INTERNAL_REQUEST_CREDENTIAL_HEADER, value.parse().unwrap());
        }
        request
    }

    #[tokio::test]
    async fn internal_guard_requires_private_credential_even_on_loopback() {
        for peer in [
            Some("127.0.0.1:1234"),
            Some("[::1]:1234"),
            Some("[::ffff:127.0.0.1]:1234"),
        ] {
            let response = app()
                .oneshot(request(
                    peer,
                    Some(internal_request_credential()),
                    Method::POST,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            for credential in [None, Some("invalid")] {
                let response = app()
                    .oneshot(request(peer, credential, Method::POST))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
        }
    }

    #[tokio::test]
    async fn internal_guard_rejects_external_missing_peers_and_duplicate_credentials() {
        for peer in [
            None,
            Some("192.0.2.1:1234"),
            Some("[::ffff:192.0.2.1]:1234"),
        ] {
            let response = app()
                .oneshot(request(
                    peer,
                    Some(internal_request_credential()),
                    Method::POST,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let mut duplicate = request(
            Some("127.0.0.1:1234"),
            Some(internal_request_credential()),
            Method::POST,
        );
        duplicate.headers_mut().append(
            INTERNAL_REQUEST_CREDENTIAL_HEADER,
            "duplicate".parse().unwrap(),
        );
        assert_eq!(
            app().oneshot(duplicate).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn internal_guard_runs_before_cors_preflight() {
        let mut request = request(Some("127.0.0.1:1234"), None, Method::OPTIONS);
        request
            .headers_mut()
            .insert("origin", "https://example.test".parse().unwrap());
        request
            .headers_mut()
            .insert("access-control-request-method", "POST".parse().unwrap());
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response
            .headers()
            .contains_key("access-control-allow-origin"));
    }
}
