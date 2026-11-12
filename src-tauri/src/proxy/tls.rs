//! HTTPS identity loading. Managed private keys remain in the native credential store.
use crate::modules::credential_store::{self, VaultRef};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path, sync::Arc};
use tokio_rustls::{rustls, TlsAcceptor};

const MAX_PEM_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GatewayTlsConfig {
    pub enabled: bool,
    pub port: u16,
    pub certificate_path: String,
    /// Explicit operator-managed alternative to a native-store reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_key_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_key_ref: Option<VaultRef>,
    /// A certificate SAN, never a wildcard listener address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advertised_host: Option<String>,
}

impl Default for GatewayTlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8443,
            certificate_path: String::new(),
            private_key_path: None,
            private_key_ref: None,
            advertised_host: None,
        }
    }
}

impl GatewayTlsConfig {
    pub fn validate(&self, http_port: u16) -> Result<(), String> {
        if self.port == 0 || (self.enabled && self.port == http_port) {
            return Err("HTTPS needs a nonzero port different from the local HTTP port".into());
        }
        if let Some(host) = self.advertised_host.as_deref().filter(|s| !s.is_empty()) {
            validate_host(host)?;
        }
        if self.enabled
            && (self.certificate_path.trim().is_empty()
                || (self.private_key_ref.is_none()
                    && self
                        .private_key_path
                        .as_deref()
                        .unwrap_or("")
                        .trim()
                        .is_empty()))
        {
            return Err("HTTPS requires a certificate and a native-store key or explicit protected key path".into());
        }
        Ok(())
    }

    pub fn base_url(&self, bind_host: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let host = self
            .advertised_host
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(bind_host);
        let host = match host.parse::<std::net::IpAddr>() {
            Ok(ip) if ip.is_unspecified() => "localhost".to_string(),
            Ok(std::net::IpAddr::V6(ip)) => format!("[{ip}]"),
            _ => host.to_string(),
        };
        Some(format!("https://{host}:{}", self.port))
    }

    pub fn acceptor(&self, http_port: u16) -> Result<Option<TlsAcceptor>, String> {
        self.validate(http_port)?;
        if !self.enabled {
            return Ok(None);
        }
        let certificate = read_pem(Path::new(&self.certificate_path), false)?;
        let key = if let Some(reference) = &self.private_key_ref {
            let value = credential_store::load_json("tls", "gateway", reference)?;
            value
                .get("private_key_pem")
                .and_then(serde_json::Value::as_str)
                .ok_or("Native HTTPS identity is missing its private key")?
                .as_bytes()
                .to_vec()
        } else {
            read_pem(
                Path::new(
                    self.private_key_path
                        .as_deref()
                        .ok_or("Missing HTTPS private key")?,
                ),
                true,
            )?
        };
        Ok(Some(TlsAcceptor::from(Arc::new(server_config(
            &certificate,
            &key,
        )?))))
    }
}

fn validate_host(host: &str) -> Result<(), String> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return if ip.is_unspecified() {
            Err("HTTPS advertised host must be a certificate SAN, not a wildcard".into())
        } else {
            Ok(())
        };
    }
    if host.len() > 253
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err("HTTPS advertised host must be a DNS hostname or IPv4/IPv6 address".into());
    }
    Ok(())
}

