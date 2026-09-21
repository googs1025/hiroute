use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::PlannerError;

pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, PlannerError> {
    let value = serde_json::to_value(value).map_err(PlannerError::Serialization)?;
    let mut output = Vec::new();
    write_value(&value, &mut output)?;
    Ok(output)
}

pub fn canonical_digest<T: Serialize>(value: &T) -> Result<String, PlannerError> {
    let bytes = canonical_bytes(value)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn write_value(value: &Value, output: &mut Vec<u8>) -> Result<(), PlannerError> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Number(number) => output.extend_from_slice(number.to_string().as_bytes()),
        Value::String(text) => {
            serde_json::to_writer(output, text).map_err(PlannerError::Serialization)?
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_value(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut keys = values.keys().collect::<Vec<_>>();
            // RFC 8785 orders object properties by raw UTF-16 code units.
            keys.sort_unstable_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key).map_err(PlannerError::Serialization)?;
                output.push(b':');
                write_value(&values[key], output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}
