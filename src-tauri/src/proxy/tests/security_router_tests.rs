//! Exercise the production middleware composition across internal, AI, and admin routes.

use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use flate2::{write::GzEncoder, Compression};
use serde_json::{json, Value};
use std::{
    io::Write,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::RwLock;
use tower::ServiceExt;

use crate::proxy::{
    middleware::{
        auth::{admin_auth_middleware, auth_middleware},
        cors::cors_layer,
        internal::{
            internal_loopback_middleware, internal_request_credential,
            require_validated_internal_request, INTERNAL_REQUEST_CREDENTIAL_HEADER,
        },
        request_body::validate_decoded_body,
        service_status::service_status_middleware_with_running,
    },
    ProxyAuthMode, ProxySecurityConfig,
};

const BODY_LIMIT: usize = 256;
const PROXY_KEY: &str = "router-proxy-key";
const ADMIN_PASSWORD: &str = "router-admin-password";

#[derive(Clone, Default)]
struct Observations {
    monitored: Arc<AtomicUsize>,
    mutations: Arc<AtomicUsize>,
}

async fn monitor(State(observed): State<Observations>, request: Request, next: Next) -> Response {
    assert!(!request
        .headers()
        .contains_key(INTERNAL_REQUEST_CREDENTIAL_HEADER));
    assert!(!request.headers().contains_key(header::CONTENT_ENCODING));
    assert!(require_validated_internal_request(&request).is_ok());
    observed.monitored.fetch_add(1, Ordering::SeqCst);
    next.run(request).await
}

async fn echo(headers: HeaderMap, Json(payload): Json<Value>) -> Json<Value> {
    assert!(!headers.contains_key(INTERNAL_REQUEST_CREDENTIAL_HEADER));
    Json(payload)
}

async fn admin_control(State(observed): State<Observations>, headers: HeaderMap) -> StatusCode {
    assert!(!headers.contains_key(INTERNAL_REQUEST_CREDENTIAL_HEADER));
    observed.mutations.fetch_add(1, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

/// Mirrors server.rs: AI-only auth/decompression, strict admin authentication,
/// then the shared service gate, CORS, private internal guard, and decoded-body limit extension.
fn router(running: bool, auth_mode: ProxyAuthMode, observed: Observations) -> Router {
    let security = Arc::new(RwLock::new(ProxySecurityConfig {
        auth_mode,
        api_key: PROXY_KEY.to_string(),
        admin_password: Some(ADMIN_PASSWORD.to_string()),
        allow_lan_access: true,
        port: 8045,
        security_monitor: Default::default(),
    }));
    let ai = Router::new()
        .route("/internal/warmup", post(echo))
        .route("/v1/messages", post(echo))
        .layer(middleware::from_fn_with_state(observed.clone(), monitor))
        .layer(middleware::from_fn(validate_decoded_body))
        .layer(tower_http::decompression::RequestDecompressionLayer::new())
        .layer(middleware::from_fn_with_state(
            security.clone(),
            auth_middleware,
        ));
    let admin = Router::new()
        .route("/settings", post(admin_control))
        .route("/health", get(|| async { StatusCode::NO_CONTENT }))
        .with_state(observed)
        .layer(middleware::from_fn_with_state(
            security,
            admin_auth_middleware,
        ));
    Router::new()
        .merge(ai)
        .nest("/api", admin)
        .route("/health", get(|| async { StatusCode::NO_CONTENT }))
        .layer(middleware::from_fn(move |request, next| {
            service_status_middleware_with_running(running, request, next)
        }))
        .layer(cors_layer())
        .layer(middleware::from_fn(internal_loopback_middleware))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

fn request(
    path: &str,
    peer: Option<&str>,
    private: Option<&str>,
    key: Option<&str>,
    body: Body,
) -> Request {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", "127.0.0.1");
    if let Some(private) = private {
        builder = builder.header(INTERNAL_REQUEST_CREDENTIAL_HEADER, private);
    }
    if let Some(key) = key {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    let mut request = builder.body(body).unwrap();
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    }
    request
}

fn unread_body() -> Body {
    Body::from_stream(futures::stream::once(async {
        panic!("rejected request must not poll its body");
        #[allow(unreachable_code)]
        Ok::<Bytes, std::io::Error>(Bytes::new())
    }))
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[tokio::test]
async fn stopped_service_still_requires_private_loopback_credential_before_body_or_cors() {
    for mode in [ProxyAuthMode::Off, ProxyAuthMode::Strict] {
        let observed = Observations::default();
        let app = router(false, mode, observed.clone());
        for (peer, private) in [
            (None, Some(internal_request_credential())),
            (Some("192.0.2.1:1234"), Some(internal_request_credential())),
            (
                Some("[::ffff:192.0.2.1]:1234"),
                Some(internal_request_credential()),
            ),
            (Some("127.0.0.1:1234"), None),
            (Some("127.0.0.1:1234"), Some("wrong-private-credential")),
        ] {
            for method in [Method::POST, Method::OPTIONS] {
                let mut request = request(
                    "/internal/warmup",
                    peer,
                    private,
                    Some(PROXY_KEY),
                    unread_body(),
                );
                *request.method_mut() = method;
                request
                    .headers_mut()
                    .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
                request
                    .headers_mut()
                    .insert(header::ORIGIN, "https://example.test".parse().unwrap());
                request.headers_mut().insert(
                    header::ACCESS_CONTROL_REQUEST_METHOD,
                    "POST".parse().unwrap(),
                );
                let response = app.clone().oneshot(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                assert!(!response
                    .headers()
                    .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
            }
        }
        let mut duplicate = request(
            "/internal/warmup",
            Some("127.0.0.1:1234"),
            Some(internal_request_credential()),
            None,
            unread_body(),
        );
        duplicate.headers_mut().append(
            INTERNAL_REQUEST_CREDENTIAL_HEADER,
            internal_request_credential().parse().unwrap(),
        );
        assert_eq!(
            app.oneshot(duplicate).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(observed.monitored.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn verified_internal_requests_bypass_stopped_service_auth_and_decode_before_monitoring() {
    let payload = json!({"model":"integration-model","input":"hello"});
    let bytes = serde_json::to_vec(&payload).unwrap();
    for peer in ["127.0.0.1:1234", "[::1]:1234", "[::ffff:127.0.0.1]:1234"] {
        let observed = Observations::default();
        let mut request = request(
            "/internal/warmup",
            Some(peer),
            Some(internal_request_credential()),
            None,
            Body::from(gzip(&bytes)),
        );
        request
            .headers_mut()
            .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
        let response = router(false, ProxyAuthMode::Strict, observed.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), BODY_LIMIT)
            .await
            .unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), payload);
        assert_eq!(observed.monitored.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn internal_gzip_errors_and_decoded_overflow_are_rejected_before_monitoring() {
    for (body, status) in [
        (b"malformed gzip".to_vec(), StatusCode::BAD_REQUEST),
        (
            gzip(&serde_json::to_vec(&json!({"input":"x".repeat(BODY_LIMIT * 4)})).unwrap()),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ] {
        let observed = Observations::default();
        let mut request = request(
            "/internal/warmup",
            Some("127.0.0.1:1234"),
            Some(internal_request_credential()),
            None,
            Body::from(body),
        );
        request
            .headers_mut()
            .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
        let response = router(false, ProxyAuthMode::Off, observed.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(observed.monitored.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn normal_ai_routes_preserve_service_and_auth_policy_without_internal_credentials() {
    for (running, mode, key, status) in [
        (
            false,
            ProxyAuthMode::Off,
            None,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (true, ProxyAuthMode::Off, None, StatusCode::OK),
        (true, ProxyAuthMode::Strict, None, StatusCode::UNAUTHORIZED),
        (true, ProxyAuthMode::Strict, Some(PROXY_KEY), StatusCode::OK),
    ] {
        let observed = Observations::default();
        let request = request(
            "/v1/messages",
            Some("192.0.2.1:1234"),
            None,
            key,
            Body::from("{}"),
        );
        let response = router(running, mode, observed.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(
            observed.monitored.load(Ordering::SeqCst),
            usize::from(status == StatusCode::OK)
        );
    }
    let observed = Observations::default();
    let request = request(
        "/v1/messages",
        Some("127.0.0.1:1234"),
        Some(internal_request_credential()),
        None,
        unread_body(),
    );
    let response = router(true, ProxyAuthMode::Strict, observed.clone())
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(observed.monitored.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn normal_preflight_and_health_remain_available_while_service_is_stopped() {
    let app = router(false, ProxyAuthMode::Strict, Observations::default());
    let mut preflight = request("/v1/messages", None, None, None, unread_body());
    *preflight.method_mut() = Method::OPTIONS;
    preflight
        .headers_mut()
        .insert(header::ORIGIN, "https://example.test".parse().unwrap());
    preflight.headers_mut().insert(
        header::ACCESS_CONTROL_REQUEST_METHOD,
        "POST".parse().unwrap(),
    );
    let response = app.clone().oneshot(preflight).await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    let health = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.oneshot(health).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn proxy_auth_off_never_authorizes_admin_controls_or_private_credential_reuse() {
    for running in [false, true] {
        let observed = Observations::default();
        let app = router(running, ProxyAuthMode::Off, observed.clone());
        for key in [None, Some(PROXY_KEY)] {
            let request = request(
                "/api/settings",
                Some("127.0.0.1:1234"),
                Some(internal_request_credential()),
                key,
                unread_body(),
            );
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(observed.mutations.load(Ordering::SeqCst), 0);
        }
        let request = request(
            "/api/settings",
            Some("192.0.2.1:1234"),
            None,
            Some(ADMIN_PASSWORD),
            Body::empty(),
        );
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(observed.mutations.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn real_loopback_transport_preserves_internal_auth_and_stopped_service_boundaries() {
    let observed = Observations::default();
    let app = router(false, ProxyAuthMode::Strict, observed.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let payload = json!({"model":"transport-model","input":"hello"});
    let response = client
        .post(format!("http://{address}/internal/warmup"))
        .header(
            INTERNAL_REQUEST_CREDENTIAL_HEADER,
            internal_request_credential(),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_ENCODING, "gzip")
        .body(gzip(&serde_json::to_vec(&payload).unwrap()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap(), payload);
    assert_eq!(observed.monitored.load(Ordering::SeqCst), 1);

    let response = client
        .post(format!("http://{address}/internal/warmup"))
        .bearer_auth(PROXY_KEY)
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = client
        .post(format!("http://{address}/v1/messages"))
        .header(
            INTERNAL_REQUEST_CREDENTIAL_HEADER,
            internal_request_credential(),
        )
        .bearer_auth(PROXY_KEY)
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = client
        .post(format!("http://{address}/api/settings"))
        .header(
            INTERNAL_REQUEST_CREDENTIAL_HEADER,
            internal_request_credential(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(observed.mutations.load(Ordering::SeqCst), 0);

    drop(response);
    drop(client);
    shutdown.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}
