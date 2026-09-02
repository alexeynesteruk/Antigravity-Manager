//! Tool result output compression module
//!
//! Provides smart compression features:
//! - Browser snapshot compression (head+tail retention)
//! - Large-file notice compression (extract key information)
//! - Generic truncation (200,000 character limit)

use regex::Regex;
use serde_json::Value;
use tracing::{debug, info};

/// Maximum tool result character count (~200k, to prevent an overlong prompt)
const MAX_TOOL_RESULT_CHARS: usize = 200_000;

/// Browser snapshot detection threshold
const SNAPSHOT_DETECTION_THRESHOLD: usize = 20_000;

/// Maximum character count after browser snapshot compression
const SNAPSHOT_MAX_CHARS: usize = 16_000;

/// Browser snapshot head retention ratio
const SNAPSHOT_HEAD_RATIO: f64 = 0.7;

/// Browser snapshot tail retention ratio
#[allow(dead_code)]
const SNAPSHOT_TAIL_RATIO: f64 = 0.3;

/// Compress tool result text
///
/// Automatically choose the best compression strategy based on content type:
/// 1. Large-file notice → extract key information
/// 2. Browser snapshot → head+tail retention
/// 3. Other → simple truncation
pub fn compact_tool_result_text(text: &str, max_chars: usize) -> String {
    if text.is_empty() || text.len() <= max_chars {
        return text.to_string();
    }

    // [NEW] Deep-preprocess potential HTML content
    let cleaned_text =
        if text.contains("<html") || text.contains("<body") || text.contains("<!DOCTYPE") {
            let cleaned = deep_clean_html(text);
            debug!(
                "[ToolCompressor] Deep cleaned HTML, reduced {} -> {} chars",
                text.len(),
                cleaned.len()
            );
            cleaned
        } else {
            text.to_string()
        };

    if cleaned_text.len() <= max_chars {
        return cleaned_text;
    }

    // 1. Detect the large-file notice pattern
    if let Some(compacted) = compact_saved_output_notice(&cleaned_text, max_chars) {
        debug!(
            "[ToolCompressor] Detected saved output notice, compacted to {} chars",
            compacted.len()
        );
        return compacted;
    }

    // 2. Detect the browser snapshot pattern
    if cleaned_text.len() > SNAPSHOT_DETECTION_THRESHOLD {
        if let Some(compacted) = compact_browser_snapshot(&cleaned_text, max_chars) {
            debug!(
                "[ToolCompressor] Detected browser snapshot, compacted to {} chars",
                compacted.len()
            );
            return compacted;
        }
    }

    // 3. Structured truncation
    debug!(
        "[ToolCompressor] Using structured truncation for {} chars",
        cleaned_text.len()
    );
    truncate_text_safe(&cleaned_text, max_chars)
}