fn read_pem(path: &Path, private: bool) -> Result<Vec<u8>, String> {
    let initial = std::fs::metadata(path).map_err(|_| "Cannot inspect HTTPS identity file")?;
    if !initial.is_file() || initial.len() > MAX_PEM_BYTES {
        return Err("HTTPS identity must be a regular PEM file no larger than 64 KiB".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A path swapped to a FIFO after the precheck must not block startup.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| "Cannot open HTTPS identity file")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect HTTPS identity file")?;
    if !metadata.is_file() || metadata.len() > MAX_PEM_BYTES {
        return Err("HTTPS identity must be a regular PEM file no larger than 64 KiB".into());
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("HTTPS private key must be readable only by its owner (chmod 600)".into());
        }
    }
    #[cfg(not(unix))]
    let _ = private; // Windows operator imports must use a private ACL; managed keys use the OS vault.
    let mut bytes = Vec::new();
    file.take(MAX_PEM_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read HTTPS identity file")?;
    if bytes.len() as u64 > MAX_PEM_BYTES {
        return Err("HTTPS identity exceeds 64 KiB".into());
    }
    Ok(bytes)
}

fn server_config(certificate: &[u8], key: &[u8]) -> Result<rustls::ServerConfig, String> {
    let certificates: Vec<_> = CertificateDer::pem_slice_iter(certificate)
        .collect::<Result<_, _>>()
        .map_err(|_| "Invalid HTTPS certificate PEM")?;
    if certificates.is_empty() {
        return Err("HTTPS certificate chain is empty".into());
    }
    let key = PrivateKeyDer::from_pem_slice(key).map_err(|_| "Invalid HTTPS private-key PEM")?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "Cannot configure HTTPS protocol versions")?
    .with_no_client_auth()
    .with_single_cert(certificates, key)
    .map_err(|_| "HTTPS certificate and private key are invalid or do not match")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// Both transports share the identical router and actual TCP peer address.
pub(super) async fn serve_listener(
    listener: tokio::net::TcpListener,
    acceptor: Option<TlsAcceptor>,
    app: axum::Router,
    cancel: tokio_util::sync::CancellationToken,
) {
    let handshakes = Arc::new(tokio::sync::Semaphore::new(128));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let accepted = tokio::select! {
            _ = cancel.cancelled() => break,
            _ = connections.join_next(), if !connections.is_empty() => continue,
            result = listener.accept() => result,
        };
        let (stream, peer) = match accepted {
            Ok(value) => value,
            Err(error) => {
                tracing::error!("Gateway accept failed: {error}");
                continue;
            }
        };
        let Ok(local_addr) = stream.local_addr() else {
            continue;
        };
        let authority = crate::proxy::middleware::cors::ListenerAuthority {
            local_addr,
            tls: acceptor.is_some(),
        };
        let app = app.clone();
        let cancel = cancel.clone();
        if let Some(acceptor) = acceptor.clone() {
            let Ok(permit) = handshakes.clone().try_acquire_owned() else {
                continue;
            };
            connections.spawn(async move {
                let result = tokio::select! {
                    _ = cancel.cancelled() => return,
                    result = tokio::time::timeout(std::time::Duration::from_secs(10), acceptor.accept(stream)) => result,
                };
                drop(permit);
                if let Ok(Ok(stream)) = result {
                    serve_connection(stream, peer, authority, app, cancel).await;
                }
            });
        } else {
            connections.spawn(serve_connection(stream, peer, authority, app, cancel));
        }
    }
    // Accepted connections observe the same cancellation token and receive a
    // bounded graceful drain before this listener's owner can finish shutdown.
    if tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
}

async fn serve_connection<S>(
    stream: S,
    peer: std::net::SocketAddr,
    authority: crate::proxy::middleware::cors::ListenerAuthority,
    app: axum::Router,
    cancel: tokio_util::sync::CancellationToken,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tower::ServiceExt;
    let peer = match peer {
        std::net::SocketAddr::V6(ip) => ip
            .ip()
            .to_ipv4_mapped()
            .map(|v4| std::net::SocketAddr::from((v4, ip.port())))
            .unwrap_or(peer),
        _ => peer,
    };
    let app = app.map_request(
        move |mut request: axum::http::Request<hyper::body::Incoming>| {
            request
                .extensions_mut()
                .insert(axum::extract::ConnectInfo(peer));
            request.extensions_mut().insert(authority);
            request
        },
    );
    let service = hyper_util::service::TowerToHyperService::new(app);
    if authority.tls {
        let connection = hyper::server::conn::http1::Builder::new()
            .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
            .with_upgrades();
        tokio::pin!(connection);
        tokio::select! {
            result = connection.as_mut() => { if let Err(error) = result { tracing::debug!("Gateway connection ended: {error}"); } },
            _ = cancel.cancelled() => {
                connection.as_mut().graceful_shutdown();
                let _ = tokio::time::timeout(std::time::Duration::from_millis(500), connection).await;
            }
        }
    } else {
        // Match Axum's original plaintext HTTP/1 + prior-knowledge HTTP/2
        // support. HTTP/2 requests may supply :authority without a Host header.
        let builder =
            hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
        let connection =
            builder.serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), service);
        tokio::pin!(connection);
        tokio::select! {
            result = connection.as_mut() => { if let Err(error) = result { tracing::debug!("Gateway connection ended: {error}"); } },
            _ = cancel.cancelled() => {
                connection.as_mut().graceful_shutdown();
                let _ = tokio::time::timeout(std::time::Duration::from_millis(500), connection).await;
            }
        }
    }
}

