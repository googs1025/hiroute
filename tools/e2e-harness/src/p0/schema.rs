//! Small strict evaluator for the JSON-Schema subset used by the frozen P0
//! artifacts. Unsupported assertion keywords are rejected instead of being
//! silently ignored.

use std::collections::BTreeSet;

use serde_json::Value;

use super::canonical::canonical_json_bytes;

pub fn validate_document(schema: &Value, instance: &Value) -> Result<(), String> {
    validate_node(schema, instance, "$", "$schema")
}

fn validate_node(
    schema: &Value,
    instance: &Value,
    instance_path: &str,
    schema_path: &str,
) -> Result<(), String> {
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{schema_path} must be an object"))?;
    reject_unknown_keywords(object.keys().map(String::as_str), schema_path)?;

    if let Some(variants) = object.get("anyOf").and_then(Value::as_array)
        && !variants.iter().enumerate().any(|(index, variant)| {
            validate_node(
                variant,
                instance,
                instance_path,
                &format!("{schema_path}/anyOf/{index}"),
            )
            .is_ok()
        })
    {
        return Err(format!(
            "{instance_path} matches no {schema_path}.anyOf variant"
        ));
    }
    if let Some(variants) = object.get("oneOf").and_then(Value::as_array) {
        let matches = variants
            .iter()
            .enumerate()
            .filter(|(index, variant)| {
                validate_node(
                    variant,
                    instance,
                    instance_path,
                    &format!("{schema_path}/oneOf/{index}"),
                )
                .is_ok()
            })
            .count();
        if matches != 1 {
            return Err(format!(
                "{instance_path} matches {matches} {schema_path}.oneOf variants"
            ));
        }
    }
    if let Some(rejected) = object.get("not")
        && validate_node(
            rejected,
            instance,
            instance_path,
            &format!("{schema_path}/not"),
        )
        .is_ok()
    {
        return Err(format!("{instance_path} matches {schema_path}.not"));
    }

    if let Some(expected) = object.get("const")
        && expected != instance
    {
        return Err(format!(
            "{instance_path} does not equal {schema_path}.const"
        ));
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array)
        && !values.iter().any(|value| value == instance)
    {
        return Err(format!("{instance_path} is not in {schema_path}.enum"));
    }
    if let Some(kind) = object.get("type").and_then(Value::as_str) {
        validate_type(kind, instance, instance_path)?;
    }

    if let Some(value) = instance.as_object() {
        validate_object(object, value, instance_path, schema_path)?;
    }
    if let Some(value) = instance.as_array() {
        validate_array(object, value, instance_path, schema_path)?;
    }
    if let Some(value) = instance.as_str() {
        if let Some(minimum) = object.get("minLength").and_then(Value::as_u64)
            && value.chars().count() < minimum as usize
        {
            return Err(format!(
                "{instance_path} is shorter than {minimum} characters"
            ));
        }
        if let Some(maximum) = object.get("maxLength").and_then(Value::as_u64)
            && value.chars().count() > maximum as usize
        {
            return Err(format!(
                "{instance_path} is longer than {maximum} characters"
            ));
        }
        if let Some(pattern) = object.get("pattern").and_then(Value::as_str)
            && !matches_frozen_pattern(pattern, value)?
        {
            return Err(format!("{instance_path} does not match {pattern}"));
        }
    }
    if let Some(value) = instance.as_i64() {
        if let Some(minimum) = object.get("minimum").and_then(Value::as_i64)
            && value < minimum
        {
            return Err(format!("{instance_path} is below minimum {minimum}"));
        }
        if let Some(maximum) = object.get("maximum").and_then(Value::as_i64)
            && value > maximum
        {
            return Err(format!("{instance_path} is above maximum {maximum}"));
        }
    }
    Ok(())
}

