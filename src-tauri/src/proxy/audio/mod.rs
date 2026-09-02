use base64::{engine::general_purpose, Engine as _};
use serde_json::{json, Value};
use std::path::Path;

pub struct AudioProcessor;

impl AudioProcessor {
    /// Detects the audio MIME type
    pub fn detect_mime_type(filename: &str) -> Result<String, String> {
        let ext = Path::new(filename)
            .extension()
            .and_then(|s| s.to_str())
            .ok_or("Could not get file extension")?;

        match ext.to_lowercase().as_str() {
            "mp3" => Ok("audio/mp3".to_string()),
            "wav" => Ok("audio/wav".to_string()),
            "m4a" => Ok("audio/aac".to_string()),
            "ogg" => Ok("audio/ogg".to_string()),
            "flac" => Ok("audio/flac".to_string()),
            "aiff" | "aif" => Ok("audio/aiff".to_string()),
            _ => Err(format!("Unsupported audio format: {}", ext)),
        }
    }

    /// Encodes audio data to Base64
    pub fn encode_to_base64(audio_data: &[u8]) -> String {
        general_purpose::STANDARD.encode(audio_data)
    }

    /// Determines whether a file exceeds the size limit
    pub fn exceeds_size_limit(size_bytes: usize) -> bool {
        const MAX_SIZE: usize = 15 * 1024 * 1024; // 15MB
        size_bytes > MAX_SIZE
    }
}

/// Normalizes an OpenAI audio format identifier (e.g. "wav" / "mp3" / "audio/wav") into the MIME type Gemini requires
pub fn normalize_audio_mime(format: &str) -> String {
    let f = format.trim().to_lowercase();
    let bare = f.strip_prefix("audio/").unwrap_or(&f);
    match bare {
        "mp3" | "mpeg" | "mpga" => "audio/mp3".to_string(),
        "wav" | "wave" | "x-wav" | "vnd.wave" => "audio/wav".to_string(),
        "m4a" | "aac" | "mp4" | "x-m4a" => "audio/aac".to_string(),
        "ogg" | "opus" | "oga" => "audio/ogg".to_string(),
        "flac" | "x-flac" => "audio/flac".to_string(),
        "aiff" | "aif" | "x-aiff" => "audio/aiff".to_string(),
        other => format!("audio/{}", other),
    }
}

/// Infers the audio MIME type from a file path/URL extension, falling back to audio/mp3 on failure
fn mime_from_path(path: &str) -> String {
    let clean = path.split(['?', '#']).next().unwrap_or(path);
    AudioProcessor::detect_mime_type(clean).unwrap_or_else(|_| "audio/mp3".to_string())
}