/// Local CLI only: validate before storing; publish the reference before deleting any old key.
pub fn import_identity(
    certificate_path: &str,
    key_path: &str,
    advertised_host: Option<String>,
    port: u16,
) -> Result<(), String> {
    let certificate_path =
        std::fs::canonicalize(certificate_path).map_err(|_| "Cannot resolve certificate path")?;
    let certificate = read_pem(&certificate_path, false)?;
    let key = read_pem(Path::new(key_path), true)?;
    let _ = server_config(&certificate, &key)?;
    let mut config = crate::modules::config::load_app_config()?;
    let mut tls = GatewayTlsConfig {
        enabled: true,
        port,
        certificate_path: certificate_path.to_string_lossy().into_owned(),
        advertised_host,
        ..Default::default()
    };
    // Validate all public options before creating a native-store generation.
    tls.private_key_path = Some(key_path.into());
    tls.validate(config.proxy.port)?;
    let key = String::from_utf8(key).map_err(|_| "HTTPS private key must be PEM text")?;
    let reference = credential_store::store_json(
        "tls",
        "gateway",
        &serde_json::json!({"private_key_pem":key}),
    )?;
    tls.private_key_path = None;
    tls.private_key_ref = Some(reference.clone());
    config.proxy.tls = tls;
    if let Err(error) = crate::modules::config::save_app_config(&config) {
        let _ = credential_store::delete("tls", &reference);
        return Err(error);
    }
    // An already running GUI may retain an older configuration snapshot. Keep its
    // immutable key generation usable; close the app before rotating an identity.
    Ok(())
}

pub fn import_cli(args: &[String]) -> Result<(), String> {
    let mut certificate = None;
    let mut key = None;
    let mut host = None;
    let mut port = 8443;
    let mut args = args.iter();
    while let Some(option) = args.next() {
        let value = args.next().ok_or("Missing tls-import option value")?;
        match option.as_str() {
            "--cert" => certificate = Some(value.as_str()),
            "--key" => key = Some(value.as_str()),
            "--advertise" => host = Some(value.clone()),
            "--port" => {
                port = value
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or("Invalid HTTPS port")?
            }
            _ => return Err("Unknown tls-import option".into()),
        }
    }
    import_identity(
        certificate.ok_or("tls-import requires --cert")?,
        key.ok_or("tls-import requires --key")?,
        host,
        port,
    )
}

