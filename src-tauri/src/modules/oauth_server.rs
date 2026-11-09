use crate::modules::oauth;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::watch;

const OAUTH_FLOW_TTL: Duration = Duration::from_secs(10 * 60);

struct OAuthFlowState {
    auth_url: String,
    #[allow(dead_code)]
    redirect_uri: String,
    state: String,
    client_key: String,
    created_at: Instant,
    code_claimed: bool,
    cancel_tx: watch::Sender<bool>,
    code_tx: mpsc::Sender<Result<String, String>>,
    code_rx: Option<mpsc::Receiver<Result<String, String>>>,
}

static OAUTH_FLOW_STATE: OnceLock<Mutex<Option<OAuthFlowState>>> = OnceLock::new();

fn get_oauth_flow_state() -> &'static Mutex<Option<OAuthFlowState>> {
    OAUTH_FLOW_STATE.get_or_init(|| Mutex::new(None))
}

impl OAuthFlowState {
    fn claim_code(
        &mut self,
        nonce: Option<&str>,
        redirect: Option<&str>,
        require_nonce: bool,
    ) -> Result<String, String> {
        if self.created_at.elapsed() >= OAUTH_FLOW_TTL {
            return Err("OAuth flow expired; start authorization again".to_string());
        }
        if self.code_claimed {
            return Err("OAuth callback was already consumed".to_string());
        }
        if require_nonce && nonce.is_none_or(str::is_empty) {
            return Err("OAuth state is required".to_string());
        }
        if nonce.is_some_and(|value| value != self.state) {
            return Err("OAuth state mismatch".to_string());
        }
        if redirect.is_some_and(|value| value != self.redirect_uri) {
            return Err("OAuth redirect does not match the pending flow".to_string());
        }
        self.code_claimed = true;
        Ok(self.client_key.clone())
    }
}

/// Atomically reserve the public callback before exchanging its code. The nonce
/// selects a pending flow bound to its redirect and selected OAuth client.
pub fn claim_pending_oauth_callback(nonce: Option<&str>, redirect: &str) -> Result<String, String> {
    let mut pending = get_oauth_flow_state()
        .lock()
        .map_err(|_| "OAuth state lock corrupted".to_string())?;
    pending
        .as_mut()
        .ok_or_else(|| "No active OAuth flow found".to_string())?
        .claim_code(nonce, Some(redirect), true)
}

fn claim_listener_callback(nonce: &str, listener_nonce: &str, redirect: &str) -> bool {
    if nonce != listener_nonce {
        return false;
    }
    get_oauth_flow_state()
        .lock()
        .ok()
        .and_then(|mut pending| {
            pending
                .as_mut()?
                .claim_code(Some(nonce), Some(redirect), true)
                .ok()
        })
        .is_some()
}

pub fn finish_pending_oauth_callback(nonce: &str) {
    if let Ok(mut pending) = get_oauth_flow_state().lock() {
        if pending.as_ref().is_some_and(|flow| flow.state == nonce) {
            if let Some(flow) = pending.take() {
                let _ = flow.cancel_tx.send(true);
            }
        }
    }
}

fn oauth_success_html() -> &'static str {
    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
    <html>\
    <body style='font-family: sans-serif; text-align: center; padding: 50px;'>\
    <h1 style='color: green;'>✅ Authorization Successful!</h1>\
    <p>You can close this window and return to the application.</p>\
    <script>setTimeout(function() { window.close(); }, 2000);</script>\
    </body>\
    </html>"
}

fn oauth_fail_html() -> &'static str {
    "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
    <html>\
    <body style='font-family: sans-serif; text-align: center; padding: 50px;'>\
    <h1 style='color: red;'>❌ Authorization Failed</h1>\
    <p>Failed to obtain Authorization Code. Please return to the app and try again.</p>\
    </body>\
    </html>"
}

