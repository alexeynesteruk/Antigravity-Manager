//! Bound client tool schemas before adapters copy or expand them.
//! Keep the existing cleaner's depth, cycle, and unresolved-reference policy.

use serde_json::Value;
use std::collections::{HashMap, HashSet};

const MAX_NODES: usize = 65_536;
const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_RAW_DEPTH: usize = 256;

#[derive(Default)]
struct Budget {
    nodes: usize,
    text_bytes: usize,
}

impl Budget {
    fn charge(&mut self, value: &Value) -> Result<(), String> {
        self.nodes = self.nodes.saturating_add(1);
        self.text_bytes = self.text_bytes.saturating_add(match value {
            Value::String(text) => text.len(),
            Value::Object(map) => map.keys().map(String::len).sum(),
            _ => 0,
        });
        if self.nodes > MAX_NODES {
            return Err(rejected("node budget"));
        }
        if self.text_bytes > MAX_TEXT_BYTES {
            return Err(rejected("text byte budget"));
        }
        Ok(())
    }
}

fn rejected(limit: &str) -> String {
    format!("client tool schema rejected: {limit} exceeded")
}

fn validate_raw(value: &Value, depth: usize, budget: &mut Budget) -> Result<(), String> {
    if depth > MAX_RAW_DEPTH {
        return Err(rejected("structural depth"));
    }
    budget.charge(value)?;
    match value {
        Value::Object(map) => {
            for child in map.values() {
                validate_raw(child, depth + 1, budget)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                validate_raw(child, depth + 1, budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_defs<'a>(value: &'a Value, defs: &mut HashMap<&'a str, &'a Value>) {
    match value {
        Value::Object(map) => {
            for key in ["$defs", "definitions"] {
                if let Some(definitions) = map.get(key).and_then(Value::as_object) {
                    for (name, value) in definitions {
                        defs.entry(name.as_str()).or_insert(value);
                    }
                }
            }
            for (key, child) in map {
                if key != "$defs" && key != "definitions" {
                    collect_defs(child, defs);
                }
            }
        }
        Value::Array(array) => {
            for child in array {
                collect_defs(child, defs);
            }
        }
        _ => {}
    }
}

fn validate_expansion<'a>(
    value: &'a Value,
    defs: &HashMap<&'a str, &'a Value>,
    depth: usize,
    active: &mut HashSet<&'a str>,
    budget: &mut Budget,
) -> Result<(), String> {
    // The compatibility cleaner terminates references at this same depth.
    if depth > crate::proxy::common::json_schema::MAX_REF_RECURSION_DEPTH {
        return Ok(());
    }
    budget.charge(value)?;
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                let name = reference.rsplit('/').next().unwrap_or(reference);
                if active.insert(name) {
                    if let Some(target) = defs.get(name) {
                        validate_expansion(target, defs, depth + 1, active, budget)?;
                    }
                    active.remove(name);
                }
            }
            for (key, child) in map {
                if key != "$defs" && key != "definitions" && key != "$ref" {
                    validate_expansion(child, defs, depth + 1, active, budget)?;
                }
            }
        }
        Value::Array(array) => {
            for child in array {
                validate_expansion(child, defs, depth + 1, active, budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_schema(value: &Value, budget: &mut Budget) -> Result<(), String> {
    let mut defs = HashMap::new();
    collect_defs(value, &mut defs);
    validate_expansion(value, &defs, 0, &mut HashSet::new(), budget)
}

fn validate_declaration(declaration: &Value, budget: &mut Budget) -> Result<(), String> {
    for key in [
        "parameters",
        "parametersJsonSchema",
        "parameters_json_schema",
        "input_schema",
    ] {
        if let Some(schema) = declaration.get(key) {
            validate_schema(schema, budget)?;
        }
    }
    Ok(())
}

/// Supports raw adapter inputs and the canonical Gemini request envelope.
/// This borrowed preflight never rewrites or clones a schema, even on rejection.
pub fn validate_request_tool_schemas(body: &Value) -> Result<(), String> {
    let body = body.get("request").unwrap_or(body);
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut raw = Budget::default();
    for tool in tools {
        validate_raw(tool, 0, &mut raw)?;
    }
    let mut expanded = Budget::default();
    for tool in tools {
        validate_declaration(tool, &mut expanded)?;
        if let Some(function) = tool.get("function") {
            validate_declaration(function, &mut expanded)?;
        }
        for key in ["functionDeclarations", "function_declarations"] {
            if let Some(declarations) = tool.get(key).and_then(Value::as_array) {
                for declaration in declarations {
                    validate_declaration(declaration, &mut expanded)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn expanding_schema() -> Value {
        let mut defs = serde_json::Map::new();
        defs.insert("leaf".into(), json!({"type":"string"}));
        let mut previous = "leaf".to_string();
        for index in 0..20 {
            let name = format!("level{index}");
            defs.insert(name.clone(), json!({"type":"object","properties":{"left":{"$ref":format!("#/$defs/{previous}")},"right":{"$ref":format!("#/$defs/{previous}")}}}));
            previous = name;
        }
        json!({"$defs":defs,"$ref":format!("#/$defs/{previous}")})
    }

    #[test]
    fn rejects_acyclic_expansion_before_all_four_adapter_shapes_copy_it() {
        let schema = expanding_schema();
        for tool in [
            json!({"type":"function","function":{"name":"t","parameters":schema}}),
            json!({"type":"function","name":"t","parameters":schema}),
            json!({"name":"t","input_schema":schema}),
            json!({"functionDeclarations":[{"name":"t","parameters":schema}]}),
        ] {
            let body = json!({"tools":[tool]});
            let original = body.clone();
            assert!(validate_request_tool_schemas(&body)
                .unwrap_err()
                .contains("node budget"));
            assert_eq!(body, original);
        }
    }

    #[test]
    fn retains_cycle_and_unknown_reference_compatibility() {
        for schema in [
            json!({"$defs":{"node":{"type":"object","properties":{"child":{"$ref":"#/$defs/node"}}}},"$ref":"#/$defs/node"}),
            json!({"$ref":"#/$defs/missing"}),
        ] {
            assert!(
                validate_request_tool_schemas(&json!({"tools":[{"input_schema":schema}]})).is_ok()
            );
        }
    }

    #[test]
    fn bounds_opaque_annotations_and_checks_all_aliases() {
        let large = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(validate_request_tool_schemas(
            &json!({"tools":[{"input_schema":{"default":large}}]})
        )
        .is_err());
        let body = json!({"request":{"tools":[{"functionDeclarations":[{"parameters":{"type":"object"}}],"function_declarations":[{"parametersJsonSchema":expanding_schema()}]}]}});
        assert!(validate_request_tool_schemas(&body).is_err());
    }
}
