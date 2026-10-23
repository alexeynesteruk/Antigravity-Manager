//! Conservative repair for explicitly requested JSON output.

use std::borrow::Cow;

pub fn normalize_json_text(text: &str) -> Cow<'_, str> {
    let trimmed = text.trim();
    let fenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"));
    if let Some(fenced) = fenced {
        if let Some(inner) = fenced.strip_suffix("```") {
            let inner = inner.trim();
            if serde_json::from_str::<serde_json::Value>(inner).is_ok() {
                return Cow::Owned(inner.to_string());
            }
        }
    }
    Cow::Borrowed(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_only_complete_valid_json_fences() {
        assert_eq!(
            normalize_json_text("```json\n{\"code\":\"```rust\"}\n```"),
            "{\"code\":\"```rust\"}"
        );
        for text in [
            "  {\"title\":\"ok\"}  ",
            "```rust\nfn main() {}\n```",
            "```json\n{broken}\n```",
            "```json\n{}\n```\nKeep this prose",
        ] {
            assert_eq!(normalize_json_text(text), text);
        }
    }
}
