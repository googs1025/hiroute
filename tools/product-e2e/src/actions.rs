use std::collections::BTreeSet;

use hiroute_application_api::{CanonicalDigest, SchemaVersion, command_by_id};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionKind {
    FormalDaemonStarted,
    RealCliUsed,
    LocalControlBoundaryUsed,
    DescriptorDigestExact,
    VersionMismatchFailsClosed,
    PreviewHasZeroSideEffects,
    ApplyRejectionHasZeroSideEffects,
    ReservedOperationFailsClosed,
    NativePayloadExact,
    GoldenExact,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidencePolarity {
    Positive,
    Negative,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonRole {
    All,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityCase {
    Missing,
    Wrong,
    Revoked,
    Valid,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOperationClass {
    Query,
    Command,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonFailpoint {
    Journal,
    Bridge,
    Publication,
    AgentWrite,
    Rollback,
    ProbeLease,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ProductActionV1 {
    StartFormalDaemon {
        role: DaemonRole,
        proof: ScenarioProofV1,
    },
    RunCli {
        command_id: String,
        polarity: EvidencePolarity,
        fixture_ref: String,
        proof: ScenarioProofV1,
    },
    ControlApiProbe {
        principal: String,
        capability: CapabilityCase,
        operation: ControlOperationClass,
        proof: ScenarioProofV1,
    },
    SnapshotSideEffects {
        label: String,
        supports: Vec<AssertionKind>,
    },
    AssertNoSideEffects {
        before_label: String,
        after_label: String,
        proof: ScenarioProofV1,
    },
    InjectDaemonFault {
        fault: DaemonFailpoint,
        supports: Vec<AssertionKind>,
    },
    RestartFormalDaemon {
        role: DaemonRole,
        proof: ScenarioProofV1,
    },
}

impl ProductActionV1 {
    fn validate(&self) -> Result<(), ScenarioContractError> {
        match self {
            Self::RunCli {
                command_id,
                fixture_ref,
                proof,
                ..
            } => {
                if command_by_id(command_id).is_none() {
                    return Err(ScenarioContractError::UnknownCommand(command_id.clone()));
                }
                if fixture_ref.is_empty() || fixture_ref.contains('/') || fixture_ref.contains('\\')
                {
                    return Err(ScenarioContractError::InvalidFixtureRef(
                        fixture_ref.clone(),
                    ));
                }
                proof.validate()
            }
            Self::StartFormalDaemon { proof, .. }
            | Self::ControlApiProbe { proof, .. }
            | Self::AssertNoSideEffects { proof, .. }
            | Self::RestartFormalDaemon { proof, .. } => proof.validate(),
            Self::SnapshotSideEffects { label, .. } => validate_label(label),
            Self::InjectDaemonFault { .. } => Ok(()),
        }
    }
}

fn validate_label(label: &str) -> Result<(), ScenarioContractError> {
    if label.is_empty()
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Err(ScenarioContractError::InvalidLabel(label.to_owned()))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioProofV1 {
    pub proves: Vec<AssertionKind>,
    pub expected_evidence_digest: CanonicalDigest,
}

impl ScenarioProofV1 {
    fn validate(&self) -> Result<(), ScenarioContractError> {
        if self.proves.is_empty() {
            Err(ScenarioContractError::MissingTypedAssertion)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductScenarioV1 {
    pub schema_version: SchemaVersion,
    pub scenario_id: String,
    pub descriptor_digest: CanonicalDigest,
    pub covers: BTreeSet<AssertionKind>,
    pub actions: Vec<ProductActionV1>,
}

impl ProductScenarioV1 {
    pub fn validate(&self) -> Result<(), ScenarioContractError> {
        if self.schema_version.major != 1 {
            return Err(ScenarioContractError::UnknownMajor(
                self.schema_version.major,
            ));
        }
        validate_label(&self.scenario_id)?;
        if self.actions.is_empty() {
            return Err(ScenarioContractError::NoActions);
        }
        for action in &self.actions {
            action.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ScenarioContractError {
    #[error("unknown product action schema major {0}")]
    UnknownMajor(u16),
    #[error("scenario contains no action")]
    NoActions,
    #[error("concluding action has no exact typed assertion")]
    MissingTypedAssertion,
    #[error("unknown command_id {0}")]
    UnknownCommand(String),
    #[error("fixture_ref must be a registered symbolic identifier: {0}")]
    InvalidFixtureRef(String),
    #[error("invalid stable label {0}")]
    InvalidLabel(String),
}
