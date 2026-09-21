use crate::{
    ModelDataBundleV1, ModelNativeReasoningV1, NativeReasoningCapabilityV1, RatingSnapshotRefV1,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceCapabilityStateV1 {
    KnownSupported,
    KnownUnsupported,
    Unknown,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCapabilityV1 {
    pub state: ReferenceCapabilityStateV1,
    pub evidence_kind: String,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelReferenceViewV1 {
    pub model_configuration_id: String,
    pub model_revision: u64,
    pub display_name: String,
    pub publisher_id: String,
    pub data_version: String,
    pub tool: ReferenceCapabilityV1,
    pub vision: ReferenceCapabilityV1,
    pub streaming: ReferenceCapabilityV1,
    pub context_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub native_reasoning: Option<NativeReasoningCapabilityV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_render_convention: Option<crate::NativeReasoningRenderConventionV1>,
    pub rating_snapshots: Vec<RatingSnapshotRefV1>,
}
/// Catalog claims do not establish any source's verified execution capability. In particular,
/// a legacy false boolean is not evidence of verified non-support.
pub fn model_reference_view(
    data: &ModelDataBundleV1,
    native: &[ModelNativeReasoningV1],
    snapshots: Vec<RatingSnapshotRefV1>,
    model_id: &str,
) -> Option<ModelReferenceViewV1> {
    let model = data.model(model_id)?;
    let capability = |known| ReferenceCapabilityV1 {
        state: if known {
            ReferenceCapabilityStateV1::KnownSupported
        } else {
            ReferenceCapabilityStateV1::Unknown
        },
        evidence_kind: "catalog_reference".into(),
    };
    Some(ModelReferenceViewV1 {
        model_configuration_id: model.model_configuration_id.clone(),
        model_revision: model.revision,
        display_name: model.display_name.clone(),
        publisher_id: model.publisher_id.clone(),
        data_version: data.bundle_version.clone(),
        tool: capability(model.capabilities.tool),
        vision: capability(model.capabilities.vision),
        streaming: capability(model.capabilities.streaming),
        context_tokens: (model.capabilities.context_tokens > 0)
            .then_some(model.capabilities.context_tokens),
        max_output_tokens: (model.capabilities.max_output_tokens > 0)
            .then_some(model.capabilities.max_output_tokens),
        native_reasoning: native
            .iter()
            .find(|n| n.model_configuration_id == model_id)
            .map(|n| n.capability.clone()),
        native_render_convention: native
            .iter()
            .find(|n| n.model_configuration_id == model_id)
            .and_then(|n| n.native_render_convention),
        rating_snapshots: snapshots,
    })
}
