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
            Ok(json!({
                "title": unique_title(path, titles),
                "description": null_description(description.as_ref(), path),
                "anyOf": [Value::Object(out), {"type": "null"}],
            }))
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

/// The schema sent to the model: `prepared` with every `number` and `integer`
/// field asked for as text, which [`read_numbers`] turns back into numbers.
/// Told to copy values exactly, the model often wrote `$1,280.00` in a number
/// field as `null` (10 of 50 runs over 5 invoices), and telling it to drop the
/// `$` and commas made it drop the decimal point too (`12,450.50` became
/// `1245050`). Asked for text, it copied every amount it finished (46 of 46),
/// measured 2026-10-11.
pub fn for_model(prepared: &Value) -> Value {
    let mut schema = prepared.clone();
    numbers_as_text(&mut schema);
    schema
}

fn numbers_as_text(node: &mut Value) {
    if let Some(options) = node.get_mut("anyOf").and_then(Value::as_array_mut) {
        for option in options {
            if numeric(option).is_some() {
                option["type"] = json!("string");
            }
        }
        return;
    }
    if let Some(properties) = node.get_mut("properties").and_then(Value::as_object_mut) {
        properties.values_mut().for_each(numbers_as_text);
    }
    if let Some(items) = node.get_mut("items") {
        numbers_as_text(items);
    }
}

/// `Some(true)` for an integer field, `Some(false)` for a number field, `None`
/// otherwise. A field with `enum` or `const` is left to the model as it is.
fn numeric(node: &Value) -> Option<bool> {
    if node.get("enum").is_some() || node.get("const").is_some() {
        return None;
    }
    match node["type"].as_str() {
        Some("integer") => Some(true),
        Some("number") => Some(false),
        _ => None,
    }
}

/// Turns the text the model wrote for each number field (see [`for_model`])
/// into a number, or `null` when it isn't plainly one amount.
pub fn read_numbers(value: &mut Value, prepared: &Value) {
    if let Some(options) = prepared["anyOf"].as_array() {
        if let (Some(integer), Value::String(text)) = (options.iter().find_map(numeric), &*value) {
            *value = parse_number(text, integer).unwrap_or(Value::Null);
        }
        return;
    }
    if let (Some(properties), Some(object)) =
        (prepared["properties"].as_object(), value.as_object_mut())
    {
        for (name, sub) in properties {
            if let Some(child) = object.get_mut(name) {
                read_numbers(child, sub);
            }
        }
    }
    if let Some(items) = value.as_array_mut() {
        for item in items {
            read_numbers(item, &prepared["items"]);
        }
    }
}

/// Reads one amount such as `1280`, `$1,280.00`, `AUD 12,450.50`, `-4.5` or
/// `3,000 USD`: an optional currency code and sign, then digits with optional
/// thousands commas and decimals. Anything else (`about 300`, `1.234,56`,
/// `2 hours`) is `None`, so a value the model mangled becomes `null`, not a
/// wrong number.
fn parse_number(text: &str, integer: bool) -> Option<Value> {
    let mut rest = text.trim();
    let negative = strip(&mut rest, "-");
    strip_code(&mut rest, true);
    let negative = negative | strip(&mut rest, "-");
    for sign in ["$", "€", "£", "¥"] {
        if strip(&mut rest, sign) {
            break;
        }
    }
    let negative = negative | strip(&mut rest, "-");
    strip_code(&mut rest, false);

    let (whole, fraction) = match rest.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let grouped = match whole.split(',').collect::<Vec<_>>().as_slice() {
        [single] => digits(single),
        [first, groups @ ..] => {
            digits(first) && first.len() <= 3 && groups.iter().all(|g| g.len() == 3 && digits(g))
        }
        [] => false,
    };
    if !grouped || fraction.is_some_and(|f| !digits(f)) {
        return None;
    }
    let plain = format!(
        "{}{}",
        if negative { "-" } else { "" },
        whole.replace(',', "")
    );
    match fraction {
        None => plain.parse::<i64>().ok().map(Value::from),
        Some(f) if integer && f.bytes().all(|b| b == b'0') => {
            plain.parse::<i64>().ok().map(Value::from)
        }
        Some(_) if integer => None,
        Some(f) => format!("{plain}.{f}").parse::<f64>().ok().map(Value::from),
    }
}