fn parse_listener_callback(request: &[u8]) -> Option<(String, Result<String, String>)> {
    let request = std::str::from_utf8(request).ok()?;
    let mut parts = request.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let url = Url::parse(&format!("http://localhost{target}")).ok()?;
    if url.path() != "/oauth-callback" {
        return None;
    }
    let mut code = None;
    let mut nonce = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" if code.is_none() => code = Some(value.into_owned()),
            "state" if nonce.is_none() => nonce = Some(value.into_owned()),
            "error" if error.is_none() => error = Some(value.into_owned()),
            "code" | "state" | "error" => return None,
            _ => {}
        }
    }
    let nonce = nonce.filter(|value| !value.is_empty())?;
    match (code.filter(|value| !value.is_empty()), error) {
        (Some(code), None) => Some((nonce, Ok(code))),
        (None, Some(_)) => Some((nonce, Err("OAuth authorization was declined".into()))),
        _ => None,
    }
}

async fn serve_oauth_listener(
    listener: TcpListener,
    mut cancel_rx: watch::Receiver<bool>,
    code_tx: mpsc::Sender<Result<String, String>>,
    app_handle: Option<tauri::AppHandle>,
    listener_nonce: String,
    redirect_uri: String,
) {
    let deadline = tokio::time::Instant::now() + OAUTH_FLOW_TTL;
    loop {
        if *cancel_rx.borrow() {
            return;
        }
        let (mut stream, _) = tokio::select! {
            _ = cancel_rx.changed() => return,
            accepted = tokio::time::timeout_at(deadline, listener.accept()) => {
                match accepted {
                    Ok(Ok(connection)) => connection,
                    _ => return,
                }
            }
        };
        let read_deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(10));
        let read = async {
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while request.len() < 8192 {
                let count = stream.read(&mut buffer).await.ok()?;
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.contains(&b'\n') {
                    return parse_listener_callback(&request);
                }
            }
            None
        };
        let callback = tokio::select! {
            _ = cancel_rx.changed() => return,
            result = tokio::time::timeout_at(read_deadline, read) => result.ok().flatten(),
        };
        let callback = callback
            .filter(|(nonce, _)| claim_listener_callback(nonce, &listener_nonce, &redirect_uri));
        let accepted = callback.is_some();
        let response = if matches!(callback, Some((_, Ok(_)))) {
            oauth_success_html()
        } else {
            oauth_fail_html()
        };
        let _ = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(5)),
            stream.write_all(response.as_bytes()),
        )
        .await;
        if let Some((_, result)) = callback {
            // Invalid requests never consume the flow or occupy this channel.
            let _ = code_tx.try_send(result);
            if let Some(handle) = app_handle.as_ref() {
                use tauri::Emitter;
                let _ = handle.emit("oauth-callback-received", ());
            }
        }
        if accepted {
            return;
        }
    }
}

