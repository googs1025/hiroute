//! Share immutable Operation inputs in memory; retain the exact existing serialized value.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;

pub(crate) fn serialize<T: Serialize, S: Serializer>(
    value: &Arc<T>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value.as_ref().serialize(serializer)
}

pub(super) fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Arc<T>, D::Error> {
    T::deserialize(deserializer).map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[derive(Clone, Deserialize, Serialize)]
    struct Input {
        #[serde(with = "super")]
        content: Arc<Value>,
    }

    #[test]
    fn sharing_preserves_wire_shape_and_mutation_cannot_change_another_owner() {
        let input = Input {
            content: Arc::new(json!({"before": [1, 2, 3]})),
        };
        let mut copy = input.clone();
        assert!(Arc::ptr_eq(&input.content, &copy.content));
        let encoded = serde_json::to_value(&input).unwrap();
        assert_eq!(encoded, json!({"content": {"before": [1, 2, 3]}}));
        Arc::make_mut(&mut copy.content)["before"][0] = json!(9);
        assert_eq!(input.content["before"][0], 1);
        assert_eq!(copy.content["before"][0], 9);
        let decoded: Input = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.content, input.content);
        assert!(!Arc::ptr_eq(&decoded.content, &input.content));
    }
}
