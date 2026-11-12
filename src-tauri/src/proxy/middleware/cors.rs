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

/// Supplied by our TCP transport, never by a request or forwarded header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ListenerAuthority {
    pub local_addr: std::net::SocketAddr,
    pub tls: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct Authority {
    host: String,
    port: u16,
}

fn canonical_ip(ip: std::net::IpAddr) -> std::net::IpAddr {
    match ip {
        std::net::IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(std::net::IpAddr::V4)
            .unwrap_or(ip.into()),
        ip => ip,
    }
}

fn parse_authority(value: &str, default_port: u16) -> Option<Authority> {
    if value.is_empty()
        || value.len() > 512
        || !value.is_ascii()
        || value.bytes().any(|c| {
            c.is_ascii_whitespace()
                || c.is_ascii_control()
                || matches!(c, b'@' | b'/' | b'\\' | b'?' | b'#' | b'%' | b',' | b'*')
        })
    {
        return None;
    }
    let authority: axum::http::uri::Authority = value.parse().ok()?;
    if authority.host().starts_with('[')
        && authority
            .host()
            .strip_prefix('[')?
            .strip_suffix(']')?
            .parse::<std::net::Ipv6Addr>()
            .is_err()
    {
        return None;
    }
    // http::Authority::port() returns None for malformed/overflowing ports too.
    // Inspect the suffix so those cannot silently become a default port.
    let suffix = value.strip_prefix(authority.host())?;
    let port = if suffix.is_empty() {
        default_port
    } else {
        let digits = suffix.strip_prefix(':')?;
        if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u16>().ok().filter(|port| *port != 0)?
    };
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let host = if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        canonical_ip(ip).to_string()
    } else {
        let host = host.strip_suffix('.').unwrap_or(host);
        if host.is_empty()
            || host.len() > 253
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            })
        {
            return None;
        }
        host.to_ascii_lowercase()
    };
    Some(Authority { host, port })
}

fn display_authority(authority: &Authority) -> String {
    if authority.host.contains(':') {
        format!("[{}]:{}", authority.host, authority.port)
    } else {
        format!("{}:{}", authority.host, authority.port)
    }
}

/// Entries are exact authorities. A bare host permits only HTTP/HTTPS default
/// ports; nonstandard listener/reverse-proxy ports must be explicit.
pub fn normalize_trusted_hosts(entries: &[String]) -> Result<Vec<String>, String> {
    if entries.len() > 256 {
        return Err("Invalid trusted host configuration".into());
    }
    let mut normalized = Vec::new();
    for entry in entries {
        let entry = entry.trim();
        for port in [80, 443] {
            let authority =
                parse_authority(entry, port).ok_or("Invalid trusted host configuration")?;
            let canonical = display_authority(&authority);
            if !normalized.contains(&canonical) {
                normalized.push(canonical);
                if normalized.len() > 256 {
                    return Err("Invalid trusted host configuration".into());
                }
            }
        }
    }
    Ok(normalized)
}

pub(crate) fn configured_trusted_hosts(configured: &[String]) -> Result<Vec<String>, String> {
    let entries = match std::env::var("ABV_TRUSTED_HOSTS") {
        Ok(value) if value.trim().is_empty() => Vec::new(),
        Ok(value) => value
            .split(',')
            .map(|entry| entry.trim().to_owned())
            .collect(),
        Err(std::env::VarError::NotPresent) => configured.to_vec(),
        Err(_) => return Err("Invalid trusted host configuration".into()),
    };
    normalize_trusted_hosts(&entries)
}

pub(crate) fn listener_authorities(
    http_host: &str,
    http_port: u16,
    https: Option<(&str, u16, Option<&str>)>,
) -> Result<Vec<String>, String> {
    let mut entries = Vec::new();
    let mut add = |host: &str, port: u16| {
        for loopback in ["localhost", "127.0.0.1", "[::1]"] {
            entries.push(format!("{loopback}:{port}"));
        }
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if !ip.is_unspecified() {
                entries.push(display_authority(&Authority {
                    host: canonical_ip(ip).to_string(),
                    port,
                }));
            }
        }
    };
    add(http_host, http_port);
    if let Some((host, port, advertised)) = https {
        add(host, port);
        if let Some(host) = advertised {
            // TLS validates the advertised SAN host separately; no URL/scheme here.
            let host = host.trim_start_matches('[').trim_end_matches(']');
            entries.push(display_authority(&Authority {
                host: host.to_owned(),
                port,
            }));
        }
    }
    normalize_trusted_hosts(&entries)
}

