use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunProcessObservationV1 {
    Running,
    Exited { code: Option<i32> },
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStopScopeV1 {
    Root,
    ProcessGroup,
    Job,
}

/// Only actual launcher facts, not authorization or proof about arbitrary descendants.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunStopEvidenceV1 {
    pub scope: RunStopScopeV1,
    pub observation: RunProcessObservationV1,
    pub scope_stopped: bool,
    pub residual_unknown: bool,
}

impl RunStopEvidenceV1 {
    pub fn valid(&self) -> bool {
        !self.scope_stopped || matches!(self.observation, RunProcessObservationV1::Exited { .. })
    }
}
