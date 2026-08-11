//! JSON Schema → TypeScript type expression emitter.
//!
//! Pure functions: each takes a `&serde_json::Value` (JSON Schema fragment)
//! and returns a `String` containing the TypeScript type expression.
//!
//! Output style:
//! - `interface Foo { ... }` for object schemas (named types)
//! - `type Foo = string | null;` for primitive named schemas (rare)
//! - Indentation: 2 spaces, no tabs
//! - Property names preserved unless they contain non-identifier chars
//! - `?` suffix on optional properties (not in `required`)

use serde_json::Value;

use crate::naming::{safe_property_name, to_pascal_case};

/// Emit a `interface <Name> { ... }` (or `type` alias for primitives)
/// for a JSON Schema with the given name.
#[must_use]
pub fn schema_to_typescript(name: &str, schema: &Value) -> String {
    let type_name = to_pascal_case(name);
    let ts = schema_to_type_expr(schema, &[]);
    if let Some(obj) = schema.as_object() {
        // Interface iff there are *named* properties to enumerate. A bare
        // `additionalProperties` map, `enum`, `oneOf` etc. all become `type X = ...`.
        let has_named_properties = obj
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|m| !m.is_empty())
            .unwrap_or(false);
        if has_named_properties {
            format!("export interface {type_name} {ts}\n")
        } else {
            format!("export type {type_name} = {ts};\n")
        }
    } else {
        format!("export type {type_name} = {ts};\n")
    }
}

/// Convert a JSON Schema fragment into a TypeScript type expression.
/// `ref_chain` tracks the recursion path so we don't infinite-loop on
/// self-referential schemas (currently unused but reserved for future
/// $ref resolution).
#[must_use]
pub fn schema_to_type_expr(schema: &Value, ref_chain: &[String]) -> String {
    match schema {
        Value::Bool(b) => {
            if *b {
                "unknown".to_string()
            } else {
                "never".to_string()
            }
        }
        Value::Object(obj) => object_to_type(obj, ref_chain),
        _ => "unknown".to_string(),
    }
}

fn object_to_type(obj: &serde_json::Map<String, Value>, ref_chain: &[String]) -> String {
    // Handle $ref first.
    if let Some(Value::String(reference)) = obj.get("$ref") {
        return ref_to_type(reference);
    }

    // oneOf / anyOf → union
    if let Some(Value::Array(variants)) = obj.get("oneOf").or_else(|| obj.get("anyOf")) {
        return union_type(variants, ref_chain);
    }

    // allOf → intersection
    if let Some(Value::Array(parts)) = obj.get("allOf") {
        return intersection_type(parts, ref_chain);
    }

    // enum → literal union
    if let Some(Value::Array(values)) = obj.get("enum") {
        return enum_type(values);
    }

    // const → literal
    if let Some(const_value) = obj.get("const") {
        return literal_type(const_value);
    }

    // Type-based dispatch. `type` can be either a string or an array
    // (the latter used by OpenAPI 3.1 nullable: ["string", "null"]).
    let type_value = obj.get("type");

    // object shape with properties → inline object type
    if obj.contains_key("properties") {
        return object_with_properties(obj, ref_chain);
    }

    match type_value {
        Some(Value::String(t)) => primitive_type(t, obj),
        Some(Value::Array(types)) => nullable_union(types),
        _ => {
            // No recognized shape — fallback to unknown unless additionalProperties
            // signals a generic index signature.
            if obj.contains_key("additionalProperties") {
                additional_properties_type(&obj["additionalProperties"], ref_chain)
            } else {
                "unknown".to_string()
            }
        }
    }
}

fn primitive_type(t: &str, obj: &serde_json::Map<String, Value>) -> String {
    let mut out = match t {
        "string" => "string".to_string(),
        "integer" | "number" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => array_type(obj, &[]),
        "object" => {
            // object without properties → use additionalProperties or {}
            if let Some(ap) = obj.get("additionalProperties") {
                additional_properties_type(ap, &[])
            } else {
                "Record<string, unknown>".to_string()
            }
        }
        _ => "unknown".to_string(),
    };
    // OAS 3.0 nullable: true → append ` | null`.
    if obj.get("nullable") == Some(&Value::Bool(true)) {
        out.push_str(" | null");
    }
    out
}

fn nullable_union(types: &[Value]) -> String {
    let mut parts = Vec::with_capacity(types.len());
    for t in types {
        if let Value::String(s) = t {
            let mapped = match s.as_str() {
                "string" => "string",
                "integer" | "number" => "number",
                "boolean" => "boolean",
                "null" => "null",
                _ => "unknown",
            };
            parts.push(mapped.to_string());
        }
    }
    if parts.is_empty() {
        "unknown".to_string()
    } else {
        parts.join(" | ")
    }
}

