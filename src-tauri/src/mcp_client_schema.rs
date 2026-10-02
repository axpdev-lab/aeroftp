//! Backend-owned, fail-closed subset matching the MCP registry contract.
// SPDX-License-Identifier: GPL-3.0-or-later

use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashSet;

// Includes our 77 primary/compatibility names while keeping discovery bounded.
const MAX_TOOLS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchemaError {
    Unavailable,
    Unsupported,
    Arguments,
}

fn keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.keys().all(|k| allowed.contains(&k.as_str())))
}
fn text(value: Option<&Value>, max: usize) -> bool {
    value.is_none_or(|v| {
        v.as_str()
            .is_some_and(|s| s.len() <= max && !s.chars().any(char::is_control))
    })
}
fn parameter(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && (name.as_bytes()[0].is_ascii_alphabetic() || name.starts_with('_'))
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// No caller-supplied schema enters this function: only a fresh tools/list reply.
/// Incomplete/paginated catalogs and duplicate names are unavailable.
pub(crate) fn discover(result: &Value, name: &str) -> Result<Value, SchemaError> {
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or(SchemaError::Unavailable)?;
    if tools.len() > MAX_TOOLS
        || result.get("nextCursor").is_some()
        || name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
        || !name.as_bytes()[0].is_ascii_alphanumeric()
    {
        return Err(SchemaError::Unavailable);
    }
    let matching: Vec<_> = tools
        .iter()
        .filter(|t| t.get("name").and_then(Value::as_str) == Some(name))
        .collect();
    if matching.len() != 1 {
        return Err(SchemaError::Unavailable);
    }
    let tool = matching[0];
    if !text(tool.get("description"), 512) {
        return Err(SchemaError::Unsupported);
    }
    let schema = tool.get("inputSchema").ok_or(SchemaError::Unsupported)?;
    validate_schema(schema)?;
    Ok(schema.clone())
}

fn typed_value(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "number" => value.as_f64().is_some_and(f64::is_finite),
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n.fract() == 0.0)
        }
        "boolean" => value.is_boolean(),
        "array" => value
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string)),
        _ => false,
    }
}

// Preserve exact integer comparisons beyond f64's 53-bit mantissa. Mixed
// comparisons first order the integer parts, then the float's fractional part.
fn numeric_order(left: &Value, right: &Value) -> Option<Ordering> {
    fn integer(v: &Value) -> Option<i128> {
        v.as_i64()
            .map(i128::from)
            .or_else(|| v.as_u64().map(i128::from))
    }
    fn mixed(i: i128, f: f64) -> Ordering {
        let order = i.cmp(&(f.trunc() as i128));
        if order == Ordering::Equal {
            0.0_f64.partial_cmp(&f.fract()).expect("finite JSON number")
        } else {
            order
        }
    }
    let a = left.as_f64().filter(|f| f.is_finite())?;
    let b = right.as_f64().filter(|f| f.is_finite())?;
    Some(match (integer(left), integer(right)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(a), None) => mixed(a, b),
        (None, Some(b)) => mixed(b, a).reverse(),
        (None, None) => a.partial_cmp(&b)?,
    })
}

fn equal_scalar(left: &Value, right: &Value) -> bool {
    if left.is_number() && right.is_number() {
        numeric_order(left, right) == Some(Ordering::Equal)
    } else {
        left == right
    }
}

fn constraints(property: &Value, value: &Value) -> bool {
    property.get("enum").is_none_or(|values| {
        values
            .as_array()
            .is_some_and(|a| a.iter().any(|v| equal_scalar(v, value)))
    }) && property
        .get("minimum")
        .is_none_or(|min| numeric_order(value, min).is_some_and(|o| o != Ordering::Less))
        && property
            .get("maximum")
            .is_none_or(|max| numeric_order(value, max).is_some_and(|o| o != Ordering::Greater))
}

