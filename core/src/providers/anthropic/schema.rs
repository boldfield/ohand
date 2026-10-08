//! Minimal structural JSON-schema check for the `interpret` tool input.
//!
//! Supports `type`, `enum`, `required`, `properties`, `additionalProperties: false` and
//! `items`. Semantic validation of the proposal belongs to interpretation; this only keeps
//! structurally wrong tool input from leaving the adapter.

use serde_json::Value;

pub(super) fn conforms(value: &Value, schema: &Value) -> bool {
    let Some(schema) = schema.as_object() else {
        return true;
    };
    if let Some(expected_type) = schema.get("type").and_then(Value::as_str) {
        if !type_matches(value, expected_type) {
            return false;
        }
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        if !options.contains(value) {
            return false;
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            let all_present = required
                .iter()
                .filter_map(Value::as_str)
                .all(|key| object.contains_key(key));
            if !all_present {
                return false;
            }
        }
        if let Some(properties) = properties {
            for (key, property_schema) in properties {
                if let Some(property_value) = object.get(key) {
                    if !conforms(property_value, property_schema) {
                        return false;
                    }
                }
            }
        }
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            let declared = |key: &String| properties.is_some_and(|map| map.contains_key(key));
            if !object.keys().all(declared) {
                return false;
            }
        }
    }
    if let (Some(items), Some(item_schema)) = (value.as_array(), schema.get("items")) {
        if !items.iter().all(|item| conforms(item, item_schema)) {
            return false;
        }
    }
    true
}

fn type_matches(value: &Value, expected_type: &str) -> bool {
    match expected_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        _ => false,
    }
}