async fn ensure_oauth_flow_prepared(
    app_handle: Option<tauri::AppHandle>,
    requested_client_key: Option<String>,
) -> Result<String, String> {
    if let Ok(mut state) = get_oauth_flow_state().lock() {
        if let Some(s) = state.as_mut() {
            if let Some(requested_key) = requested_client_key.as_ref() {
                if s.client_key != requested_key.to_ascii_lowercase() {
                    let _ = s.cancel_tx.send(true);
                    *state = None;
                }
            }
        }
    }

    // Return URL if flow already exists and is still "fresh" (receiver hasn't been taken)
    if let Ok(mut state) = get_oauth_flow_state().lock() {
        if let Some(s) = state.as_mut() {
            if s.code_rx.is_some() && s.created_at.elapsed() < OAUTH_FLOW_TTL {
                return Ok(s.auth_url.clone());
            } else {
                // Flow is already "in progress" (rx taken), but user requested a NEW one.
                // Force cancel the old one to allow a new attempt.
                let _ = s.cancel_tx.send(true);
                *state = None;
            }
        }
    }

    // Create loopback listeners.
    // Some browsers resolve `localhost` to IPv6 (::1). To avoid "localhost refused connection",
    // we try to listen on BOTH IPv6 and IPv4 with the same port when possible.
    let mut ipv4_listener: Option<TcpListener> = None;
    let mut ipv6_listener: Option<TcpListener> = None;

    // Prefer creating one listener on an ephemeral port first, then bind the other stack to same port.
    // If both are available -> use `http://localhost:<port>` as redirect URI.
    // If only one is available -> use an explicit IP to force correct stack.
    let port: u16;
    match TcpListener::bind("[::1]:0").await {
        Ok(l6) => {
            port = l6
                .local_addr()
                .map_err(|e| format!("failed_to_get_local_port: {}", e))?
                .port();
            ipv6_listener = Some(l6);

            match TcpListener::bind(format!("127.0.0.1:{}", port)).await {
                Ok(l4) => ipv4_listener = Some(l4),
                Err(e) => {
                    crate::modules::logger::log_warn(&format!(
                        "failed_to_bind_ipv4_callback_port_127_0_0_1:{} (will only listen on IPv6): {}",
                        port, e
                    ));
                }
            }
        }
        Err(_) => {
            let l4 = TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|e| format!("failed_to_bind_local_port: {}", e))?;
            port = l4
                .local_addr()
                .map_err(|e| format!("failed_to_get_local_port: {}", e))?
                .port();
            ipv4_listener = Some(l4);

            match TcpListener::bind(format!("[::1]:{}", port)).await {
                Ok(l6) => ipv6_listener = Some(l6),
                Err(e) => {
                    crate::modules::logger::log_warn(&format!(
                        "failed_to_bind_ipv6_callback_port_::1:{} (will only listen on IPv4): {}",
                        port, e
                    ));
                }
            }
        }
    }

    let has_ipv4 = ipv4_listener.is_some();
    let has_ipv6 = ipv6_listener.is_some();

    let redirect_uri = if has_ipv4 && has_ipv6 {
        format!("http://localhost:{}/oauth-callback", port)
    } else if has_ipv4 {
        format!("http://127.0.0.1:{}/oauth-callback", port)
    } else {
        format!("http://[::1]:{}/oauth-callback", port)
    };

    let state_str = uuid::Uuid::new_v4().to_string();
    let (auth_url, resolved_client_key) = oauth::get_auth_url_with_client(
        &redirect_uri,
        &state_str,
        requested_client_key.as_deref(),
    )?;

    // Cancellation signal (supports multiple consumers)
    let (cancel_tx, cancel_rx) = watch::channel(false);
    // Use mpsc instead of oneshot to allow multiple senders (listener OR manual input)
    let (code_tx, code_rx) = mpsc::channel::<Result<String, String>>(1);

    // Install the flow before accepting callbacks. Concurrent preparations for
    // the same client reuse the first installed flow instead of orphaning it.
    {
        let mut pending = get_oauth_flow_state()
            .lock()
            .map_err(|_| "OAuth state lock corrupted".to_string())?;
        if let Some(flow) = pending.as_ref() {
            if flow.client_key == resolved_client_key
                && flow.code_rx.is_some()
                && flow.created_at.elapsed() < OAUTH_FLOW_TTL
            {
                return Ok(flow.auth_url.clone());
            }
        }
        if let Some(previous) = pending.replace(OAuthFlowState {
            auth_url: auth_url.clone(),
            redirect_uri: redirect_uri.clone(),
            state: state_str.clone(),
            client_key: resolved_client_key,
            created_at: Instant::now(),
            code_claimed: false,
            cancel_tx,
            code_tx: code_tx.clone(),
            code_rx: Some(code_rx),
        }) {
            let _ = previous.cancel_tx.send(true);
        }
    }
    for listener in [ipv4_listener, ipv6_listener].into_iter().flatten() {
        tokio::spawn(serve_oauth_listener(
            listener,
            cancel_rx.clone(),
            code_tx.clone(),
            app_handle.clone(),
            state_str.clone(),
            redirect_uri.clone(),
        ));
    }

    // Send event to frontend (for display/copying link)
    if let Some(h) = app_handle {
        use tauri::Emitter;
        let _ = h.emit("oauth-url-generated", &auth_url);
    }

    Ok(auth_url)
}

/// Pre-generate OAuth URL (does not open browser, does not block waiting for callback)
pub async fn prepare_oauth_url(
    app_handle: Option<tauri::AppHandle>,
    oauth_client_key: Option<String>,
) -> Result<String, String> {
    ensure_oauth_flow_prepared(app_handle, oauth_client_key).await
}

