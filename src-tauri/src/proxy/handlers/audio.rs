use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use tracing::{debug, info};
use uuid::Uuid;

use crate::proxy::{audio::AudioProcessor, server::AppState};

/// Handle an audio transcription request (OpenAI Whisper API compatible)
pub async fn handle_audio_transcription(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut audio_data: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;
    let mut model = "gemini-2.0-flash-exp".to_string();
    let mut prompt = "Generate a transcript of the speech.".to_string();

    // 1. Parse multipart/form-data
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed to parse form: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();

        match name.as_str() {
            "file" => {
                filename = field.file_name().map(|s| s.to_string());
                audio_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed to read file: {}", e)))?
                        .to_vec(),
                );
            }
            "model" => {
                model = field.text().await.unwrap_or(model);
            }
            "prompt" => {
                prompt = field.text().await.unwrap_or(prompt);
            }
            _ => {}
        }
    }

    let audio_bytes = audio_data.ok_or((StatusCode::BAD_REQUEST, "Missing audio file".to_string()))?;

    let file_name = filename.ok_or((StatusCode::BAD_REQUEST, "Unable to get file name".to_string()))?;

    info!(
        "Received audio transcription request: file={}, size={} bytes, model={}",
        file_name,
        audio_bytes.len(),
        model
    );

    // 2. Detect MIME type
    let mime_type =
        AudioProcessor::detect_mime_type(&file_name).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // 3. Validate file size
    if AudioProcessor::exceeds_size_limit(audio_bytes.len()) {
        let size_mb = audio_bytes.len() as f64 / (1024.0 * 1024.0);
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "Audio file too large ({:.1} MB). Max supported is 15 MB (~16 minutes of MP3). Suggestions: 1) compress audio quality 2) upload in segments",
                size_mb
            ),
        ));
    }

    // 4. Use Inline Data approach
    debug!("Processing via Inline Data approach");
    let base64_audio = AudioProcessor::encode_to_base64(&audio_bytes);

    // 5. Build the Gemini request
    let gemini_request = json!({
        "contents": [{
            "parts": [
                {"text": prompt},
                {
                    "inlineData": {
                        "mimeType": mime_type,
                        "data": base64_audio
                    }
                }
            ]
        }]
    });

    // 6. Obtain a token and the upstream client
    let token_manager = state.token_manager;
    let (access_token, project_id, email, account_id, _wait_ms) = token_manager
        .get_token("text", false, None, &model)
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e))?;

    info!("Using account: {}", email);

    // 7. Wrap the request in v1internal format
    let wrapped_body = json!({
        "project": project_id,
        "requestId": format!("audio-{}", Uuid::new_v4()),
        "request": gemini_request,
        "model": model,
        "userAgent": "antigravity",
        "requestType": "text"
    });

    // 8. Send the request to Gemini
    let upstream = state.upstream.clone();
    let response = upstream
        .call_v1_internal(
            "generateContent",
            &access_token,
            wrapped_body,
            None,
            Some(account_id.as_str()),
        )
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Upstream request failed: {}", e)))?
        .response;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Gemini API error: {}", error_text),
        ));
    }

    let result: Value = response
        .json()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Failed to parse response: {}", e)))?;

    // 9. Extract the text response (unwrap the v1internal response)
    let inner_response = result.get("response").unwrap_or(&result);
    let text = inner_response
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.get(0))
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");

    info!("Audio transcription complete, returning {} characters", text.len());

    // 10. Return the standard format response
    Ok((
        StatusCode::OK,
        [("X-Account-Email", email.as_str())],
        Json(json!({
            "text": text
        })),
    )
        .into_response())
}
