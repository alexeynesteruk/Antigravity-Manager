use super::client_adapters::OpencodeAdapter;
use axum::http::HeaderMap;
use once_cell::sync::Lazy;
use std::sync::Arc; // [NEW] Import Arc

/// The client adapter trait
///
/// Provides customized protocol-handling strategies for different clients (e.g. opencode, Cherry Studio).
/// Each client can implement its own adapter to handle its specific needs.
///
/// # Design principles
/// 1. **Full isolation**: an adapter is an optional enhancement layer and does not modify the existing core protocol logic
/// 2. **Backward compatibility**: a request that matches no adapter is handled exactly by the existing flow
/// 3. **Single-file changes**: client-specific logic is encapsulated in its own adapter file
pub trait ClientAdapter: Send + Sync {
    /// Determines whether this adapter matches the given request
    ///
    /// # Arguments
    /// * `headers` - the request headers, the client is typically identified via User-Agent or similar fields
    ///
    /// # Returns
    /// true if it matches, false otherwise
    fn matches(&self, headers: &HeaderMap) -> bool;

    /// Whether to bypass signature validation
    ///
    /// Some clients may not require strict thinking-signature matching
    #[allow(dead_code)]
    fn bypass_signature_matching(&self) -> bool {
        false
    }

    /// Whether to adopt a "let it crash" philosophy
    ///
    /// Reduces unnecessary retry and recovery logic, letting errors surface quickly
    fn let_it_crash(&self) -> bool {
        false
    }

    /// Signature buffer strategy
    ///
    /// Different clients may need different signature management approaches (FIFO/LIFO)
    fn signature_buffer_strategy(&self) -> SignatureBufferStrategy {
        SignatureBufferStrategy::Default
    }

    /// Injects a Beta Header the client is missing
    ///
    /// Some clients may need a specific Beta Header to work correctly
    fn inject_beta_headers(&self, _headers: &mut HeaderMap) {
        // Do not inject by default
    }

    /// Declares the supported protocols
    ///
    /// Used for multi-protocol clients (e.g. opencode)
    #[allow(dead_code)]
    fn supported_protocols(&self) -> Vec<Protocol> {
        vec![Protocol::Anthropic] // Only Anthropic is supported by default
    }
}

/// Signature buffer strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureBufferStrategy {
    /// Default strategy (current implementation)
    Default,
    /// FIFO (first in, first out) - suited to concurrent tool calls
    Fifo,
    /// LIFO (last in, first out) - suited to nested calls
    #[allow(dead_code)]
    Lifo,
}

/// The supported protocol types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Protocol {
    Anthropic,
    OpenAI,
    OACompatible,
    GoogleGemini,
}

/// Global client adapter registry
///
/// All registered adapters are checked when a request is processed
pub static CLIENT_ADAPTERS: Lazy<Vec<Arc<dyn ClientAdapter>>> = Lazy::new(|| {
    vec![
        Arc::new(OpencodeAdapter),
        // More adapters can be added easily in the future:
        // Arc::new(CherryStudioAdapter),
    ]
});

/// Helper function: extract the User-Agent from a HeaderMap
pub fn get_user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    struct TestAdapter;

    impl ClientAdapter for TestAdapter {
        fn matches(&self, headers: &HeaderMap) -> bool {
            get_user_agent(headers)
                .map(|ua| ua.contains("test-client"))
                .unwrap_or(false)
        }

        fn bypass_signature_matching(&self) -> bool {
            true
        }
    }

    #[test]
    fn test_adapter_matches() {
        let adapter = TestAdapter;

        let mut headers = HeaderMap::new();
        headers.insert("user-agent", HeaderValue::from_static("test-client/1.0"));

        assert!(adapter.matches(&headers));
        assert!(adapter.bypass_signature_matching());
    }

    #[test]
    fn test_adapter_no_match() {
        let adapter = TestAdapter;

        let mut headers = HeaderMap::new();
        headers.insert("user-agent", HeaderValue::from_static("other-client/1.0"));

        assert!(!adapter.matches(&headers));
    }

    #[test]
    fn test_get_user_agent() {
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", HeaderValue::from_static("opencode/1.0"));

        assert_eq!(get_user_agent(&headers), Some("opencode/1.0".to_string()));
    }
}