/// Cancel current OAuth flow
pub fn cancel_oauth_flow() {
    if let Ok(mut state) = get_oauth_flow_state().lock() {
        if let Some(s) = state.take() {
            let _ = s.cancel_tx.send(true);
            crate::modules::logger::log_info("Sent OAuth cancellation signal");
        }
    }
}

/// Start OAuth flow and wait for callback, then exchange token
pub async fn start_oauth_flow(
    app_handle: Option<tauri::AppHandle>,
    oauth_client_key: Option<String>,
) -> Result<oauth::TokenResponse, String> {
    // Ensure URL + listener are ready (this way if the user authorizes first, it won't get stuck)
    let auth_url = ensure_oauth_flow_prepared(app_handle.clone(), oauth_client_key).await?;

    if let Some(h) = app_handle {
        // Open default browser
        use tauri_plugin_opener::OpenerExt;
        h.opener()
            .open_url(&auth_url, None::<String>)
            .map_err(|e| format!("failed_to_open_browser: {}", e))?;
    }

    // Take code_rx to wait for it
    let (mut code_rx, redirect_uri, client_key, flow_nonce) = {
        let mut lock = get_oauth_flow_state()
            .lock()
            .map_err(|_| "OAuth state lock corrupted".to_string())?;
        let Some(state) = lock.as_mut() else {
            return Err("OAuth state does not exist".to_string());
        };
        if state.auth_url != auth_url {
            return Err("OAuth flow was replaced; start authorization again".to_string());
        }
        let rx = state
            .code_rx
            .take()
            .ok_or_else(|| "OAuth authorization already in progress".to_string())?;
        (
            rx,
            state.redirect_uri.clone(),
            state.client_key.clone(),
            state.state.clone(),
        )
    };

    // Wait for code (if user has already authorized, this returns immediately)
    // For mpsc, we use recv()
    let received = tokio::time::timeout(OAUTH_FLOW_TTL, code_rx.recv()).await;
    finish_pending_oauth_callback(&flow_nonce);
    let code =
        match received.map_err(|_| "OAuth flow expired; start authorization again".to_string())? {
            Some(Ok(code)) => code,
            Some(Err(e)) => return Err(e),
            None => return Err("OAuth flow channel closed unexpectedly".to_string()),
        };

    // Clean up flow state (release cancel_tx, etc.)
    oauth::exchange_code_with_client(&code, &redirect_uri, Some(&client_key)).await
}

/// Завершить OAuth flow без открытия браузера.
/// Предполагается, что пользователь открыл ссылку вручную (или ранее была открыта),
/// а мы только ждём callback и обмениваем code на token.
pub async fn complete_oauth_flow(
    app_handle: Option<tauri::AppHandle>,
) -> Result<oauth::TokenResponse, String> {
    // Ensure URL + listeners exist
    let auth_url = ensure_oauth_flow_prepared(app_handle, None).await?;

    // Take receiver to wait for code
    let (mut code_rx, redirect_uri, client_key, flow_nonce) = {
        let mut lock = get_oauth_flow_state()
            .lock()
            .map_err(|_| "OAuth state lock corrupted".to_string())?;
        let Some(state) = lock.as_mut() else {
            return Err("OAuth state does not exist".to_string());
        };
        if state.auth_url != auth_url {
            return Err("OAuth flow was replaced; start authorization again".to_string());
        }
        let rx = state
            .code_rx
            .take()
            .ok_or_else(|| "OAuth authorization already in progress".to_string())?;
        (
            rx,
            state.redirect_uri.clone(),
            state.client_key.clone(),
            state.state.clone(),
        )
    };

    let received = tokio::time::timeout(OAUTH_FLOW_TTL, code_rx.recv()).await;
    finish_pending_oauth_callback(&flow_nonce);
    let code =
        match received.map_err(|_| "OAuth flow expired; start authorization again".to_string())? {
            Some(Ok(code)) => code,
            Some(Err(e)) => return Err(e),
            None => return Err("OAuth flow channel closed unexpectedly".to_string()),
        };

    oauth::exchange_code_with_client(&code, &redirect_uri, Some(&client_key)).await
}

