use crate::modules::security_db;
use crate::proxy::server::AppState;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

/// IP 黑白名单过滤中间件
pub async fn ip_filter_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    // 提取客户端 IP
    let security_config = state.security.read().await.security_monitor.clone();
    let trusted = effective_trusted_proxies(&security_config.trusted_proxies);
    let client_ip = resolve_client_ip(&request, &trusted);
    if let Some(ip) = &client_ip {
        request
            .extensions_mut()
            .insert(ResolvedClientIp(ip.clone()));
    } else if security_config.whitelist.enabled || peer_ip(&request).is_some() {
        // A trusted proxy with an invalid forwarding chain must not inherit the
        // proxy's own whitelist/token permissions. Reject it before auth and logs.
        return create_blocked_response(
            "unknown",
            "Unable to verify the client IP.",
        );
    }

    if let Some(ip) = &client_ip {
        // 读取安全配置

        // 1. 检查白名单 (如果启用白名单模式,只允许白名单 IP)
        if security_config.whitelist.enabled {
            match security_db::is_ip_in_whitelist(ip) {
                Ok(true) => {
                    // 在白名单中,直接放行
                    tracing::debug!("[IP Filter] IP {} is in whitelist, allowing", ip);
                    return next.run(request).await;
                }
                Ok(false) => {
                    // 不在白名单中,且启用了白名单模式,拒绝访问
                    tracing::warn!("[IP Filter] IP {} not in whitelist, blocking", ip);
                    return create_blocked_response(
                        ip,
                        "Access denied. Your IP is not in the whitelist.",
                    );
                }
                Err(e) => {
                    tracing::error!("[IP Filter] Failed to check whitelist: {}", e);
                }
            }
        } else {
            // 白名单优先模式: 如果在白名单中,跳过黑名单检查
            if security_config.whitelist.whitelist_priority {
                match security_db::is_ip_in_whitelist(ip) {
                    Ok(true) => {
                        tracing::debug!("[IP Filter] IP {} is in whitelist (priority mode), skipping blacklist check", ip);
                        return next.run(request).await;
                    }
                    Ok(false) => {
                        // 继续检查黑名单
                    }
                    Err(e) => {
                        tracing::error!("[IP Filter] Failed to check whitelist: {}", e);
                    }
                }
            }
        }

        // 2. 检查黑名单
        if security_config.blacklist.enabled {
            match security_db::get_blacklist_entry_for_ip(ip) {
                Ok(Some(entry)) => {
                    tracing::warn!("[IP Filter] IP {} is in blacklist, blocking", ip);

                    // 构建详细的封禁消息
                    let reason = entry
                        .reason
                        .as_deref()
                        .unwrap_or("Malicious activity detected");
                    let ban_type = if let Some(expires_at) = entry.expires_at {
                        let now = chrono::Utc::now().timestamp();
                        let remaining_seconds = expires_at - now;

                        if remaining_seconds > 0 {
                            let hours = remaining_seconds / 3600;
                            let minutes = (remaining_seconds % 3600) / 60;

                            if hours > 24 {
                                let days = hours / 24;
                                format!("Temporary ban. Please try again after {} day(s).", days)
                            } else if hours > 0 {
                                format!("Temporary ban. Please try again after {} hour(s) and {} minute(s).", hours, minutes)
                            } else {
                                format!(
                                    "Temporary ban. Please try again after {} minute(s).",
                                    minutes
                                )
                            }
                        } else {
                            "Temporary ban (expired, will be removed soon).".to_string()
                        }
                    } else {
                        "Permanent ban.".to_string()
                    };

                    let detailed_message =
                        format!("Access denied. Reason: {}. {}", reason, ban_type);

                    // 记录被封禁的访问日志
                    let log = security_db::IpAccessLog {
                        id: uuid::Uuid::new_v4().to_string(),
                        client_ip: ip.clone(),
                        timestamp: chrono::Utc::now().timestamp(),
                        method: Some(request.method().to_string()),
                        // Rejections happen before auth strips Gemini query keys.
                        path: Some(request.uri().path().to_string()),
                        user_agent: request
                            .headers()
                            .get("user-agent")
                            .and_then(|v| v.to_str().ok())
                            .map(|s| s.to_string()),
                        status: Some(403),
                        duration: Some(0),
                        api_key_hash: None,
                        blocked: true,
                        block_reason: Some(format!("IP in blacklist: {}", reason)),
                        username: None,
                    };

                    tokio::spawn(async move {
                        if let Err(e) = security_db::save_ip_access_log(&log) {
                            tracing::error!("[IP Filter] Failed to save blocked access log: {}", e);
                        }
                    });

                    return create_blocked_response(ip, &detailed_message);
                }
                Ok(None) => {
                    // 不在黑名单中,放行
                    tracing::debug!("[IP Filter] IP {} not in blacklist, allowing", ip);
                }
                Err(e) => {
                    tracing::error!("[IP Filter] Failed to check blacklist: {}", e);
                }
            }
        }
    } else {
        tracing::warn!("[IP Filter] Unable to extract client IP from request");
    }

    // 放行请求
    next.run(request).await
}

