//! The draft and terminal Operation become visible in one control.db transaction.
use super::*;
use hiroute_domain::{
    OperationState, OperationV1, PlanDraftActionV1, PortError, PortErrorCode, PortResult,
};

pub(in crate::control) fn finish_draft_operation(
    tx: &Connection,
    operation: &OperationV1,
) -> PortResult<()> {
    let invalid = || PortError::new(PortErrorCode::Conflict, "plan.draft.operation-conflict");
    let Some(change) = operation.plan.plan_draft_change().map_err(|_| invalid())? else {
        return Ok(());
    };
    if operation.state != OperationState::Succeeded {
        return Ok(());
    }
    if change.workspace_id != operation.workspace_id {
        return Err(invalid());
    }
    let actual = tx
        .query_row(
            "SELECT revision FROM plan_drafts WHERE workspace_id=?1 AND draft_id=?2",
            params![change.workspace_id.as_str(), change.draft_id],
            |r| r.get::<_, u64>(0),
        )
        .optional()
        .map_err(|_| invalid())?;
    if actual != change.expected_revision {
        return Err(invalid());
    }
    if let Some(legacy) = operation
        .plan
        .draft_legacy_source()
        .map_err(|_| invalid())?
    {
        migrate_legacy(tx, operation, &legacy).map_err(|_| invalid())?;
    }
    match change.action {
        PlanDraftActionV1::Save { draft } => {
            tx.execute("INSERT INTO plan_drafts(workspace_id,draft_id,revision,draft_json) VALUES(?1,?2,?3,?4) ON CONFLICT(workspace_id,draft_id) DO UPDATE SET revision=excluded.revision,draft_json=excluded.draft_json",
                params![draft.workspace_id.as_str(), draft.draft_id, draft.revision, encode(&draft).map_err(|_| invalid())?]).map_err(|_| invalid())?;
        }
        PlanDraftActionV1::Discard => {
            tx.execute(
                "DELETE FROM plan_drafts WHERE workspace_id=?1 AND draft_id=?2 AND revision=?3",
                params![
                    change.workspace_id.as_str(),
                    change.draft_id,
                    change.expected_revision
                ],
            )
            .map_err(|_| invalid())?;
        }
    }
    Ok(())
}

fn migrate_legacy(
    tx: &Connection,
    operation: &OperationV1,
    legacy: &PlanVersionV1,
) -> Result<(), PlanVersionError> {
    let r = &legacy.reference;
    if head_in(tx, &r.workspace_id, &r.plan_id)?.is_some() {
        return Err(PlanVersionError::Conflict);
    }
    let bytes: Vec<u8> = tx.query_row(
        "SELECT publication_bytes FROM gateway_publications WHERE workspace_id=?1 AND state='active'",
        [r.workspace_id.as_str()], |row| row.get(0)).map_err(storage)?;
    let publication = hiroute_domain::GatewayPublicationV1::decode_persisted(&bytes)
        .map_err(|_| PlanVersionError::Invalid)?;
    if !publication.plans.contains(&legacy.compiled) {
        return Err(PlanVersionError::Conflict);
    }
    let head = PlanHeadV1 {
        reference: r.clone(),
        model_alias: legacy.compiled.model_alias().clone(),
        head_revision: r.content_revision,
        status: hiroute_domain::PlanLifecycleV1::Enabled,
    };
    head.validate()?;
    tx.execute("INSERT INTO plan_versions(workspace_id,plan_id,content_revision,content_digest,version_json,state,owner_operation_id) VALUES(?1,?2,?3,?4,?5,'published',?6)",
        params![r.workspace_id.as_str(), r.plan_id.as_str(), r.content_revision, r.content_digest.as_str(), encode(legacy)?, operation.operation_id.as_str()]).map_err(storage)?;
    tx.execute(
        "INSERT INTO plan_heads(workspace_id,plan_id,head_revision,head_json) VALUES(?1,?2,?3,?4)",
        params![
            r.workspace_id.as_str(),
            r.plan_id.as_str(),
            head.head_revision,
            encode(&head)?
        ],
    )
    .map_err(storage)?;
    Ok(())
}
