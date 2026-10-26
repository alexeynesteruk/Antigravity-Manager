//! Exercise the same auth/decompression/body-validation order as the AI router.

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::post,
    Json, Router,
};
use flate2::{write::GzEncoder, Compression};
use futures::{StreamExt, TryStreamExt};
use serde_json::{json, Value};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::RwLock;
use tower::ServiceExt;

use crate::proxy::{
    middleware::{auth::auth_middleware, request_body::validate_decoded_body},
    ProxyAuthMode, ProxySecurityConfig,
};

const BODY_LIMIT: usize = 1024;
const TEST_KEY: &str = "gzip-test-key";

#[derive(Clone)]
struct AuthenticatedMarker;

#[derive(Clone, Default)]
struct Capture(Arc<RwLock<Option<Value>>>, Arc<AtomicUsize>);

async fn attach_marker(mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(AuthenticatedMarker);
    next.run(request).await
}

async fn capture_decoded_body(
    State(capture): State<Capture>,
    request: Request,
    next: Next,
) -> Response {
    assert!(request.extensions().get::<AuthenticatedMarker>().is_some());
    assert_eq!(request.uri().path(), "/v1/messages");
    assert_eq!(request.headers()[header::CONTENT_TYPE], "application/json");
    assert!(!request.headers().contains_key(header::CONTENT_ENCODING));
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
    *capture.0.write().await = serde_json::from_slice(&bytes).ok();
    capture.1.fetch_add(1, Ordering::SeqCst);
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

fn app(capture: Capture) -> Router {
    app_with_limit(capture, BODY_LIMIT)
}

fn app_with_limit(capture: Capture, body_limit: usize) -> Router {
    let security = Arc::new(RwLock::new(ProxySecurityConfig {
        auth_mode: ProxyAuthMode::Strict,
        api_key: TEST_KEY.to_string(),
        admin_password: None,
        allow_lan_access: false,
        port: 8045,
        security_monitor: Default::default(),
    }));

    Router::new()
        .route(
            "/v1/messages",
            post(|Json(body): Json<Value>| async move { Json(body) }),
        )
        .layer(middleware::from_fn_with_state(
            capture,
            capture_decoded_body,
        ))
        .layer(middleware::from_fn(validate_decoded_body))
        .layer(tower_http::decompression::RequestDecompressionLayer::new())
        .layer(middleware::from_fn(attach_marker))
        .layer(middleware::from_fn_with_state(security, auth_middleware))
        .layer(DefaultBodyLimit::max(body_limit))
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn request(body: impl Into<Body>, compressed: bool, authenticated: bool) -> Request {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(header::CONTENT_TYPE, "application/json");
    if compressed {
        builder = builder.header(header::CONTENT_ENCODING, "gzip");
    }
    if authenticated {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {TEST_KEY}"));
    }
    builder.body(body.into()).unwrap()
}

#[tokio::test]
async fn gzip_and_plain_requests_reach_monitor_as_identical_json() {
    let payload = json!({"model":"test-model","messages":[{"role":"user","content":"hello"}]});
    let bytes = serde_json::to_vec(&payload).unwrap();
    for compressed in [false, true] {
        let capture = Capture::default();
        let body = if compressed {
            gzip(&bytes)
        } else {
            bytes.clone()
        };
        let response = app(capture.clone())
            .oneshot(request(body, compressed, true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(*capture.0.read().await, Some(payload.clone()));
        let returned = axum::body::to_bytes(response.into_body(), BODY_LIMIT)
            .await
            .unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&returned).unwrap(), payload);
    }
}

#[tokio::test]
async fn unauthenticated_invalid_gzip_is_rejected_before_body_is_polled() {
    let body = Body::from_stream(futures::stream::once(async {
        panic!("authentication must reject before polling the body");
        #[allow(unreachable_code)]
        Ok::<Bytes, std::io::Error>(Bytes::new())
    }));
    let capture = Capture::default();
    let response = app(capture.clone())
        .oneshot(request(body, true, false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(capture.0.read().await.is_none());
}

#[tokio::test]
async fn malformed_gzip_is_rejected_before_monitoring() {
    let capture = Capture::default();
    let response = app(capture.clone())
        .oneshot(request(b"not a gzip stream".to_vec(), true, true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(capture.0.read().await.is_none());
}

#[tokio::test]
async fn truncated_gzip_is_rejected_before_monitoring() {
    let mut compressed = gzip(br#"{"model":"test-model"}"#);
    compressed.truncate(compressed.len() - 8);
    let capture = Capture::default();
    let response = app(capture.clone())
        .oneshot(request(compressed, true, true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(capture.0.read().await.is_none());
}

#[tokio::test]
async fn decoded_body_limit_applies_to_compressed_and_plain_requests() {
    let payload = json!({"model":"test-model","input":"x".repeat(BODY_LIMIT * 4)});
    let bytes = serde_json::to_vec(&payload).unwrap();
    let compressed = gzip(&bytes);
    assert!(compressed.len() < BODY_LIMIT);
    for (body, encoded) in [(compressed, true), (bytes, false)] {
        let capture = Capture::default();
        let response = app(capture.clone())
            .oneshot(request(body, encoded, true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(capture.0.read().await.is_none());
    }
}

#[tokio::test]
async fn unsupported_content_encoding_is_rejected_without_monitoring() {
    let mut request = request(b"{}".to_vec(), false, true);
    request
        .headers_mut()
        .insert(header::CONTENT_ENCODING, "unsupported".parse().unwrap());
    let capture = Capture::default();
    let response = app(capture.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert!(capture.0.read().await.is_none());
}

/// Drop aborts the listener even if an assertion fails, keeping tests isolated.
struct LoopbackServer {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_loopback(router: Router) -> LoopbackServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    LoopbackServer { url, task }
}

fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_transport_keeps_concurrent_gzip_plain_and_chunked_requests_isolated() {
    let capture = Capture::default();
    let server = serve_loopback(app(capture.clone())).await;
    let client = loopback_client();
    let url = format!("{}/v1/messages", server.url);
    const REQUEST_COUNT: usize = 192;

    let results: Vec<_> = futures::stream::iter(0..REQUEST_COUNT)
        .map(|sequence| {
            let client = client.clone();
            let url = url.clone();
            async move {
                let payload = json!({
                    "model":"test-model",
                    "sequence":sequence,
                    "messages":[{"role":"user","content":format!("request {sequence}: café 日本語 🦀")}]
                });
                let raw = serde_json::to_vec(&payload).unwrap();
                let compressed = sequence % 2 == 0;
                let bytes = if compressed { gzip(&raw) } else { raw };
                let mut request = client
                    .post(url)
                    .bearer_auth(TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/json");
                if compressed {
                    request = request.header(header::CONTENT_ENCODING, "gzip");
                }
                // HTTP chunked transfer splits gzip headers and UTF-8 sequences
                // independently of logical JSON/SSE boundaries.
                request = if sequence % 3 == 0 {
                    let split = if compressed {
                        bytes.len().min(7)
                    } else {
                        bytes
                            .iter()
                            .position(|byte| !byte.is_ascii())
                            .map(|index| index + 1)
                            .unwrap_or(bytes.len().min(7))
                    };
                    let frames = vec![
                        Ok::<_, std::io::Error>(bytes[..split].to_vec()),
                        Ok(bytes[split..].to_vec()),
                    ];
                    request.body(reqwest::Body::wrap_stream(futures::stream::iter(frames)))
                } else {
                    request.body(bytes)
                };
                let response = request.send().await?;
                assert_eq!(response.status(), StatusCode::OK);
                let returned = response.json::<Value>().await?;
                assert_eq!(returned, payload, "response crossed request boundaries");
                Ok::<_, reqwest::Error>(sequence)
            }
        })
        .buffer_unordered(16)
        .try_collect()
        .await
        .unwrap();

    assert_eq!(results.len(), REQUEST_COUNT);
    assert_eq!(capture.1.load(Ordering::SeqCst), REQUEST_COUNT);
    assert!(
        !server.task.is_finished(),
        "listener failed during concurrent traffic"
    );
}

#[tokio::test]
async fn loopback_transport_enforces_auth_and_exact_custom_decoded_limits_after_rejections() {
    let client = loopback_client();
    for body_limit in [257, 4096] {
        let capture = Capture::default();
        let server = serve_loopback(app_with_limit(capture.clone(), body_limit)).await;
        let url = format!("{}/v1/messages", server.url);
        let base = serde_json::to_vec(&json!({"model":"test-model","input":""})).unwrap();
        let payload = json!({"model":"test-model","input":"x".repeat(body_limit - base.len())});
        let exact = serde_json::to_vec(&payload).unwrap();
        assert_eq!(exact.len(), body_limit);
        let mut oversized = exact.clone();
        oversized.push(b' ');
        assert_eq!(oversized.len(), body_limit + 1);

        // Authentication wins even when decoding would fail or exceed the cap.
        for bytes in [b"malformed gzip".to_vec(), gzip(&oversized)] {
            let response = client
                .post(&url)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::CONTENT_ENCODING, "gzip")
                .body(bytes)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        assert_eq!(capture.1.load(Ordering::SeqCst), 0);

        for compressed in [true, false] {
            let mut request = client
                .post(&url)
                .bearer_auth(TEST_KEY)
                .header(header::CONTENT_TYPE, "application/json");
            if compressed {
                request = request.header(header::CONTENT_ENCODING, "gzip");
            }
            let response = request
                .body(if compressed {
                    gzip(&oversized)
                } else {
                    oversized.clone()
                })
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        }
        let response = client
            .post(&url)
            .bearer_auth(TEST_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_ENCODING, "gzip")
            .body(b"malformed gzip".to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(capture.1.load(Ordering::SeqCst), 0);

        // Exact-cap requests succeed after bad traffic; keep-alive and decoded
        // limits must not contaminate the next exchange on the same client.
        for compressed in [true, false] {
            let mut request = client
                .post(&url)
                .bearer_auth(TEST_KEY)
                .header(header::CONTENT_TYPE, "application/json");
            if compressed {
                request = request.header(header::CONTENT_ENCODING, "gzip");
            }
            let response = request
                .body(if compressed {
                    gzip(&exact)
                } else {
                    exact.clone()
                })
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.json::<Value>().await.unwrap(), payload);
        }
        assert_eq!(capture.1.load(Ordering::SeqCst), 2);
        assert!(!server.task.is_finished());
    }
}
