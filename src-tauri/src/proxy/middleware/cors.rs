// CORS middleware
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

/// Creates the CORS layer
pub fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::HEAD,
            Method::OPTIONS,
            Method::PATCH,
        ])
        .allow_headers(Any)
        .expose_headers([
            axum::http::HeaderName::from_static("x-account-email"),
            axum::http::HeaderName::from_static("x-mapped-model"),
            axum::http::HeaderName::from_static("x-upstream-model"),
        ])
        .allow_credentials(false)
        .max_age(std::time::Duration::from_secs(3600))
}

fn configured_origins(configured: &[String]) -> Vec<String> {
    match std::env::var("ABV_CORS_ALLOWED_ORIGINS") {
        Ok(value) => value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Err(_) => configured.to_vec(),
    }
}

fn parse_origin(value: &str) -> Option<url::Url> {
    let url = url::Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

fn builtin_tauri_origin(value: &str) -> bool {
    matches!(
        value,
        "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
    )
}

/// Shared HTTP/WebSocket policy. Native clients send no Origin. Hosted clients
/// may use their own origin; cross-origin clients require an explicit allowlist.
pub fn validate_websocket_origin(
    headers: &HeaderMap,
    security: &crate::proxy::ProxySecurityConfig,
) -> Result<(), StatusCode> {
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return Ok(());
    };
    if origins.next().is_some() {
        return Err(StatusCode::FORBIDDEN);
    }
    let raw = origin.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
    // The built-in request probe runs in the Tauri WebView. Its application
    // origins are allowed only with the configured API key on the actual request.
    if builtin_tauri_origin(raw) {
        return if !security.api_key.is_empty()
            && crate::proxy::middleware::auth::extract_header_api_key(headers)
                == Some(security.api_key.as_str())
        {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        };
    }
    let origin = parse_origin(raw).ok_or(StatusCode::FORBIDDEN)?;
    let allowed = configured_origins(&security.security_monitor.cors_allowed_origins);
    if allowed.iter().any(|entry| {
        entry.trim() == "*"
            || parse_origin(entry.trim()).is_some_and(|url| url.origin() == origin.origin())
    }) {
        return Ok(());
    }
    // Use Host, never a caller-supplied forwarded-host/origin header. Reverse
    // proxies preserving Host retain same-origin Web UI compatibility.
    let mut hosts = headers.get_all(header::HOST).iter();
    if let (Some(host), None) = (hosts.next(), hosts.next()) {
        if let Ok(host) = host.to_str() {
            if let Some(host_url) = parse_origin(&format!("{}://{}", origin.scheme(), host)) {
                if host_url.origin() == origin.origin() {
                    return Ok(());
                }
            }
        }
    }
    Err(StatusCode::FORBIDDEN)
}

/// Validate before the CORS transport layer, including simple requests. Re-read
/// live settings so GUI and headless configuration changes apply immediately.
pub async fn origin_policy_middleware(
    State(security): State<Arc<RwLock<crate::proxy::ProxySecurityConfig>>>,
    request: Request,
    next: Next,
) -> Response {
    let builtin_preflight = request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
        && request.headers().get_all(header::ORIGIN).iter().count() == 1
        && request
            .headers()
            .get(header::ORIGIN)
            .and_then(|origin| origin.to_str().ok())
            .is_some_and(builtin_tauri_origin);
    if !builtin_preflight {
        if let Err(status) = validate_websocket_origin(request.headers(), &*security.read().await) {
            return status.into_response();
        }
    }
    let origin = request.headers().get(header::ORIGIN).cloned();
    let mut response = next.run(request).await;
    if let Some(origin) = origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .append(header::VARY, header::HeaderValue::from_static("Origin"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn security(origins: &[&str]) -> crate::proxy::ProxySecurityConfig {
        let mut config = crate::proxy::ProxySecurityConfig::from_proxy_config(&Default::default());
        config.security_monitor.cors_allowed_origins =
            origins.iter().map(|s| s.to_string()).collect();
        config
    }

    fn headers(origin: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:8045".parse().unwrap());
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, origin.parse().unwrap());
        }
        headers
    }

    #[test]
    fn http_and_websocket_policy_preserves_native_and_same_origin_clients() {
        let policy = security(&[]);
        assert!(validate_websocket_origin(&headers(None), &policy).is_ok());
        assert!(
            validate_websocket_origin(&headers(Some("http://127.0.0.1:8045")), &policy).is_ok()
        );
        for origin in [
            "https://attacker.example",
            "null",
            "https://attacker.example/path",
            "http://127.0.0.1:8046",
        ] {
            assert_eq!(
                validate_websocket_origin(&headers(Some(origin)), &policy),
                Err(StatusCode::FORBIDDEN)
            );
        }
        assert!(validate_websocket_origin(
            &headers(Some("https://client.example")),
            &security(&["https://client.example"])
        )
        .is_ok());
        assert!(validate_websocket_origin(
            &headers(Some("https://client.example")),
            &security(&["*"])
        )
        .is_ok());
        let mut duplicate = headers(Some("http://127.0.0.1:8045"));
        duplicate.append(header::ORIGIN, "https://attacker.example".parse().unwrap());
        assert!(validate_websocket_origin(&duplicate, &policy).is_err());
    }

    #[tokio::test]
    async fn builtin_webview_preflight_does_not_allow_an_unauthenticated_generation() {
        use tower::ServiceExt;
        let mut policy = security(&[]);
        policy.api_key = "probe-key".into();
        let app = axum::Router::new()
            .route(
                "/v1/chat/completions",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            )
            .layer(cors_layer())
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(RwLock::new(policy)),
                origin_policy_middleware,
            ));
        for origin in [
            "tauri://localhost",
            "http://tauri.localhost",
            "https://tauri.localhost",
        ] {
            let preflight = Request::builder()
                .method("OPTIONS")
                .uri("/v1/chat/completions")
                .header("origin", origin)
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    "authorization,content-type",
                )
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.clone().oneshot(preflight).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
                origin
            );
            for (key, expected) in [
                (None, StatusCode::FORBIDDEN),
                (Some("wrong"), StatusCode::FORBIDDEN),
                (Some("probe-key"), StatusCode::NO_CONTENT),
            ] {
                let mut request = Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("origin", origin);
                if let Some(key) = key {
                    request = request.header("authorization", format!("Bearer {key}"));
                }
                assert_eq!(
                    app.clone()
                        .oneshot(request.body(axum::body::Body::empty()).unwrap())
                        .await
                        .unwrap()
                        .status(),
                    expected
                );
            }
        }
    }

    #[tokio::test]
    async fn json_preflight_obeys_the_live_origin_policy() {
        use tower::ServiceExt;
        let live = Arc::new(RwLock::new(security(&[])));
        let app = axum::Router::new()
            .route(
                "/v1/chat/completions",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            )
            .layer(cors_layer())
            .layer(axum::middleware::from_fn_with_state(
                live.clone(),
                origin_policy_middleware,
            ));
        let request = || {
            axum::http::Request::builder()
                .method("OPTIONS")
                .uri("/v1/chat/completions")
                .header("host", "127.0.0.1:8045")
                .header("origin", "https://client.example")
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        live.write().await.security_monitor.cors_allowed_origins =
            vec!["https://client.example".into()];
        let response = app.oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://client.example"
        );
    }

    #[test]
    fn test_cors_layer_creation() {
        let _layer = cors_layer();
        // Layer created successfully
        assert!(true);
    }
}
