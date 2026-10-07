//! JSON Schema handling for `extract`.
//!
//! The model's structured output supports only part of JSON Schema, and it can
//! run away when a requested field isn't in the text (see AGENTS.md). So an
//! agent's schema is checked against what is known to work, then rewritten so
//! every simple field may be `null`, which lets the model say "not found".

use std::collections::HashSet;

use serde_json::{Map, Value, json};

/// Keywords allowed in an agent's schema, all tested against real `fm`.
const ALLOWED: &[&str] = &[
    "type",
    "properties",
    "required",
    "items",
    "enum",
    "const",
    "description",
    "title",
    "minItems",
    "maxItems",
    "additionalProperties",
    "$schema",
];

/// Keywords with a known problem, and what goes wrong.
const KNOWN_BAD: &[(&str, &str)] = &[
    (
        "$ref",
        "references are not supported by the on-device model",
    ),
    (
        "$defs",
        "references are not supported by the on-device model",
    ),
    (
        "definitions",
        "references are not supported by the on-device model",
    ),
    (
        "pattern",
        "the on-device model fails with \"unsupported generation guide\"",
    ),
    (
        "minimum",
        "the model bends values to fit the limit instead of reporting them",
    ),
    (
        "maximum",
        "the model bends values to fit the limit instead of reporting them",
    ),
    (
        "exclusiveMinimum",
        "the model bends values to fit the limit instead of reporting them",
    ),
    (
        "exclusiveMaximum",
        "the model bends values to fit the limit instead of reporting them",
    ),
    ("format", "the on-device model ignores formats"),
    (
        "anyOf",
        "use a plain type; every field already allows null for \"not found\"",
    ),
    ("oneOf", "not supported; use a plain type or enum"),
    ("allOf", "not supported; use a plain type"),
];

const SCALARS: &[&str] = &["string", "number", "integer", "boolean"];

/// Checks an agent's schema and rewrites it for the model: the top level must
/// be an object, every property becomes required, and every scalar field
/// becomes `anyOf: [<type>, null]` with a unique title (the model needs one).
pub fn prepare(schema: &Value) -> Result<Value, String> {
    if schema["type"] != json!("object") {
        return Err("the schema's top level must be `\"type\": \"object\"`".into());
    }
    let mut titles = HashSet::new();
    rewrite(schema, &mut Vec::new(), &mut titles)
}

fn rewrite(
    node: &Value,
    path: &mut Vec<String>,
    titles: &mut HashSet<String>,
) -> Result<Value, String> {
    let Some(object) = node.as_object() else {
        return Err(format!("{}: a schema must be an object", show(path)));
    };
    for key in object.keys() {
        if let Some((_, why)) = KNOWN_BAD.iter().find(|(bad, _)| bad == key) {
            return Err(format!("{}: `{key}` can't be used: {why}", show(path)));
        }
        if !ALLOWED.contains(&key.as_str()) {
            return Err(format!(
                "{}: `{key}` is not supported. Supported keywords: {}",
                show(path),
                ALLOWED.join(", ")
            ));
        }
    }

    let (kind, nullable) = node_type(node, path)?;
    // Scalars become nullable anyway. A nullable object or array is untested
    // with the model, so ask for the plain type instead of silently dropping null.
    if nullable && matches!(kind.as_deref(), Some("object" | "array")) {
        return Err(format!(
            "{}: a nullable object or array is not supported; use the plain type \
             (an empty array or null fields already mean \"not found\")",
            show(path)
        ));
    }
    let mut out = object.clone();
    out.remove("$schema");
    out.remove("title");

    match kind.as_deref() {
        Some("object") => {
            let Some(properties) = object.get("properties").and_then(Value::as_object) else {
                return Err(format!("{}: an object needs `properties`", show(path)));
            };
            let mut rewritten = Map::new();
            for (name, property) in properties {
                path.push(name.clone());
                rewritten.insert(name.clone(), rewrite(property, path, titles)?);
                path.pop();
            }
            // Every field is required; "not found" is expressed as null instead.
            out.insert(
                "required".into(),
                properties.keys().cloned().collect::<Vec<_>>().into(),
            );
            out.insert("properties".into(), Value::Object(rewritten));
            out.insert("type".into(), json!("object"));
            Ok(Value::Object(out))
        }
        Some("array") => {
            let items = object
                .get("items")
                .ok_or_else(|| format!("{}: an array needs `items`", show(path)))?;
            path.push("item".into());
            let rewritten = rewrite(items, path, titles)?;
            path.pop();
            out.insert("items".into(), rewritten);
            out.insert("type".into(), json!("array"));
            Ok(Value::Object(out))
        }
        Some(scalar) if SCALARS.contains(&scalar) => {
            out.insert("type".into(), json!(scalar));
            let description = out.remove("description");
            let mut wrapped = json!({
                "title": unique_title(path, titles),
                "anyOf": [Value::Object(out), {"type": "null"}],
            });
            if let Some(description) = description {
                wrapped["description"] = description;
            }
            Ok(wrapped)
        }
        // `enum` / `const` without a type: kept as they are. They can't run away,
        // and wrapping them in anyOf is untested.
        None if object.contains_key("enum") || object.contains_key("const") => {
            Ok(Value::Object(out))
        }
        Some(other) => Err(format!("{}: type `{other}` is not supported", show(path))),
        None => Err(format!("{}: missing `type`", show(path))),
    }
}