/// Converts an OpenAI-style audio reference into a Gemini part.
///
/// Supports four kinds of sources:
///   * `data:audio/wav;base64,...`  -> inlineData
///   * `http(s)://...`              -> fileData (fileUri)
///   * `file:///path` or a local path -> inlineData after reading from disk
///   * bare base64 (input_audio.data) -> inlineData (requires declared_mime)
pub fn audio_part_from_source(src: &str, declared_mime: Option<&str>) -> Option<Value> {
    let declared = declared_mime.map(normalize_audio_mime);

    // 1) data: URL
    if src.starts_with("data:") {
        let pos = src.find(',')?;
        let meta = &src[5..pos];
        let mime_part = meta.split(';').next().unwrap_or("");
        let mime = if mime_part.contains('/') {
            normalize_audio_mime(mime_part)
        } else {
            declared.clone().unwrap_or_else(|| "audio/mp3".to_string())
        };
        let data = &src[pos + 1..];
        warn_if_oversized(data.len(), &mime);
        return Some(json!({ "inlineData": { "mimeType": mime, "data": data } }));
    }

    // 2) remote URL: let Gemini's side fetch it
    if src.starts_with("http://") || src.starts_with("https://") {
        let mime = declared.unwrap_or_else(|| mime_from_path(src));
        return Some(json!({ "fileData": { "fileUri": src, "mimeType": mime } }));
    }

    // 3) local file (file:// or a plain path)
    let looks_like_path = src.starts_with("file://") || (src.len() < 4096 && Path::new(src).is_file());
    if looks_like_path {
        let file_path = if let Some(rest) = src.strip_prefix("file://") {
            #[cfg(target_os = "windows")]
            {
                rest.trim_start_matches('/').replace('/', "\\")
            }
            #[cfg(not(target_os = "windows"))]
            {
                rest.to_string()
            }
        } else {
            src.to_string()
        };

        match std::fs::read(&file_path) {
            Ok(bytes) => {
                if AudioProcessor::exceeds_size_limit(bytes.len()) {
                    tracing::warn!(
                        "[Audio] Local audio exceeds 15MB ({} bytes), will still attempt upload: {}",
                        bytes.len(),
                        file_path
                    );
                }
                let mime = declared.unwrap_or_else(|| mime_from_path(&file_path));
                let b64 = AudioProcessor::encode_to_base64(&bytes);
                tracing::debug!("[Audio] Loaded local audio {} ({} bytes, {})", file_path, bytes.len(), mime);
                return Some(json!({ "inlineData": { "mimeType": mime, "data": b64 } }));
            }
            Err(e) => {
                tracing::warn!("[Audio] Failed to read local audio {}: {}", file_path, e);
                return None;
            }
        }
    }

    // 4) bare base64 (OpenAI input_audio.data)
    if src.is_empty() {
        return None;
    }
    let mime = declared.unwrap_or_else(|| "audio/mp3".to_string());
    warn_if_oversized(src.len(), &mime);
    Some(json!({ "inlineData": { "mimeType": mime, "data": src } }))
}

fn warn_if_oversized(base64_len: usize, mime: &str) {
    let raw = (base64_len * 3) / 4;
    if AudioProcessor::exceeds_size_limit(raw) {
        tracing::warn!("[Audio] Inline audio {} is approximately {} bytes, exceeding the recommended 15MB limit", mime, raw);
    }
}

#[cfg(test)]
mod part_tests {
    use super::*;

    #[test]
    fn test_data_url_to_inline_data() {
        let part = audio_part_from_source("data:audio/wav;base64,QUJD", None).unwrap();
        assert_eq!(part["inlineData"]["mimeType"], "audio/wav");
        assert_eq!(part["inlineData"]["data"], "QUJD");
    }

    #[test]
    fn test_bare_base64_uses_declared_format() {
        let part = audio_part_from_source("QUJD", Some("wav")).unwrap();
        assert_eq!(part["inlineData"]["mimeType"], "audio/wav");
    }

    #[test]
    fn test_http_url_to_file_data() {
        let part = audio_part_from_source("https://example.com/a.mp3", None).unwrap();
        assert_eq!(part["fileData"]["fileUri"], "https://example.com/a.mp3");
        assert_eq!(part["fileData"]["mimeType"], "audio/mp3");
    }

    #[test]
    fn test_normalize() {
        assert_eq!(normalize_audio_mime("wav"), "audio/wav");
        assert_eq!(normalize_audio_mime("audio/mpeg"), "audio/mp3");
        assert_eq!(normalize_audio_mime("m4a"), "audio/aac");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_mime_type() {
        assert_eq!(
            AudioProcessor::detect_mime_type("audio.mp3").unwrap(),
            "audio/mp3"
        );
        assert_eq!(
            AudioProcessor::detect_mime_type("audio.wav").unwrap(),
            "audio/wav"
        );
        assert!(AudioProcessor::detect_mime_type("audio.txt").is_err());
    }

    #[test]
    fn test_exceeds_size_limit() {
        assert!(!AudioProcessor::exceeds_size_limit(10 * 1024 * 1024)); // 10MB
        assert!(AudioProcessor::exceeds_size_limit(20 * 1024 * 1024)); // 20MB
        assert!(AudioProcessor::exceeds_size_limit(15 * 1024 * 1024 + 1)); // just over the limit
        assert!(!AudioProcessor::exceeds_size_limit(15 * 1024 * 1024)); // exactly at the limit
    }

    #[test]
    fn test_base64_encoding() {
        let data = b"test audio data";
        let encoded = AudioProcessor::encode_to_base64(data);
        assert!(!encoded.is_empty());
    }
}
