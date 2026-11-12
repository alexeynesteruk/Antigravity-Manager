//! Admission for untrusted media references, before adapters normalize payloads.
//! This module never inspects or opens a server filesystem path.

use base64::{engine::general_purpose, read::DecoderReader};
use serde_json::Value;
use std::io::Read;

fn rejected(detail: &str) -> String {
    format!("Invalid media source: {detail}. Upload the bytes or use a supported remote URL; server-local paths are not accepted")
}

/// Validate encoded bytes without allocating a decoded copy of a large upload.
pub fn validate_inline_base64(data: &str) -> Result<(), String> {
    if data.is_empty() {
        return Err(rejected("empty inline data"));
    }
    let engine = if data.ends_with('=') {
        &general_purpose::STANDARD
    } else {
        &general_purpose::STANDARD_NO_PAD
    };
    let mut reader = DecoderReader::new(data.as_bytes(), engine);
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(_) => return Err(rejected("inline data must contain valid base64")),
        }
    }
}

pub fn is_data_url(source: &str) -> bool {
    source
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
}

/// URL-bearing fields accept HTTP(S) or inline data, never filesystem references.
/// Native Gemini fileData additionally supports Google Cloud Storage URIs.
pub fn validate_media_reference(source: &str, allow_gcs: bool) -> Result<(), String> {
    if is_data_url(source) {
        let (metadata, data) = source
            .split_once(',')
            .ok_or_else(|| rejected("data URL is missing its comma separator"))?;
        if !metadata
            .split(';')
            .skip(1)
            .any(|part| part.eq_ignore_ascii_case("base64"))
        {
            return Err(rejected("data URL must use base64 encoding"));
        }
        return validate_inline_base64(data);
    }
    let url =
        url::Url::parse(source).map_err(|_| rejected("expected a remote URL or inline data"))?;
    match url.scheme() {
        "http" | "https"
            if url.host_str().is_some_and(|host| !host.is_empty())
                && source.get(..url.scheme().len() + 3).is_some_and(|prefix| {
                    prefix.eq_ignore_ascii_case(&format!("{}://", url.scheme()))
                }) =>
        {
            Ok(())
        }
        "gs" if allow_gcs
            && url.host_str().is_some_and(|host| !host.is_empty())
            && !url.path().trim_start_matches('/').is_empty()
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none() =>
        {
            Ok(())
        }
        _ => Err(rejected("unsupported or malformed media URI")),
    }
}

/// Only fields explicitly representing inline audio may contain bare base64.
pub fn validate_inline_audio(source: &str) -> Result<(), String> {
    if is_data_url(source) {
        validate_media_reference(source, false)
    } else {
        validate_inline_base64(source)
    }
}

fn url_value(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("url").and_then(Value::as_str))
}

fn validate_content_value(content: &Value, depth: usize) -> Result<(), String> {
    match content {
        Value::Array(blocks) => validate_blocks(blocks, depth, false, false),
        Value::Object(_) if content.get("type").is_some() => {
            validate_blocks(std::slice::from_ref(content), depth, false, false)
        }
        _ => Ok(()),
    }
}

fn validate_blocks(
    blocks: &[Value],
    depth: usize,
    canonical: bool,
    message_items: bool,
) -> Result<(), String> {
    if depth > 64 {
        return Err(rejected("nested media content is too deep"));
    }
    for block in blocks {
        // Gemini's canonical parts are checked independently of their source protocol.
        if canonical {
            // Validate every supplied alias; a valid camelCase URI must not hide
            // a disallowed snake_case reference (or vice versa).
            for file_key in ["fileData", "file_data"] {
                if let Some(file) = block.get(file_key) {
                    for uri_key in ["fileUri", "file_uri"] {
                        if let Some(uri) = file.get(uri_key).and_then(Value::as_str) {
                            validate_media_reference(uri, true)?;
                        }
                    }
                }
            }
        }
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "image_url" | "input_image" => {
                if let Some(source) = block.get("image_url").and_then(url_value) {
                    validate_media_reference(source, false)?;
                }
            }
            "audio_url" => {
                if let Some(source) = block.get("audio_url").and_then(url_value) {
                    validate_media_reference(source, false)?;
                }
            }
            "video_url" | "input_video" => {
                if let Some(source) = block.get("video_url").and_then(url_value) {
                    validate_media_reference(source, false)?;
                }
            }
            "input_audio" | "audio" => {
                if let Some(data) = block
                    .get("input_audio")
                    .and_then(|audio| audio.get("data"))
                    .and_then(Value::as_str)
                {
                    validate_inline_audio(data)?;
                }
            }
            "image" | "document" => {
                if let Some(source) = block
                    .get("source")
                    .and_then(|source| source.get("url"))
                    .and_then(Value::as_str)
                {
                    validate_media_reference(source, false)?;
                }
                // Opaque uploaded-file IDs are protocol-native identifiers, not paths.
            }
            "function_call_output" | "custom_tool_call_output" => {
                if let Some(output) = block.get("output") {
                    // Match Responses normalization: outputs may be a direct typed
                    // part, an array, or an object wrapping a content container.
                    let content = output
                        .as_object()
                        .and_then(|value| value.get("content"))
                        .unwrap_or(output);
                    validate_content_value(content, depth + 1)?;
                }
            }
            _ => {}
        }
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
        if (message_items && block.get("role").is_some())
            || matches!(kind, "message" | "tool_result")
        {
            if let Some(content) = block.get("content") {
                validate_content_value(content, depth + 1)?;
            }
        }
        if canonical {
            if let Some(parts) = block.get("parts").and_then(Value::as_array) {
                validate_blocks(parts, depth + 1, true, false)?;
            }
        }
    }
    Ok(())
}

