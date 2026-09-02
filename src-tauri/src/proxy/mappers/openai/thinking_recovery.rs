use serde_json::{json, Value};

/// Strip all content marked as a thinking block (thought: true)
pub fn strip_all_thinking_blocks(contents: Vec<Value>) -> Vec<Value> {
    contents
        .into_iter()
        .map(|mut content| {
            if let Some(parts) = content.get_mut("parts").and_then(|v| v.as_array_mut()) {
                parts.retain(|part| {
                    !part
                        .get("thought")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                });
            }
            content
        })
        .filter(|msg| {
            !msg["parts"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(true)
        })
        .collect()
}

/// Close the tool loop for a thinking model
/// First strip thinking blocks, then inject a synthetic Model acknowledgment and User continue instruction
#[allow(dead_code)]
pub fn close_tool_loop_for_thinking(contents: Vec<Value>) -> Vec<Value> {
    let mut stripped = strip_all_thinking_blocks(contents);

    // If there's no content left, return empty
    if stripped.is_empty() {
        return stripped;
    }

    // Synthetic model message: tool execution completed
    stripped.push(json!({
        "role": "model",
        "parts": [{"text": "[Tool execution completed.]"}]
    }));

    // Synthetic user message: prompt to continue
    stripped.push(json!({
        "role": "user",
        "parts": [{"text": "[Continue]"}]
    }));

    stripped
}
