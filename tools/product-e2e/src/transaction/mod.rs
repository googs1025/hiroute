use std::collections::BTreeSet;

use hiroute_domain::{OperationState, SchemaVersion};
use hiroute_product_e2e::FaultLedgerV1;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const FORMAL_COMPOSITION_BLOCKER: &str = "formal composition not yet available";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExpectedRedV1 {
    pub expected_red: bool,
    pub reason: String,
    pub composition_owner: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionRecoveryContractV1 {
    pub schema_version: SchemaVersion,
    pub scenario_id: String,
    pub boundary: String,
    pub formal_composition: ExpectedRedV1,
    pub operation_states: Vec<OperationState>,
    pub fault_ledger: FaultLedgerV1,
    pub required_assertions: BTreeSet<String>,
}

impl TransactionRecoveryContractV1 {
    pub fn validate(&self) -> Result<(), TransactionContractError> {
        if self.schema_version.major != 1 {
            return Err(TransactionContractError::UnknownMajor(
                self.schema_version.major,
            ));
        }
        if self.scenario_id != "transaction.recovery.contract" {
            return Err(TransactionContractError::ScenarioId);
        }
        if self.boundary != "black_box_contract_scenario_ledger_oracle" {
            return Err(TransactionContractError::Boundary);
        }
        if !self.formal_composition.expected_red
            || self.formal_composition.reason != FORMAL_COMPOSITION_BLOCKER
            || self.formal_composition.composition_owner != "PROCESS-25009"
        {
            return Err(TransactionContractError::ExpectedRed);
        }
        if self.operation_states != exact_operation_states() {
            return Err(TransactionContractError::OperationSequence);
        }
        if !self.fault_ledger.exact() || self.fault_ledger.injected != exact_faults() {
            return Err(TransactionContractError::FaultLedger);
        }
        let required = BTreeSet::from([
            "apply_checks_idempotency_before_revision".to_owned(),
            "digest_and_revision_rejections_have_zero_side_effects".to_owned(),
            "operation_terminal_state_is_unique".to_owned(),
            "owned_effect_compensation_is_conditional".to_owned(),
            "preview_has_zero_writes".to_owned(),
            "secret_plaintext_absent_from_durable_evidence".to_owned(),
        ]);
        if self.required_assertions != required {
            return Err(TransactionContractError::Assertions);
        }
        Ok(())
    }
}

pub fn exact_operation_states() -> Vec<OperationState> {
    vec![
        OperationState::Accepted,
        OperationState::Preparing,
        OperationState::ApplyingSecrets,
        OperationState::MaterializingSources,
        OperationState::CompilingPublication,
        OperationState::ApplyingAgentArtifacts,
        OperationState::Activating,
        OperationState::Succeeded,
        OperationState::RollingBack,
        OperationState::RolledBack,
        OperationState::NeedsAttention,
    ]
}

pub fn exact_faults() -> Vec<String> {
    [
        "prepare",
        "apply_secrets",
        "materialize_sources",
        "compile_publication",
        "apply_agent_artifacts",
        "activate",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TransactionContractError {
    #[error("unknown transaction scenario schema major {0}")]
    UnknownMajor(u16),
    #[error("transaction scenario id is not canonical")]
    ScenarioId,
    #[error("transaction scenario crosses the frozen dependency boundary")]
    Boundary,
    #[error("formal composition expected-red attribution is not exact")]
    ExpectedRed,
    #[error("public Operation state sequence drifted")]
    OperationSequence,
    #[error("fault ledger is incomplete or reordered")]
    FaultLedger,
    #[error("transaction scenario omits a required black-box assertion")]
    Assertions,
}
