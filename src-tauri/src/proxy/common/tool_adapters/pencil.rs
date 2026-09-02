use super::super::tool_adapter::{append_hint_to_schema, ToolAdapter};
use serde_json::Value;

/// The Pencil MCP tool adapter
///
/// Provides Schema optimizations specific to the Pencil drawing tool:
/// 1. Handles non-standard visual property fields (cornerRadius, strokeWidth, etc.)
/// 2. Improves the description of file-path parameters
/// 3. Adds Pencil-specific usage hints
pub struct PencilAdapter;

impl ToolAdapter for PencilAdapter {
    fn matches(&self, tool_name: &str) -> bool {
        tool_name.starts_with("mcp__pencil__")
    }

    fn pre_process(&self, schema: &mut Value) -> Result<(), String> {
        if let Value::Object(map) = schema {
            // 1. Handle visual property fields
            self.handle_visual_properties(map);

            // 2. Improve file-path parameters
            self.optimize_path_parameters(map);
        }
        Ok(())
    }
}

impl PencilAdapter {
    /// Handles Pencil-specific visual property fields
    fn handle_visual_properties(&self, map: &mut serde_json::Map<String, Value>) {
        // Non-standard visual properties used by Pencil
        let visual_props = ["cornerRadius", "strokeWidth", "opacity", "rotation"];

        for prop in visual_props {
            if map.contains_key(prop) {
                let hint = format!("Visual property: {}", prop);
                append_hint_to_schema(&mut Value::Object(map.clone()), &hint);
            }
        }

        // Handle visual properties inside properties
        if let Some(Value::Object(props)) = map.get_mut("properties") {
            for (key, value) in props.iter_mut() {
                if visual_props.contains(&key.as_str()) {
                    if let Value::Object(prop_map) = value {
                        prop_map
                            .entry("description".to_string())
                            .and_modify(|d| {
                                if let Value::String(s) = d {
                                    if !s.contains("visual property") {
                                        *s = format!("{} (visual property for UI elements)", s);
                                    }
                                }
                            })
                            .or_insert_with(|| {
                                Value::String("Visual property for UI elements".to_string())
                            });
                    }
                }
            }
        }
    }

    /// Improves the description of file-path-related parameters
    fn optimize_path_parameters(&self, map: &mut serde_json::Map<String, Value>) {
        if let Some(Value::Object(props)) = map.get_mut("properties") {
            for (key, value) in props.iter_mut() {
                // Identify path-related parameters
                let is_path_param = key.contains("path")
                    || key.contains("file")
                    || key.contains("File")
                    || key.contains("Path");

                if is_path_param {
                    if let Value::Object(prop_map) = value {
                        prop_map
                            .entry("description".to_string())
                            .and_modify(|d| {
                                if let Value::String(s) = d {
                                    if !s.contains("absolute path") {
                                        *s = format!(
                                            "{} (use absolute path, e.g., /path/to/file.pen)",
                                            s
                                        );
                                    }
                                }
                            })
                            .or_insert_with(|| {
                                Value::String("File path (use absolute path)".to_string())
                            });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_pencil_adapter_matches() {
        let adapter = PencilAdapter;
        assert!(adapter.matches("mcp__pencil__create_shape"));
        assert!(adapter.matches("mcp__pencil__update_properties"));
        assert!(!adapter.matches("mcp__filesystem__read"));
    }

    #[test]
    fn test_visual_properties_handling() {
        let adapter = PencilAdapter;
        let mut schema = json!({
            "type": "object",
            "properties": {
                "cornerRadius": {
                    "type": "number"
                },
                "color": {
                    "type": "string"
                }
            }
        });

        adapter.pre_process(&mut schema).unwrap();

        // Verify a description was added for cornerRadius
        assert!(schema["properties"]["cornerRadius"]["description"].is_string());
        let desc = schema["properties"]["cornerRadius"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("Visual property"));
    }

    #[test]
    fn test_path_parameter_optimization() {
        let adapter = PencilAdapter;
        let mut schema = json!({
            "type": "object",
            "properties": {
                "filePath": {
                    "type": "string",
                    "description": "Path to the file"
                }
            }
        });

        adapter.pre_process(&mut schema).unwrap();

        let desc = schema["properties"]["filePath"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("absolute path"));
    }
}
