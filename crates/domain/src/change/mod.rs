use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    CHANGE_SPEC_SCHEMA_V1, CanonicalDigest, CanonicalDigestError, ChangeSpecV1, RevisionSetV1,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NormalizedChangeV1 {
    pub spec: ChangeSpecV1,
    pub revisions: RevisionSetV1,
    pub digest: CanonicalDigest,
}

pub fn normalize_change(
    mut spec: ChangeSpecV1,
    revisions: RevisionSetV1,
) -> Result<NormalizedChangeV1, ChangeValidationError> {
    if spec.schema_version.major != CHANGE_SPEC_SCHEMA_V1.major {
        return Err(ChangeValidationError::UnknownMajor(
            spec.schema_version.major,
        ));
    }
    if spec.command_id.is_empty() || spec.command_id.len() > 128 {
        return Err(ChangeValidationError::InvalidCommandId);
    }
    if spec
        .resource_id
        .as_deref()
        .is_some_and(|value| !valid_resource_identifier(value))
    {
        return Err(ChangeValidationError::InvalidResourceId);
    }
    spec.desired_state = canonicalize_json(spec.desired_state);
    let digest = spec.canonical_digest(&revisions)?;
    Ok(NormalizedChangeV1 {
        spec,
        revisions,
        digest,
    })
}

fn valid_resource_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains("//")
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b':' | b'-')
        })
}

pub fn canonicalize_json(mut value: Value) -> Value {
    // Sort in place with preserve_order; without it every object is already a BTreeMap.
    // Arrays retain their order, including the frozen candidate and credential sequences.
    value.sort_all_objects();
    value
}

#[derive(Debug, Error)]
pub enum ChangeValidationError {
    #[error("unknown ChangeSpec major {0}")]
    UnknownMajor(u16),
    #[error("command_id must be a bounded non-empty identifier")]
    InvalidCommandId,
    #[error("resource_id must be a bounded portable identifier")]
    InvalidResourceId,
    #[error(transparent)]
    Digest(#[from] CanonicalDigestError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::SchemaVersion;

    #[test]
    fn normalization_preserves_arrays_but_orders_nested_objects() {
        let normalized = normalize_change(
            ChangeSpecV1 {
                schema_version: SchemaVersion::new(1, 0),
                command_id: "settings.apply".to_owned(),
                resource_id: None,
                desired_state: json!({"z": [{"b": 2, "a": 1}], "a": true}),
            },
            RevisionSetV1 {
                target: 0,
                dependencies: BTreeMap::new(),
            },
        )
        .unwrap();
        assert_eq!(
            normalized.spec.desired_state,
            json!({"a": true, "z": [{"a": 1, "b": 2}]})
        );
    }
}
