use std::collections::BTreeMap;

use hiroute_domain::{EffectChannel, SideEffectSnapshotV1};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default)]
pub struct SideEffectLedgerV1 {
    generations: BTreeMap<EffectChannel, u64>,
}

impl SideEffectLedgerV1 {
    pub fn record(&mut self, channel: EffectChannel) {
        *self.generations.entry(channel).or_default() += 1;
    }

    pub fn snapshot(&self) -> SideEffectSnapshotV1 {
        SideEffectSnapshotV1 {
            generations: self.generations.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultLedgerV1 {
    #[serde(default)]
    pub injected: Vec<String>,
    #[serde(default)]
    pub observed: Vec<String>,
}

impl FaultLedgerV1 {
    pub fn record_injected(&mut self, fault_id: impl Into<String>) {
        self.injected.push(fault_id.into());
    }

    pub fn record_observed(&mut self, fault_id: impl Into<String>) {
        self.observed.push(fault_id.into());
    }

    pub fn exact(&self) -> bool {
        self.injected == self.observed
    }
}
