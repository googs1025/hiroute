//! Draft Preview permits incomplete parked editor groups, but always seals exact CAS content.
use super::PlanPreviewError;
use hiroute_application_api::*;
use hiroute_domain::*;
pub struct PlanDraftSnapshotV1 {
    pub workspace: WorkspaceId,
    pub draft: Option<PlanDraftV1>,
    pub legacy_source: Option<PlanVersionV1>,
    pub expected_revisions: RevisionSetV1,
}
pub fn preview_plan_draft(
    change: &PlanDraftChangeV1,
    state: &PlanDraftSnapshotV1,
) -> Result<PlanDraftPreviewV1, PlanPreviewError> {
    change.validate().map_err(|_| PlanPreviewError::Invalid)?;
    if change.workspace_id != state.workspace {
        return Err(PlanPreviewError::Invalid);
    }
    if let Some(draft) = &state.draft {
        draft.validate().map_err(|_| PlanPreviewError::Invalid)?;
        if draft.workspace_id != change.workspace_id || draft.draft_id != change.draft_id {
            return Err(PlanPreviewError::Invalid);
        }
    }
    if state.draft.as_ref().map(|d| d.revision) != change.expected_revision {
        return Err(PlanPreviewError::Stale);
    }
    let after = match &change.action {
        PlanDraftActionV1::Save { draft } => {
            if state
                .draft
                .as_ref()
                .is_some_and(|old| old.plan_id != draft.plan_id)
            {
                return Err(PlanPreviewError::Invalid);
            }
            Some(draft.as_ref().clone())
        }
        PlanDraftActionV1::Discard => None,
    };
    if let Some(legacy) = &state.legacy_source {
        let spec = ChangeSpecV1 {
            schema_version: CHANGE_SPEC_SCHEMA_V1,
            command_id: "routing.apply".into(),
            resource_id: Some(format!("plan-draft/{}", change.draft_id)),
            desired_state: serde_json::to_value(change).map_err(|_| PlanPreviewError::Invalid)?,
        };
        TransactionPlanV1::from_plan_draft_planner(spec)
            .and_then(|p| p.with_draft_legacy_source(legacy.clone()))
            .map_err(|_| PlanPreviewError::Invalid)?;
    }
    let change_digest = CanonicalDigest::of(&(
        PLAN_DRAFT_PREVIEW_SCHEMA_V1,
        change,
        &state.draft,
        &state.legacy_source,
        &state.expected_revisions,
    ))
    .map_err(|_| PlanPreviewError::Invalid)?;
    Ok(PlanDraftPreviewV1 {
        schema: PLAN_DRAFT_PREVIEW_SCHEMA_V1.into(),
        change_digest,
        expected_revisions: state.expected_revisions.clone(),
        before: state.draft.clone(),
        legacy_source: state.legacy_source.clone(),
        after,
    })
}
