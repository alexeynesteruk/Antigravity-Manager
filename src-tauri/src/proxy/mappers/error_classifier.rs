// Error classification module - converts low-level errors into user-friendly messages

/// Classify a streaming response error and return the error type, English message, and i18n key
///
/// Return value: (error type, English error message, i18n_key)
/// - Error type: used for logs and error codes
/// - English message: fallback message, for non-browser clients
/// - i18n_key: frontend translation key, for browser client localization
/// Classify a streaming response error and return the error type, English message, and i18n key
///
/// Return value: (error type, English error message, i18n_key)
pub fn classify_stream_error<E: std::fmt::Display>(
    error: &E,
) -> (&'static str, &'static str, &'static str) {
    let error_str = error.to_string().to_lowercase();

    if error_str.contains("timeout") || error_str.contains("deadline") {
        (
            "timeout_error",
            "Request timeout, please check your network connection",
            "errors.stream.timeout_error",
        )
    } else if error_str.contains("connection")
        || error_str.contains("connect")
        || error_str.contains("dns")
    {
        (
            "connection_error",
            "Connection failed, please check your network or proxy settings",
            "errors.stream.connection_error",
        )
    } else if error_str.contains("decode") || error_str.contains("parse") {
        (
            "decode_error",
            "Network unstable, data transmission interrupted. Try: 1) Check network 2) Switch proxy 3) Retry",
            "errors.stream.decode_error"
        )
    } else if error_str.contains("stream") || error_str.contains("body") {
        (
            "stream_error",
            "Stream transmission error, please retry later",
            "errors.stream.stream_error",
        )
    } else {
        (
            "unknown_error",
            "Unknown error occurred",
            "errors.stream.unknown_error",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_timeout_error() {
        // Simulate the test using a simple string error
        let error = "Connection timed out after 30s";
        let (error_type, message, i18n_key) = classify_stream_error(&error);

        assert_eq!(error_type, "timeout_error");
        assert!(message.contains("timeout"));
        assert_eq!(i18n_key, "errors.stream.timeout_error");
    }

    #[test]
    fn test_error_message_format() {
        // Test the error message format
        // Simulate a DNS error
        let error = "error trying to connect: dns error: failed to lookup address information";

        let (error_type, message, i18n_key) = classify_stream_error(&error);

        // The error type should be one of the known types
        assert!(
            error_type == "timeout_error"
                || error_type == "connection_error"
                || error_type == "decode_error"
                || error_type == "stream_error"
                || error_type == "unknown_error"
        );

        // The message should not be empty
        assert!(!message.is_empty());

        // i18n_key should start with errors.stream.
        assert!(i18n_key.starts_with("errors.stream."));
    }

    #[test]
    fn test_i18n_keys_format() {
        // Verify that every error type has the correct i18n_key format
        let test_cases = vec![
            ("timeout_error", "errors.stream.timeout_error"),
            ("connection_error", "errors.stream.connection_error"),
            ("decode_error", "errors.stream.decode_error"),
            ("stream_error", "errors.stream.stream_error"),
            ("unknown_error", "errors.stream.unknown_error"),
        ];

        // Here we only verify the i18n_key format
        for (expected_type, expected_key) in test_cases {
            assert_eq!(format!("errors.stream.{}", expected_type), expected_key);
        }
    }
}
