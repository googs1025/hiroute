use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::CanonicalDigest;

pub const AGENT_CONFIG_CHANGE_SCHEMA_V1: &str = "hiroute.agent-config-change/v1";
pub const AGENT_CONFIG_RESTORE_SCHEMA_V1: &str = "hiroute.agent-config-restore/v1";

/// A flattened semantic view. Adapters map exact registered paths to their native file format.
/// Unlisted paths are always preserved.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfigDocumentV1 {
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
}

impl AgentConfigDocumentV1 {
    pub fn fingerprint(&self) -> Result<CanonicalDigest, AgentConfigError> {
        CanonicalDigest::of(self).map_err(|_| AgentConfigError::Encoding)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfigFieldChangeV1 {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
    pub before_digest: CanonicalDigest,
    pub after_digest: CanonicalDigest,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfigChangeV1 {
    pub schema: String,
    pub before_document_digest: CanonicalDigest,
    pub fields: Vec<AgentConfigFieldChangeV1>,
    pub digest: CanonicalDigest,
}

impl AgentConfigChangeV1 {
    pub fn preview(
        current: &AgentConfigDocumentV1,
        desired_owned_fields: BTreeMap<String, Option<Value>>,
    ) -> Result<Self, AgentConfigError> {
        let mut fields = Vec::new();
        for (path, after) in desired_owned_fields {
            validate_path(&path)?;
            if let Some(after) = &after {
                validate_value(after)?;
            }
            let before = current.fields.get(&path).cloned();
            if before == after {
                continue;
            }
            fields.push(AgentConfigFieldChangeV1 {
                path,
                before_digest: digest_optional(before.as_ref())?,
                after_digest: digest_optional(after.as_ref())?,
                before,
                after,
            });
        }
        let before_document_digest = current.fingerprint()?;
        let body = ChangeBodyV1 {
            schema: AGENT_CONFIG_CHANGE_SCHEMA_V1,
            before_document_digest: &before_document_digest,
            fields: &fields,
        };
        let digest = CanonicalDigest::of(&body).map_err(|_| AgentConfigError::Encoding)?;
        Ok(Self {
            schema: AGENT_CONFIG_CHANGE_SCHEMA_V1.to_owned(),
            before_document_digest,
            fields,
            digest,
        })
    }

    pub fn validate(&self) -> Result<(), AgentConfigError> {
        if self.schema != AGENT_CONFIG_CHANGE_SCHEMA_V1 {
            return Err(AgentConfigError::UnsupportedSchema);
        }
        CanonicalDigest::parse(self.before_document_digest.as_str().to_owned())
            .map_err(|_| AgentConfigError::InvalidChange)?;
        let mut previous = None;
        for field in &self.fields {
            validate_path(&field.path)?;
            if let Some(after) = &field.after {
                validate_value(after)?;
            }
            if previous.is_some_and(|value: &str| value >= field.path.as_str())
                || CanonicalDigest::parse(field.before_digest.as_str().to_owned()).is_err()
                || CanonicalDigest::parse(field.after_digest.as_str().to_owned()).is_err()
                || digest_optional(field.before.as_ref())? != field.before_digest
                || digest_optional(field.after.as_ref())? != field.after_digest
            {
                return Err(AgentConfigError::InvalidChange);
            }
            previous = Some(field.path.as_str());
        }
        let body = ChangeBodyV1 {
            schema: AGENT_CONFIG_CHANGE_SCHEMA_V1,
            before_document_digest: &self.before_document_digest,
            fields: &self.fields,
        };
        if CanonicalDigest::of(&body).map_err(|_| AgentConfigError::Encoding)? != self.digest {
            return Err(AgentConfigError::DigestMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfigRestorePointV1 {
    pub schema: String,
    pub profile_id: String,
    pub change: AgentConfigChangeV1,
    pub digest: CanonicalDigest,
}

impl AgentConfigRestorePointV1 {
    pub fn from_change(
        profile_id: impl Into<String>,
        change: AgentConfigChangeV1,
    ) -> Result<Self, AgentConfigError> {
        change.validate()?;
        let profile_id = profile_id.into();
        if !valid_profile_id(&profile_id) {
            return Err(AgentConfigError::InvalidProfile);
        }
        let body = RestoreBodyV1 {
            schema: AGENT_CONFIG_RESTORE_SCHEMA_V1,
            profile_id: &profile_id,
            change: &change,
        };
        let digest = CanonicalDigest::of(&body).map_err(|_| AgentConfigError::Encoding)?;
        Ok(Self {
            schema: AGENT_CONFIG_RESTORE_SCHEMA_V1.to_owned(),
            profile_id,
            change,
            digest,
        })
    }

    pub fn validate(&self) -> Result<(), AgentConfigError> {
        if self.schema != AGENT_CONFIG_RESTORE_SCHEMA_V1 {
            return Err(AgentConfigError::UnsupportedSchema);
        }
        if !valid_profile_id(&self.profile_id) {
            return Err(AgentConfigError::InvalidProfile);
        }
        self.change.validate()?;
        let body = RestoreBodyV1 {
            schema: AGENT_CONFIG_RESTORE_SCHEMA_V1,
            profile_id: &self.profile_id,
            change: &self.change,
        };
        if CanonicalDigest::of(&body).map_err(|_| AgentConfigError::Encoding)? != self.digest {
            return Err(AgentConfigError::DigestMismatch);
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct RestoreBodyV1<'a> {
    schema: &'static str,
    profile_id: &'a str,
    change: &'a AgentConfigChangeV1,
}

#[derive(Serialize)]
struct ChangeBodyV1<'a> {
    schema: &'static str,
    before_document_digest: &'a CanonicalDigest,
    fields: &'a [AgentConfigFieldChangeV1],
}

fn digest_optional(value: Option<&Value>) -> Result<CanonicalDigest, AgentConfigError> {
    CanonicalDigest::of(&value).map_err(|_| AgentConfigError::Encoding)
}

fn validate_path(value: &str) -> Result<(), AgentConfigError> {
    if value.is_empty()
        || value.len() > 256
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Err(AgentConfigError::InvalidPath)
    } else {
        Ok(())
    }
}

fn valid_profile_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.contains("..")
        && !value.contains("//")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

fn validate_value(value: &Value) -> Result<(), AgentConfigError> {
    if serde_json::to_vec(value)
        .map_err(|_| AgentConfigError::Encoding)?
        .len()
        > 64 * 1024
        || contains_nul(value)
    {
        Err(AgentConfigError::InvalidValue)
    } else {
        Ok(())
    }
}

fn contains_nul(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_nul),
        Value::Object(values) => values.values().any(contains_nul),
        Value::String(value) => value.contains('\0'),
        _ => false,
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AgentConfigError {
    #[error("Agent config change schema is unsupported")]
    UnsupportedSchema,
    #[error("Agent config path is invalid")]
    InvalidPath,
    #[error("Agent config value is invalid")]
    InvalidValue,
    #[error("Agent config change has no owned field inputs")]
    EmptyChange,
    #[error("Agent config change is not ordered or fingerprint-closed")]
    InvalidChange,
    #[error("Agent config change digest is invalid")]
    DigestMismatch,
    #[error("Agent config value cannot be encoded")]
    Encoding,
    #[error("Agent config restore profile is invalid")]
    InvalidProfile,
}
