use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    let mut output = Vec::new();
    write_value(value, &mut output);
    output
}

pub fn canonical_json_digest(value: &Value) -> String {
    sha256_hex(&canonical_json_bytes(value))
}

pub fn canonical_json_value(value: &Value) -> Value {
    serde_json::from_slice(&canonical_json_bytes(value))
        .expect("canonical JSON bytes must deserialize")
}

fn write_value(value: &Value, output: &mut Vec<u8>) {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        Value::String(value) => {
            serde_json::to_writer(output, value).expect("writing JSON to Vec cannot fail")
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_value(value, output);
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key).expect("writing JSON to Vec cannot fail");
                output.push(b':');
                write_value(value, output);
            }
            output.push(b'}');
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_objects_ignore_source_key_order_but_not_array_order() {
        let left = json!({"b": 2, "a": [1, 2]});
        let right = json!({"a": [1, 2], "b": 2});
        let reordered = json!({"a": [2, 1], "b": 2});
        assert_eq!(canonical_json_digest(&left), canonical_json_digest(&right));
        assert_ne!(
            canonical_json_digest(&left),
            canonical_json_digest(&reordered)
        );
        assert_eq!(
            canonical_json_value(&left)
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }
}
