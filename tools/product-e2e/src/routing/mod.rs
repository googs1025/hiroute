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
pub struct RoutingScenarioStateV1 {
    pub scenario_id: String,
    pub state: ScenarioState,
    pub evidence_owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPlanContractV1 {
    pub schema: String,
    pub scenario_id: String,
    pub process: String,
    pub proofs: BTreeSet<String>,
    pub scenario_states: Vec<RoutingScenarioStateV1>,
}

impl RoutingPlanContractV1 {
    pub fn validate(&self) -> Result<(), RoutingContractError> {
        if self.schema != "hiroute.product-e2e.routing-plans/v1"
            || self.scenario_id != "process-25004-routing-plans"
            || self.process != "PROCESS-25004"
            || self.proofs
                != BTreeSet::from([
                    "alias_tombstone".to_owned(),
                    "authority_catalog_grant_digest".to_owned(),
                    "closed_secret_free_publication".to_owned(),
                    "custom_explicit_order".to_owned(),
                    "deterministic_compiler_digest".to_owned(),
                    "exact_candidate_endpoint_adapter_credential_ref".to_owned(),
                    "exact_native_reasoning".to_owned(),
                    "free_first_two_policies".to_owned(),
                    "g0_direct_projection".to_owned(),
                    "grant_generation_and_verifier".to_owned(),
                    PRODUCTION_EVIDENCE.to_owned(),
                    "multi_connection_shared_plan_grant".to_owned(),
                    "prepared_activate_lkg".to_owned(),
                    "request_attempt_scope".to_owned(),
                    "smart_saving_two_branches".to_owned(),
                ])
        {
            return Err(RoutingContractError::InvalidContract);
        }
        let compiler = self
            .scenario_states
            .iter()
            .find(|value| value.scenario_id == "compiler-contract")
            .ok_or(RoutingContractError::MissingCompilerState)?;
        if compiler.state != ScenarioState::Green
            || compiler.evidence_owner != "PROCESS-25004"
            || compiler.blocker.is_some()
        {
            return Err(RoutingContractError::InvalidCompilerState);
        }
        let repair = self
            .scenario_states
            .iter()
            .find(|value| value.scenario_id == "publication-closure-repair")
            .ok_or(RoutingContractError::MissingRepairState)?;
        if repair.state != ScenarioState::Green
            || repair.evidence_owner != "PROCESS-25012"
            || repair.blocker.is_some()
        {
            return Err(RoutingContractError::InvalidRepairState);
        }
        let product = self
            .scenario_states
            .iter()
            .find(|value| value.scenario_id == "production-cli-daemon-routing")
            .ok_or(RoutingContractError::MissingProductState)?;
        if product.state != ScenarioState::Green
            || product.evidence_owner != PRODUCTION_EVIDENCE_OWNER
            || product.blocker.is_some()
        {
            return Err(RoutingContractError::InvalidProductState);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RoutingContractError {
    #[error("routing Product E2E contract is invalid")]
    InvalidContract,
    #[error("compiler scenario state is missing")]
    MissingCompilerState,
    #[error("compiler scenario state is not exact")]
    InvalidCompilerState,
    #[error("publication closure repair state is missing")]
    MissingRepairState,
    #[error("publication closure repair state is not exact")]
    InvalidRepairState,
    #[error("production routing scenario state is missing")]
    MissingProductState,
    #[error("production routing scenario is not green")]
    InvalidProductState,
}