fn validate_request_authority(
    request: &Request,
    security: &crate::proxy::ProxySecurityConfig,
) -> Result<(), StatusCode> {
    let local = request.extensions().get::<ListenerAuthority>();
    let default_port = match request.uri().scheme_str() {
        Some("https") => 443,
        Some("http") => 80,
        Some(_) => return Err(StatusCode::FORBIDDEN),
        None if local.is_some_and(|local| local.tls) => 443,
        None => 80,
    };
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let host = hosts.next();
    if hosts.next().is_some() {
        return Err(StatusCode::FORBIDDEN);
    }
    let header_authority = host
        .map(|host| {
            host.to_str()
                .ok()
                .and_then(|host| parse_authority(host, default_port))
                .ok_or(StatusCode::FORBIDDEN)
        })
        .transpose()?;
    let uri_authority = request
        .uri()
        .authority()
        .map(|authority| {
            parse_authority(authority.as_str(), default_port).ok_or(StatusCode::FORBIDDEN)
        })
        .transpose()?;
    if let (Some(header), Some(uri)) = (&header_authority, &uri_authority) {
        if header != uri {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    let authority = header_authority
        .or(uri_authority)
        .ok_or(StatusCode::FORBIDDEN)?;
    let trusted = configured_trusted_hosts(&security.security_monitor.trusted_hosts)
        .map_err(|_| StatusCode::FORBIDDEN)?;
    let pinned = &security.listener_authorities;
    let normalized = display_authority(&authority);
    if trusted.contains(&normalized) || pinned.contains(&normalized) {
        return Ok(());
    }
    if let Some(local) = local {
        if authority.port == local.local_addr.port() {
            let ip = canonical_ip(local.local_addr.ip());
            if authority.host == ip.to_string()
                || (ip.is_loopback()
                    && matches!(authority.host.as_str(), "localhost" | "127.0.0.1" | "::1"))
            {
                return Ok(());
            }
        }
    }
    Err(StatusCode::FORBIDDEN)
}

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

/// Origin policy for WS upgrades, after the global request-authority gate.
/// Native clients send no Origin; hosted clients require same-origin or a list.
pub fn validate_websocket_origin(
    headers: &HeaderMap,
    security: &crate::proxy::ProxySecurityConfig,
) -> Result<(), StatusCode> {
    validate_origin(headers, security, None)
}

fn validate_origin(
    headers: &HeaderMap,
    security: &crate::proxy::ProxySecurityConfig,
    uri_authority: Option<&str>,
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
    // The global gate has already established this authority belongs to the
    // live listener or an explicitly trusted proxy. Never use forwarded-host.
    let mut hosts = headers.get_all(header::HOST).iter();
    if let (Some(host), None) = (hosts.next(), hosts.next()) {
        if let Ok(host) = host.to_str() {
            if let Some(host_url) = parse_origin(&format!("{}://{}", origin.scheme(), host)) {
                if host_url.origin() == origin.origin() {
                    return Ok(());
                }
            }
        }
    } else if !headers.contains_key(header::HOST) {
        if let Some(host_url) =
            uri_authority.and_then(|host| parse_origin(&format!("{}://{}", origin.scheme(), host)))
        {
            if host_url.origin() == origin.origin() {
                return Ok(());
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
    let policy = security.read().await;
    if let Err(status) = validate_request_authority(&request, &policy) {
        return status.into_response();
    }
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
        if let Err(status) = validate_origin(
            request.headers(),
            &policy,
            request
                .uri()
                .authority()
                .map(|authority| authority.as_str()),
        ) {
            return status.into_response();
        }
    }
    drop(policy);
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

    fn authority_request(host: Option<&str>, local: Option<(&str, bool)>) -> Request {
        let mut builder = Request::builder().uri("/v1/messages");
        if let Some(host) = host {
            builder = builder.header(header::HOST, host);
        }
        let mut request = builder.body(axum::body::Body::empty()).unwrap();
        if let Some((local, tls)) = local {
            request.extensions_mut().insert(ListenerAuthority {
                local_addr: local.parse().unwrap(),
                tls,
            });
        }
        request
    }

    #[test]
    fn trusted_hosts_are_exact_authorities_and_invalid_settings_fail_closed() {
        assert_eq!(
            normalize_trusted_hosts(&["Gateway.EXAMPLE.".into(), "[::1]:8443".into()]).unwrap(),
            vec!["gateway.example:80", "gateway.example:443", "[::1]:8443"]
        );
        for invalid in [
            "*",
            "https://gateway.example",
            "user@gateway.example:443",
            "a/b",
            "a?b",
            "a#b",
            "a%2eb",
            "a,b",
            "a:",
            "a:0",
            "a:65536",
            "a:invalid",
            "a b",
            ".",
            "a..b",
            "-bad.example",
            "",
            "[::1",
            "[localhost]:8045",
            "[127.0.0.1]:8045",
        ] {
            assert!(
                normalize_trusted_hosts(&[invalid.into()]).is_err(),
                "{invalid}"
            );
        }
        let mut policy = security(&["*"]);
        policy.security_monitor.trusted_hosts = vec!["*".into()];
        assert!(validate_request_authority(
            &authority_request(Some("localhost:8045"), None),
            &policy
        )
        .is_err());
    }

    #[test]
    fn rebinding_hosts_fail_even_without_origin_and_with_spoofed_proxy_headers() {
        let policy = security(&["*"]);
        for host in [
            "attacker.example:8045",
            "attacker.example",
            "127.0.0.1:8046",
            "localhost",
            "192.168.1.99:8045",
            "0.0.0.0:8045",
            "127.0.0.1:8045@attacker.example",
        ] {
            let mut request = authority_request(Some(host), Some(("192.168.1.10:8045", false)));
            request
                .headers_mut()
                .insert("x-forwarded-host", "localhost:8045".parse().unwrap());
            request
                .headers_mut()
                .insert("forwarded", "host=localhost:8045".parse().unwrap());
            assert!(
                validate_request_authority(&request, &policy).is_err(),
                "{host}"
            );
        }
        let mut duplicate = authority_request(Some("localhost:8045"), None);
        duplicate
            .headers_mut()
            .append(header::HOST, "localhost:8045".parse().unwrap());
        assert!(validate_request_authority(&duplicate, &policy).is_err());
        assert!(validate_request_authority(&authority_request(None, None), &policy).is_err());
    }

    #[test]
    fn live_ipv4_ipv6_and_tls_advertisement_require_the_real_port() {
        let mut config = crate::proxy::config::ProxyConfig::default();
        config.allow_lan_access = true;
        config.tls.enabled = true;
        config.tls.advertised_host = Some("gateway.home.arpa".into());
        let policy = crate::proxy::ProxySecurityConfig::from_proxy_config(&config);
        for (host, local, tls) in [
            ("192.168.1.10:8443", "192.168.1.10:8443", true),
            ("gateway.home.arpa:8443", "192.168.1.10:8443", true),
            ("[fd12::10]:8443", "[fd12::10]:8443", true),
            ("192.168.1.10:8443", "[::ffff:192.168.1.10]:8443", true),
            ("localhost:8045", "127.0.0.1:8045", false),
        ] {
            assert!(
                validate_request_authority(
                    &authority_request(Some(host), Some((local, tls))),
                    &policy
                )
                .is_ok(),
                "{host}"
            );
        }
        for host in [
            "gateway.home.arpa:443",
            "gateway.home.arpa:8045",
            "[fd12::11]:8443",
            "192.168.1.10:8046",
        ] {
            assert!(
                validate_request_authority(
                    &authority_request(Some(host), Some(("192.168.1.10:8443", true))),
                    &policy
                )
                .is_err(),
                "{host}"
            );
        }
    }

    #[test]
    fn uri_authority_is_supported_but_cannot_disagree_with_host() {
        let policy = security(&[]);
        let mut request = authority_request(None, None);
        *request.uri_mut() = "http://localhost:8045/v1/messages".parse().unwrap();
        assert!(validate_request_authority(&request, &policy).is_ok());
        request
            .headers_mut()
            .insert(header::HOST, "attacker.example:8045".parse().unwrap());
        assert!(validate_request_authority(&request, &policy).is_err());
        request
            .headers_mut()
            .insert(header::HOST, "localhost:8045".parse().unwrap());
        assert!(validate_request_authority(&request, &policy).is_ok());
        *request.uri_mut() = "http://attacker.example:8045/v1/messages".parse().unwrap();
        assert!(validate_request_authority(&request, &policy).is_err());
    }

    #[test]
    fn trusted_reverse_proxy_hosts_are_explicit_and_port_scoped() {
        let mut policy = security(&[]);
        policy.security_monitor.trusted_hosts =
            vec!["gateway.example".into(), "proxy.example:9443".into()];
        for host in [
            "gateway.example",
            "gateway.example:443",
            "proxy.example:9443",
        ] {
            assert!(
                validate_request_authority(&authority_request(Some(host), None), &policy).is_ok(),
                "{host}"
            );
        }
        for host in [
            "gateway.example:8045",
            "proxy.example:443",
            "sub.gateway.example",
        ] {
            assert!(
                validate_request_authority(&authority_request(Some(host), None), &policy).is_err(),
                "{host}"
            );
        }
    }

    #[tokio::test]
    async fn mandatory_host_gate_precedes_preflight_and_websocket_upgrade() {
        use tower::ServiceExt;
        let policy = security(&["*"]);
        let app = axum::Router::new()
            .fallback(|| async { StatusCode::NO_CONTENT })
            .layer(cors_layer())
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(RwLock::new(policy)),
                origin_policy_middleware,
            ));
        for origin in [
            None,
            Some("http://attacker.example:8045"),
            Some("tauri://localhost"),
        ] {
            for method in [Method::GET, Method::OPTIONS] {
                let mut request = authority_request(
                    Some("attacker.example:8045"),
                    Some(("127.0.0.1:8045", false)),
                );
                *request.method_mut() = method;
                if let Some(origin) = origin {
                    request
                        .headers_mut()
                        .insert(header::ORIGIN, origin.parse().unwrap());
                }
                request.headers_mut().insert(
                    header::ACCESS_CONTROL_REQUEST_METHOD,
                    "GET".parse().unwrap(),
                );
                request
                    .headers_mut()
                    .insert(header::UPGRADE, "websocket".parse().unwrap());
                request
                    .headers_mut()
                    .insert(header::CONNECTION, "upgrade".parse().unwrap());
                assert_eq!(
                    app.clone().oneshot(request).await.unwrap().status(),
                    StatusCode::FORBIDDEN
                );
            }
        }
        let request = authority_request(Some("localhost:8045"), None);
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
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
                .header("host", "127.0.0.1:8045")
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
                    .header("host", "127.0.0.1:8045")
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