/// Snapshot active OAuth flow context `(redirect_uri, state, client_key)` if present.
pub fn get_active_oauth_flow_context() -> Option<(String, String, String)> {
    let lock = get_oauth_flow_state().lock().ok()?;
    let state = lock.as_ref()?;
    Some((
        state.redirect_uri.clone(),
        state.state.clone(),
        state.client_key.clone(),
    ))
}

/// Parse manual OAuth input (full callback URL, schemeless callback URL, query string, or raw code).
/// Returns `(decoded_code, extracted_state)`.
pub fn parse_manual_oauth_input(raw_input: &str) -> Result<(String, Option<String>), String> {
    let trimmed = raw_input.trim();
    if trimmed.is_empty() {
        return Err("Authorization code cannot be empty".to_string());
    }

    let extract_from_url = |url: &Url| -> Result<Option<(String, Option<String>)>, String> {
        let mut code = None;
        let mut state = None;
        let mut oauth_error = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "code" if !v.trim().is_empty() => code = Some(v.trim().to_string()),
                "state" if !v.trim().is_empty() => state = Some(v.trim().to_string()),
                "error" if !v.trim().is_empty() => oauth_error = Some(v.trim().to_string()),
                _ => {}
            }
        }
        if let Some(err) = oauth_error {
            return Err(format!("OAuth callback returned error: {}", err));
        }
        Ok(code.map(|c| (c, state)))
    };

    // 1. Standard URL starting with http:// or https://
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let url = Url::parse(trimmed).map_err(|e| format!("Invalid callback URL: {}", e))?;
        if let Some(res) = extract_from_url(&url)? {
            return Ok(res);
        }
        return Err("Callback URL does not contain a 'code' query parameter".to_string());
    }

    // 2. Schemeless localhost/127.0.0.1/[::1] URL or query fragment containing "code=" or "error="
    if trimmed.starts_with("localhost:")
        || trimmed.starts_with("127.0.0.1:")
        || trimmed.starts_with("[::1]:")
        || trimmed.contains("code=")
        || trimmed.contains("error=")
    {
        let candidate = if trimmed.starts_with('?') || trimmed.starts_with('/') {
            format!("http://localhost{}", trimmed)
        } else if trimmed.contains("://") {
            trimmed.to_string()
        } else if trimmed.starts_with("localhost")
            || trimmed.starts_with("127.0.0.1")
            || trimmed.starts_with("[::1]")
        {
            format!("http://{}", trimmed)
        } else {
            format!("http://localhost/?{}", trimmed.trim_start_matches('&'))
        };

        if let Ok(url) = Url::parse(&candidate) {
            if let Some(res) = extract_from_url(&url)? {
                return Ok(res);
            }
        }
    }

    // 3. Raw authorization code (possibly URL-encoded like "4%2F0A...")
    let dummy_url = Url::parse(&format!("http://localhost/?code={}", trimmed))
        .map_err(|_| "Invalid authorization code format".to_string())?;
    let decoded_code = dummy_url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "Authorization code cannot be empty".to_string())?;

    Ok((decoded_code, None))
}