/// Reads `type`, accepting `["<type>", "null"]` (a common way to say "optional").
fn node_type(node: &Value, path: &[String]) -> Result<(Option<String>, bool), String> {
    match &node["type"] {
        Value::Null => Ok((None, false)),
        Value::String(kind) => Ok((Some(kind.clone()), false)),
        Value::Array(kinds) => {
            let named: Vec<&str> = kinds
                .iter()
                .filter_map(Value::as_str)
                .filter(|k| *k != "null")
                .collect();
            match named.as_slice() {
                [kind] if kinds.len() == 2 => Ok((Some((*kind).to_owned()), true)),
                _ => Err(format!(
                    "{}: a type list may only be `[\"<type>\", \"null\"]`",
                    show(path)
                )),
            }
        }
        _ => Err(format!("{}: `type` must be a string", show(path))),
    }
}

/// A title from the field's path, e.g. `CustomerPhone` or `ItemSku`. The model
/// needs specific, unique titles: short ones like `name` made it run away.
fn unique_title(path: &[String], used: &mut HashSet<String>) -> String {
    let base: String = path
        .iter()
        .flat_map(|segment| segment.split(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect();
    let base = if base.is_empty() {
        "Field".to_owned()
    } else {
        base
    };
    let mut title = base.clone();
    let mut n = 2;
    while !used.insert(title.clone()) {
        title = format!("{base}{n}");
        n += 1;
    }
    title
}

fn show(path: &[String]) -> String {
    if path.is_empty() {
        "schema".into()
    } else {
        format!("schema field `{}`", path.join("."))
    }
}

/// Checks `value` against a schema produced by [`prepare`] (or the fixed
/// `classify` schema). Covers exactly the keywords those can contain.
pub fn validate(value: &Value, schema: &Value) -> Result<(), String> {
    check(value, schema, "$")
}

fn check(value: &Value, schema: &Value, at: &str) -> Result<(), String> {
    if let Some(options) = schema["anyOf"].as_array() {
        return if options.iter().any(|o| check(value, o, at).is_ok()) {
            Ok(())
        } else {
            Err(format!("{at}: matches none of the allowed types"))
        };
    }
    if let Some(allowed) = schema["enum"].as_array()
        && !allowed.contains(value)
    {
        return Err(format!("{at}: {value} is not one of the allowed values"));
    }
    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return Err(format!("{at}: expected {expected}"));
    }
    match schema["type"].as_str() {
        Some("object") => {
            let object = value
                .as_object()
                .ok_or_else(|| format!("{at}: expected an object"))?;
            for name in schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !object.contains_key(name) {
                    return Err(format!("{at}: missing `{name}`"));
                }
            }
            if let Some(properties) = schema["properties"].as_object() {
                for (name, sub) in properties {
                    if let Some(child) = object.get(name) {
                        check(child, sub, &format!("{at}.{name}"))?;
                    }
                }
            }
            Ok(())
        }
        Some("array") => {
            let items = value
                .as_array()
                .ok_or_else(|| format!("{at}: expected an array"))?;
            let count = items.len() as u64;
            if schema["minItems"].as_u64().is_some_and(|min| count < min) {
                return Err(format!("{at}: too few items"));
            }
            if schema["maxItems"].as_u64().is_some_and(|max| count > max) {
                return Err(format!("{at}: too many items"));
            }
            for (i, item) in items.iter().enumerate() {
                check(item, &schema["items"], &format!("{at}[{i}]"))?;
            }
            Ok(())
        }
        Some("string") if !value.is_string() => Err(format!("{at}: expected a string")),
        Some("number") if !value.is_number() => Err(format!("{at}: expected a number")),
        Some("integer") if !(value.is_i64() || value.is_u64()) => {
            Err(format!("{at}: expected an integer"))
        }
        Some("boolean") if !value.is_boolean() => Err(format!("{at}: expected true or false")),
        Some("null") if !value.is_null() => Err(format!("{at}: expected null")),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoice_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "invoice": {"type": "string", "description": "Invoice number"},
                "total": {"type": "number"},
                "status": {"enum": ["paid", "unpaid"]},
                "items": {"type": "array", "items": {
                    "type": "object",
                    "properties": {"name": {"type": "string"}, "sku": {"type": ["string", "null"]}}
                }}
            },
            "required": ["invoice"]
        })
    }

    #[test]
    fn prepare_should_make_scalars_nullable_with_unique_titles() {
        let prepared = prepare(&invoice_schema()).unwrap();
        assert_eq!(
            prepared["properties"]["invoice"],
            json!({"title": "Invoice", "description": "Invoice number",
                   "anyOf": [{"type": "string"}, {"type": "null"}]})
        );
    }

    #[test]
    fn prepare_should_title_nested_fields_by_path() {
        let prepared = prepare(&invoice_schema()).unwrap();
        let item = &prepared["properties"]["items"]["items"]["properties"];
        assert_eq!(
            (&item["name"]["title"], &item["sku"]["title"]),
            (&json!("ItemsItemName"), &json!("ItemsItemSku"))
        );
    }

    #[test]
    fn prepare_should_require_every_property_in_the_schemas_order() {
        let prepared = prepare(&invoice_schema()).unwrap();
        assert_eq!(
            prepared["required"],
            json!(["invoice", "total", "status", "items"])
        );
    }

    #[test]
    fn prepare_should_keep_enums_unwrapped() {
        let prepared = prepare(&invoice_schema()).unwrap();
        assert_eq!(
            prepared["properties"]["status"],
            json!({"enum": ["paid", "unpaid"]})
        );
    }

    #[test]
    fn prepare_should_reject_known_bad_keywords_with_the_reason() {
        let schema =
            json!({"type": "object", "properties": {"id": {"type": "string", "pattern": "^A"}}});
        assert_eq!(
            prepare(&schema).unwrap_err(),
            "schema field `id`: `pattern` can't be used: the on-device model fails with \"unsupported generation guide\""
        );
    }

    #[test]
    fn prepare_should_reject_unknown_keywords() {
        let schema =
            json!({"type": "object", "properties": {"id": {"type": "string", "minLength": 2}}});
        assert!(
            prepare(&schema)
                .unwrap_err()
                .starts_with("schema field `id`: `minLength` is not supported")
        );
    }

    #[test]
    fn prepare_should_reject_a_nullable_array() {
        let schema = json!({"type": "object", "properties": {
            "tags": {"type": ["array", "null"], "items": {"type": "string"}}}});
        assert!(
            prepare(&schema)
                .unwrap_err()
                .starts_with("schema field `tags`: a nullable object or array")
        );
    }

    #[test]
    fn prepare_should_reject_a_non_object_top_level() {
        assert!(prepare(&json!({"type": "array", "items": {"type": "string"}})).is_err());
    }

    #[test]
    fn unique_title_should_number_duplicates() {
        let mut used = HashSet::new();
        let path = vec!["a".to_owned()];
        assert_eq!(
            (
                unique_title(&path, &mut used),
                unique_title(&path, &mut used)
            ),
            ("A".into(), "A2".into())
        );
    }

    #[test]
    fn validate_should_accept_null_for_missing_fields() {
        let prepared = prepare(&invoice_schema()).unwrap();
        let value = json!({"invoice": "INV-1", "total": null, "status": "paid", "items": [{"name": "Widget", "sku": null}]});
        assert_eq!(validate(&value, &prepared), Ok(()));
    }

    #[test]
    fn validate_should_reject_a_value_outside_the_enum() {
        let prepared = prepare(&invoice_schema()).unwrap();
        let value = json!({"invoice": "INV-1", "total": 1, "status": "maybe", "items": []});
        assert!(
            validate(&value, &prepared)
                .unwrap_err()
                .contains("$.status")
        );
    }

    #[test]
    fn validate_should_reject_a_missing_required_field() {
        let prepared = prepare(&invoice_schema()).unwrap();
        assert!(
            validate(&json!({"invoice": "INV-1"}), &prepared)
                .unwrap_err()
                .contains("missing")
        );
    }
}
