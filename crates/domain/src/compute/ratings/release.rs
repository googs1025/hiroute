use super::*;
use crate::{
    CanonicalDigest, ComputeContractError, ConnectorRegistryBundleV1, ModelDataBundleV1,
    ModelMetadataCatalogV1,
};
use serde::{Deserialize, Serialize};

pub const RELEASE_MODEL_DATA_SCHEMA_V2: &str = "hiroute.release-model-data/v2";
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseModelDataBundleV2 {
    pub schema: String,
    /// Stable Model/Offer payload; model-level ratings stay empty because the current rating
    /// snapshot is configuration-scoped.
    pub data: ModelDataBundleV1,
    pub rating_snapshot: RatingSnapshotV2,
    /// Broad, partial metadata is bundled beside executable facts but cannot create execution
    /// eligibility. It exists for scoped directory and form prefill only.
    pub metadata_catalog: ModelMetadataCatalogV1,
}
impl ReleaseModelDataBundleV2 {
    pub fn validate_against(
        &self,
        registry: &ConnectorRegistryBundleV1,
    ) -> Result<(), ComputeContractError> {
        if self.schema != RELEASE_MODEL_DATA_SCHEMA_V2 {
            return Err(ComputeContractError::UnsupportedSchema);
        }
        self.data.validate_against(registry)?;
        self.rating_snapshot.validate()?;
        self.metadata_catalog.validate()?;
        if !self.data.ratings.is_empty()
            || self.data.ratings_slice_version != self.rating_snapshot.version
        {
            return Err(ComputeContractError::MixedReleaseSlice);
        }
        let catalog_digest = CanonicalDigest::of(&self.data.models)
            .map_err(|_| ComputeContractError::InvalidModelData)?;
        if catalog_digest != self.rating_snapshot.model_catalog_digest {
            return Err(ComputeContractError::CrossReference);
        }
        let ids = self
            .data
            .models
            .iter()
            .map(|m| &m.model_configuration_id)
            .collect::<BTreeSet<_>>();
        let native_ids = self
            .rating_snapshot
            .models
            .iter()
            .map(|m| &m.model_configuration_id)
            .collect::<BTreeSet<_>>();
        if ids != native_ids {
            return Err(ComputeContractError::CrossReference);
        }
        Ok(())
    }
    pub fn cross_reference_digest(
        &self,
        registry: &ConnectorRegistryBundleV1,
    ) -> Result<CanonicalDigest, ComputeContractError> {
        self.validate_against(registry)?;
        // The current format closes over complete validated structures including native
        // configurations, not a model-only rating ID list. The manifest also binds the original
        // payload bytes embedded in this client.
        CanonicalDigest::of(&("hiroute.release-facts-cross-reference/v2", registry, self))
            .map_err(|_| ComputeContractError::InvalidModelData)
    }
}