fn array_type(obj: &serde_json::Map<String, Value>, ref_chain: &[String]) -> String {
    let Some(items) = obj.get("items") else {
        return "unknown[]".to_string();
    };
    let item_type = schema_to_type_expr(items, ref_chain);
    format!("{item_type}[]")
}

fn object_with_properties(obj: &serde_json::Map<String, Value>, ref_chain: &[String]) -> String {
    let Some(properties) = obj.get("properties").and_then(|v| v.as_object()) else {
        return "Record<string, unknown>".to_string();
    };
    let required: std::collections::BTreeSet<String> = obj
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut lines = Vec::with_capacity(properties.len());
    // Sorted for deterministic output.
    let mut prop_names: Vec<&String> = properties.keys().collect();
    prop_names.sort();

    for prop_name in prop_names {
        let Some(prop_schema) = properties.get(prop_name) else {
            continue;
        };
        let (safe, _) = safe_property_name(prop_name);
        let is_optional = !required.contains(prop_name);
        let suffix = if is_optional { "?" } else { "" };
        let prop_type = schema_to_type_expr(prop_schema, ref_chain);
        // Skip null-only properties (shouldn't happen but be defensive).
        if prop_type == "never" {
            continue;
        }
        lines.push(format!("  {safe}{suffix}: {prop_type};"));
    }

    if lines.is_empty() {
        return "{}".to_string();
    }

    let mut out = String::from("{\n");
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push('}');
    out
}

fn additional_properties_type(schema: &Value, ref_chain: &[String]) -> String {
    if schema == &Value::Bool(true) {
        return "Record<string, unknown>".to_string();
    }
    if schema == &Value::Bool(false) {
        // Closed object with no extras — handled by the calling property.
        return "Record<string, never>".to_string();
    }
    let value_type = schema_to_type_expr(schema, ref_chain);
    format!("Record<string, {value_type}>")
}

fn union_type(variants: &[Value], ref_chain: &[String]) -> String {
    let mut parts = Vec::with_capacity(variants.len());
    for v in variants {
        let t = schema_to_type_expr(v, ref_chain);
        // Deduplicate consecutive identical types (e.g. T | T).
        if parts.last().map(String::as_str) != Some(t.as_str()) {
            parts.push(t);
        }
    }
    if parts.is_empty() {
        "unknown".to_string()
    } else {
        parts.join(" | ")
    }
}

fn intersection_type(parts: &[Value], ref_chain: &[String]) -> String {
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        let t = schema_to_type_expr(p, ref_chain);
        if t != "unknown" {
            out.push(t);
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out.join(" & ")
    }
}

fn enum_type(values: &[Value]) -> String {
    let mut literals: Vec<String> = values
        .iter()
        .map(literal_type)
        .filter(|s| s != "never")
        .collect();
    literals.sort();
    literals.dedup();
    if literals.is_empty() {
        "never".to_string()
    } else {
        literals.join(" | ")
    }
}

fn literal_type(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Array(_) | Value::Object(_) => "unknown".to_string(),
    }
}

fn ref_to_type(reference: &str) -> String {
    // Convert "#/components/schemas/Foo" → "Foo".
    let parts: Vec<&str> = reference.split('/').collect();
    if let Some(last) = parts.last() {
        to_pascal_case(last)
    } else {
        "unknown".to_string()
    }
}

/// Public helper for tests: format a property name for TS output.
/// Returns the original name (or sanitized form if it was unsafe).
#[must_use]
pub fn format_property_name(name: &str) -> String {
    safe_property_name(name).0
}