fn matches_frozen_pattern(pattern: &str, value: &str) -> Result<bool, String> {
    let exact_hex_suffix = |prefix: &str| {
        value.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.len() == 64
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    };
    match pattern {
        "^sha256:[0-9a-f]{64}$" => Ok(exact_hex_suffix("sha256:")),
        "^fact-producer-[0-9a-f]{64}$" => Ok(exact_hex_suffix("fact-producer-")),
        "^fact-epoch-[0-9a-f]{64}$" => Ok(exact_hex_suffix("fact-epoch-")),
        "^fact-event-[0-9a-f]{64}$" => Ok(exact_hex_suffix("fact-event-")),
        "^fact-stream-[0-9a-f]{64}$" => Ok(exact_hex_suffix("fact-stream-")),
        "^content-producer-[0-9a-f]{64}$" => Ok(exact_hex_suffix("content-producer-")),
        "^content-epoch-[0-9a-f]{64}$" => Ok(exact_hex_suffix("content-epoch-")),
        "^content-stream-[0-9a-f]{64}$" => Ok(exact_hex_suffix("content-stream-")),
        "^content-event-[1-4]-[0-9a-f]{64}$" => Ok(['1', '2', '3', '4']
            .into_iter()
            .any(|ordinal| exact_hex_suffix(&format!("content-event-{ordinal}-")))),
        _ => Err(format!("unsupported frozen schema pattern {pattern}")),
    }
}

fn validate_object(
    schema: &serde_json::Map<String, Value>,
    instance: &serde_json::Map<String, Value>,
    instance_path: &str,
    schema_path: &str,
) -> Result<(), String> {
    if let Some(minimum) = schema.get("minProperties").and_then(Value::as_u64)
        && instance.len() < minimum as usize
    {
        return Err(format!(
            "{instance_path} has fewer than {minimum} properties"
        ));
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !instance.contains_key(name) {
                return Err(format!(
                    "{instance_path} is missing required property {name}"
                ));
            }
        }
    }
    let properties = schema.get("properties").and_then(Value::as_object);
    for (name, value) in instance {
        if let Some(property_schema) = properties.and_then(|items| items.get(name)) {
            validate_node(
                property_schema,
                value,
                &format!("{instance_path}/{}", escape_pointer(name)),
                &format!("{schema_path}/properties/{}", escape_pointer(name)),
            )?;
        } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            return Err(format!("{instance_path} has unknown property {name}"));
        }
    }
    Ok(())
}

fn validate_array(
    schema: &serde_json::Map<String, Value>,
    instance: &[Value],
    instance_path: &str,
    schema_path: &str,
) -> Result<(), String> {
    if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64)
        && instance.len() < minimum as usize
    {
        return Err(format!("{instance_path} has fewer than {minimum} items"));
    }
    if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64)
        && instance.len() > maximum as usize
    {
        return Err(format!("{instance_path} has more than {maximum} items"));
    }
    if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
        let mut seen = BTreeSet::new();
        for value in instance {
            if !seen.insert(canonical_json_bytes(value)) {
                return Err(format!("{instance_path} contains a duplicate item"));
            }
        }
    }
    if let Some(item_schema) = schema.get("items") {
        for (index, value) in instance.iter().enumerate() {
            validate_node(
                item_schema,
                value,
                &format!("{instance_path}/{index}"),
                &format!("{schema_path}/items"),
            )?;
        }
    }
    Ok(())
}

fn validate_type(kind: &str, value: &Value, path: &str) -> Result<(), String> {
    let matches = match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        other => return Err(format!("unsupported schema type {other}")),
    };
    if matches {
        Ok(())
    } else {
        Err(format!("{path} is not a {kind}"))
    }
}

fn reject_unknown_keywords<'a>(
    keys: impl Iterator<Item = &'a str>,
    path: &str,
) -> Result<(), String> {
    const SUPPORTED: &[&str] = &[
        "$id",
        "$schema",
        "additionalProperties",
        "anyOf",
        "const",
        "description",
        "enum",
        "items",
        "maximum",
        "maxItems",
        "maxLength",
        "minimum",
        "minItems",
        "minLength",
        "minProperties",
        "not",
        "properties",
        "oneOf",
        "pattern",
        "required",
        "title",
        "type",
        "uniqueItems",
    ];
    for key in keys {
        if !SUPPORTED.contains(&key) {
            return Err(format!("{path} uses unsupported schema keyword {key}"));
        }
    }
    Ok(())
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rejects_unknown_properties_and_missing_required_values() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["id"],
            "properties": {"id": {"type": "string", "minLength": 1}}
        });
        assert!(validate_document(&schema, &json!({"id": "case"})).is_ok());
        assert!(validate_document(&schema, &json!({})).is_err());
        assert!(validate_document(&schema, &json!({"id": "case", "pass": true})).is_err());
    }

    #[test]
    fn supports_not_for_property_absence_discriminants() {
        let schema = json!({
            "type": "object",
            "not": {"required": ["pricing"]}
        });
        assert!(validate_document(&schema, &json!({"kind": "route_decision"})).is_ok());
        assert!(validate_document(&schema, &json!({"pricing": {}})).is_err());
    }
}
