use std::collections::BTreeSet;

use serde::Deserialize;
use thiserror::Error;

pub const COMPOSITION_BLOCKER: &str =
    "PROCESS-25009 must compose the six adapters into hirouted before a real local smoke";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayAdapterContractV1 {
    pub schema: String,
    pub process: String,
    pub six_ports: BTreeSet<String>,
    pub invariant: AdapterInvariantV1,
    pub composition: CompositionStateV1,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterInvariantV1 {
    pub publication: String,
    pub credential: String,
    pub runtime_state: String,
    pub observation: String,
    pub adapter_time_join: bool,
    pub acknowledgement_cache: bool,
    pub second_content_queue: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionStateV1 {
    pub state: String,
    pub owner: String,
    pub blocker: String,
}

impl GatewayAdapterContractV1 {
    pub fn validate(&self) -> Result<(), GatewayAdapterContractError> {
        if self.schema != "hiroute.product-e2e.gateway-exact-adapters/v1"
            || self.process != "PROCESS-25028"
            || self.six_ports
                != BTreeSet::from([
                    "conversation_content".into(),
                    "credential".into(),
                    "execution_fact".into(),
                    "lifecycle".into(),
                    "runtime_publication".into(),
                    "runtime_state".into(),
                ])
            || self.invariant.publication != "single_sealed_executable_aggregate"
            || self.invariant.credential != "request_scoped_opaque_capability"
            || self.invariant.runtime_state != "product_owned_durable_exact_cas"
            || self.invariant.observation != "three_independent_lossless_event_receivers"
            || self.invariant.adapter_time_join
            || self.invariant.acknowledgement_cache
            || self.invariant.second_content_queue
            || self.composition.state != "expected_red"
            || self.composition.owner != "PROCESS-25009"
            || self.composition.blocker != COMPOSITION_BLOCKER
        {
            return Err(GatewayAdapterContractError::InvalidBoundary);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum GatewayAdapterContractError {
    #[error("gateway exact-adapter boundary is invalid")]
    InvalidBoundary,
}