/// Manually submit an OAuth code to complete the flow.
/// Synchronously exchanges the code using the prepared flow's redirect_uri and client_key.
pub async fn submit_oauth_code(
    code_input: String,
    state_input: Option<String>,
) -> Result<oauth::TokenResponse, String> {
    let (code, extracted_state) = parse_manual_oauth_input(&code_input)?;
    let effective_state = state_input
        .filter(|s| !s.trim().is_empty())
        .or(extracted_state);

    let (redirect_uri, client_key, code_tx) = {
        let lock = get_oauth_flow_state().lock().map_err(|e| e.to_string())?;
        if let Some(state) = lock.as_ref() {
            if let Some(ref provided_state) = effective_state {
                if provided_state != &state.state {
                    return Err("OAuth state mismatch (CSRF protection)".to_string());
                }
            }
            (
                state.redirect_uri.clone(),
                state.client_key.clone(),
                state.code_tx.clone(),
            )
        } else {
            return Err(
                "No active OAuth flow found. Please click 'Start OAuth' first.".to_string(),
            );
        }
    };

    crate::modules::logger::log_info(&format!(
        "Exchanging manual OAuth code with redirect_uri: {}, client_key: {}",
        redirect_uri, client_key
    ));

    // Exchange with the exact redirect_uri and client_key stored in the flow.
    // If exchange fails, cancel flow to ensure corrupted/expired state is not reused.
    let token_res =
        match oauth::exchange_code_with_client(&code, &redirect_uri, Some(&client_key)).await {
            Ok(res) => res,
            Err(e) => {
                crate::modules::logger::log_warn(&format!(
                    "OAuth exchange failed during manual submission, cleaning up flow state: {}",
                    e
                ));
                cancel_oauth_flow();
                return Err(e);
            }
        };

    // Also notify code_tx in case a background listener (e.g. Desktop flow) is awaiting code_rx
    let _ = code_tx.try_send(Ok(code));

    // Clean up flow state
    cancel_oauth_flow();

    Ok(token_res)
}

/// Get the active flow's client_key for callback matching
pub fn get_active_flow_client_key(state_param: Option<&str>) -> Option<String> {
    if let Ok(lock) = get_oauth_flow_state().lock() {
        if let Some(s) = lock.as_ref() {
            if let Some(expected_state) = state_param {
                if s.state == expected_state {
                    return Some(s.client_key.clone());
                }
            } else {
                return Some(s.client_key.clone());
            }
        }
    }
    None
}

/// Manually prepare an OAuth flow without starting listeners.
/// Useful for Web/Docker environments where we only need manual code submission.
pub fn prepare_oauth_flow_manually(
    redirect_uri: String,
    oauth_client_key: Option<String>,
) -> Result<String, String> {
    // 1. Check if we can reuse existing flow state
    if let Ok(lock) = get_oauth_flow_state().lock() {
        if let Some(s) = lock.as_ref() {
            let same_client = match oauth_client_key.as_ref() {
                Some(key) => s.client_key == key.to_ascii_lowercase(),
                None => true,
            };
            if same_client && s.redirect_uri == redirect_uri {
                return Ok(s.auth_url.clone());
            }
        }
    }

    // 2. Clear old state if different or fresh flow requested
    if let Ok(mut lock) = get_oauth_flow_state().lock() {
        if let Some(s) = lock.as_mut() {
            let _ = s.cancel_tx.send(true);
            *lock = None;
        }
    }

    let state_str = uuid::Uuid::new_v4().to_string();
    let (auth_url, resolved_client_key) =
        oauth::get_auth_url_with_client(&redirect_uri, &state_str, oauth_client_key.as_deref())?;

    let (cancel_tx, _cancel_rx) = watch::channel(false);
    let (code_tx, code_rx) = mpsc::channel(1);

    {
        let mut pending = get_oauth_flow_state()
            .lock()
            .map_err(|_| "OAuth state lock corrupted".to_string())?;
        // Replacing and cancelling the previous flow is one atomic transition.
        if let Some(previous) = pending.replace(OAuthFlowState {
            auth_url: auth_url.clone(),
            redirect_uri,
            state: state_str,
            client_key: resolved_client_key,
            created_at: Instant::now(),
            code_claimed: false,
            cancel_tx,
            code_tx,
            code_rx: Some(code_rx),
        }) {
            let _ = previous.cancel_tx.send(true);
        }
    }

    Ok(auth_url)
}

#[cfg(test)]
mod callback_security_tests {
    use super::*;

    struct CleanupFlow;

    impl Drop for CleanupFlow {
        fn drop(&mut self) {
            cancel_oauth_flow();
        }
    }

    fn pending() -> OAuthFlowState {
        let (cancel_tx, _) = watch::channel(false);
        let (code_tx, code_rx) = mpsc::channel(1);
        OAuthFlowState {
            auth_url: String::new(),
            redirect_uri: "http://localhost:8045/auth/callback".into(),
            state: "pending-nonce".into(),
            client_key: "selected-client".into(),
            created_at: Instant::now(),
            code_claimed: false,
            cancel_tx,
            code_tx,
            code_rx: Some(code_rx),
        }
    }

