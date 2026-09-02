use super::tool_adapter::ToolAdapter;
use super::tool_adapters::PencilAdapter;
use once_cell::sync::Lazy;
use serde_json::{json, Value};

/// Constraint fields not supported by Gemini but carrying important semantic information
/// These fields are converted into a description hint before being removed
const CONSTRAINT_FIELDS: &[(&str, &str)] = &[
    ("minLength", "minLen"),
    ("maxLength", "maxLen"),
    ("pattern", "pattern"),
    ("minimum", "min"),
    ("maximum", "max"),
    ("multipleOf", "multipleOf"),
    ("exclusiveMinimum", "exclMin"),
    ("exclusiveMaximum", "exclMax"),
    ("minItems", "minItems"),
    ("maxItems", "maxItems"),
    ("format", "format"),
];

/// Global tool adapter registry
///
/// All registered adapters are checked and applied during Schema cleaning
static TOOL_ADAPTERS: Lazy<Vec<Box<dyn ToolAdapter>>> = Lazy::new(|| {
    vec![
        Box::new(PencilAdapter),
        // More adapters can be added easily in the future:
        // Box::new(FilesystemAdapter),
        // Box::new(DatabaseAdapter),
    ]
});

const MAX_RECURSION_DEPTH: usize = 10;

/// Recursively cleans a JSON Schema to comply with the Gemini API's requirements
///
/// 1. [New] Expand $ref and $defs: replace references with their actual definitions, working around Gemini's lack of $ref support
/// 2. Remove unsupported fields: $schema, additionalProperties, format, default, uniqueItems, validation fields
/// 3. Handle union types: ["string", "null"] -> "string"
/// 4. [NEW] Handle anyOf union types: anyOf: [{"type": "string"}, {"type": "null"}] -> "type": "string"
/// 5. Lowercase the value of the type field (required by Gemini v1internal)
/// 6. Remove numeric validation fields: multipleOf, exclusiveMinimum, exclusiveMaximum, etc.
/// Cleans a JSON Schema intended for use as responseSchema
pub fn clean_response_schema(value: &mut Value) {
    clean_json_schema(value);
}

pub fn clean_json_schema(value: &mut Value) {
    // 0. Preprocessing: expand $ref (Schema Flattening)
    // [FIX #952] Recursively collect $defs/definitions at every level, not just the root
    let mut all_defs = serde_json::Map::new();
    collect_all_defs(value, &mut all_defs);

    // Remove root-level $defs/definitions (kept for backward compatibility)
    if let Value::Object(map) = value {
        map.remove("$defs");
        map.remove("definitions");
    }

    // [FIX #952] Always run flatten_refs, even when defs is empty
    // This lets us catch and handle unresolvable $ref entries (downgraded to type string)
    if let Value::Object(map) = value {
        flatten_refs(map, &all_defs, 0);
    }

    // Recursively clean
    clean_json_schema_recursive(value, true, 0);
}

/// Schema cleaning with tool adapter support
///
/// This is the recommended cleaning entry point, supporting tool-specific optimizations
///
/// # Arguments
/// * `value` - the JSON Schema to clean
/// * `tool_name` - the tool name, used to match an adapter
///
/// # Processing flow
/// 1. Look up a matching tool adapter
/// 2. Run the adapter's pre-processing (tool-specific optimizations)
/// 3. Run the common cleaning logic
/// 4. Run the adapter's post-processing (final adjustments)
pub fn clean_json_schema_for_tool(value: &mut Value, tool_name: &str) {
    // 1. Look up a matching adapter
    let adapter = TOOL_ADAPTERS.iter().find(|a| a.matches(tool_name));

    // 2. Run pre-processing
    if let Some(adapter) = adapter {
        let _ = adapter.pre_process(value);
    }

    // 3. Run common cleaning
    clean_json_schema(value);

    // 4. Run post-processing
    if let Some(adapter) = adapter {
        let _ = adapter.post_process(value);
    }
}

