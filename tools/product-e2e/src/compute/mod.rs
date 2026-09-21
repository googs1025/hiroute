use std::collections::BTreeSet;

use serde::Deserialize;
use thiserror::Error;

pub const PRODUCTION_EVIDENCE: &str = "installed-standalone-headless-management-loop";
pub const PRODUCTION_EVIDENCE_OWNER: &str = "TASK-142003";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputePoolContractV1 {
    pub schema: String,
    pub scenario_id: String,
    pub process: String,
    pub fixture_class: String,
    pub proofs: BTreeSet<String>,
    pub runtime_state: RuntimeStateContractV1,
    pub formal_composition: FormalCompositionV1,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStateContractV1 {
    pub repair_owner: String,
    pub state_schema: String,
    pub identity_schema: String,
    pub lease_schema: String,
    pub authority: String,
    pub identity_source: String,
    pub state_source: String,
    pub diagnostics: String,
    pub persisted_clock: String,
    pub expiry_rule: String,
    pub legacy_policy: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormalCompositionV1 {
    pub state: String,
    pub composition_owner: String,
    pub evidence: String,
}

impl ComputePoolContractV1 {
    pub fn validate(&self) -> Result<(), ComputePoolContractError> {
        if self.schema != "hiroute.product-e2e.compute-pool/v1"
            || self.scenario_id != "process-25003-compute-pool"
            || self.process != "PROCESS-25003"
            || self.fixture_class != "typed_contract_fake_ports"
            || self.runtime_state.repair_owner != "PROCESS-25015"
            || self.runtime_state.state_schema != "hiroute.compute-runtime-state/v1"
            || self.runtime_state.identity_schema != "hiroute.compute-runtime-identity/v1"
            || self.runtime_state.lease_schema != "hiroute.compute-probe-lease/v1"
            || self.runtime_state.authority != "single_sqlite_compare_and_set_store"
            || self.runtime_state.identity_source != "frozen_g0_exact_fields_no_lookup_or_hash"
            || self.runtime_state.state_source != "gateway_exact_next_state"
            || self.runtime_state.diagnostics != "absent_from_state_authority"
            || self.runtime_state.persisted_clock != "unix_epoch_milliseconds"
            || self.runtime_state.expiry_rule != "sample_greater_than_or_equal_deadline"
            || self.runtime_state.legacy_policy != "read_only_quarantine_not_exact_state"
            || self.formal_composition.state != "green"
            || self.formal_composition.composition_owner != PRODUCTION_EVIDENCE_OWNER
            || self.formal_composition.evidence != PRODUCTION_EVIDENCE
        {
            return Err(ComputePoolContractError::InvalidBoundary);
        }
        let required = BTreeSet::from([
            "current_release_facts".to_owned(),
            "registry_only_endpoint_authority".to_owned(),
            "registered_option_only".to_owned(),
            "two_key_homogeneous_pool".to_owned(),
            "inventory_cannot_expand_authority".to_owned(),
            PRODUCTION_EVIDENCE.to_owned(),
            "effective_price_freeze".to_owned(),
            "credential_binding_runtime_cas".to_owned(),
            "exact_runtime_identity_generation_isolation".to_owned(),
            "frozen_g0_lossless_runtime_mapping".to_owned(),
            "gateway_owned_health_transition".to_owned(),
            "opaque_key_id_preserved".to_owned(),
            "atomic_restart_safe_single_probe_lease".to_owned(),
            "legacy_availability_not_fabricated_as_exact_state".to_owned(),
            "secret_and_network_zero_on_rejection".to_owned(),
        ]);
        if self.proofs != required {
            return Err(ComputePoolContractError::MissingProof);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ComputePoolContractError {
    #[error("compute-pool scenario boundary is invalid")]
    InvalidBoundary,
    #[error("compute-pool scenario proof set is incomplete or self-reported")]
    MissingProof,
}
