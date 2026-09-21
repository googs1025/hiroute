//! Head-only lifecycle policy. Reference facts are required even when there are no references;
//! an unavailable peer reader is not an empty query result.
use super::PlanPreviewError;
use hiroute_application_api::*;
use hiroute_domain::*;

pub struct PlanLifecycleSnapshotV1 {
    pub head: PlanHeadV1,
    pub version: PlanVersionV1,
    pub publication: GatewayPublicationV1,
    pub expected_revisions: RevisionSetV1,
    pub references_digest: CanonicalDigest,
    pub has_agent_references: bool,
    /// Default-model references block disabling; allowed-list references do not.
    pub has_default_model_reference: bool,
    /// Covers every retained revision of this Plan, including continuation holds.
    pub has_version_holds: bool,
}

pub fn preview_plan_lifecycle(
    change: &PlanLifecycleChangeV1,
    state: &PlanLifecycleSnapshotV1,
) -> Result<PlanLifecyclePreviewV1, PlanPreviewError> {
    state
        .head
        .validate()
        .map_err(|_| PlanPreviewError::Invalid)?;
    state
        .version
        .validate()
        .map_err(|_| PlanPreviewError::Invalid)?;
    state
        .publication
        .validate()
        .map_err(|_| PlanPreviewError::Invalid)?;
    if change.schema != PLAN_LIFECYCLE_CHANGE_SCHEMA_V1
        || state.references_digest == CanonicalDigest::of_bytes(&[])
        || (state.has_default_model_reference && !state.has_agent_references)
        || state.version.reference != state.head.reference
        || !state.publication.plan_heads.contains(&state.head)
        || state.publication.workspace_id != state.head.reference.workspace_id
        || !state.publication.plans.contains(&state.version.compiled)
    {
        return Err(PlanPreviewError::Invalid);
    }
    if change.plan_id != state.head.reference.plan_id
        || change.expected_head_revision != state.head.head_revision
        || state.head.status == PlanLifecycleV1::Deleted
        || state.head.status == change.status
    {
        return Err(PlanPreviewError::Stale);
    }
    if change.status == PlanLifecycleV1::Disabled && state.has_default_model_reference {
        return Err(PlanPreviewError::Referenced);
    }
    if change.status == PlanLifecycleV1::Deleted
        && (state.has_agent_references || state.has_version_holds)
    {
        return Err(PlanPreviewError::Referenced);
    }
    let mut head = state.head.clone();
    head.head_revision = head
        .head_revision
        .checked_add(1)
        .ok_or(PlanPreviewError::Invalid)?;
    head.status = change.status;
    let publication = lifecycle_publication(state, head.clone())?;
    let no_new_calls = publication.aliases.is_empty();
    let change_digest = CanonicalDigest::of(&(
        PLAN_LIFECYCLE_PREVIEW_SCHEMA_V1,
        change,
        &state.expected_revisions,
        &state.head,
        &head,
        &state.references_digest,
        state.has_agent_references,
        state.has_default_model_reference,
        state.has_version_holds,
        publication
            .digest()
            .map_err(|_| PlanPreviewError::Invalid)?,
    ))
    .map_err(|_| PlanPreviewError::Invalid)?;
    Ok(PlanLifecyclePreviewV1 {
        schema: PLAN_LIFECYCLE_PREVIEW_SCHEMA_V1.into(),
        change_digest,
        expected_revisions: state.expected_revisions.clone(),
        before_head: state.head.clone(),
        plan_head: head,
        references_digest: state.references_digest.clone(),
        has_agent_references: state.has_agent_references,
        has_version_holds: state.has_version_holds,
        no_new_calls,
    })
}

pub fn lifecycle_publication(
    state: &PlanLifecycleSnapshotV1,
    head: PlanHeadV1,
) -> Result<GatewayPublicationV1, PlanPreviewError> {
    let revision = state
        .publication
        .publication_revision
        .get()
        .checked_add(1)
        .and_then(|r| GatewayPublicationRevision::new(r).ok())
        .ok_or(PlanPreviewError::Invalid)?;
    state
        .publication
        .next_with_plan_lifecycle(revision, head)
        .map_err(|_| PlanPreviewError::Invalid)
}

#[cfg(test)]
mod tests;
