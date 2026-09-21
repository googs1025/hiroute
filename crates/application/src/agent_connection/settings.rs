//! Facet-independent, read-only settings planning. The daemon supplies current native/Plan/20 facts.
use hiroute_application_api::{
    AGENT_SETTINGS_SCHEMA_V2, AgentFacetIntent, AgentSettingsFacet, AgentSettingsSpecV2,
};
use hiroute_domain::{
    AgentAction, AgentCapability, AgentCapabilitySet, AgentCollaborationTriggerModeV2,
    AgentIngressProtocolV1, AgentModelDefaultSelectionV2, AgentModelGrantV2, AgentModelRouteV2,
    AgentModelSelectionV2, CanonicalDigest, GatewayPublicationV1,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub struct AgentSettingsFacts {
    pub context_id: String,
    pub dependency_digest: CanonicalDigest,
    pub capabilities: AgentCapabilitySet,
    pub ingress: AgentIngressProtocolV1,
    pub available_surfaces: BTreeSet<hiroute_domain::AgentModelSurfaceV2>,
    pub model_publication: Option<GatewayPublicationV1>,
    /// The immutable model catalog artifact for a plan-carrying Codex selection; built by the
    /// backend from a structurally valid full-catalog source and the merged permitted plans.
    /// Absent when that catalog cannot be derived without losing source fidelity.
    pub model_catalog: Option<SettingsModelCatalogFacts>,
    /// True when this apply configures the model facet while no managed connection holds an
    /// active grant anywhere: the first resident-service connection, which requires the host
    /// to establish the login item before the Operation is sealed.
    pub login_item_required: bool,
    /// True when this restore removes the last managed connection while journal evidence
    /// proves this feature owns the login item: the host must unregister that owned item as
    /// part of the restore. A pre-existing user login item is never owned and never removed.
    pub login_item_removal_required: bool,
    pub fixed_candidate_facts: Vec<crate::compiler::CandidateCompilationFactV1>,
    /// Native Codex source/account bindings added by the trusted backend during initial adoption.
    /// An explicit selection for the same client model wins, so later full-state edits can replace
    /// or remove an earlier binding.
    pub preserved_codex_models: Vec<hiroute_domain::AgentFixedModelSelectionV2>,
    pub native_default_model: Option<String>,
    pub native_claude_presets: Option<hiroute_domain::AgentClaudePresetValuesV2>,
    /// Preview-time ownership check for an existing collaboration Skill file.
    pub collaboration_file_conflict: bool,
    /// Only restoration points owned by this context, projected from the protected store.
    pub restore_points: BTreeMap<String, AgentSettingsFacet>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SettingsModelCatalogFacts {
    pub source_revision: String,
    /// Digest of the private merged artifact written by HiRoute.
    pub content_digest: CanonicalDigest,
    pub producer_kind: CodexCatalogProducerKindV1,
    pub producer_path: String,
    pub producer_content_digest: CanonicalDigest,
    pub producer_context_digest: CanonicalDigest,
    pub producer_dependency_digest: CanonicalDigest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexCatalogProducerKindV1 {
    UserConfigured,
    TargetCache,
    TargetBundled,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentSettingsPreview {
    pub spec: AgentSettingsSpecV2,
    pub dependency_digest: CanonicalDigest,
    pub accept_digest: CanonicalDigest,
    pub changed_facets: BTreeSet<AgentSettingsFacet>,
    pub model_grant: Option<AgentModelGrantV2>,
    /// None means keep/restore; Configure always persists the selected Skill trigger mode.
    pub collaboration_trigger_mode: Option<AgentCollaborationTriggerModeV2>,
    pub blockers: Vec<AgentSettingsBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AgentSettingsBlock {
    pub facet: AgentSettingsFacet,
    pub reason: SettingsBlockReason,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<hiroute_domain::CapabilityBlock>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsBlockReason {
    CapabilityUnavailable,
    ModelPlanUnavailable,
    RestorePointUnavailable,
    SkillFileConflict,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SettingsPlanningError {
    #[error("Agent settings schema or context is invalid")]
    InvalidContext,
    #[error("Agent settings do not change a facet")]
    NoChanges,
    #[error("Agent settings input exceeds its bound")]
    InvalidSelection,
    #[error("Agent settings digest could not be encoded")]
    Encoding,
}

pub fn preview_agent_settings(
    mut spec: AgentSettingsSpecV2,
    facts: &AgentSettingsFacts,
) -> Result<AgentSettingsPreview, SettingsPlanningError> {
    if AGENT_SETTINGS_SCHEMA_V2
        .negotiate(spec.schema_version)
        .is_none()
        || !identity(&spec.context_id)
        || spec.context_id != facts.context_id
    {
        return Err(SettingsPlanningError::InvalidContext);
    }
    if let AgentFacetIntent::Configure {
        settings: AgentModelSelectionV2::CodexDefault { fixed_models, .. },
    } = &mut spec.model
    {
        for preserved in &facts.preserved_codex_models {
            if !fixed_models
                .iter()
                .any(|selected| selected.client_model_id == preserved.client_model_id)
            {
                fixed_models.push(preserved.clone());
            }
        }
        fixed_models.sort_by(|left, right| left.client_model_id.cmp(&right.client_model_id));
    }
    let mut changed_facets = BTreeSet::new();
    let mut blockers = Vec::new();
    let mut model_grant = None;
    let mut collaboration_trigger_mode = None;
    match &spec.model {
        AgentFacetIntent::Keep => {}
        AgentFacetIntent::Configure { settings } => {
            changed_facets.insert(AgentSettingsFacet::Model);
            require(
                facts,
                AgentAction::ConfigureModel,
                AgentSettingsFacet::Model,
                &mut blockers,
            );
            settings
                .validate()
                .map_err(|_| SettingsPlanningError::InvalidSelection)?;
            if let AgentModelSelectionV2::ClaudeLauncher { surfaces, .. } = settings
                && !surfaces.is_subset(&facts.available_surfaces)
            {
                return Err(SettingsPlanningError::InvalidSelection);
            }
            let grant = derive_model_grant(settings, facts);
            match grant {
                Ok(grant) => {
                    // Plan-carrying Codex selections ride an immutable catalog artifact; a
                    // selection whose merged catalog is not derivable cannot be published.
                    if facts.ingress == AgentIngressProtocolV1::Responses
                        && !settings.allowed_plan_ids().is_empty()
                        && facts.model_catalog.is_none()
                    {
                        blockers.push(AgentSettingsBlock {
                            facet: AgentSettingsFacet::Model,
                            reason: SettingsBlockReason::CapabilityUnavailable,
                            capabilities: vec![hiroute_domain::CapabilityBlock {
                                capability: AgentCapability::ModelCatalog,
                                reason: missing_capability(facts, AgentCapability::ModelCatalog)
                                    .unwrap_or(hiroute_domain::CapabilityBlockReason::Unavailable),
                            }],
                        });
                    }
                    model_grant = Some(grant)
                }
                Err(_) => blockers.push(AgentSettingsBlock {
                    facet: AgentSettingsFacet::Model,
                    reason: SettingsBlockReason::ModelPlanUnavailable,
                    capabilities: Vec::new(),
                }),
            }
        }
        AgentFacetIntent::Restore { restore_point_ref } => {
            changed_facets.insert(AgentSettingsFacet::Model);
            restore(
                facts,
                AgentAction::RestoreModel,
                AgentSettingsFacet::Model,
                restore_point_ref,
                &mut blockers,
            );
        }
    }
    match &spec.collaboration {
        AgentFacetIntent::Keep => {}
        AgentFacetIntent::Configure { settings } => {
            changed_facets.insert(AgentSettingsFacet::Collaboration);
            require(
                facts,
                AgentAction::InstallCollaborationSkill,
                AgentSettingsFacet::Collaboration,
                &mut blockers,
            );
            collaboration_trigger_mode = Some(settings.trigger_mode);
            if facts.collaboration_file_conflict {
                blockers.push(AgentSettingsBlock {
                    facet: AgentSettingsFacet::Collaboration,
                    reason: SettingsBlockReason::SkillFileConflict,
                    capabilities: Vec::new(),
                });
            }
        }
        AgentFacetIntent::Restore { restore_point_ref } => {
            changed_facets.insert(AgentSettingsFacet::Collaboration);
            restore(
                facts,
                AgentAction::RemoveCollaborationSkill,
                AgentSettingsFacet::Collaboration,
                restore_point_ref,
                &mut blockers,
            );
            if facts.collaboration_file_conflict {
                blockers.push(AgentSettingsBlock {
                    facet: AgentSettingsFacet::Collaboration,
                    reason: SettingsBlockReason::SkillFileConflict,
                    capabilities: Vec::new(),
                });
            }
        }
    }
    if changed_facets.is_empty() {
        return Err(SettingsPlanningError::NoChanges);
    }
    // Exclude diagnostic probe timestamps from the semantic confirmation digest. Include every
    // action-relevant proof and selection so recapture cannot silently change a confirmed facet.
    let proofs = [
        AgentCapability::EffectiveConfiguration,
        AgentCapability::AtomicManagedReplace,
        AgentCapability::IngressAuthentication,
        AgentCapability::ModelCatalog,
        AgentCapability::SkillLoading,
        AgentCapability::TrustedCliExecution,
        AgentCapability::IsolatedVerification,
    ]
    .into_iter()
    .map(|capability| {
        (
            capability,
            facts.capabilities.get(capability).map(|proof| {
                (
                    proof.state,
                    &proof.adapter_contract,
                    &proof.dependency_digest,
                )
            }),
        )
    })
    .collect::<Vec<_>>();
    let accept_digest = CanonicalDigest::of(&(
        "hiroute.agent-settings-preview/v2",
        &spec,
        &facts.dependency_digest,
        proofs,
        &model_grant,
        &collaboration_trigger_mode,
        &facts.restore_points,
        &blockers,
    ))
    .map_err(|_| SettingsPlanningError::Encoding)?;
    Ok(AgentSettingsPreview {
        spec,
        dependency_digest: facts.dependency_digest.clone(),
        accept_digest,
        changed_facets,
        model_grant,
        collaboration_trigger_mode,
        blockers,
    })
}

fn derive_model_grant(
    settings: &AgentModelSelectionV2,
    facts: &AgentSettingsFacts,
) -> Result<AgentModelGrantV2, SettingsPlanningError> {
    let invalid = || SettingsPlanningError::InvalidSelection;
    let fixed = crate::compiler::compile_fixed_model_bindings(
        settings.fixed_models(),
        &facts.fixed_candidate_facts,
    )
    .map_err(|_| invalid())?;
    let grant = AgentModelGrantV2::derive(
        facts.ingress,
        settings,
        facts.model_publication.as_ref().ok_or_else(invalid)?,
        &fixed,
    )
    .map_err(|_| invalid())?;
    validate_model_default(settings, facts, &grant)?;
    Ok(grant)
}

fn validate_model_default(
    settings: &AgentModelSelectionV2,
    facts: &AgentSettingsFacts,
    grant: &AgentModelGrantV2,
) -> Result<(), SettingsPlanningError> {
    let invalid = || SettingsPlanningError::InvalidSelection;
    let claude_presets;
    let default_name = match settings {
        AgentModelSelectionV2::CodexDefault { default_selection, .. } => match default_selection {
            AgentModelDefaultSelectionV2::PreserveNative => facts.native_default_model.as_deref(),
            AgentModelDefaultSelectionV2::FixedModel { client_model_id } => Some(client_model_id.as_str()),
            AgentModelDefaultSelectionV2::Plan { plan_id } => grant.routes.iter().find_map(|(name, route)| {
                matches!(route, AgentModelRouteV2::Plan { plan_id: selected, .. } if selected == plan_id)
                    .then_some(name.as_str())
            }),
        },
        AgentModelSelectionV2::ClaudeLauncher { .. } => {
            claude_presets = grant
                .claude_preset_values(
                    settings,
                    facts.native_claude_presets.as_ref().ok_or_else(invalid)?,
                )
                .map_err(|_| invalid())?;
            match facts.native_default_model.as_deref() {
                Some("opus") => claude_presets.opus.as_deref(),
                Some("sonnet") => claude_presets.sonnet.as_deref(),
                Some("haiku") => claude_presets.haiku.as_deref(),
                other => other,
            }
        }
    };
    if !default_name.is_some_and(|name| grant.permits_name(name)) {
        return Err(invalid());
    }
    Ok(())
}

fn require(
    facts: &AgentSettingsFacts,
    action: AgentAction,
    facet: AgentSettingsFacet,
    blockers: &mut Vec<AgentSettingsBlock>,
) {
    if let Err(capabilities) = facts.capabilities.require(action, &facts.dependency_digest) {
        blockers.push(AgentSettingsBlock {
            facet,
            reason: SettingsBlockReason::CapabilityUnavailable,
            capabilities,
        });
    }
}
fn restore(
    facts: &AgentSettingsFacts,
    action: AgentAction,
    facet: AgentSettingsFacet,
    reference: &str,
    blockers: &mut Vec<AgentSettingsBlock>,
) {
    require(facts, action, facet, blockers);
    if !identity(reference) || facts.restore_points.get(reference) != Some(&facet) {
        blockers.push(AgentSettingsBlock {
            facet,
            reason: SettingsBlockReason::RestorePointUnavailable,
            capabilities: Vec::new(),
        });
    }
}
/// None means the capability is proven against the current dependency digest.
fn missing_capability(
    facts: &AgentSettingsFacts,
    capability: AgentCapability,
) -> Option<hiroute_domain::CapabilityBlockReason> {
    match facts.capabilities.get(capability) {
        None => Some(hiroute_domain::CapabilityBlockReason::Unknown),
        Some(proof) if proof.dependency_digest != facts.dependency_digest => {
            Some(hiroute_domain::CapabilityBlockReason::Stale)
        }
        Some(proof) => match proof.state {
            hiroute_domain::CapabilityState::Proven => None,
            hiroute_domain::CapabilityState::Unknown => {
                Some(hiroute_domain::CapabilityBlockReason::Unknown)
            }
            hiroute_domain::CapabilityState::Unavailable => {
                Some(hiroute_domain::CapabilityBlockReason::Unavailable)
            }
        },
    }
}
pub(super) fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('/')
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./:-".contains(&byte))
}

mod confirmation;
mod transaction;
pub use confirmation::*;

#[cfg(test)]
mod tests;
