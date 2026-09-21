use super::*;
use serde::{Deserialize, Serialize};

pub const SOURCE_PRICE_CHANGE_SCHEMA_V2: &str = "hiroute.source-price-change/v2";
/// Application-resolved target and exact before/after facts, replayed by the existing journal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePriceChangeV2 {
    pub schema: String,
    pub target: PriceTargetV1,
    pub binding_id: String,
    pub expected_binding_revision: u64,
    pub expected_source_revision: u64,
    pub before: Option<SourcePriceOverrideV1>,
    pub after: SourcePriceOverrideV1,
}
impl SourcePriceChangeV2 {
    pub fn validate(&self) -> Result<(), ComputeContractError> {
        if self.schema != SOURCE_PRICE_CHANGE_SCHEMA_V2 {
            return Err(ComputeContractError::UnsupportedSchema);
        }
        self.target.validate()?;
        validate_identifier(&self.binding_id)?;
        self.after.validate()?;
        if self.expected_source_revision == 0
            || self.expected_binding_revision == 0
            || self.after.target != self.target
        {
            return Err(ComputeContractError::InvalidPrice);
        }
        if let Some(before) = &self.before {
            before.validate()?;
            if before.target != self.target {
                return Err(ComputeContractError::InvalidPrice);
            }
        }
        let revision = self
            .before
            .as_ref()
            .map_or(0, |b| b.revision)
            .checked_add(1)
            .ok_or(ComputeContractError::InvalidPrice)?;
        if self.after.revision != revision {
            return Err(ComputeContractError::InvalidPrice);
        }
        Ok(())
    }
}