pub fn apply_environment(config: &mut GatewayTlsConfig, http_port: u16) -> Result<bool, String> {
    let mut changed = false;
    if let Ok(value) = std::env::var("ABV_TLS_ENABLED") {
        config.enabled = match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => return Err("ABV_TLS_ENABLED must be a boolean".into()),
        };
        changed = true;
    }
    if let Ok(value) = std::env::var("ABV_TLS_PORT") {
        config.port = value
            .parse()
            .map_err(|_| "ABV_TLS_PORT must be a valid port")?;
        changed = true;
    }
    if let Ok(value) = std::env::var("ABV_TLS_CERT_PATH") {
        config.certificate_path = value;
        changed = true;
    }
    if let Ok(value) = std::env::var("ABV_TLS_KEY_PATH") {
        // An explicit operator override chooses its key; never fall back on vault failure.
        config.private_key_path = Some(value);
        config.private_key_ref = None;
        changed = true;
    }
    if let Ok(value) = std::env::var("ABV_TLS_ADVERTISED_HOST") {
        config.advertised_host = Some(value);
        changed = true;
    }
    config.validate(http_port)?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEST_CERT: &[u8] = include_bytes!("testdata/localhost-test-only.cert.pem");
    const TEST_KEY: &[u8] = include_bytes!("testdata/localhost-test-only.key.pem");
    #[test]
    fn tls_configuration_rejects_ambiguous_or_injected_endpoints() {
        let mut tls = GatewayTlsConfig::default();
        assert!(tls.validate(8045).is_ok());
        for host in [
            "0.0.0.0",
            "::",
            "host/path",
            "host:8443",
            "user@host",
            "-invalid",
        ] {
            tls.advertised_host = Some(host.into());
            assert!(tls.validate(8045).is_err(), "{host}");
        }
        tls.advertised_host = Some("192.168.0.168".into());
        tls.enabled = true;
        tls.certificate_path = "cert.pem".into();
        tls.private_key_path = Some("key.pem".into());
        assert!(tls.validate(8045).is_ok());
        assert_eq!(
            tls.base_url("0.0.0.0").unwrap(),
            "https://192.168.0.168:8443"
        );
        tls.port = 8045;
        assert!(tls.validate(8045).is_err());
        assert!(server_config(b"bad", b"bad").is_err());
    }

    #[test]
    fn tls_urls_preserve_ipv6_and_never_publish_a_wildcard() {
        let mut tls = GatewayTlsConfig {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(tls.base_url("::").unwrap(), "https://localhost:8443");
        tls.advertised_host = Some("::1".into());
        assert_eq!(tls.base_url("0.0.0.0").unwrap(), "https://[::1]:8443");
        let mut proxy = crate::proxy::ProxyConfig::default();
        proxy.bind_address = Some("192.168.0.168".into());
        proxy.tls.enabled = true;
        assert_eq!(proxy.local_base_url(), "http://127.0.0.1:8045");
    }

    #[test]
    fn tls_import_validates_before_publish_and_round_trips_only_a_vault_reference() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        let dir = crate::modules::account::get_data_dir().unwrap();
        crate::modules::config::save_app_config(&crate::models::AppConfig::default()).unwrap();
        let saved_path = dir.join("gui_config.json");
        let original = std::fs::read(&saved_path).unwrap();
        let cert_path = dir.join("public-test-only-cert.pem");
        let key_path = dir.join("public-test-only-key.pem");
        std::fs::write(&cert_path, TEST_CERT).unwrap();
        std::fs::write(&key_path, b"not a private key").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let cert = cert_path.to_str().unwrap();
        let key = key_path.to_str().unwrap();
        assert!(import_identity(cert, key, Some("localhost".into()), 8443).is_err());
        assert_eq!(std::fs::read(&saved_path).unwrap(), original);
        std::fs::write(&key_path, TEST_KEY).unwrap();
        import_identity(cert, key, Some("localhost".into()), 8443).unwrap();
        let config = crate::modules::config::load_app_config().unwrap();
        assert!(config.proxy.tls.private_key_ref.is_some());
        assert!(config.proxy.tls.private_key_path.is_none());
        assert!(config
            .proxy
            .tls
            .acceptor(config.proxy.port)
            .unwrap()
            .is_some());
        let saved = std::fs::read_to_string(&saved_path).unwrap();
        assert!(!saved.contains("BEGIN PRIVATE KEY"));
        assert!(!saved.contains("private_key_pem"));
        assert!(
            key_path.exists(),
            "import must retain the source until operator cleanup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tls_identity_rejects_named_pipes_and_shared_private_key_permissions() {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let pipe = dir.path().join("not-a-key.fifo");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_pem(&pipe, true).is_err());
        let key = dir.path().join("test-only-key.pem");
        std::fs::write(&key, TEST_KEY).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_pem(&key, true).is_err());
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_pem(&key, true).unwrap(), TEST_KEY);
    }

    fn connector(trust_fixture: bool) -> tokio_rustls::TlsConnector {
        let mut roots = rustls::RootCertStore::empty();
        if trust_fixture {
            roots
                .add(CertificateDer::from_pem_slice(TEST_CERT).unwrap())
                .unwrap();
        }
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        tokio_rustls::TlsConnector::from(Arc::new(client))
    }

    #[tokio::test]
    async fn plaintext_http2_authority_is_guarded_and_connection_drains_on_shutdown() {
        let security = Arc::new(tokio::sync::RwLock::new(
            crate::proxy::ProxySecurityConfig::from_proxy_config(&Default::default()),
        ));
        let app = axum::Router::new()
            .fallback(|| async { axum::http::StatusCode::NO_CONTENT })
            .layer(axum::middleware::from_fn_with_state(
                security,
                crate::proxy::middleware::cors::origin_policy_middleware,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let server = tokio::spawn(serve_listener(listener, None, app, cancel.clone()));
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, connection) = hyper::client::conn::http2::handshake(
            hyper_util::rt::TokioExecutor::new(),
            hyper_util::rt::TokioIo::new(socket),
        )
        .await
        .unwrap();
        let client_task = tokio::spawn(connection);
        for (host, status) in [
            ("localhost", axum::http::StatusCode::NO_CONTENT),
            ("attacker.example", axum::http::StatusCode::FORBIDDEN),
        ] {
            // No Host header: the HTTP/2 :authority must reach the policy.
            let request = axum::http::Request::builder()
                .uri(format!("http://{host}:{}/v1/messages", address.port()))
                .body(axum::body::Body::empty())
                .unwrap();
            assert!(!request.headers().contains_key(axum::http::header::HOST));
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client.send_request(request),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(response.version(), axum::http::Version::HTTP_2);
            assert_eq!(response.status(), status);
        }
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), client_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn https_transport_preserves_auth_peer_sse_and_websocket_upgrade() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut config = crate::proxy::ProxyConfig::default();
        config.allow_lan_access = true;
        config.tls.enabled = true;
        config.auth_mode = crate::proxy::ProxyAuthMode::Off;
        config.api_key = "test-only-transport-key".into();
        let security = Arc::new(tokio::sync::RwLock::new(
            crate::proxy::ProxySecurityConfig::from_proxy_config(&config),
        ));
        let app = axum::Router::new()
            .route(
                "/sse",
                axum::routing::get(
                    |axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<
                        std::net::SocketAddr,
                    >| async move {
                        assert!(peer.ip().is_loopback());
                        (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            "data: test-only\n\n",
                        )
                    },
                ),
            )
            .route(
                "/ws",
                axum::routing::get(|upgrade: axum::extract::WebSocketUpgrade| async move {
                    upgrade.on_upgrade(|mut socket| async move {
                        let _ = socket
                            .send(axum::extract::ws::Message::Text("test-only".into()))
                            .await;
                    })
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                security.clone(),
                crate::proxy::middleware::auth::auth_middleware,
            ))
            .layer(axum::middleware::from_fn_with_state(
                security,
                crate::proxy::middleware::cors::origin_policy_middleware,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(serve_listener(
            listener,
            Some(TlsAcceptor::from(Arc::new(
                server_config(TEST_CERT, TEST_KEY).unwrap(),
            ))),
            app,
            cancel.clone(),
        ));
        let valid_host = format!("localhost:{}", address.port());
        for (path, host, authorization, websocket, expected) in [
            ("/sse", valid_host.as_str(), "", false, "401"),
            (
                "/sse",
                valid_host.as_str(),
                "Authorization: Bearer test-only-transport-key\r\n",
                false,
                "data: test-only",
            ),
            (
                "/ws",
                valid_host.as_str(),
                "Authorization: Bearer test-only-transport-key\r\n",
                true,
                "101 Switching Protocols",
            ),
            (
                "/sse",
                "attacker.example:8443",
                "Authorization: Bearer test-only-transport-key\r\n",
                false,
                "403",
            ),
            (
                "/ws",
                "attacker.example:8443",
                "Authorization: Bearer test-only-transport-key\r\n",
                true,
                "403",
            ),
        ] {
            let socket = tokio::net::TcpStream::connect(address).await.unwrap();
            let mut stream = connector(true)
                .connect("localhost".try_into().unwrap(), socket)
                .await
                .unwrap();
            let upgrade = if websocket {
                "Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
            } else {
                "Connection: close\r\n"
            };
            stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{authorization}{upgrade}\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = Vec::new();
            // A rejected upgrade is an ordinary HTTP response and may keep its
            // connection open. Read the response boundary instead of EOF.
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                let mut chunk = [0u8; 4096];
                loop {
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(count, 0, "response ended before its expected boundary");
                    response.extend_from_slice(&chunk[..count]);
                    assert!(response.len() <= 64 * 1024, "oversized test response");
                    if response.windows(4).any(|bytes| bytes == b"\r\n\r\n")
                        && (expected != "data: test-only"
                            || response
                                .windows(b"data: test-only\n\n".len())
                                .any(|bytes| bytes == b"data: test-only\n\n"))
                    {
                        break;
                    }
                }
            })
            .await
            .expect("HTTP response boundary timed out");
            let expected_status = if expected == "data: test-only" {
                "200"
            } else {
                expected
            };
            assert!(String::from_utf8_lossy(&response)
                .starts_with(&format!("HTTP/1.1 {expected_status}")));
            assert!(
                String::from_utf8_lossy(&response).contains(expected),
                "{}",
                String::from_utf8_lossy(&response)
            );
        }
        for (trusted, name) in [(false, "localhost"), (true, "wrong-host.invalid")] {
            let socket = tokio::net::TcpStream::connect(address).await.unwrap();
            assert!(connector(trusted)
                .connect(name.try_into().unwrap(), socket)
                .await
                .is_err());
        }
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}