/// 规范化 IP 地址字符串：
/// - 清理首尾空格及方括号 `[...]`
/// - 剥离客户端端口（例如 `192.168.1.1:54321` 或 `[2409:8a55::1]:8046`）
/// - 将 IPv4 映射的 IPv6 地址（如 `::ffff:192.168.1.1`）还原为原生 IPv4（`192.168.1.1`）
pub fn normalize_ip_str(raw: &str) -> String {
    let trimmed = raw.trim();

    // 优先尝试直接按 IP 地址解析
    let clean = trimmed.trim_matches('[').trim_matches(']');
    if let Ok(ip) = clean.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V6(v6) => {
                if let Some(v4) = v6.to_ipv4_mapped() {
                    v4.to_string()
                } else {
                    v6.to_string()
                }
            }
            std::net::IpAddr::V4(v4) => v4.to_string(),
        };
    }

    // 若带有端口（如 192.168.1.100:12345 或 [::1]:8080），尝试解析为 SocketAddr
    if let Ok(socket_addr) = trimmed.parse::<std::net::SocketAddr>() {
        return match socket_addr.ip() {
            std::net::IpAddr::V6(v6) => {
                if let Some(v4) = v6.to_ipv4_mapped() {
                    v4.to_string()
                } else {
                    v6.to_string()
                }
            }
            std::net::IpAddr::V4(v4) => v4.to_string(),
        };
    }

    // 手动前缀回退检测
    if let Some(stripped) = clean.strip_prefix("::ffff:") {
        if stripped.parse::<std::net::Ipv4Addr>().is_ok() {
            return stripped.to_string();
        }
    }

    clean.to_string()
}

/// 仅根据 TCP 对端地址判断是否为本机回环。
///
/// 不读取 `X-Forwarded-For` / `X-Real-IP`。这两项由客户端或反代填写，
/// 不能用来决定 `/internal/` 是否免鉴权，否则伪造 `127.0.0.1` 即可绕过。
/// 对端地址由 `server.rs` 的 accept 循环写入 `ConnectInfo`。
pub fn is_direct_loopback(request: &Request) -> bool {
    request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| ip_is_loopback(info.0.ip()))
        .unwrap_or(false)
}

fn ip_is_loopback(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Resolved once at the policy boundary, then shared by auth, token ACLs and logging.
#[derive(Clone, Debug)]
pub struct ResolvedClientIp(pub String);

pub fn effective_trusted_proxies(configured: &[String]) -> Vec<String> {
    match std::env::var("ABV_TRUSTED_PROXIES") {
        Ok(value) => value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Err(_) => configured.to_vec(),
    }
}

fn canonical_ip(ip: std::net::IpAddr) -> std::net::IpAddr {
    match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map(std::net::IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}

fn peer_ip(request: &Request) -> Option<std::net::IpAddr> {
    request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| canonical_ip(info.0.ip()))
}

fn trusted_ip(ip: std::net::IpAddr, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim();
        let (address, prefix) = match pattern.split_once('/') {
            Some((address, prefix)) => match prefix.parse::<u32>() {
                Ok(prefix) => (address, Some(prefix)),
                Err(_) => return false,
            },
            None => (pattern, None),
        };
        let Ok(network) = address.parse::<std::net::IpAddr>() else {
            return false;
        };
        match (canonical_ip(ip), network) {
            (std::net::IpAddr::V4(ip), std::net::IpAddr::V4(network)) => {
                let prefix = prefix.unwrap_or(32);
                if prefix > 32 {
                    return false;
                }
                let mask = if prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - prefix)
                };
                u32::from(ip) & mask == u32::from(network) & mask
            }
            (std::net::IpAddr::V6(ip), std::net::IpAddr::V6(network)) => {
                let prefix = prefix.unwrap_or(128);
                if prefix > 128 {
                    return false;
                }
                let mask = if prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - prefix)
                };
                u128::from(ip) & mask == u128::from(network) & mask
            }
            (std::net::IpAddr::V4(ip), std::net::IpAddr::V6(network)) => {
                let prefix = prefix.unwrap_or(128);
                let Some(network) = network.to_ipv4_mapped() else {
                    return false;
                };
                if !(96..=128).contains(&prefix) {
                    return false;
                }
                let prefix = prefix - 96;
                let mask = if prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - prefix)
                };
                u32::from(ip) & mask == u32::from(network) & mask
            }
            _ => false,
        }
    })
}