/// Removes `prefix` (and the spaces after it) from the start of `rest`.
fn strip(rest: &mut &str, prefix: &str) -> bool {
    match rest.strip_prefix(prefix) {
        Some(after) => {
            *rest = after.trim_start();
            true
        }
        None => false,
    }
}

/// Removes a three-letter currency code such as `AUD` from the start, or from
/// the end when `leading` is false.
fn strip_code(rest: &mut &str, leading: bool) {
    let is_code = |s: &str| s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase());
    if leading {
        if let Some(code) = rest.get(..3)
            && is_code(code)
        {
            *rest = rest[3..].trim_start();
        }
    } else if let Some((amount, code)) = rest.rsplit_once(' ')
        && is_code(code)
    {
        *rest = amount.trim_end();
    }
}

/// Rescues an answer the cap cut off. On a flat schema the field the model was
/// stuck on is left out and the finished fields are whole: 12 of 12 runaways on
/// short invoices left out the field the text lacked, with nothing broken in the
/// rest (2026-10-11). So each left-out field becomes null and is returned by
/// name. `None` when that can't be trusted: a nested schema, or no field left
/// out (the cut may then be inside a value).
pub fn fill_stuck(value: &mut Value, prepared: &Value) -> Option<Vec<String>> {
    let properties = prepared["properties"].as_object()?;
    let nullable = |p: &Value| p.get("anyOf").is_some();
    let scalar = |p: &Value| nullable(p) || p.get("enum").is_some() || p.get("const").is_some();
    if !properties.values().all(scalar) {
        return None;
    }
    let object = value.as_object_mut()?;
    let stuck: Vec<String> = properties
        .iter()
        .filter(|(name, _)| !object.contains_key(*name))
        .map(|(name, _)| name.clone())
        .collect();
    // An enum field can't be null, so a cut that left one out can't be rescued.
    if stuck.is_empty() || stuck.iter().any(|name| !nullable(&properties[name])) {
        return None;
    }
    for name in &stuck {
        object.insert(name.clone(), Value::Null);
    }
    Some(stuck)
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

/// The field's description (or its name) plus a reminder that it may be null.
/// Without it, a missing field close in meaning to other text (a PO number next
/// to the line items) took that text in 10 of 20 runs; with it, 1 of 20
/// (measured 2026-10-08, together with the stronger `extract` system prompt).
fn null_description(description: Option<&Value>, path: &[String]) -> String {
    let base = match description.and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => text.trim().trim_end_matches('.').to_owned(),
        _ => path
            .iter()
            .rev()
            .find(|part| *part != "item")
            .map_or_else(String::new, |name| name.replace('_', " ")),
    };
    format!("{base}. Null unless the text states it.")
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
    fn fill_stuck_should_null_the_fields_a_cut_answer_left_out() {
        let prepared = prepare(&json!({"type": "object", "properties": {
            "invoice": {"type": "string"}, "tracking": {"type": "string"}}}))
        .unwrap();
        let mut answer = json!({"invoice": "INV-7"});
        let stuck = fill_stuck(&mut answer, &prepared);
        assert_eq!(
            (stuck, answer),
            (
                Some(vec!["tracking".to_owned()]),
                json!({"invoice": "INV-7", "tracking": null})
            )
        );
    }

    #[test]
    fn fill_stuck_should_not_trust_a_cut_answer_with_every_field() {
        let prepared =
            prepare(&json!({"type": "object", "properties": {"invoice": {"type": "string"}}}))
                .unwrap();
        assert_eq!(
            fill_stuck(&mut json!({"invoice": "INV-7 and junk"}), &prepared),
            None
        );
    }

    #[test]
    fn fill_stuck_should_not_trust_a_nested_schema() {
        let prepared = prepare(&json!({"type": "object", "properties": {
            "invoice": {"type": "string"},
            "lines": {"type": "array", "items": {"type": "string"}}}}))
        .unwrap();
        assert_eq!(
            fill_stuck(&mut json!({"invoice": "INV-7"}), &prepared),
            None
        );
    }

    #[test]
    fn for_model_should_ask_for_number_fields_as_text() {
        let prepared = prepare(&json!({"type": "object", "properties": {
            "total": {"type": "number"},
            "lines": {"type": "array", "items": {"type": "object", "properties": {"qty": {"type": "integer"}}}},
            "vat": {"type": "number", "enum": [10, 20]}
        }}))
        .unwrap();
        let sent = for_model(&prepared);
        assert_eq!(
            (
                &sent["properties"]["total"]["anyOf"][0]["type"],
                &sent["properties"]["lines"]["items"]["properties"]["qty"]["anyOf"][0]["type"],
                &sent["properties"]["vat"]["anyOf"][0]["type"],
            ),
            (&json!("string"), &json!("string"), &json!("number"))
        );
    }

    #[test]
    fn read_numbers_should_turn_copied_amounts_into_numbers() {
        let prepared = prepare(&json!({"type": "object", "properties": {
            "total": {"type": "number"},
            "note": {"type": "string"},
            "lines": {"type": "array", "items": {"type": "object", "properties": {"qty": {"type": "integer"}}}}
        }}))
        .unwrap();
        let mut answer = json!({"total": "$1,280.50", "note": "12", "lines": [{"qty": "3"}, {"qty": "2 boxes"}]});
        read_numbers(&mut answer, &prepared);
        assert_eq!(
            answer,
            json!({"total": 1280.5, "note": "12", "lines": [{"qty": 3}, {"qty": null}]})
        );
    }

    #[test]
    fn parse_number_should_read_common_amounts() {
        let read = |text| parse_number(text, false);
        assert_eq!(
            [
                read("1280"),
                read("$1,280.00"),
                read("AUD 12,450.50"),
                read("USD 3,000"),
                read("3,000 EUR"),
                read("-$4.50"),
                read("£ 0.99"),
            ],
            [
                Some(json!(1280)),
                Some(json!(1280.0)),
                Some(json!(12450.5)),
                Some(json!(3000)),
                Some(json!(3000)),
                Some(json!(-4.5)),
                Some(json!(0.99)),
            ]
        );
    }

    #[test]
    fn parse_number_should_refuse_anything_but_one_plain_amount() {
        let refused: Vec<_> = [
            "about 300",
            "1.234,56",
            "3,00",
            "12,4567",
            "2 hours",
            "",
            "$",
            "1.2.3",
            "N/A",
        ]
        .into_iter()
        .filter(|text| parse_number(text, false).is_some())
        .collect();
        assert!(refused.is_empty(), "read as numbers: {refused:?}");
    }

    #[test]
    fn parse_number_should_keep_integers_whole() {
        assert_eq!(
            [
                parse_number("3", true),
                parse_number("3.00", true),
                parse_number("3.5", true)
            ],
            [Some(json!(3)), Some(json!(3)), None]
        );
    }

    #[test]
    fn prepare_should_make_scalars_nullable_with_unique_titles() {
        let prepared = prepare(&invoice_schema()).unwrap();
        assert_eq!(
            prepared["properties"]["invoice"],
            json!({"title": "Invoice", "description": "Invoice number. Null unless the text states it.",
                   "anyOf": [{"type": "string"}, {"type": "null"}]})
        );
    }

    #[test]
    fn prepare_should_describe_a_field_without_description_by_its_name() {
        let schema =
            json!({"type": "object", "properties": {"purchase_order": {"type": "string"}}});
        let prepared = prepare(&schema).unwrap();
        assert_eq!(
            prepared["properties"]["purchase_order"]["description"],
            json!("purchase order. Null unless the text states it.")
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
