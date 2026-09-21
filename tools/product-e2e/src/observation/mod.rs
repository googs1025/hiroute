use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PRODUCTION_EVIDENCE: &str = "installed-standalone-headless-management-loop";
pub const PRODUCTION_EVIDENCE_OWNER: &str = "TASK-142003";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioState {
    Green,
    ExpectedRed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationScenarioStateV2 {
    pub scenario_id: String,
    pub state: ScenarioState,
    pub evidence_owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationContractV2 {
    pub schema: String,
    pub scenario_id: String,
    pub process: String,
    pub proofs: BTreeSet<String>,
    pub scenario_states: Vec<ObservationScenarioStateV2>,
}

impl ObservationContractV2 {
    pub fn validate(&self) -> Result<(), ObservationContractError> {
        if self.schema != "hiroute.product-e2e.local-observation/v2"
            || self.scenario_id != "process-25018-local-observation-v2"
            || self.process != "PROCESS-25018"
            || self.proofs != required_proofs()
        {
            return Err(ObservationContractError::InvalidContract);
        }
        let local = self
            .scenario_states
            .iter()
            .find(|state| state.scenario_id == "local-writer-query-contract")
            .ok_or(ObservationContractError::MissingLocalState)?;
        if local.state != ScenarioState::Green
            || local.evidence_owner != "PROCESS-25018"
            || local.blocker.is_some()
        {
            return Err(ObservationContractError::InvalidLocalState);
        }
        let production = self
            .scenario_states
            .iter()
            .find(|state| state.scenario_id == "production-cli-daemon-observation")
            .ok_or(ObservationContractError::MissingProductionState)?;
        if production.state != ScenarioState::Green
            || production.evidence_owner != PRODUCTION_EVIDENCE_OWNER
            || production.blocker.is_some()
        {
            return Err(ObservationContractError::InvalidProductionState);
        }
        Ok(())
    }
}

fn required_proofs() -> BTreeSet<String> {
    [
        "accepted_response_attempt_frame_ref",
        "authorization_search_list_detail",
        "begin_append_finish_abort",
        "byte_bounded_failure_isolation",
        "durable_rich_ack_typed_nack_gap",
        "delete_rollups_scope_matrix",
        "exact_content_blob_transcript_identity",
        "independent_cache_affinity",
        "independent_fact_content_channels",
        PRODUCTION_EVIDENCE,
        "incremental_request_direction_fork",
        "immutable_receipt_value",
        "immutable_value_axes_signed_unknown",
        "large_stream_bounded_install",
        "retention_delete_tombstone_gc",
        "rollup_identity_plan_day_currency",
        "versioned_adapter_execution_fact_contract",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ObservationContractError {
    #[error("local observation Product E2E contract is invalid")]
    InvalidContract,
    #[error("local Writer/Query scenario state is missing")]
    MissingLocalState,
    #[error("local Writer/Query scenario is not green")]
    InvalidLocalState,
    #[error("production observation scenario state is missing")]
    MissingProductionState,
    #[error("production observation scenario is not green")]
    InvalidProductionState,
}