    #[test]
    fn public_callback_requires_matching_nonce_redirect_and_is_one_use() {
        let mut flow = pending();
        let redirect = flow.redirect_uri.clone();
        assert!(flow.claim_code(None, Some(&redirect), true).is_err());
        assert!(flow
            .claim_code(Some("wrong"), Some(&redirect), true)
            .is_err());
        assert!(flow
            .claim_code(
                Some("pending-nonce"),
                Some("https://other.example/callback"),
                true
            )
            .is_err());
        assert!(!flow.code_claimed);
        assert_eq!(
            flow.claim_code(Some("pending-nonce"), Some(&redirect), true)
                .unwrap(),
            "selected-client"
        );
        assert!(flow
            .claim_code(Some("pending-nonce"), Some(&redirect), true)
            .is_err());
    }

    #[test]
    fn expired_flows_are_rejected_and_authenticated_bare_code_remains_supported() {
        let mut expired = pending();
        expired.created_at = Instant::now() - OAUTH_FLOW_TTL;
        assert!(expired
            .claim_code(Some("pending-nonce"), None, true)
            .is_err());
        let mut manual = pending();
        assert_eq!(
            manual.claim_code(None, None, false).unwrap(),
            "selected-client"
        );
        assert!(manual.claim_code(None, None, false).is_err());
    }