/// Visit declared message/content/part containers only. Tool schemas, arguments,
/// ordinary text, and arbitrary tool-result JSON are not interpreted as media.
pub fn validate_request_media_sources(body: &Value) -> Result<(), String> {
    for key in ["messages", "contents", "input"] {
        if let Some(items) = body.get(key).and_then(Value::as_array) {
            validate_blocks(items, 0, key == "contents", key != "contents")?;
        }
    }
    // Also accept a canonical internal envelope, without traversing arbitrary objects.
    if let Some(request) = body.get("request") {
        if let Some(contents) = request.get("contents").and_then(Value::as_array) {
            validate_blocks(contents, 0, true, false)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_and_unsupported_references_are_rejected_without_path_resolution() {
        for source in [
            "file:///etc/passwd",
            "FILE:///C:/Users/key",
            "/etc/passwd",
            "C:\\Users\\key",
            "C:/Users/key",
            "\\\\host\\share\\key",
            "//host/share/key",
            "../key",
            "relative/image.png",
            "image.png",
            "ftp://example.com/a",
            "javascript:alert(1)",
            "https:/missing-authority",
            "",
        ] {
            assert!(validate_media_reference(source, false).is_err(), "{source}");
        }
        for source in [
            "https://example.com/image.png?signature=value",
            "http://example.com/audio.mp3",
            "data:audio/wav;base64,QUJD",
        ] {
            assert!(validate_media_reference(source, false).is_ok(), "{source}");
        }
    }

    #[test]
    fn native_gcs_and_uploaded_ids_are_preserved() {
        assert!(validate_media_reference("gs://bucket/path/object.mp4", true).is_ok());
        for source in ["gs://", "gs://bucket", "gs://bucket/", "gs://bucket/path"] {
            assert!(validate_media_reference(source, false).is_err());
        }
        assert!(validate_media_reference("gs://bucket/", true).is_err());
        validate_request_media_sources(&json!({"contents":[{"parts":[{"fileData":{"fileUri":"gs://bucket/path/object.mp4"}}]}]})).unwrap();
        validate_request_media_sources(&json!({"input":[{"type":"message","content":[{"type":"input_file","file_id":"file_opaque_id"}]}]})).unwrap();
    }

    #[test]
    fn raw_audio_is_distinct_from_url_fields_and_validation_is_bounded() {
        let large = "AAAA".repeat(4096);
        for data in ["QUJD", "QUI=", "QUI", large.as_str()] {
            validate_inline_audio(data).unwrap();
        }
        for data in [
            "",
            "file:///secret",
            "../audio.wav",
            "C:\\secret.wav",
            "not-base64!",
        ] {
            assert!(validate_inline_audio(data).is_err());
        }
        assert!(validate_media_reference("QUJD", false).is_err());
        assert!(validate_media_reference("data:audio/wav;base64,not-base64!", false).is_err());
    }

    #[test]
    fn four_protocol_media_containers_share_the_same_policy() {
        let path = "file:///server/private-key";
        for body in [
            json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":path}}]}]}),
            json!({"input":[{"type":"message","role":"user","content":[{"type":"input_image","image_url":path}]}]}),
            json!({"input":[{"type":"message","role":"user","content":{"type":"input_image","image_url":path}}]}),
            json!({"input":[{"type":"function_call_output","output":{"content":[{"type":"image_url","image_url":{"url":path}}]}}]}),
            json!({"input":[{"type":"function_call_output","output":{"type":"audio_url","audio_url":{"url":path}}}]}),
            json!({"messages":[{"role":"user","content":[{"type":"image","source":{"type":"url","url":path}}]}]}),
            json!({"contents":[{"role":"user","parts":[{"file_data":{"file_uri":path}}]}]}),
            json!({"messages":[{"role":"tool","content":[{"type":"audio_url","audio_url":{"url":path}}]}]}),
            json!({"messages":[{"role":"user","content":[{"type":"tool_result","content":[{"type":"video_url","video_url":{"url":path}}]}]}]}),
        ] {
            assert!(validate_request_media_sources(&body).is_err());
        }
    }

    #[test]
    fn canonical_aliases_cannot_hide_disallowed_references() {
        for part in [
            json!({"fileData":{"fileUri":"https://example.com/ok"}, "file_data":{"file_uri":"file:///secret"}}),
            json!({"fileData":{"fileUri":"https://example.com/ok", "file_uri":"../secret"}}),
            json!({"file_data":{"file_uri":"gs://bucket/ok", "fileUri":"C:\\server\\secret"}}),
        ] {
            let body = json!({"contents":[{"parts":[part]}]});
            assert!(validate_request_media_sources(&body).is_err());
        }
    }

    #[test]
    fn ordinary_text_schemas_and_tool_arguments_are_not_media() {
        let body = json!({
            "messages":[{"role":"user","content":"Read C:\\Users\\key and file:///etc/passwd"},
                {"role":"assistant","content":[{"type":"tool_use","name":"inspect","input":{"content":[{"type":"image_url","image_url":{"url":"relative/example.png"}}]}}]},
                {"role":"tool","content":"{\"type\":\"image_url\",\"image_url\":\"../key\"}"}],
            "tools":[{"input_schema":{"properties":{"image_url":{"default":"relative/example.png"}},"content":[{"type":"image_url","image_url":"../key"}]}}],
            "metadata":{"fileData":{"fileUri":"file:///metadata"}}
        });
        validate_request_media_sources(&body).unwrap();
        for role in ["employee", "user"] {
            validate_request_media_sources(&json!({"input":[{
                "type":"function_call_output", "output":[{
                    "role":role,
                    "content":[{"type":"image_url","image_url":"../ordinary-record"}]
                }]
            }]}))
            .unwrap();
        }
        validate_request_media_sources(&json!({"input":[{
            "type":"function_call_output", "output":[{
                "fileData":{"fileUri":"file:///ordinary-record"},
                "content":[{"type":"image_url","image_url":"../ordinary-record"}]
            }]
        }]}))
        .unwrap();
    }

    #[tokio::test]
    async fn every_http_adapter_rejects_files_before_account_selection() {
        use axum::{routing::post, Router};
        use tower::ServiceExt;
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        let state = crate::proxy::server::config_reload_tests::test_state();
        let app = Router::new()
            .route(
                "/chat",
                post(crate::proxy::handlers::openai::handle_chat_completions),
            )
            .route(
                "/responses",
                post(crate::proxy::handlers::openai::handle_completions),
            )
            .route(
                "/messages",
                post(crate::proxy::handlers::claude::handle_messages),
            )
            .route(
                "/gemini/:model_action",
                post(crate::proxy::handlers::gemini::handle_generate),
            )
            .route(
                "/video",
                post(crate::proxy::handlers::openai::handle_videos_generations),
            )
            .with_state(state);
        for (route, body) in [
            (
                "/chat",
                json!({"model":"gemini","messages":[{"role":"user","content":[{"type":"audio_url","audio_url":{"url":"file:///secret.wav"}}]}]}),
            ),
            (
                "/responses",
                json!({"model":"gemini","input":[{"type":"message","role":"user","content":[{"type":"input_image","image_url":"C:\\server\\secret.png"}]}]}),
            ),
            (
                "/messages",
                json!({"model":"claude","messages":[{"role":"user","content":[{"type":"image","source":{"type":"url","url":"../secret.png"}}]}]}),
            ),
            (
                "/gemini/gemini:generateContent",
                json!({"contents":[{"role":"user","parts":[{"fileData":{"fileUri":"file:///secret"}}]}]}),
            ),
            (
                "/video",
                json!({"prompt":"fixture", "image":"file:///secret.mp4"}),
            ),
        ] {
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(route)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                axum::http::StatusCode::BAD_REQUEST,
                "{route}"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert!(
                String::from_utf8_lossy(&bytes).contains("media source"),
                "{route}"
            );
        }
    }
}