/// Walk from the TCP peer toward the client, stopping at the first untrusted hop.
/// Malformed/ambiguous forwarding headers have no verified identity. Falling
/// back to a trusted proxy's address could grant its whitelist/token permissions.
pub fn resolve_client_ip(request: &Request, trusted: &[String]) -> Option<String> {
    let peer = peer_ip(request)?;
    let fallback = || Some(peer.to_string());
    if !trusted_ip(peer, trusted) {
        return fallback();
    }
    let mut xff = request.headers().get_all("x-forwarded-for").iter();
    if let Some(header) = xff.next() {
        if xff.next().is_some() {
            return None;
        }
        let Ok(raw) = header.to_str() else {
            return None;
        };
        let mut chain = Vec::new();
        for part in raw.split(',') {
            if chain.len() >= 64 {
                return None;
            }
            let Ok(ip) = part.trim().parse::<std::net::IpAddr>() else {
                return None;
            };
            chain.push(canonical_ip(ip));
        }
        let mut current = peer;
        for previous in chain.into_iter().rev() {
            if !trusted_ip(current, trusted) {
                break;
            }
            current = previous;
        }
        return Some(current.to_string());
    }
    let mut xri = request.headers().get_all("x-real-ip").iter();
    if let Some(header) = xri.next() {
        if xri.next().is_some() {
            return None;
        }
        let ip = header.to_str().ok()?.trim().parse().ok()?;
        return Some(canonical_ip(ip).to_string());
    }
    fallback()
}

/// Downstream consumers only use the verified identity or the actual TCP peer.
pub fn extract_client_ip(request: &Request) -> Option<String> {
    request
        .extensions()
        .get::<ResolvedClientIp>()
        .map(|ip| ip.0.clone())
        .or_else(|| peer_ip(request).map(|ip| ip.to_string()))
}