/// Public helper for tests: check whether a name is a valid TS identifier.
#[must_use]
pub fn is_simple_identifier(name: &str) -> bool {
    !safe_property_name(name).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn emit(name: &str, schema: Value) -> String {
        schema_to_typescript(name, &schema)
    }

    #[test]
    fn emit_primitive_string() {
        assert_eq!(
            emit("Name", json!({"type": "string"})),
            "export type Name = string;\n"
        );
    }

    #[test]
    fn emit_primitive_integer() {
        assert_eq!(
            emit("Count", json!({"type": "integer"})),
            "export type Count = number;\n"
        );
    }

    #[test]
    fn emit_interface_with_required() {
        let s = json!({
            "type": "object",
            "required": ["id", "title"],
            "properties": {
                "id": {"type": "string"},
                "title": {"type": "string"},
                "description": {"type": "string"}
            }
        });
        let out = emit("Decision", s);
        assert!(out.contains("export interface Decision {"));
        assert!(out.contains("id: string;"));
        assert!(out.contains("title: string;"));
        assert!(out.contains("description?: string;"));
    }

    #[test]
    fn emit_nullable_via_type_array() {
        // OpenAPI 3.1 nullable: type: ["string", "null"]
        let out = emit("Maybe", json!({"type": ["string", "null"]}));
        assert_eq!(out, "export type Maybe = string | null;\n");
    }

    #[test]
    fn emit_nullable_via_30_compat() {
        let out = emit("Maybe", json!({"type": "string", "nullable": true}));
        assert_eq!(out, "export type Maybe = string | null;\n");
    }

    #[test]
    fn emit_enum_string_literal() {
        let out = emit(
            "Status",
            json!({"type": "string", "enum": ["active", "archived"]}),
        );
        // Sorted alphabetically by enum_type().
        assert_eq!(out, "export type Status = \"active\" | \"archived\";\n");
    }

    #[test]
    fn emit_ref_resolves_to_pascal_name() {
        let out = emit("Owner", json!({"$ref": "#/components/schemas/Company"}));
        assert_eq!(out, "export type Owner = Company;\n");
    }

    #[test]
    fn emit_array_of_refs() {
        let out = emit(
            "Companies",
            json!({
                "type": "array",
                "items": {"$ref": "#/components/schemas/Company"}
            }),
        );
        assert_eq!(out, "export type Companies = Company[];\n");
    }

    #[test]
    fn emit_one_of_union() {
        let out = emit(
            "Result",
            json!({
                "oneOf": [
                    {"type": "string"},
                    {"$ref": "#/components/schemas/Error"}
                ]
            }),
        );
        assert_eq!(out, "export type Result = string | Error;\n");
    }

    #[test]
    fn emit_intersection() {
        let out = emit(
            "Combined",
            json!({
                "allOf": [
                    {"$ref": "#/components/schemas/Base"},
                    {"type": "object", "properties": {"extra": {"type": "string"}}}
                ]
            }),
        );
        assert!(out.contains("Base &"));
        assert!(out.contains("extra?: string;"));
    }

    #[test]
    fn emit_additional_properties_map() {
        let out = emit(
            "Metadata",
            json!({
                "type": "object",
                "additionalProperties": {"type": "string"}
            }),
        );
        assert_eq!(out, "export type Metadata = Record<string, string>;\n");
    }

    #[test]
    fn emit_object_no_properties() {
        let out = emit("Empty", json!({"type": "object"}));
        assert_eq!(out, "export type Empty = Record<string, unknown>;\n");
    }

    #[test]
    fn emit_const_literal() {
        let out = emit("Verdict", json!({"const": "approved"}));
        assert_eq!(out, "export type Verdict = \"approved\";\n");
    }

    #[test]
    fn emit_bool_schema_true() {
        // JSON Schema allows boolean schemas: `true` accepts anything, `false` accepts nothing.
        assert_eq!(
            emit("Anything", Value::Bool(true)),
            "export type Anything = unknown;\n"
        );
        assert_eq!(
            emit("Nothing", Value::Bool(false)),
            "export type Nothing = never;\n"
        );
    }

    #[test]
    fn emit_property_name_with_dash_gets_underscored() {
        let s = json!({
            "type": "object",
            "required": ["x-rate-limit"],
            "properties": {
                "x-rate-limit": {"type": "integer"}
            }
        });
        let out = emit("Headers", s);
        // Required → no `?` suffix.
        assert!(out.contains("x_rate_limit: number;"), "got: {out}");
    }

    #[test]
    fn emit_property_name_with_dash_gets_underscored_and_optional() {
        let s = json!({
            "type": "object",
            "properties": {
                "x-rate-limit": {"type": "integer"}
            }
        });
        let out = emit("Headers", s);
        // Not required → `?` suffix.
        assert!(out.contains("x_rate_limit?: number;"), "got: {out}");
    }

    #[test]
    fn emit_nested_object_inline() {
        let s = json!({
            "type": "object",
            "required": ["owner"],
            "properties": {
                "owner": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {
                        "id": {"type": "string"},
                        "name": {"type": "string"}
                    }
                }
            }
        });
        let out = emit("WithNested", s);
        assert!(out.contains("owner: {"));
        assert!(out.contains("id: string;"));
        assert!(out.contains("name?: string;"));
    }

    #[test]
    fn emit_datetime_string_format_kept_as_string() {
        // OpenAPI date-time format → still TS string (no special handling).
        let out = emit("Created", json!({"type": "string", "format": "date-time"}));
        assert_eq!(out, "export type Created = string;\n");
    }

    #[test]
    fn emit_uuid_format_kept_as_string() {
        let out = emit("Id", json!({"type": "string", "format": "uuid"}));
        assert_eq!(out, "export type Id = string;\n");
    }

    #[test]
    fn deterministic_output_for_unsorted_property_order() {
        let s1 = json!({
            "type": "object",
            "required": ["z", "a"],
            "properties": {
                "z": {"type": "string"},
                "a": {"type": "string"},
                "m": {"type": "string"}
            }
        });
        let s2 = json!({
            "type": "object",
            "required": ["a", "z"],
            "properties": {
                "m": {"type": "string"},
                "z": {"type": "string"},
                "a": {"type": "string"}
            }
        });
        assert_eq!(emit("X", s1.clone()), emit("X", s2));
    }
}