    #[test]
    fn concurrent_callbacks_cannot_claim_the_same_flow() {
        let flow = std::sync::Arc::new(Mutex::new(pending()));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let flow = flow.clone();
                std::thread::spawn(move || {
                    flow.lock()
                        .unwrap()
                        .claim_code(Some("pending-nonce"), None, true)
                        .is_ok()
                })
            })
            .collect();
        let winners = threads
            .into_iter()
            .map(|thread| usize::from(thread.join().unwrap()))
            .sum::<usize>();
        assert_eq!(winners, 1);
    }

    #[tokio::test]
    async fn stray_loopback_requests_do_not_consume_a_pending_oauth_callback() {
        let _dir = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        let _cleanup = CleanupFlow;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut flow = pending();
        flow.redirect_uri = format!("http://{address}/oauth-callback");
        let mut code_rx = flow.code_rx.take().unwrap();
        let task = tokio::spawn(serve_oauth_listener(
            listener,
            flow.cancel_tx.subscribe(),
            flow.code_tx.clone(),
            None,
            flow.state.clone(),
            flow.redirect_uri.clone(),
        ));
        *get_oauth_flow_state().lock().unwrap() = Some(flow);

        for target in [
            "/favicon.ico",
            "/oauth-callback?code=untrusted",
            "/oauth-callback?code=untrusted&state=wrong",
            "/oauth-callback?code=&state=pending-nonce",
            "/oauth-callback?code=untrusted&state=pending-nonce&state=wrong",
        ] {
            let response = callback_request(address, target, false).await;
            assert!(response.starts_with("HTTP/1.1 400"), "{response}");
            assert!(matches!(
                code_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            assert!(
                !get_oauth_flow_state()
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .code_claimed
            );
        }

        let response = callback_request(
            address,
            "/oauth-callback?code=accepted-code&state=pending-nonce",
            true,
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), code_rx.recv())
                .await
                .unwrap(),
            Some(Ok("accepted-code".into()))
        );
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    async fn callback_request(
        address: std::net::SocketAddr,
        target: &str,
        fragmented: bool,
    ) -> String {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let request = format!("GET {target} HTTP/1.1\r\nHost: {address}\r\n\r\n");
            if fragmented {
                let split = request.find("state=").unwrap() + 3;
                stream
                    .write_all(&request.as_bytes()[..split])
                    .await
                    .unwrap();
                tokio::task::yield_now().await;
                stream
                    .write_all(&request.as_bytes()[split..])
                    .await
                    .unwrap();
            } else {
                stream.write_all(request.as_bytes()).await.unwrap();
            }
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        })
        .await
        .unwrap()
    }

    #[test]
    fn listeners_cannot_claim_a_replacement_flows_nonce() {
        let _dir = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        let _cleanup = CleanupFlow;
        let mut flow = pending();
        flow.state = "replacement-nonce".into();
        let redirect = flow.redirect_uri.clone();
        *get_oauth_flow_state().lock().unwrap() = Some(flow);
        assert!(!claim_listener_callback(
            "replacement-nonce",
            "old-listener-nonce",
            &redirect,
        ));
        assert!(
            !get_oauth_flow_state()
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .code_claimed
        );
    }

    #[tokio::test]
    async fn concurrent_preparation_reuses_one_flow_and_callback_listener() {
        let _dir = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        let _cleanup = CleanupFlow;
        cancel_oauth_flow();
        let (first, second, third) = tokio::join!(
            prepare_oauth_url(None, None),
            prepare_oauth_url(None, None),
            prepare_oauth_url(None, None),
        );
        let first = first.unwrap();
        assert_eq!(second.unwrap(), first);
        assert_eq!(third.unwrap(), first);
        assert_eq!(
            get_oauth_flow_state()
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .auth_url,
            first
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manual_oauth_input_full_url() {
        let input = "http://localhost:8045/auth/callback?state=state-123&code=4%2F0AeanS0_test_code&scope=email+profile";
        let (code, state) =
            parse_manual_oauth_input(input).expect("should parse full callback url");
        assert_eq!(code, "4/0AeanS0_test_code");
        assert_eq!(state.as_deref(), Some("state-123"));
    }

    #[test]
    fn test_parse_manual_oauth_input_schemeless_localhost_url() {
        let input = "localhost:8045/auth/callback?code=4%2F0AeanS0_schemeless&state=st-999";
        let (code, state) =
            parse_manual_oauth_input(input).expect("should parse schemeless callback url");
        assert_eq!(code, "4/0AeanS0_schemeless");
        assert_eq!(state.as_deref(), Some("st-999"));
    }

    #[test]
    fn test_parse_manual_oauth_input_raw_encoded_and_decoded_code() {
        let (code1, state1) =
            parse_manual_oauth_input("  4%2F0AeanS0_encoded_raw  ").expect("should decode raw %2F");
        assert_eq!(code1, "4/0AeanS0_encoded_raw");
        assert!(state1.is_none());

        let (code2, state2) =
            parse_manual_oauth_input("4/0AeanS0_plain_raw").expect("should keep plain raw code");
        assert_eq!(code2, "4/0AeanS0_plain_raw");
        assert!(state2.is_none());
    }

    #[test]
    fn test_parse_manual_oauth_input_rejects_error_callback_and_empty() {
        let err = parse_manual_oauth_input(
            "http://localhost:8045/auth/callback?error=access_denied&state=s1",
        )
        .unwrap_err();
        assert!(err.contains("access_denied"));

        assert!(parse_manual_oauth_input("   ").is_err());
    }

    #[test]
    fn test_prepare_oauth_flow_manually_reuse() {
        let redirect_uri = "http://localhost:8045/auth/callback".to_string();
        let url1 = prepare_oauth_flow_manually(redirect_uri.clone(), None)
            .expect("First prepare should succeed");
        let url2 = prepare_oauth_flow_manually(redirect_uri.clone(), None)
            .expect("Second prepare should succeed");

        assert_eq!(
            url1, url2,
            "Subsequent calls with same parameters should reuse the active flow URL"
        );
    }

    #[tokio::test]
    async fn test_submit_oauth_code_state_mismatch_and_cleanup() {
        let redirect_uri = "http://localhost:8045/auth/callback".to_string();
        let _ = prepare_oauth_flow_manually(redirect_uri.clone(), None)
            .expect("prepare should succeed");

        // Submit with deliberately mismatched state
        let err = submit_oauth_code("test_code".to_string(), Some("wrong-state".to_string()))
            .await
            .unwrap_err();
        assert!(err.contains("CSRF protection"));

        // Clean up
        cancel_oauth_flow();
        assert!(get_active_oauth_flow_context().is_none());
    }
}