/// 创建被封禁的响应
fn create_blocked_response(ip: &str, message: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "ip_blocked",
            "code": "ip_blocked",
            "ip": ip,
        }
    });

    (
        StatusCode::FORBIDDEN,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&body).unwrap_or_else(|_| message.to_string()),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn malformed_proxy_headers_cannot_inherit_the_proxys_whitelist() {
        use tower::ServiceExt;
        let _dir = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        security_db::init_db().unwrap();
        security_db::add_to_whitelist("127.0.0.1", Some("local reverse proxy")).unwrap();
        let state = crate::proxy::server::config_reload_tests::test_state();
        {
            let mut security = state.security.write().await;
            security.security_monitor.trusted_proxies = vec!["127.0.0.1".into()];
            security.security_monitor.whitelist.enabled = true;
        }
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { StatusCode::NO_CONTENT }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                ip_filter_middleware,
            ));
        // A common reverse proxy appends the real client to the caller's XFF.
        // Neither that malformed chain nor a duplicate/invalid alternative may
        // fall back to the whitelisted loopback peer and reach the handler.
        for (name, value, duplicate) in [
            ("x-forwarded-for", "garbage, 203.0.113.8", false),
            ("x-forwarded-for", "203.0.113.8", true),
            ("x-real-ip", "garbage", false),
            ("x-real-ip", "203.0.113.8", true),
        ] {
            let mut request = request_from("127.0.0.1".parse().unwrap(), None);
            request.headers_mut().insert(name, value.parse().unwrap());
            if duplicate {
                request
                    .headers_mut()
                    .append(name, "192.0.2.99".parse().unwrap());
            }
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        // The same invalid identity is rejected with the whitelist disabled,
        // before token ACLs could treat the proxy itself as the caller.
        state.security.write().await.security_monitor.whitelist.enabled = false;
        assert_eq!(
            app.clone()
                .oneshot(request_from(
                    "127.0.0.1".parse().unwrap(),
                    Some("garbage, 203.0.113.8"),
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.oneshot(request_from("127.0.0.1".parse().unwrap(), None))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn rejected_gemini_queries_never_persist_url_credentials() {
        use tower::ServiceExt;
        let _dir = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        security_db::init_db().unwrap();
        security_db::add_to_blacklist("203.0.113.88", Some("test"), None, "test").unwrap();
        let state = crate::proxy::server::config_reload_tests::test_state();
        state
            .security
            .write()
            .await
            .security_monitor
            .blacklist
            .enabled = true;
        let app = axum::Router::new()
            .route(
                "/v1beta/models",
                axum::routing::get(|| async { StatusCode::NO_CONTENT }),
            )
            .layer(axum::middleware::from_fn_with_state(
                state,
                ip_filter_middleware,
            ));
        let mut request = Request::builder()
            .uri("/v1beta/models?key=private%2Bkey&other=private-value")
            .body(axum::body::Body::empty())
            .unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            "203.0.113.88:1234".parse::<std::net::SocketAddr>().unwrap(),
        ));
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let logs = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let logs =
                    security_db::get_ip_access_logs(10, 0, Some("203.0.113.88"), true).unwrap();
                if !logs.is_empty() {
                    break logs;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(logs[0].path.as_deref(), Some("/v1beta/models"));
    }

    #[test]
    fn forwarding_headers_require_a_trusted_tcp_peer() {
        let request = request_from("203.0.113.8".parse().unwrap(), Some("127.0.0.1"));
        assert_eq!(
            resolve_client_ip(&request, &[]).as_deref(),
            Some("203.0.113.8")
        );
        assert_eq!(
            resolve_client_ip(&request, &["127.0.0.1/32".into()]).as_deref(),
            Some("203.0.113.8")
        );
        assert_eq!(extract_client_ip(&request).as_deref(), Some("203.0.113.8"));
    }

    #[test]
    fn trusted_chains_stop_at_the_rightmost_untrusted_hop() {
        let request = request_from(
            "127.0.0.1".parse().unwrap(),
            Some("192.0.2.99, 203.0.113.8, 10.1.2.3"),
        );
        let trusted = vec!["127.0.0.0/8".into(), "10.0.0.0/8".into()];
        assert_eq!(
            resolve_client_ip(&request, &trusted).as_deref(),
            Some("203.0.113.8")
        );
        let request = request_from(
            "::ffff:127.0.0.1".parse().unwrap(),
            Some("2001:db8::8, fd00::9"),
        );
        let trusted = vec!["127.0.0.1".into(), "fd00::/8".into()];
        assert_eq!(
            resolve_client_ip(&request, &trusted).as_deref(),
            Some("2001:db8::8")
        );
    }

    #[test]
    fn ambiguous_or_malformed_forwarding_fails_closed() {
        let trusted = vec!["127.0.0.1".into()];
        for chain in ["garbage, 203.0.113.8", "", "203.0.113.8,", "203.0.113.8:80"] {
            let mut request = request_from("127.0.0.1".parse().unwrap(), Some(chain));
            request
                .headers_mut()
                .insert("x-real-ip", "192.0.2.99".parse().unwrap());
            assert_eq!(resolve_client_ip(&request, &trusted), None);
        }
        let mut request = request_from("127.0.0.1".parse().unwrap(), Some("203.0.113.8"));
        request
            .headers_mut()
            .append("x-forwarded-for", "192.0.2.99".parse().unwrap());
        assert_eq!(resolve_client_ip(&request, &trusted), None);
        request
            .extensions_mut()
            .insert(ResolvedClientIp("203.0.113.8".into()));
        assert_eq!(extract_client_ip(&request).as_deref(), Some("203.0.113.8"));
        assert!(is_direct_loopback(&request));
    }

    #[test]
    fn test_normalize_ip_str() {
        assert_eq!(normalize_ip_str("192.168.1.1"), "192.168.1.1");
        assert_eq!(normalize_ip_str("192.168.1.1:8080"), "192.168.1.1");
        assert_eq!(normalize_ip_str("::ffff:192.168.1.1"), "192.168.1.1");
        assert_eq!(normalize_ip_str("[::ffff:192.168.1.1]:8046"), "192.168.1.1");
        assert_eq!(normalize_ip_str("::1"), "::1");
        assert_eq!(normalize_ip_str("[::1]:8046"), "::1");
        assert_eq!(
            normalize_ip_str("2409:8a55:a21:2e60:2e2:69ff:fe17:95cb"),
            "2409:8a55:a21:2e60:2e2:69ff:fe17:95cb"
        );
    }

    fn request_from(ip: std::net::IpAddr, forwarded_for: Option<&str>) -> Request {
        let mut builder = Request::builder();
        if let Some(xff) = forwarded_for {
            builder = builder.header("x-forwarded-for", xff);
        }
        let mut request = builder.body(axum::body::Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(
                ip, 12345,
            )));
        request
    }

    #[test]
    fn direct_loopback_uses_connect_info_only() {
        let v4_loopback: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let v6_loopback: std::net::IpAddr = "::1".parse().unwrap();
        let mapped_loopback: std::net::IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        let lan: std::net::IpAddr = "192.168.1.10".parse().unwrap();

        assert!(is_direct_loopback(&request_from(v4_loopback, None)));
        assert!(is_direct_loopback(&request_from(v6_loopback, None)));
        assert!(is_direct_loopback(&request_from(mapped_loopback, None)));
        assert!(!is_direct_loopback(&request_from(lan, None)));
        // 伪造转发头不能把公网连接伪装成本机
        assert!(!is_direct_loopback(&request_from(lan, Some("127.0.0.1"))));

        let missing = Request::builder().body(axum::body::Body::empty()).unwrap();
        assert!(!is_direct_loopback(&missing));
    }
}
