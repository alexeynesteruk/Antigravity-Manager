use serde_json::Value;

/// The MCP tool adapter trait
///
/// Provides customized Schema-processing strategies for different MCP tools.
/// Each tool can implement its own adapter to handle its specific needs.
pub trait ToolAdapter: Send + Sync {
    /// Determines whether this adapter matches the given tool name
    ///
    /// # Arguments
    /// * `tool_name` - the tool name, usually in the form "mcp__provider__function"
    ///
    /// # Returns
    /// true if it matches, false otherwise
    fn matches(&self, tool_name: &str) -> bool;

    /// Pre-processing run before the common cleaning pass
    ///
    /// Tool-specific field handling, hint additions, etc. can be added here
    ///
    /// # Arguments
    /// * `schema` - the JSON Schema to process
    ///
    /// # Returns
    /// Ok(()) on success, an error message on failure
    fn pre_process(&self, _schema: &mut Value) -> Result<(), String> {
        Ok(())
    }

    /// Post-processing run after the common cleaning pass
    ///
    /// Final adjustments and optimizations can be made here
    ///
    /// # Arguments
    /// * `schema` - the already-cleaned JSON Schema
    ///
    /// # Returns
    /// Ok(()) on success, an error message on failure
    fn post_process(&self, _schema: &mut Value) -> Result<(), String> {
        Ok(())
    }
}

/// Helper function: appends a hint to a Schema's description field
pub fn append_hint_to_schema(schema: &mut Value, hint: &str) {
    if let Value::Object(map) = schema {
        let desc_val = map
            .entry("description".to_string())
            .or_insert_with(|| Value::String("".to_string()));

        if let Value::String(s) = desc_val {
            if s.is_empty() {
                *s = hint.to_string();
            } else if !s.contains(hint) {
                *s = format!("{} {}", s, hint);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestAdapter;

    impl ToolAdapter for TestAdapter {
        fn matches(&self, tool_name: &str) -> bool {
            tool_name.starts_with("test__")
        }

        fn pre_process(&self, schema: &mut Value) -> Result<(), String> {
            append_hint_to_schema(schema, "[Test Adapter]");
            Ok(())
        }
    }

    #[test]
    fn test_adapter_matches() {
        let adapter = TestAdapter;
        assert!(adapter.matches("test__function"));
        assert!(!adapter.matches("other__function"));
    }

    #[test]
    fn test_append_hint() {
        let mut schema = json!({"type": "string"});
        append_hint_to_schema(&mut schema, "Test hint");
        assert_eq!(schema["description"], "Test hint");

        append_hint_to_schema(&mut schema, "Another hint");
        assert_eq!(schema["description"], "Test hint Another hint");
    }
}