/// Compress "output saved to a file" style notices
///
/// Detection pattern: "result (N characters) exceeds maximum allowed tokens. Output saved to <path>"
/// Strategy: extract key information (file path, character count, format description)
///
/// Automatically extract key information based on the notice content type
fn compact_saved_output_notice(text: &str, max_chars: usize) -> Option<String> {
    // Regex match: result (N characters) exceeds maximum allowed tokens. Output saved to <path>
    let re = Regex::new(
        r"(?i)result\s*\(\s*(?P<count>[\d,]+)\s*characters\s*\)\s*exceeds\s+maximum\s+allowed\s+tokens\.\s*Output\s+(?:has\s+been\s+)?saved\s+to\s+(?P<path>[^\r\n]+)"
    ).ok()?;

    let caps = re.captures(text)?;
    let count = caps.name("count")?.as_str();
    let raw_path = caps.name("path")?.as_str();

    // Clean up the file path (remove trailing parentheses, quotes, periods)
    let file_path = raw_path
        .trim()
        .trim_end_matches(&[')', ']', '"', '\'', '.'][..])
        .trim();

    // Extract the key line
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    // Find the notice line
    let notice_line = lines.iter()
        .find(|l| l.to_lowercase().contains("exceeds maximum allowed tokens") && l.to_lowercase().contains("saved to"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("result ({} characters) exceeds maximum allowed tokens. Output has been saved to {}", count, file_path));

    // Find the format description line
    let format_line = lines
        .iter()
        .find(|l| {
            l.starts_with("Format:")
                || l.contains("JSON array with schema")
                || l.to_lowercase().starts_with("schema:")
        })
        .map(|s| s.to_string());

    // Build the compressed output
    let mut compact_lines = vec![notice_line];
    if let Some(fmt) = format_line {
        if !compact_lines.contains(&fmt) {
            compact_lines.push(fmt);
        }
    }
    compact_lines.push(format!(
        "[tool_result omitted to reduce prompt size; read file locally if needed: {}]",
        file_path
    ));

    let result = compact_lines.join("\n");
    Some(truncate_text_safe(&result, max_chars))
}

/// Compress a browser snapshot (head+tail retention strategy)
///
/// Detection: "page snapshot" or "页面快照" or a large number of "ref=" references
/// Strategy: keep the first 70% + last 30%, omit the middle
///
/// Compress long page snapshot data using a head+tail retention strategy
fn compact_browser_snapshot(text: &str, max_chars: usize) -> Option<String> {
    // Detect whether this is a browser snapshot
    let is_snapshot = text.to_lowercase().contains("page snapshot")
        // PROTECTED: this Chinese literal matches text arriving at runtime from a
        // browser tool (e.g. a Chinese-locale page snapshot label), not code we
        // control. Do not translate it - do not change this literal.
        || text.contains("页面快照")
        || text.matches("ref=").count() > 30
        || text.matches("[ref=").count() > 30;

    if !is_snapshot {
        return None;
    }

    let desired_max = max_chars.min(SNAPSHOT_MAX_CHARS);
    if desired_max < 2000 || text.len() <= desired_max {
        return None;
    }

    let meta = format!(
        "[page snapshot summarized to reduce prompt size; original {} chars]",
        text.len()
    );
    let overhead = meta.len() + 200;
    let budget = desired_max.saturating_sub(overhead);

    if budget < 1000 {
        return None;
    }

    // Compute the head and tail lengths
    let head_len = (budget as f64 * SNAPSHOT_HEAD_RATIO).floor() as usize;
    let head_len = head_len.min(10_000).max(500);
    let tail_len = budget.saturating_sub(head_len).min(3_000);

    let head = &text[..head_len.min(text.len())];
    let tail = if tail_len > 0 && text.len() > head_len {
        let start = text.len().saturating_sub(tail_len);
        &text[start..]
    } else {
        ""
    };

    let omitted = text.len().saturating_sub(head_len).saturating_sub(tail_len);

    let summarized = if tail.is_empty() {
        format!(
            "{}\n---[HEAD]---\n{}\n---[...omitted {} chars]---",
            meta, head, omitted
        )
    } else {
        format!(
            "{}\n---[HEAD]---\n{}\n---[...omitted {} chars]---\n---[TAIL]---\n{}",
            meta, head, omitted, tail
        )
    };

    Some(truncate_text_safe(&summarized, max_chars))
}

/// Safe text truncation (avoid truncating in the middle of a tag where possible)
fn truncate_text_safe(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    // Try to find a safe truncation point (not between < and >)
    let mut split_pos = max_chars;

    // Look backward for an unclosed tag opening character
    let sub = &text[..max_chars];
    if let Some(last_open) = sub.rfind('<') {
        if let Some(last_close) = sub.rfind('>') {
            if last_open > last_close {
                // The truncation point is inside a tag; back off to before the tag starts
                split_pos = last_open;
            }
        } else {
            // There's an opening but no closing; back off to before the tag starts
            split_pos = last_open;
        }
    }

    // Also avoid truncating in the middle of JSON braces
    if let Some(last_open_brace) = sub.rfind('{') {
        if let Some(last_close_brace) = sub.rfind('}') {
            if last_open_brace > last_close_brace {
                // Possibly in the middle of JSON; if close to the truncation point, try backing off
                if max_chars - last_open_brace < 100 {
                    split_pos = split_pos.min(last_open_brace);
                }
            }
        }
    }

    let truncated = &text[..split_pos];
    let omitted = text.len() - split_pos;
    format!("{}\n...[truncated {} chars]", truncated, omitted)
}

/// Deep-clean HTML (remove style, script, base64, etc.)
fn deep_clean_html(html: &str) -> String {
    let mut result = html.to_string();

    // 1. Remove <style>...</style> and its content
    if let Ok(re) = Regex::new(r"(?is)<style\b[^>]*>.*?</style>") {
        result = re.replace_all(&result, "[style omitted]").to_string();
    }

    // 2. Remove <script>...</script> and its content
    if let Ok(re) = Regex::new(r"(?is)<script\b[^>]*>.*?</script>") {
        result = re.replace_all(&result, "[script omitted]").to_string();
    }

    // 3. Remove inline Base64 data (e.g. src="data:image/png;base64,...")
    if let Ok(re) = Regex::new(r#"(?i)data:[^;/]+/[^;]+;base64,[A-Za-z0-9+/=]+"#) {
        result = re.replace_all(&result, "[base64 omitted]").to_string();
    }

    // 4. Remove redundant whitespace
    if let Ok(re) = Regex::new(r"\n\s*\n") {
        result = re.replace_all(&result, "\n").to_string();
    }

    result
}

/// Clean up tool result content blocks
///
/// Processing logic:
/// 1. Remove base64 images (to avoid excessive size)
/// 2. Compress text content (using a smart compression strategy)
/// 3. Limit the total character count (default 200,000)
///
/// Clean up and truncate tool call result content blocks
pub fn sanitize_tool_result_blocks(blocks: &mut Vec<Value>) {
    let mut used_chars = 0;
    let mut cleaned_blocks = Vec::new();

    if !blocks.is_empty() {
        info!(
            "[ToolCompressor] Processing {} blocks for truncation (MAX: {} chars)",
            blocks.len(),
            MAX_TOOL_RESULT_CHARS
        );
    }

    for block in blocks.iter() {
        // Compress the text content
        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
            let remaining = MAX_TOOL_RESULT_CHARS.saturating_sub(used_chars);
            if remaining == 0 {
                debug!("[ToolCompressor] Reached character limit, stopping");
                break;
            }

            let compacted = compact_tool_result_text(text, remaining);
            let mut new_block = block.clone();
            new_block["text"] = Value::String(compacted.clone());
            cleaned_blocks.push(new_block);
            used_chars += compacted.len();

            debug!(
                "[ToolCompressor] Compacted text block: {} → {} chars",
                text.len(),
                compacted.len()
            );
        } else {
            // Keep other block types (e.g. images), but subject to the overall length/block-count limit; not individually truncated here
            cleaned_blocks.push(block.clone());
            used_chars += 100; // Estimate the size of a non-text block
        }

        if used_chars >= MAX_TOOL_RESULT_CHARS {
            break;
        }
    }

    info!(
        "[ToolCompressor] Sanitization complete: {} → {} blocks, {} chars used",
        blocks.len(),
        cleaned_blocks.len(),
        used_chars
    );

    *blocks = cleaned_blocks;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_text() {
        let text = "a".repeat(300_000);
        let result = truncate_text_safe(&text, 200_000);
        assert!(result.len() < 210_000); // Includes the truncation notice
        assert!(result.contains("[truncated"));
        assert!(result.contains("100000 chars]"));
    }

    #[test]
    fn test_truncate_text_no_truncation() {
        let text = "short text";
        let result = truncate_text_safe(text, 1000);
        assert_eq!(result, text);
    }

    #[test]
    fn test_compact_browser_snapshot() {
        let snapshot = format!("page snapshot: {}", "ref=abc ".repeat(10_000));
        let result = compact_tool_result_text(&snapshot, 16_000);

        assert!(result.len() <= 16_500); // Allow some overhead
        assert!(result.contains("[HEAD]"));
        assert!(result.contains("[TAIL]"));
        assert!(result.contains("page snapshot summarized"));
    }

    #[test]
    fn test_compact_saved_output_notice() {
        let text = r#"result (150000 characters) exceeds maximum allowed tokens. Output has been saved to /tmp/output.txt
Format: JSON array with schema
Please read the file locally."#;

        let result = compact_tool_result_text(text, 500);
        println!("Result: {}", result);
        assert!(result.contains("150000 characters") || result.contains("150,000 characters"));
        assert!(result.contains("/tmp/output.txt"));
        assert!(result.contains("[tool_result omitted") || result.len() <= 500);
    }

    #[test]
    fn test_sanitize_tool_result_blocks() {
        let mut blocks = vec![
            serde_json::json!({
                "type": "text",
                "text": "a".repeat(100_000)
            }),
            serde_json::json!({
                "type": "text",
                "text": "b".repeat(150_000)
            }),
            serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
                }
            }),
            serde_json::json!({
                "type": "text",
                "text": "some text"
            }),
        ];

        // Confirm that tool results no longer strip out images
        sanitize_tool_result_blocks(&mut blocks);
        assert_eq!(blocks.len(), 4);
    }
}