/// [NEW #952] Recursively collects $defs and definitions at every level
///
/// An MCP tool's schema may define $defs at any nesting level, not only at the root.
/// This function deep-walks the entire schema, collecting all definitions into a single map.
fn collect_all_defs(value: &Value, defs: &mut serde_json::Map<String, Value>) {
    if let Value::Object(map) = value {
        // Collect $defs at the current level
        if let Some(Value::Object(d)) = map.get("$defs") {
            for (k, v) in d {
                // Avoid overwriting an existing definition (first-defined wins)
                defs.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        // Collect definitions at the current level (Draft-07 style)
        if let Some(Value::Object(d)) = map.get("definitions") {
            for (k, v) in d {
                defs.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        // Recursively process every child node
        for (key, v) in map {
            // Skip $defs/definitions themselves to avoid reprocessing
            if key != "$defs" && key != "definitions" {
                collect_all_defs(v, defs);
            }
        }
    } else if let Value::Array(arr) = value {
        for item in arr {
            collect_all_defs(item, defs);
        }
    }
}

/// Recursively expands $ref
fn flatten_refs(
    map: &mut serde_json::Map<String, Value>,
    defs: &serde_json::Map<String, Value>,
    depth: usize,
) {
    if depth > MAX_RECURSION_DEPTH {
        tracing::warn!("[Schema-Flatten] Max recursion depth reached, stopping ref expansion.");
        return;
    }

    // Check for and replace $ref
    if let Some(Value::String(ref_path)) = map.remove("$ref") {
        // Parse the reference name (e.g. #/$defs/MyType -> MyType)
        let ref_name = ref_path.split('/').last().unwrap_or(&ref_path);

        if let Some(def_schema) = defs.get(ref_name) {
            // Merge the definition's content into the current map
            if let Value::Object(def_map) = def_schema {
                for (k, v) in def_map {
                    // Only insert if the current map doesn't already have this key (avoid overwriting)
                    // though a $ref node normally shouldn't have other properties anyway
                    map.entry(k.clone()).or_insert_with(|| v.clone());
                }

                // Recursively process any $ref that may be present in the content just merged in
                // Note: with the depth limit in place, circular references no longer cause a stack overflow
                flatten_refs(map, defs, depth + 1);
            }
        } else {
            // [FIX #952] Unresolvable $ref: fall back to a permissive string type to avoid an API 400 error
            // This is better than failing the request outright; the tool call can at least still proceed
            map.insert("type".to_string(), serde_json::json!("string"));
            let hint = format!("(Unresolved $ref: {})", ref_path);
            let desc_val = map
                .entry("description".to_string())
                .or_insert_with(|| Value::String(String::new()));
            if let Value::String(s) = desc_val {
                if !s.contains(&hint) {
                    if !s.is_empty() {
                        s.push(' ');
                    }
                    s.push_str(&hint);
                }
            }
        }
    }

    // Walk child nodes
    for (_, v) in map.iter_mut() {
        if let Value::Object(child_map) = v {
            flatten_refs(child_map, defs, depth + 1);
        } else if let Value::Array(arr) = v {
            for item in arr {
                if let Value::Object(item_map) = item {
                    flatten_refs(item_map, defs, depth + 1);
                }
            }
        }
    }
}

fn clean_json_schema_recursive(value: &mut Value, is_schema_node: bool, depth: usize) -> bool {
    if depth > MAX_RECURSION_DEPTH {
        debug_assert!(
            false,
            "Max recursion depth reached in clean_json_schema_recursive"
        );
        return false;
    }
    let mut is_effectively_nullable = false;

    match value {
        Value::Object(map) => {
            // 0. [NEW] Merge allOf
            merge_all_of(map);

            // 0.1 [NEW #3327] Normalize the const keyword (convert to enum plus a matching type)
            // Gemini/Vertex's Schema proto doesn't support const; passing it through as-is causes a 400 INVALID_ARGUMENT
            // e.g. {"const": "element"} -> {"type": "string", "enum": ["element"]}
            if let Some(const_val) = map.remove("const") {
                if !map.contains_key("type") {
                    let inferred_type = match &const_val {
                        Value::String(_) => Some("string"),
                        Value::Number(n) => {
                            if n.is_i64() || n.is_u64() {
                                Some("integer")
                            } else {
                                Some("number")
                            }
                        }
                        Value::Bool(_) => Some("boolean"),
                        Value::Array(_) => Some("array"),
                        Value::Object(_) => Some("object"),
                        Value::Null => None,
                    };
                    if let Some(t) = inferred_type {
                        map.insert("type".to_string(), Value::String(t.to_string()));
                    }
                }
                let enum_entry = map
                    .entry("enum".to_string())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(enum_arr) = enum_entry {
                    if !enum_arr.contains(&const_val) {
                        enum_arr.push(const_val);
                    }
                }
            }

            // 0.5 [NEW] Structural normalization
            // Fixes cases where some MCP tools (e.g. pencil) misuse items to define object properties.
            // If type=object or properties is present but items is also defined, Gemini errors because items may only appear on an array.
            // We "align" the content of items into properties.
            if map.get("type").and_then(|t| t.as_str()) == Some("object")
                || map.contains_key("properties")
            {
                if let Some(items) = map.remove("items") {
                    tracing::warn!("[Schema-Normalization] Found 'items' in an Object-like node. Moving content to 'properties'.");
                    let target_props = map
                        .entry("properties".to_string())
                        .or_insert_with(|| json!({}));
                    if let Some(target_map) = target_props.as_object_mut() {
                        if let Some(source_map) = items.as_object() {
                            for (k, v) in source_map {
                                target_map.entry(k.clone()).or_insert_with(|| v.clone());
                            }
                        }
                    }
                }
            }

            // 1. [CRITICAL] Deeply and recursively process child items
            // Handle properties (object)
            if let Some(Value::Object(props)) = map.get_mut("properties") {
                // [FIX] Drop boolean / non-object sub-schemas. JSON Schema allows
                // `prop: true|false`, but Gemini's Schema proto requires every property
                // value to be an object; a bare boolean triggers an upstream 400
                // ("Invalid value at '...properties[N].value' ... false").
                let dropped_keys: Vec<String> = props
                    .iter()
                    .filter(|(_, v)| !v.is_object())
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in &dropped_keys {
                    props.remove(k);
                }

                let mut nullable_keys = std::collections::HashSet::new();
                for (k, v) in props.iter_mut() {
                    // Every value under properties must be an independent Schema node
                    if clean_json_schema_recursive(v, true, depth + 1) {
                        nullable_keys.insert(k.clone());
                    }
                }

                if !nullable_keys.is_empty() || !dropped_keys.is_empty() {
                    if let Some(Value::Array(req_arr)) = map.get_mut("required") {
                        req_arr.retain(|r| {
                            r.as_str()
                                .map(|s| {
                                    !nullable_keys.contains(s)
                                        && !dropped_keys.iter().any(|d| d == s)
                                })
                                .unwrap_or(true)
                        });
                        if req_arr.is_empty() {
                            map.remove("required");
                        }
                    }
                }

                // [NEW] Implicit type injection: if properties is present but type is missing, fill in object
                if !map.contains_key("type") {
                    map.insert("type".to_string(), Value::String("object".to_string()));
                }
            }

            // Handle items (array)
            // [FIX] items must be a Schema object; drop bare boolean / invalid items
            // (JSON Schema allows boolean `items`, Gemini's Schema proto rejects it).
            if map.get("items").map(|i| !i.is_object()).unwrap_or(false) {
                map.remove("items");
            }
            if let Some(items) = map.get_mut("items") {
                // The content of items must be an independent Schema node
                clean_json_schema_recursive(items, true, depth + 1);

                // [NEW] Implicit type injection: if items is present but type is missing, fill in array
                if !map.contains_key("type") {
                    map.insert("type".to_string(), Value::String("array".to_string()));
                }
            }

            // Gemini's Schema proto requires every ARRAY node to declare `items`,
            // including nested arrays. JSON Schema permits an itemless array, so
            // clients such as Claude Code may legitimately emit {"type":"array"}.
            // Use a string item schema as a Gemini-compatible fallback for these
            // otherwise unconstrained arrays.
            let is_array = map
                .get("type")
                .and_then(Value::as_str)
                .map(|t| t.eq_ignore_ascii_case("array"))
                .unwrap_or(false);
            if is_array && !map.contains_key("items") {
                map.insert("items".to_string(), json!({ "type": "string" }));
            }

            // Fallback: clean a regular object that has neither properties nor items
            if !map.contains_key("properties") && !map.contains_key("items") {
                for (k, v) in map.iter_mut() {
                    // Exclude keywords
                    if k != "anyOf" && k != "oneOf" && k != "allOf" && k != "enum" && k != "type" {
                        clean_json_schema_recursive(v, false, depth + 1);
                    }
                }
            }

            // 1.5. [FIX] Recursively clean every branch in the anyOf/oneOf array
            // Must run before the merge logic so the branches being merged have already been cleaned
            if let Some(Value::Array(any_of)) = map.get_mut("anyOf") {
                for branch in any_of.iter_mut() {
                    clean_json_schema_recursive(branch, true, depth + 1);
                }
            }
            if let Some(Value::Array(one_of)) = map.get_mut("oneOf") {
                for branch in one_of.iter_mut() {
                    clean_json_schema_recursive(branch, true, depth + 1);
                }
            }

            // 2. [FIX #815] Handle anyOf/oneOf union types: merge properties or select the best branch
            let mut union_to_merge = None;
            if let Some(Value::Array(any_of)) = map.get("anyOf") {
                union_to_merge = Some(any_of.clone());
            } else if let Some(Value::Array(one_of)) = map.get("oneOf") {
                union_to_merge = Some(one_of.clone());
            }

            if let Some(union_array) = union_to_merge {
                if let Some((best_branch, all_types)) = extract_best_schema_from_union(&union_array)
                {
                    if let Value::Object(branch_obj) = best_branch {
                        // Merge the branch's properties into the current map
                        for (k, v) in branch_obj {
                            if k == "properties" {
                                if let Some(target_props) = map
                                    .entry("properties".to_string())
                                    .or_insert_with(|| Value::Object(serde_json::Map::new()))
                                    .as_object_mut()
                                {
                                    if let Some(source_props) = v.as_object() {
                                        for (pk, pv) in source_props {
                                            target_props
                                                .entry(pk.clone())
                                                .or_insert_with(|| pv.clone());
                                        }
                                    }
                                }
                            } else if k == "required" {
                                if let Some(target_req) = map
                                    .entry("required".to_string())
                                    .or_insert_with(|| Value::Array(Vec::new()))
                                    .as_array_mut()
                                {
                                    if let Some(source_req) = v.as_array() {
                                        for rv in source_req {
                                            if !target_req.contains(rv) {
                                                target_req.push(rv.clone());
                                            }
                                        }
                                    }
                                }
                            } else if !map.contains_key(&k) {
                                map.insert(k, v);
                            }
                        }
                    }

                    // [NEW] Add a type hint to the description (following CLIProxyAPI's approach)
                    if all_types.len() > 1 {
                        let type_hint = format!("Accepts: {}", all_types.join(" | "));
                        append_hint_to_description(map, type_hint);
                    }
                }
            }

            // 3. [SAFETY] Check whether the current object is a JSON Schema node
            // Only apply allowlist filtering when the object looks like a Schema (contains type, properties, items, enum, anyOf, etc.).
            // Otherwise, if it's a plain Value (e.g. the functionCall object in request.rs), aggressive filtering would break its structure.
            let allowed_fields = [
                "type",
                "description",
                "properties",
                "required",
                "items",
                "enum",
                "title",
            ];

            let has_standard_keyword = map.keys().any(|k| allowed_fields.contains(&k.as_str()));

            // [NEW] Heuristic repair: if this is clearly a Schema node but has no standard keyword, yet has other keys
            // we infer this is a "shorthand" object definition and try to move its keys into properties.
            // Caveat: must ensure it's not a tool call or result (containing functionCall/functionResponse), to avoid breaking its structure.
            let is_not_schema_payload =
                map.contains_key("functionCall") || map.contains_key("functionResponse");
            if is_schema_node && !has_standard_keyword && !map.is_empty() && !is_not_schema_payload
            {
                let mut properties = serde_json::Map::new();
                let keys: Vec<String> = map.keys().cloned().collect();
                for k in keys {
                    if let Some(v) = map.remove(&k) {
                        properties.insert(k, v);
                    }
                }
                map.insert("type".to_string(), Value::String("object".to_string()));
                map.insert("properties".to_string(), Value::Object(properties));

                // Recursively clean the properties just moved in
                if let Some(Value::Object(props_map)) = map.get_mut("properties") {
                    for v in props_map.values_mut() {
                        clean_json_schema_recursive(v, true, depth + 1);
                    }
                }
            }

            let looks_like_schema =
                (is_schema_node || has_standard_keyword) && !is_not_schema_payload;

            if looks_like_schema {
                // 4. [ROBUST] Constraint migration: turn validation items into a description hint before allowlist filtering
                // [NEW] Use the unified constraint backfill function
                move_constraints_to_description(map);

                // 5. [CRITICAL] Allowlist filtering: physically strip out anything Gemini doesn't support, to prevent a 400 error
                let keys_to_remove: Vec<String> = map
                    .keys()
                    .filter(|k| !allowed_fields.contains(&k.as_str()))
                    .cloned()
                    .collect();
                for k in keys_to_remove {
                    map.remove(&k);
                }

                // 6. [SAFETY] Handle an empty Object
                // [FIX] Removed the reason-field injection logic
                // The previous implementation injected a reason field into empty Objects, which caused Gemini CLI and
                // similar tools to report "malformed function call", because the model would generate a call that
                // included a reason argument that the tool definition never declared.
                // Now: an empty Object keeps an empty properties object, letting the Gemini model decide for itself whether an argument is needed.
                if map.get("type").and_then(|t| t.as_str()) == Some("object") {
                    if !map.contains_key("properties") {
                        map.insert("properties".to_string(), serde_json::json!({}));
                    }
                }

                // 7. [SAFETY] Align the required field
                let valid_prop_keys: Option<std::collections::HashSet<String>> = map
                    .get("properties")
                    .and_then(|p| p.as_object())
                    .map(|obj| obj.keys().cloned().collect());

                if let Some(required_val) = map.get_mut("required") {
                    if let Some(req_arr) = required_val.as_array_mut() {
                        if let Some(keys) = &valid_prop_keys {
                            req_arr
                                .retain(|k| k.as_str().map(|s| keys.contains(s)).unwrap_or(false));
                        } else {
                            req_arr.clear();
                        }
                    }
                }

                if !map.contains_key("type") {
                    if map.contains_key("enum") {
                        map.insert("type".to_string(), Value::String("string".to_string()));
                    } else if map.contains_key("properties") {
                        map.insert("type".to_string(), Value::String("object".to_string()));
                    } else if map.contains_key("items") {
                        map.insert("type".to_string(), Value::String("array".to_string()));
                    }
                }

                // [IMPROVED] Compute the fallback type up front to avoid a borrow conflict
                let fallback = if map.contains_key("properties") {
                    "object"
                } else if map.contains_key("items") {
                    "array"
                } else {
                    "string"
                };

                // 8. Handle the type field
                if let Some(type_val) = map.get_mut("type") {
                    let mut selected_type = None;
                    match type_val {
                        Value::String(s) => {
                            let lower = s.to_lowercase();
                            if lower == "null" {
                                is_effectively_nullable = true;
                            } else {
                                selected_type = Some(lower);
                            }
                        }
                        Value::Array(arr) => {
                            for item in arr {
                                if let Value::String(s) = item {
                                    let lower = s.to_lowercase();
                                    if lower == "null" {
                                        is_effectively_nullable = true;
                                    } else if selected_type.is_none() {
                                        selected_type = Some(lower);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }

                    *type_val =
                        Value::String(selected_type.unwrap_or_else(|| fallback.to_string()));
                }

                if is_effectively_nullable {
                    let desc_val = map
                        .entry("description".to_string())
                        .or_insert_with(|| Value::String("".to_string()));
                    if let Value::String(s) = desc_val {
                        if !s.contains("nullable") {
                            if !s.is_empty() {
                                s.push(' ');
                            }
                            s.push_str("(nullable)");
                        }
                    }
                }

                // 9. Force enum values to strings
                if let Some(Value::Array(arr)) = map.get_mut("enum") {
                    for item in arr {
                        if !item.is_string() {
                            *item = Value::String(if item.is_null() {
                                "null".to_string()
                            } else {
                                item.to_string()
                            });
                        }
                    }
                }
            }
        }
        Value::Array(arr) => {
            // [FIX] Recursively clean every element of the array
            // This ensures every array-typed value (including but not limited to anyOf, oneOf, items, enum, etc.) is processed recursively
            for item in arr.iter_mut() {
                clean_json_schema_recursive(item, is_schema_node, depth + 1);
            }
        }
        _ => {}
    }

    is_effectively_nullable
}

/// [NEW] Merges every sub-Schema in an allOf array
fn merge_all_of(map: &mut serde_json::Map<String, Value>) {
    if let Some(Value::Array(all_of)) = map.remove("allOf") {
        let mut merged_properties = serde_json::Map::new();
        let mut merged_required = std::collections::HashSet::new();
        let mut other_fields = serde_json::Map::new();

        for sub_schema in all_of {
            if let Value::Object(sub_map) = sub_schema {
                // Merge properties
                if let Some(Value::Object(props)) = sub_map.get("properties") {
                    for (k, v) in props {
                        merged_properties.insert(k.clone(), v.clone());
                    }
                }

                // Merge required
                if let Some(Value::Array(reqs)) = sub_map.get("required") {
                    for req in reqs {
                        if let Some(s) = req.as_str() {
                            merged_required.insert(s.to_string());
                        }
                    }
                }

                // Merge the remaining fields (the first occurrence wins)
                for (k, v) in sub_map {
                    if k != "properties"
                        && k != "required"
                        && k != "allOf"
                        && !other_fields.contains_key(&k)
                    {
                        other_fields.insert(k, v);
                    }
                }
            }
        }

        // Apply the merged fields
        for (k, v) in other_fields {
            if !map.contains_key(&k) {
                map.insert(k, v);
            }
        }

        if !merged_properties.is_empty() {
            let existing_props = map
                .entry("properties".to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Value::Object(existing_map) = existing_props {
                for (k, v) in merged_properties {
                    existing_map.entry(k).or_insert(v);
                }
            }
        }

        if !merged_required.is_empty() {
            let existing_reqs = map
                .entry("required".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(req_arr) = existing_reqs {
                let mut current_reqs: std::collections::HashSet<String> = req_arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                for req in merged_required {
                    if current_reqs.insert(req.clone()) {
                        req_arr.push(Value::String(req));
                    }
                }
            }
        }
    }
}

/// [NEW] Appends a hint to the description field
/// Follows CLIProxyAPI's Lazy Hint strategy
fn append_hint_to_description(map: &mut serde_json::Map<String, Value>, hint: String) {
    let desc_val = map
        .entry("description".to_string())
        .or_insert_with(|| Value::String("".to_string()));

    if let Value::String(s) = desc_val {
        if s.is_empty() {
            *s = hint;
        } else if !s.contains(&hint) {
            *s = format!("{} {}", s, hint);
        }
    }
}

/// [NEW] Converts constraint fields into a description hint
/// Preserves their semantic meaning in the description before removing the constraint fields, so the model can still understand the constraint
fn move_constraints_to_description(map: &mut serde_json::Map<String, Value>) {
    let mut hints = Vec::new();

    for (field, label) in CONSTRAINT_FIELDS {
        if let Some(val) = map.get(*field) {
            if !val.is_null() {
                let val_str = if let Some(s) = val.as_str() {
                    s.to_string()
                } else {
                    val.to_string()
                };
                hints.push(format!("{}: {}", label, val_str));
            }
        }
    }

    if !hints.is_empty() {
        let constraint_hint = format!("[Constraint: {}]", hints.join(", "));
        append_hint_to_description(map, constraint_hint);
    }
}

/// [NEW] Computes a complexity score for a Schema branch (used to pick the best anyOf/oneOf branch)
/// Scoring: Object (3) > Array (2) > Scalar (1) > Null (0)
fn score_schema_option(val: &Value) -> i32 {
    if let Value::Object(obj) = val {
        if obj.contains_key("properties")
            || obj.get("type").and_then(|t| t.as_str()) == Some("object")
        {
            return 3;
        }
        if obj.contains_key("items") || obj.get("type").and_then(|t| t.as_str()) == Some("array") {
            return 2;
        }
        if let Some(type_str) = obj.get("type").and_then(|t| t.as_str()) {
            if type_str != "null" {
                return 1;
            }
        }
    }
    0
}

/// [NEW] Picks the best non-null Schema branch from an anyOf/oneOf union array
/// Returns: (best Schema, list of all possible types)
/// Follows CLIProxyAPI's selectBest logic
fn extract_best_schema_from_union(union_array: &Vec<Value>) -> Option<(Value, Vec<String>)> {
    let mut best_option: Option<&Value> = None;
    let mut best_score = -1;
    let mut all_types = Vec::new();

    for item in union_array {
        let score = score_schema_option(item);

        // Collect type information
        if let Some(type_str) = get_schema_type_name(item) {
            if !all_types.contains(&type_str) {
                all_types.push(type_str);
            }
        }

        if score > best_score {
            best_score = score;
            best_option = Some(item);
        }
    }

    best_option.cloned().map(|schema| (schema, all_types))
}

/// [NEW] Gets the type name of a Schema
fn get_schema_type_name(schema: &Value) -> Option<String> {
    if let Value::Object(obj) = schema {
        // Prefer an explicit type field
        if let Some(type_val) = obj.get("type") {
            if let Some(s) = type_val.as_str() {
                return Some(s.to_string());
            }
        }

        // Infer the type from the structure
        if obj.contains_key("properties") {
            return Some("object".to_string());
        }
        if obj.contains_key("items") {
            return Some("array".to_string());
        }
    }

    None
}

/// Fixes the types of tool call arguments to match the schema definition
///
/// Automatically converts argument value types based on the type declared in the schema:
/// - "123" → 123 (string → number/integer)
/// - "true" → true (string → boolean)
/// - 123 → "123" (number → string)
///
/// # Arguments
/// * `args` - the tool call's argument object (modified in place)
/// * `schema` - the tool's argument schema definition (usually the parameters object)
pub fn fix_tool_call_args(args: &mut Value, schema: &Value) {
    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        if let Some(args_obj) = args.as_object_mut() {
            for (key, value) in args_obj.iter_mut() {
                if let Some(prop_schema) = properties.get(key) {
                    fix_single_arg_recursive(value, prop_schema);
                }
            }
        }
    }
}

/// Recursively fixes the type of a single argument
fn fix_single_arg_recursive(value: &mut Value, schema: &Value) {
    // 1. Handle nested objects (properties)
    if let Some(nested_props) = schema.get("properties").and_then(|p| p.as_object()) {
        if let Some(value_obj) = value.as_object_mut() {
            for (key, nested_value) in value_obj.iter_mut() {
                if let Some(nested_schema) = nested_props.get(key) {
                    fix_single_arg_recursive(nested_value, nested_schema);
                }
            }
        }
        return;
    }

    // 2. Handle arrays (items)
    let schema_type = schema
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_lowercase();
    if schema_type == "array" {
        if let Some(items_schema) = schema.get("items") {
            if let Some(arr) = value.as_array_mut() {
                for item in arr {
                    fix_single_arg_recursive(item, items_schema);
                }
            }
        }
        return;
    }

    // 3. Handle basic type fixes
    match schema_type.as_str() {
        "number" | "integer" => {
            // string → number
            if let Some(s) = value.as_str() {
                // [SAFETY] Protect version numbers or codes with a leading zero (e.g. "01", "007"), which should not be converted to numbers
                if s.starts_with('0') && s.len() > 1 && !s.starts_with("0.") {
                    return;
                }

                // Prefer parsing as an integer first
                if let Ok(i) = s.parse::<i64>() {
                    *value = Value::Number(serde_json::Number::from(i));
                } else if let Ok(f) = s.parse::<f64>() {
                    if let Some(n) = serde_json::Number::from_f64(f) {
                        *value = Value::Number(n);
                    }
                }
            }
        }
        "boolean" => {
            // string → boolean
            if let Some(s) = value.as_str() {
                match s.to_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => *value = Value::Bool(true),
                    "false" | "0" | "no" | "off" => *value = Value::Bool(false),
                    _ => {}
                }
            } else if let Some(n) = value.as_i64() {
                // number 1/0 -> boolean
                if n == 1 {
                    *value = Value::Bool(true);
                } else if n == 0 {
                    *value = Value::Bool(false);
                }
            }
        }
        "string" => {
            // non-string → string (prevents a client from mistakenly passing a number for a text field)
            if !value.is_string() && !value.is_null() && !value.is_object() && !value.is_array() {
                *value = Value::String(value.to_string());
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_drops_boolean_subschemas() {
        // JSON Schema permits boolean sub-schemas (`prop: true|false`), but Gemini's
        // Schema proto rejects a non-object property value with HTTP 400. They must be
        // stripped at every depth (including inside `items`).
        let mut schema = json!({
            "type": "object",
            "properties": {
                "outer": {
                    "type": "object",
                    "properties": {
                        "forbidden": false,
                        "allowed": { "type": "string" }
                    },
                    "required": ["forbidden", "allowed"]
                },
                "list": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "nope": false,
                            "ok": { "type": "number" }
                        }
                    }
                }
            }
        });
        clean_json_schema(&mut schema);

        let outer_props = &schema["properties"]["outer"]["properties"];
        assert!(
            outer_props.get("forbidden").is_none(),
            "boolean sub-schema must be dropped"
        );
        assert!(
            outer_props["allowed"].is_object(),
            "valid sibling must survive"
        );

        let item_props = &schema["properties"]["list"]["items"]["properties"];
        assert!(
            item_props.get("nope").is_none(),
            "nested boolean sub-schema must be dropped"
        );
        assert!(item_props["ok"].is_object());

        let req = schema["properties"]["outer"]["required"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(req.iter().all(|r| r.as_str() != Some("forbidden")));
    }
    #[test]
    fn test_clean_json_schema_draft_2020_12() {
        let mut schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "minLength": 1,
                    "format": "city"
                },
                // Simulate a property-name collision: pattern is an Object property and should not be removed
                "pattern": {
                    "type": "object",
                    "properties": {
                        "regex": { "type": "string", "pattern": "^[a-z]+$" }
                    }
                },
                "unit": {
                    "type": ["string", "null"],
                    "default": "celsius"
                }
            },
            "required": ["location"]
        });

        clean_json_schema(&mut schema);

        // 1. Verify the type stays lowercase
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["location"]["type"], "string");

        // 2. Verify standard fields are removed and converted into the description (robust constraint migration)
        assert!(schema["properties"]["location"].get("minLength").is_none());
        assert!(schema["properties"]["location"].get("format").is_none());
        assert!(schema["properties"]["location"]["description"]
            .as_str()
            .unwrap()
            .contains("[Constraint: minLen: 1, format: city]"));

        // 3. Verify the property named "pattern" was not mistakenly removed
        assert!(schema["properties"].get("pattern").is_some());
        assert_eq!(schema["properties"]["pattern"]["type"], "object");

        // 4. Verify the inner pattern validation field is removed and converted into the description
        assert!(schema["properties"]["pattern"]["properties"]["regex"]
            .get("pattern")
            .is_none());
        assert!(
            schema["properties"]["pattern"]["properties"]["regex"]["description"]
                .as_str()
                .unwrap()
                .contains("[Constraint: pattern: ^[a-z]+$]")
        );

        // 5. Verify the union type is downgraded to a single type (Protobuf compatibility)
        assert_eq!(schema["properties"]["unit"]["type"], "string");

        // 6. Verify metadata fields are removed
        assert!(schema.get("$schema").is_none());
    }

    #[test]
    fn test_type_fallback() {
        // Test ["string", "null"] -> "string"
        let mut s1 = json!({"type": ["string", "null"]});
        clean_json_schema(&mut s1);
        assert_eq!(s1["type"], "string");

        // Test ["integer", "null"] -> "integer" (and lowercase check if needed, though usually integer)
        let mut s2 = json!({"type": ["integer", "null"]});
        clean_json_schema(&mut s2);
        assert_eq!(s2["type"], "integer");
    }

    #[test]
    fn test_flatten_refs() {
        let mut schema = json!({
            "$defs": {
                "Address": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    }
                }
            },
            "properties": {
                "home": { "$ref": "#/$defs/Address" }
            }
        });

        clean_json_schema(&mut schema);

        // Verify the reference is expanded and the type is lowercased
        assert_eq!(schema["properties"]["home"]["type"], "object");
        assert_eq!(
            schema["properties"]["home"]["properties"]["city"]["type"],
            "string"
        );
    }

    #[test]
    fn test_clean_json_schema_missing_required() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "existing_prop": { "type": "string" }
            },
            "required": ["existing_prop", "missing_prop"]
        });

        clean_json_schema(&mut schema);

        // Verify missing_prop is removed from required
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str().unwrap(), "existing_prop");
    }

    // [NEW TEST] Verify anyOf type extraction
    #[test]
    fn test_anyof_type_extraction() {
        // Test a FastMCP-style Optional[str] schema
        let mut schema = json!({
            "type": "object",
            "properties": {
                "testo": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "null"}
                    ],
                    "default": null,
                    "title": "Testo"
                },
                "importo": {
                    "anyOf": [
                        {"type": "number"},
                        {"type": "null"}
                    ],
                    "default": null,
                    "title": "Importo"
                },
                "attivo": {
                    "type": "boolean",
                    "title": "Attivo"
                }
            }
        });

        clean_json_schema(&mut schema);

        // Verify anyOf is removed
        assert!(schema["properties"]["testo"].get("anyOf").is_none());
        assert!(schema["properties"]["importo"].get("anyOf").is_none());

        // Verify type is correctly extracted
        assert_eq!(schema["properties"]["testo"]["type"], "string");
        assert_eq!(schema["properties"]["importo"]["type"], "number");
        assert_eq!(schema["properties"]["attivo"]["type"], "boolean");

        // Verify default is removed (not in the allowlist)
        assert!(schema["properties"]["testo"].get("default").is_none());
    }

    // [NEW TEST] Verify oneOf type extraction
    #[test]
    fn test_oneof_type_extraction() {
        let mut schema = json!({
            "properties": {
                "value": {
                    "oneOf": [
                        {"type": "integer"},
                        {"type": "null"}
                    ]
                }
            }
        });

        clean_json_schema(&mut schema);

        assert!(schema["properties"]["value"].get("oneOf").is_none());
        assert_eq!(schema["properties"]["value"]["type"], "integer");
    }

    // [NEW TEST] Verify an existing type is not overwritten
    #[test]
    fn test_existing_type_preserved() {
        let mut schema = json!({
            "properties": {
                "name": {
                    "type": "string",
                    "anyOf": [
                        {"type": "number"}
                    ]
                }
            }
        });

        clean_json_schema(&mut schema);

        // type already exists and should not be overwritten by a type from anyOf
        assert_eq!(schema["properties"]["name"]["type"], "string");
        assert!(schema["properties"]["name"].get("anyOf").is_none());
    }

    // [NEW TEST] Verify Issue #815: properties inside anyOf are not lost
    #[test]
    fn test_issue_815_anyof_properties_preserved() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "config": {
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "recursive": { "type": "boolean" }
                            },
                            "required": ["path"]
                        },
                        { "type": "null" }
                    ]
                }
            }
        });

        clean_json_schema(&mut schema);

        let config = &schema["properties"]["config"];

        // 1. Verify the type is extracted
        assert_eq!(config["type"], "object");

        // 2. Verify the properties inside anyOf were merged up
        assert!(config.get("properties").is_some());
        assert_eq!(config["properties"]["path"]["type"], "string");
        assert_eq!(config["properties"]["recursive"]["type"], "boolean");

        // 3. Verify required was merged up
        let req = config["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "path"));

        // 4. Verify the anyOf field itself is removed
        assert!(config.get("anyOf").is_none());

        // 5. Verify no reason field was injected for being "empty" (because we preserved the properties)
        assert!(config["properties"].get("reason").is_none());
    }

    // [NEW TEST] Verify the safety check: non-Schema objects should not be processed (protects tool calls)
    #[test]
    fn test_clean_json_schema_on_non_schema_object() {
        // Simulate a half-transformed functionCall object from request.rs
        let mut tool_call = json!({
            "functionCall": {
                "name": "local_shell_call",
                "args": { "command": ["ls"] },
                "id": "call_123"
            }
        });

        // Invoke the cleaning logic
        clean_json_schema(&mut tool_call);

        // Verify: these non-Schema fields should not be removed (since they don't match the looks_like_schema check)
        let fc = &tool_call["functionCall"];
        assert_eq!(fc["name"], "local_shell_call");
        assert_eq!(fc["args"]["command"][0], "ls");
        assert_eq!(fc["id"], "call_123");
    }

    // [NEW TEST] Verify Nullable handling
    #[test]
    fn test_nullable_handling_with_description() {
        let mut schema = json!({
            "type": ["string", "null"],
            "description": "User name"
        });

        clean_json_schema(&mut schema);

        // Verify type is downgraded and (nullable) is appended to the description
        assert_eq!(schema["type"], "string");
        assert!(schema["description"]
            .as_str()
            .unwrap()
            .contains("User name"));
        assert!(schema["description"]
            .as_str()
            .unwrap()
            .contains("(nullable)"));
    }

    // [NEW TEST] Verify propertyNames inside anyOf is removed
    #[test]
    fn test_clean_anyof_with_propertynames() {
        let mut schema = json!({
            "properties": {
                "config": {
                    "anyOf": [
                        {
                            "type": "object",
                            "propertyNames": {"pattern": "^[a-z]+$"},
                            "properties": {
                                "key": {"type": "string"}
                            }
                        },
                        {"type": "null"}
                    ]
                }
            }
        });

        clean_json_schema(&mut schema);

        // Verify anyOf is removed (already merged)
        let config = &schema["properties"]["config"];
        assert!(config.get("anyOf").is_none());

        // Verify propertyNames is removed
        assert!(config.get("propertyNames").is_none());

        // Verify the merged properties exist and have no propertyNames
        assert!(config.get("properties").is_some());
        assert_eq!(config["properties"]["key"]["type"], "string");
    }

    // [NEW TEST] Verify const inside an items array is removed
    #[test]
    fn test_clean_items_array_with_const() {
        let mut schema = json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "status": {
                        "const": "active",
                        "type": "string"
                    }
                }
            }
        });

        clean_json_schema(&mut schema);

        // Verify const is removed
        let status = &schema["items"]["properties"]["status"];
        assert!(status.get("const").is_none());

        // Verify type still exists
        assert_eq!(status["type"], "string");
    }

    // [NEW TEST] Verify cleaning of multi-level nested arrays
    #[test]
    fn test_deep_nested_array_cleaning() {
        let mut schema = json!({
            "properties": {
                "data": {
                    "anyOf": [
                        {
                            "type": "array",
                            "items": {
                                "anyOf": [
                                    {
                                        "type": "object",
                                        "propertyNames": {"maxLength": 10},
                                        "const": "test",
                                        "properties": {
                                            "name": {"type": "string"}
                                        }
                                    },
                                    {"type": "null"}
                                ]
                            }
                        }
                    ]
                }
            }
        });

        clean_json_schema(&mut schema);

        // Verify illegal fields at every nesting depth are removed
        let data = &schema["properties"]["data"];

        // anyOf should be merged away
        assert!(data.get("anyOf").is_none());

        // Verify propertyNames and const did not escape to the top level
        assert!(data.get("propertyNames").is_none());
        assert!(data.get("const").is_none());

        // Verify the structure is preserved correctly
        assert_eq!(data["type"], "array");
        if let Some(items) = data.get("items") {
            // anyOf inside items should also be merged
            assert!(items.get("anyOf").is_none());
            assert!(items.get("propertyNames").is_none());
            assert!(items.get("const").is_none());
        }
    }

    #[test]
    fn test_fix_tool_call_args() {
        let mut args = serde_json::json!({
            "port": "8080",
            "enabled": "true",
            "timeout": "5.5",
            "metadata": {
                "retry": "3"
            },
            "tags": ["1", "2"]
        });

        let schema = serde_json::json!({
            "properties": {
                "port": { "type": "integer" },
                "enabled": { "type": "boolean" },
                "timeout": { "type": "number" },
                "metadata": {
                    "type": "object",
                    "properties": {
                        "retry": { "type": "integer" }
                    }
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "integer" }
                }
            }
        });

        fix_tool_call_args(&mut args, &schema);

        assert_eq!(args["port"], 8080);
        assert_eq!(args["enabled"], true);
        assert_eq!(args["timeout"], 5.5);
        assert_eq!(args["metadata"]["retry"], 3);
        assert_eq!(args["tags"], serde_json::json!([1, 2]));
    }

    #[test]
    fn test_fix_tool_call_args_protection() {
        let mut args = serde_json::json!({
            "version": "01.0",
            "code": "007"
        });

        let schema = serde_json::json!({
            "properties": {
                "version": { "type": "number" },
                "code": { "type": "integer" }
            }
        });

        fix_tool_call_args(&mut args, &schema);

        // The string should be preserved so its semantics aren't broken
        assert_eq!(args["version"], "01.0");
        assert_eq!(args["code"], "007");
    }

    // [NEW TEST #952] Verify nested-level $defs are correctly collected and expanded
    #[test]
    fn test_nested_defs_flattening() {
        // MCP tools often nest $defs inside properties rather than at the root level
        let mut schema = json!({
            "type": "object",
            "properties": {
                "config": {
                    "$defs": {
                        "Address": {
                            "type": "object",
                            "properties": {
                                "city": { "type": "string" },
                                "zip": { "type": "string" }
                            }
                        }
                    },
                    "type": "object",
                    "properties": {
                        "home": { "$ref": "#/$defs/Address" },
                        "work": { "$ref": "#/$defs/Address" }
                    }
                }
            }
        });

        clean_json_schema(&mut schema);

        // Verify the nested $ref is correctly resolved
        let home = &schema["properties"]["config"]["properties"]["home"];
        assert_eq!(
            home["type"], "object",
            "home should have type 'object' from resolved $ref"
        );
        assert_eq!(
            home["properties"]["city"]["type"], "string",
            "home.properties.city should exist from resolved Address"
        );

        // Verify no $ref remains
        assert!(
            home.get("$ref").is_none(),
            "home should not have orphan $ref"
        );

        // Verify work is also correctly resolved
        let work = &schema["properties"]["config"]["properties"]["work"];
        assert_eq!(work["type"], "object");
        assert!(work.get("$ref").is_none());
    }

    // [NEW TEST #952] Verify an unresolvable $ref degrades gracefully
    #[test]
    fn test_unresolved_ref_fallback() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "external": { "$ref": "https://example.com/schemas/External.json" },
                "missing": { "$ref": "#/$defs/NonExistent" }
            }
        });

        clean_json_schema(&mut schema);

        // Verify the external reference is downgraded to type string
        let external = &schema["properties"]["external"];
        assert_eq!(
            external["type"], "string",
            "unresolved external $ref should fallback to string"
        );
        assert!(
            external["description"]
                .as_str()
                .unwrap()
                .contains("Unresolved $ref"),
            "description should contain unresolved $ref hint"
        );

        // Verify a missing internal reference is also downgraded
        let missing = &schema["properties"]["missing"];
        assert_eq!(missing["type"], "string");
        assert!(missing["description"]
            .as_str()
            .unwrap()
            .contains("NonExistent"));
    }

    // [NEW TEST #952] Verify deeply nested, multi-level $defs are all collected
    #[test]
    fn test_deeply_nested_multi_level_defs() {
        let mut schema = json!({
            "type": "object",
            "$defs": {
                "RootDef": { "type": "integer" }
            },
            "properties": {
                "level1": {
                    "type": "object",
                    "$defs": {
                        "Level1Def": { "type": "boolean" }
                    },
                    "properties": {
                        "level2": {
                            "type": "object",
                            "$defs": {
                                "Level2Def": { "type": "number" }
                            },
                            "properties": {
                                "useRoot": { "$ref": "#/$defs/RootDef" },
                                "useLevel1": { "$ref": "#/$defs/Level1Def" },
                                "useLevel2": { "$ref": "#/$defs/Level2Def" }
                            }
                        }
                    }
                }
            }
        });

        clean_json_schema(&mut schema);

        let level2_props = &schema["properties"]["level1"]["properties"]["level2"]["properties"];

        // Verify $defs at every level are correctly resolved
        assert_eq!(
            level2_props["useRoot"]["type"], "integer",
            "RootDef should resolve"
        );
        assert_eq!(
            level2_props["useLevel1"]["type"], "boolean",
            "Level1Def should resolve"
        );
        assert_eq!(
            level2_props["useLevel2"]["type"], "number",
            "Level2Def should resolve"
        );

        // Verify no $ref remains
        assert!(level2_props["useRoot"].get("$ref").is_none());
        assert!(level2_props["useLevel1"].get("$ref").is_none());
        assert!(level2_props["useLevel2"].get("$ref").is_none());
    }

    // [NEW TEST] Verify cleaning and heuristic repair of non-standard fields (e.g. cornerRadius)
    #[test]
    fn test_non_standard_field_cleaning_and_healing() {
        let mut schema = json!({
            "type": "array",
            "items": {
                "cornerRadius": { "type": "number" },
                "fillColor": { "type": "string" }
            }
        });

        clean_json_schema(&mut schema);

        // Verify non-standard fields inside items were moved into properties, and type: object was added
        let items = &schema["items"];
        assert_eq!(
            items["type"], "object",
            "Malformed items should be healed to type object"
        );
        assert!(
            items.get("properties").is_some(),
            "Malformed items should have properties object"
        );
        assert_eq!(items["properties"]["cornerRadius"]["type"], "number");
        assert_eq!(items["properties"]["fillColor"]["type"], "string");

        // Verify the original fields were removed from the top level of items (allowlist filtering)
        assert!(items.get("cornerRadius").is_none());
        assert!(items.get("fillColor").is_none());
    }

    // [NEW TEST] Verify handling of implicit Array (only items) and implicit Object (only properties)
    #[test]
    fn test_implicit_type_injection() {
        let mut schema = json!({
            "properties": {
                "values": {
                    "items": {
                        "cornerRadius": { "type": "number" }
                    }
                }
            }
        });

        clean_json_schema(&mut schema);

        // Verify values was injected with type: array
        assert_eq!(schema["properties"]["values"]["type"], "array");

        // Verify items was heuristically repaired to type: object and includes properties
        let items = &schema["properties"]["values"]["items"];
        assert_eq!(items["type"], "object");
        assert!(items["properties"].get("cornerRadius").is_some());
    }

    #[test]
    fn test_gemini_strict_validation_injection() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "patterns": {
                    "items": {
                        "properties": {
                            "type": {
                                "enum": ["A", "B"]
                            }
                        }
                    }
                },
                "nested_props": {
                    "properties": {
                        "foo": { "type": "string" }
                    }
                }
            }
        });

        clean_json_schema(&mut schema);

        // Verify enum was auto-filled with type: string
        let type_node = &schema["properties"]["patterns"]["items"]["properties"]["type"];
        assert_eq!(type_node["type"], "string");
        assert!(type_node.get("enum").is_some());

        // Verify nested properties was auto-filled with type: object
        assert_eq!(schema["properties"]["nested_props"]["type"], "object");

        // Verify patterns was auto-filled with type: array
        assert_eq!(schema["properties"]["patterns"]["type"], "array");
    }
    #[test]
    fn test_malformed_items_as_properties() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "config": {
                    "type": "object",
                    "items": {
                        "color": { "type": "string" },
                        "size": { "type": "number" }
                    }
                }
            }
        });

        clean_json_schema(&mut schema);

        // Verify items was removed and converted into properties
        let config = &schema["properties"]["config"];
        assert!(config.get("items").is_none());
        assert_eq!(config["properties"]["color"]["type"], "string");
        assert_eq!(config["properties"]["size"]["type"], "number");
        assert_eq!(config["type"], "object");
    }

    #[test]
    fn test_circular_ref_flattening() {
        // Simulate a circular reference: A -> B, B -> A
        let mut schema = json!({
            "$defs": {
                "A": {
                    "type": "object",
                    "properties": {
                        "toB": { "$ref": "#/$defs/B" }
                    }
                },
                "B": {
                    "type": "object",
                    "properties": {
                        "toA": { "$ref": "#/$defs/A" }
                    }
                }
            },
            "properties": {
                "start": { "$ref": "#/$defs/A" }
            }
        });

        // Without the depth limit, this would cause a stack overflow
        // With the depth limit, it should return normally (though the expansion is incomplete)
        clean_json_schema(&mut schema);

        // Verify the basic structure is preserved, with no crash
        assert_eq!(schema["properties"]["start"]["type"], "object");
        assert!(schema["properties"]["start"]["properties"]
            .get("toB")
            .is_some());
    }

    #[test]
    fn test_any_of_best_branch_selection() {
        let mut schema = json!({
            "anyOf": [
                { "type": "string" },
                { "type": "object", "properties": { "foo": { "type": "string" } } },
                { "type": "null" }
            ]
        });

        clean_json_schema(&mut schema);

        // Verify the highest-scoring Object branch was selected
        assert_eq!(schema["type"], "object");
        assert!(schema.get("properties").is_some());
        assert_eq!(schema["properties"]["foo"]["type"], "string");

        // Verify the type hint was added to the description (note: after cleaning, the null branch becomes a string marked (nullable), so after dedup it's string | object)
        assert!(schema["description"]
            .as_str()
            .unwrap()
            .contains("Accepts: string | object"));
    }

    #[test]
    fn test_issue_3327_const_normalization() {
        // Scenario 1: basic const conversion
        let mut schema1 = json!({
            "type": "object",
            "properties": {
                "action_type": {
                    "const": "element"
                },
                "count": {
                    "const": 5
                },
                "enabled": {
                    "const": true
                }
            }
        });

        clean_json_schema(&mut schema1);

        assert_eq!(schema1["properties"]["action_type"]["type"], "string");
        assert_eq!(schema1["properties"]["action_type"]["enum"], json!(["element"]));
        assert!(schema1["properties"]["action_type"].get("const").is_none());

        assert_eq!(schema1["properties"]["count"]["type"], "integer");
        assert_eq!(schema1["properties"]["count"]["enum"], json!(["5"]));
        assert!(schema1["properties"]["count"].get("const").is_none());

        assert_eq!(schema1["properties"]["enabled"]["type"], "boolean");
        assert_eq!(schema1["properties"]["enabled"]["enum"], json!(["true"]));
        assert!(schema1["properties"]["enabled"].get("const").is_none());

        // Scenario 2: ZCode Computer Use MCP's nested anyOf union containing const
        let mut schema2 = json!({
            "type": "object",
            "properties": {
                "target": {
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {
                                "type": {
                                    "const": "element"
                                },
                                "state_id": {
                                    "type": "string"
                                },
                                "index": {
                                    "type": "integer"
                                }
                            },
                            "required": ["type", "state_id", "index"],
                            "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "properties": {
                                "type": {
                                    "const": "coordinate"
                                },
                                "x": {
                                    "type": "integer"
                                },
                                "y": {
                                    "type": "integer"
                                }
                            },
                            "required": ["type", "x", "y"],
                            "additionalProperties": false
                        }
                    ]
                }
            }
        });

        clean_json_schema(&mut schema2);

        let target_props = &schema2["properties"]["target"]["properties"];
        assert!(target_props.get("type").is_some());
        assert_eq!(target_props["type"]["type"], "string");
        assert_eq!(target_props["type"]["enum"], json!(["element"]));
        assert!(target_props["type"].get("const").is_none());

        // Verify there is no illegal Schema structure (e.g. properties nesting the scalar string "element")
        assert!(target_props["type"].get("properties").is_none());
    }

    #[test]
    fn test_nested_array_without_items_gets_gemini_fallback() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "object",
                    "properties": {
                        "where": {
                            "type": "array",
                            "items": { "type": "array" }
                        }
                    }
                }
            }
        });

        clean_json_schema(&mut schema);

        assert_eq!(
            schema["properties"]["query"]["properties"]["where"]["items"]["items"],
            json!({ "type": "string" })
        );
    }
}