fn validate_schema(schema: &Value) -> Result<(), SchemaError> {
    let invalid = SchemaError::Unsupported;
    if serde_json::to_vec(schema).map_err(|_| invalid)?.len() > 8192
        || !keys(
            schema,
            &[
                "type",
                "properties",
                "required",
                "additionalProperties",
                "description",
                "$schema",
                "title",
            ],
        )
        || schema.get("type").and_then(Value::as_str) != Some("object")
        || !text(schema.get("description"), 512)
        || !text(schema.get("$schema"), 512)
        || !text(schema.get("title"), 512)
        || schema
            .get("additionalProperties")
            .is_some_and(|v| v != &Value::Bool(false))
    {
        return Err(invalid);
    }
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or(invalid)?;
    if properties.len() > 16 || !properties.keys().all(|k| parameter(k)) {
        return Err(invalid);
    }
    if let Some(required) = schema.get("required") {
        let required = required.as_array().ok_or(invalid)?;
        let mut seen = HashSet::new();
        for name in required {
            let name = name.as_str().ok_or(invalid)?;
            if !properties.contains_key(name) || !seen.insert(name) {
                return Err(invalid);
            }
        }
    }
    let mut headers = HashSet::new();
    for property in properties.values() {
        if !keys(
            property,
            &[
                "type",
                "description",
                "items",
                "x-mcp-header",
                "title",
                "format",
                "default",
                "enum",
                "minimum",
                "maximum",
            ],
        ) || !text(property.get("description"), 512)
            || !text(property.get("title"), 512)
            || !text(property.get("format"), 512)
        {
            return Err(invalid);
        }
        let kind = property
            .get("type")
            .and_then(Value::as_str)
            .ok_or(invalid)?;
        match kind {
            "string" | "number" | "integer" | "boolean" => {
                if property.get("items").is_some() {
                    return Err(invalid);
                }
            }
            "array" => {
                let items = property.get("items").ok_or(invalid)?;
                if !keys(items, &["type"])
                    || items.get("type").and_then(Value::as_str) != Some("string")
                {
                    return Err(invalid);
                }
            }
            _ => return Err(invalid),
        }
        // Defaults and format/title are annotations only. Defaults are checked
        // for type compatibility, but are never inserted into caller arguments.
        if property
            .get("default")
            .is_some_and(|v| !typed_value(kind, v))
        {
            return Err(invalid);
        }
        if let Some(values) = property.get("enum") {
            let values = values.as_array().ok_or(invalid)?;
            if kind == "array"
                || values.is_empty()
                || values.len() > 64
                || values
                    .iter()
                    .any(|v| !typed_value(kind, v) || v.is_string() && !text(Some(v), 512))
            {
                return Err(invalid);
            }
        }
        for bound in ["minimum", "maximum"] {
            if let Some(value) = property.get(bound) {
                if !matches!(kind, "number" | "integer") || !typed_value("number", value) {
                    return Err(invalid);
                }
            }
        }
        if let (Some(min), Some(max)) = (property.get("minimum"), property.get("maximum")) {
            if numeric_order(min, max) == Some(Ordering::Greater) {
                return Err(invalid);
            }
        }
        if let Some(header) = property.get("x-mcp-header") {
            let header = header.as_str().ok_or(invalid)?;
            if !matches!(kind, "string" | "integer" | "boolean")
                || header.is_empty()
                || header.len() > 64
                || !header
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&b))
                || !headers.insert(header.to_ascii_lowercase())
            {
                return Err(invalid);
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_arguments(schema: &Value, arguments: &Value) -> Result<(), SchemaError> {
    validate_schema(schema)?;
    let invalid = SchemaError::Arguments;
    let arguments = arguments.as_object().ok_or(invalid)?;
    if serde_json::to_vec(arguments).map_err(|_| invalid)?.len() > 60 * 1024 {
        return Err(invalid);
    }
    let properties = schema["properties"].as_object().ok_or(invalid)?;
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        if required
            .iter()
            .any(|n| !arguments.contains_key(n.as_str().unwrap_or("")))
        {
            return Err(invalid);
        }
    }
    for (name, value) in arguments {
        let Some(property) = properties.get(name) else {
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                return Err(invalid);
            }
            continue;
        };
        if !typed_value(property["type"].as_str().unwrap_or(""), value)
            || !constraints(property, value)
        {
            return Err(invalid);
        }
        // Wire header integers use the exact JSON-safe range, and strings
        // must fit the transport's bounded mirror before approval is asked.
        if property.get("x-mcp-header").is_some() {
            match property["type"].as_str() {
                Some("integer")
                    if !value.as_i64().is_some_and(|n| {
                        (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&n)
                    }) =>
                {
                    return Err(invalid)
                }
                Some("string") if value.as_str().is_none_or(|s| s.len() > 4096) => {
                    return Err(invalid)
                }
                _ => {}
            }
        }
    }
    Ok(())
}

