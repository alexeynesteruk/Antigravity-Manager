use base64::{engine::general_purpose, Engine as _};
use serde_json::{json, Value};
use std::path::Path;

pub struct AudioProcessor;

impl AudioProcessor {
    /// 检测音频 MIME 类型
    pub fn detect_mime_type(filename: &str) -> Result<String, String> {
        let ext = Path::new(filename)
            .extension()
            .and_then(|s| s.to_str())
            .ok_or("无法获取文件扩展名")?;

        match ext.to_lowercase().as_str() {
            "mp3" => Ok("audio/mp3".to_string()),
            "wav" => Ok("audio/wav".to_string()),
            "m4a" => Ok("audio/aac".to_string()),
            "ogg" => Ok("audio/ogg".to_string()),
            "flac" => Ok("audio/flac".to_string()),
            "aiff" | "aif" => Ok("audio/aiff".to_string()),
            _ => Err(format!("不支持的音频格式: {}", ext)),
        }
    }

    /// 将音频数据编码为 Base64
    pub fn encode_to_base64(audio_data: &[u8]) -> String {
        general_purpose::STANDARD.encode(audio_data)
    }

    /// 判断文件是否超过大小限制
    pub fn exceeds_size_limit(size_bytes: usize) -> bool {
        const MAX_SIZE: usize = 15 * 1024 * 1024; // 15MB
        size_bytes > MAX_SIZE
    }
}

/// 将 OpenAI 的音频格式标识 (如 "wav" / "mp3" / "audio/wav") 归一化为 Gemini 需要的 MIME 类型
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

/// 根据文件路径/URL 的扩展名推断音频 MIME，失败时回退到 audio/mp3
fn mime_from_path(path: &str) -> String {
    let clean = path.split(['?', '#']).next().unwrap_or(path);
    AudioProcessor::detect_mime_type(clean).unwrap_or_else(|_| "audio/mp3".to_string())
}

/// 把 OpenAI 风格的音频引用转换为 Gemini part。
///
/// Supports inline data URLs and HTTP(S) references.
///   * `data:audio/wav;base64,...`  -> inlineData
///   * `http(s)://...`              -> fileData (fileUri)
/// Server-local paths are rejected without filesystem metadata or reads.
/// Bare input_audio.data is handled by audio_part_from_inline_data.
pub fn audio_part_from_source(src: &str, declared_mime: Option<&str>) -> Option<Value> {
    use crate::proxy::pipeline::media_sources::{is_data_url, validate_media_reference};
    validate_media_reference(src, false).ok()?;
    let declared = declared_mime.map(normalize_audio_mime);
    if is_data_url(src) {
        let (metadata, data) = src[5..].split_once(',')?;
        let mime_part = metadata.split(';').next().unwrap_or("");
        let mime = if mime_part.contains('/') {
            normalize_audio_mime(mime_part)
        } else {
            declared.unwrap_or_else(|| "audio/mp3".into())
        };
        warn_if_oversized(data.len(), &mime);
        Some(json!({"inlineData":{"mimeType":mime,"data":data}}))
    } else {
        let mime = declared.unwrap_or_else(|| mime_from_path(src));
        Some(json!({"fileData":{"fileUri":src,"mimeType":mime}}))
    }
}

/// Explicit inline input is distinct from a URL: bare bytes never become paths.
pub fn audio_part_from_inline_data(src: &str, declared_mime: Option<&str>) -> Option<Value> {
    use crate::proxy::pipeline::media_sources::{is_data_url, validate_inline_audio};
    validate_inline_audio(src).ok()?;
    if is_data_url(src) {
        return audio_part_from_source(src, declared_mime);
    }
    let mime = declared_mime
        .map(normalize_audio_mime)
        .unwrap_or_else(|| "audio/mp3".into());
    warn_if_oversized(src.len(), &mime);
    Some(json!({"inlineData":{"mimeType":mime,"data":src}}))
}

fn warn_if_oversized(base64_len: usize, mime: &str) {
    let raw = (base64_len * 3) / 4;
    if AudioProcessor::exceeds_size_limit(raw) {
        tracing::warn!(
            "[Audio] 内联音频 {} 约 {} bytes，超过 15MB 建议上限",
            mime,
            raw
        );
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
        let part = audio_part_from_inline_data("QUJD", Some("wav")).unwrap();
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
        assert!(AudioProcessor::exceeds_size_limit(15 * 1024 * 1024 + 1)); // 刚好超过
        assert!(!AudioProcessor::exceeds_size_limit(15 * 1024 * 1024)); // 刚好等于限制
    }

    #[test]
    fn test_base64_encoding() {
        let data = b"test audio data";
        let encoded = AudioProcessor::encode_to_base64(data);
        assert!(!encoded.is_empty());
    }
}
