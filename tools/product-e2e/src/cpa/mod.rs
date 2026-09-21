use std::collections::BTreeSet;

use hiroute_cpa_bridge::{CpaManagedConfigContract, STOCK_CPA_CONTRACT_VERSION};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const GATEWAY_ADAPTER_BLOCKER: &str =
    "PROCESS-25008 owns the exact request adapter into Gateway Runtime";
pub const COMPOSITION_BLOCKER: &str = "PROCESS-25009 owns formal hiroute/hirouted composition";
pub const PACKAGING_BLOCKER: &str = "PROCESS-25010 owns three-platform CPA artifact packaging";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioState {
    Green,
    ExpectedRed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CpaScenarioStateV1 {
    pub scenario_id: String,
    pub state: ScenarioState,
    pub evidence_owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CpaConnectorContractV1 {
    pub schema: String,
    pub scenario_id: String,
    pub process: String,
    pub stock_contract_version: String,
    pub proofs: BTreeSet<String>,
    pub scenario_states: Vec<CpaScenarioStateV1>,
}

impl CpaConnectorContractV1 {
    pub fn validate(&self) -> Result<(), CpaConnectorContractError> {
        if self.schema != "hiroute.product-e2e.cpa-connector/v1"
            || self.scenario_id != "process-25017-cpa-connector"
            || self.process != "PROCESS-25017"
            || self.stock_contract_version != STOCK_CPA_CONTRACT_VERSION
            || self.proofs != required_proofs()
        {
            return Err(CpaConnectorContractError::InvalidContract);
        }
        self.require_state(
            "managed-connector-component",
            ScenarioState::Green,
            "PROCESS-25017",
            None,
        )?;
        self.require_state(
            "gateway-exact-request-adapter",
            ScenarioState::ExpectedRed,
            "PROCESS-25008",
            Some(GATEWAY_ADAPTER_BLOCKER),
        )?;
        self.require_state(
            "production-hiroute-hirouted-composition",
            ScenarioState::ExpectedRed,
            "PROCESS-25009",
            Some(COMPOSITION_BLOCKER),
        )?;
        self.require_state(
            "three-platform-artifact-packaging",
            ScenarioState::ExpectedRed,
            "PROCESS-25010",
            Some(PACKAGING_BLOCKER),
        )?;
        if self.scenario_states.len() != 4 {
            return Err(CpaConnectorContractError::UnexpectedState);
        }
        Ok(())
    }

    fn require_state(
        &self,
        id: &str,
        state: ScenarioState,
        owner: &str,
        blocker: Option<&str>,
    ) -> Result<(), CpaConnectorContractError> {
        let actual = self
            .scenario_states
            .iter()
            .find(|actual| actual.scenario_id == id)
            .ok_or(CpaConnectorContractError::MissingState)?;
        if actual.state != state
            || actual.evidence_owner != owner
            || actual.blocker.as_deref() != blocker
        {
            return Err(CpaConnectorContractError::UnexpectedState);
        }
        Ok(())
    }
}

pub fn validate_stock_config_keys(keys: &BTreeSet<String>) -> bool {
    let mut expected = CpaManagedConfigContract::stock_contract_keys()
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    expected.insert("plugins".to_owned());
    keys == &expected
}

fn required_proofs() -> BTreeSet<String> {
    [
        "authenticated_loopback_only",
        "bounded_crash_restart_backoff",
        "connector_owned_opaque_credential",
        "exact_account_model_prefix",
        "file_backed_oauth_metadata_only",
        "native_api_key_bypass",
        "no_internal_retry_fallback_cooling",
        "owner_only_instance_state",
        "catalog_bound_logical_source_identity",
        "stale_owner_authenticated_adoption",
        "trusted_binary_version_digest",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CpaConnectorContractError {
    #[error("managed CPA connector Product E2E contract is invalid")]
    InvalidContract,
    #[error("managed CPA connector scenario state is missing")]
    MissingState,
    #[error("managed CPA connector scenario state overclaims or has the wrong owner")]
    UnexpectedState,
}