pub(crate) fn revision(schema: &Value, key: &[u8; 32]) -> String {
    blake3::keyed_hash(
        key,
        &serde_json::to_vec(schema).expect("validated JSON schema"),
    )
    .to_hex()
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn schema() -> Value {
        json!({"type":"object","properties":{"count":{"type":"integer","x-mcp-header":"Count"},"tags":{"type":"array","items":{"type":"string"}}},"required":["count"],"additionalProperties":false})
    }
    #[test]
    fn reference_everything_catalog_accepts_all_thirteen_real_schemas() {
        let reply: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/mcp-client/server-everything-2026.8.31-tools-list.json"
        ))
        .unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 13);
        for tool in tools {
            discover(&reply["result"], tool["name"].as_str().unwrap()).unwrap();
        }
    }

    #[test]
    fn scalar_constraints_are_enforced_and_defaults_are_never_inserted() {
        let s = json!({"$schema":"http://json-schema.org/draft-07/schema#","title":"Fixture","type":"object","properties":{
            "city":{"type":"string","title":"City","format":"uri","enum":["Rome","Paris"],"default":"Rome"},
            "n":{"type":"number","minimum":1,"maximum":10,"default":3},
            "flag":{"type":"boolean","enum":[true]},
            "count":{"type":"integer","enum":[1,2]}
        }});
        let omitted = json!({});
        validate_arguments(&s, &omitted).unwrap();
        assert_eq!(omitted, json!({}));
        validate_arguments(&s, &json!({"city":"Paris","n":1,"flag":true,"count":2.0})).unwrap();
        validate_arguments(&s, &json!({"n":10})).unwrap();
        for args in [
            json!({"city":"Berlin"}),
            json!({"n":0.99}),
            json!({"n":10.01}),
            json!({"flag":false}),
            json!({"count":3}),
        ] {
            assert_eq!(validate_arguments(&s, &args), Err(SchemaError::Arguments));
        }
        for (pointer, value) in [
            ("/title", json!("Changed")),
            ("/$schema", json!("other")),
            ("/properties/city/title", json!("Changed")),
            ("/properties/city/format", json!("email")),
            ("/properties/city/default", json!("Paris")),
            ("/properties/city/enum", json!(["Rome"])),
            ("/properties/n/minimum", json!(2)),
            ("/properties/n/maximum", json!(9)),
        ] {
            let mut changed = s.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert_ne!(revision(&s, &[3; 32]), revision(&changed, &[3; 32]));
        }
    }

    #[test]
    fn malformed_metadata_defaults_enums_and_ranges_fail_closed() {
        for property in [
            json!({"type":"string","default":2}),
            json!({"type":"integer","default":1.5}),
            json!({"type":"array","items":{"type":"string"},"default":[1]}),
            json!({"type":"string","enum":[]}),
            json!({"type":"string","enum":[true]}),
            json!({"type":"string","enum":vec!["x";65]}),
            json!({"type":"string","enum":["x".repeat(513)]}),
            json!({"type":"array","items":{"type":"string"},"enum":[["x"]]}),
            json!({"type":"string","minimum":0}),
            json!({"type":"number","minimum":"0"}),
            json!({"type":"number","minimum":5,"maximum":4}),
            json!({"type":"boolean","maximum":1}),
            json!({"type":"string","title":false}),
            json!({"type":"string","format":"bad\n"}),
        ] {
            let s = json!({"type":"object","properties":{"value":property}});
            assert_eq!(validate_schema(&s), Err(SchemaError::Unsupported), "{s}");
        }
    }

    #[test]
    fn numeric_constraints_do_not_round_large_integer_arguments() {
        let s = json!({"type":"object","properties":{"n":{"type":"integer","maximum":9007199254740992_u64}}});
        validate_arguments(&s, &json!({"n":9007199254740992_u64})).unwrap();
        assert_eq!(
            validate_arguments(&s, &json!({"n":9007199254740993_u64})),
            Err(SchemaError::Arguments)
        );
        let s = json!({"type":"object","properties":{"n":{"type":"integer","enum":[9007199254740992_u64]}}});
        assert_eq!(
            validate_arguments(&s, &json!({"n":9007199254740993_u64})),
            Err(SchemaError::Arguments)
        );
    }

    #[test]
    fn backend_enforces_required_integer_array_and_unknown_arguments() {
        assert!(validate_arguments(&schema(), &json!({"count":2,"tags":["a"]})).is_ok());
        for args in [
            json!({}),
            json!({"count":2.5}),
            json!({"count":null}),
            json!({"count":true}),
            json!({"count":2,"tags":[1]}),
            json!({"count":2,"unknown":1}),
        ] {
            assert_eq!(
                validate_arguments(&schema(), &args),
                Err(SchemaError::Arguments)
            );
        }
    }
    #[test]
    fn arguments_leave_room_for_the_complete_transport_envelope() {
        let schema = serde_json::json!({"type":"object","properties":{"text":{"type":"string"}}});
        assert_eq!(
            validate_arguments(&schema, &serde_json::json!({"text":"x".repeat(60 * 1024)})),
            Err(SchemaError::Arguments)
        );
        let arguments = serde_json::json!({"text":"x".repeat(60 * 1024 - 32)});
        validate_arguments(&schema, &arguments).unwrap();
        let mut params = serde_json::Map::new();
        params.insert("name".into(), serde_json::json!("x".repeat(128)));
        params.insert("arguments".into(), arguments);
        for era in [
            crate::mcp_client_protocol::Era::Modern,
            crate::mcp_client_protocol::Era::Legacy(crate::mcp_client_protocol::LEGACY_PREFERRED),
        ] {
            let request = crate::mcp_client_protocol::request(
                era,
                u64::MAX,
                "tools/call",
                params.clone(),
                "AeroFTP",
                env!("CARGO_PKG_VERSION"),
            )
            .unwrap();
            assert!(serde_json::to_vec(&request).unwrap().len() < 64 * 1024);
        }
    }

    #[test]
    fn unsupported_constraints_and_header_collisions_fail_closed() {
        for key in ["$ref", "oneOf", "exclusiveMinimum", "pattern"] {
            let mut s = schema();
            s["properties"]["count"][key] = json!(1);
            assert_eq!(validate_schema(&s), Err(SchemaError::Unsupported));
        }
        let mut s = schema();
        s["properties"]["other"] = json!({"type":"string","x-mcp-header":"count"});
        assert_eq!(validate_schema(&s), Err(SchemaError::Unsupported));
    }
    #[test]
    fn duplicate_paginated_and_oversized_catalogs_are_unavailable() {
        let tool = json!({"name":"echo","inputSchema":schema()});
        for result in [
            json!({"tools":[tool.clone(),tool.clone()]}),
            json!({"tools":[tool.clone()],"nextCursor":"more"}),
            json!({"tools":vec![tool;MAX_TOOLS + 1]}),
        ] {
            assert_eq!(discover(&result, "echo"), Err(SchemaError::Unavailable));
        }
    }
    #[test]
    fn own_server_catalog_including_compatibility_names_can_discover_diagnostics() {
        let tools: Vec<_> = crate::mcp::tools::tool_definitions().into_iter().map(|tool| {
            json!({"name":tool.name,"description":tool.description,"inputSchema":tool.input_schema})
        }).collect();
        assert!(tools.len() > 64 && tools.len() <= MAX_TOOLS);
        let schema = discover(&json!({"tools":tools}), "aeroftp_mcp_info").unwrap();
        validate_arguments(&schema, &json!({})).unwrap();
    }

    #[test]
    fn catalog_limit_accepts_128_unique_tools_and_refuses_129() {
        let mut tools: Vec<_> = (0..MAX_TOOLS)
            .map(|i| json!({"name":format!("tool_{i}"),"inputSchema":schema()}))
            .collect();
        assert!(discover(&json!({"tools":tools}), "tool_0").is_ok());
        tools.push(json!({"name":"overflow","inputSchema":schema()}));
        assert_eq!(
            discover(&json!({"tools":tools}), "tool_0"),
            Err(SchemaError::Unavailable)
        );
    }

    #[test]
    fn header_only_change_invalidates_backend_schema_revision() {
        let mut s = schema();
        let first = revision(&s, &[3; 32]);
        s["properties"]["count"]["x-mcp-header"] = json!("Other");
        assert_ne!(first, revision(&s, &[3; 32]));
    }
}
