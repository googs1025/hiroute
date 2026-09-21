use hiroute_domain::{
    AgentConfigChangeV1, AgentConfigDocumentV1, AgentConfigRestorePointV1, CanonicalDigest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq)]
pub struct AppliedAgentConfigV1 {
    pub document: AgentConfigDocumentV1,
    pub restore_point: AgentConfigRestorePointV1,
}

pub fn apply_agent_config_change(
    profile_id: &str,
    current: &AgentConfigDocumentV1,
    change: &AgentConfigChangeV1,
) -> Result<AppliedAgentConfigV1, AgentConfigMutationError> {
    change
        .validate()
        .map_err(|_| AgentConfigMutationError::InvalidContract)?;
    preflight(current, change, MutationDirectionV1::Apply)?;
    let mut document = current.clone();
    for field in &change.fields {
        set_optional(&mut document, &field.path, field.after.clone());
    }
    Ok(AppliedAgentConfigV1 {
        document,
        restore_point: AgentConfigRestorePointV1::from_change(profile_id, change.clone())
            .map_err(|_| AgentConfigMutationError::InvalidContract)?,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum RestoreAgentConfigOutcomeV1 {
    Restored(AgentConfigDocumentV1),
    ReportOnlyUnknownVersion,
}

pub fn restore_agent_config(
    current: &AgentConfigDocumentV1,
    restore_point: &AgentConfigRestorePointV1,
) -> Result<RestoreAgentConfigOutcomeV1, AgentConfigMutationError> {
    if restore_point.schema != hiroute_domain::AGENT_CONFIG_RESTORE_SCHEMA_V1 {
        return Ok(RestoreAgentConfigOutcomeV1::ReportOnlyUnknownVersion);
    }
    restore_point
        .validate()
        .map_err(|_| AgentConfigMutationError::InvalidContract)?;
    preflight(current, &restore_point.change, MutationDirectionV1::Restore)?;
    let mut document = current.clone();
    for field in &restore_point.change.fields {
        set_optional(&mut document, &field.path, field.before.clone());
    }
    Ok(RestoreAgentConfigOutcomeV1::Restored(document))
}

pub fn encode_agent_config_restore_point(
    restore_point: &AgentConfigRestorePointV1,
) -> Result<Vec<u8>, AgentConfigMutationError> {
    restore_point
        .validate()
        .map_err(|_| AgentConfigMutationError::InvalidContract)?;
    serde_json::to_vec(restore_point).map_err(|_| AgentConfigMutationError::Encoding)
}

pub fn decode_agent_config_restore_point(
    bytes: &[u8],
) -> Result<AgentConfigRestorePointV1, AgentConfigMutationError> {
    let restore_point: AgentConfigRestorePointV1 =
        serde_json::from_slice(bytes).map_err(|_| AgentConfigMutationError::InvalidContract)?;
    restore_point
        .validate()
        .map_err(|_| AgentConfigMutationError::InvalidContract)?;
    let canonical =
        serde_json::to_vec(&restore_point).map_err(|_| AgentConfigMutationError::Encoding)?;
    if canonical != bytes {
        return Err(AgentConfigMutationError::NonCanonical);
    }
    Ok(restore_point)
}

#[derive(Clone, Copy)]
enum MutationDirectionV1 {
    Apply,
    Restore,
}

fn preflight(
    current: &AgentConfigDocumentV1,
    change: &AgentConfigChangeV1,
    direction: MutationDirectionV1,
) -> Result<(), AgentConfigMutationError> {
    for field in &change.fields {
        let current_digest = digest_optional(current.fields.get(&field.path))?;
        let expected = match direction {
            MutationDirectionV1::Apply => &field.before_digest,
            MutationDirectionV1::Restore => &field.after_digest,
        };
        if &current_digest != expected {
            return Err(AgentConfigMutationError::OwnedFieldConflict {
                path: field.path.clone(),
            });
        }
    }
    Ok(())
}

fn set_optional(document: &mut AgentConfigDocumentV1, path: &str, value: Option<Value>) {
    if let Some(value) = value {
        document.fields.insert(path.to_owned(), value);
    } else {
        document.fields.remove(path);
    }
}

fn digest_optional(value: Option<&Value>) -> Result<CanonicalDigest, AgentConfigMutationError> {
    CanonicalDigest::of(&value).map_err(|_| AgentConfigMutationError::Encoding)
}

#[derive(Clone, Debug, Error, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentConfigMutationError {
    #[error("Agent config change or restore point is invalid")]
    InvalidContract,
    #[error("owned Agent config field changed concurrently: {path}")]
    OwnedFieldConflict { path: String },
    #[error("Agent config restore point encoding failed")]
    Encoding,
    #[error("Agent config restore point is not canonical")]
    NonCanonical,
}
