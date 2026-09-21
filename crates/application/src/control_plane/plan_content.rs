//! V2 content uses the owner-only Local Control command and single recoverable writer.
use super::*;
use crate::control::RoutingFactsPort;
use crate::routing::{PlanPreviewError, preview_plan_content};
use hiroute_application_api::*;
use hiroute_domain::{
    AliasRegistryV1, ChangeSpecV1, ConsumedPlanDraftV1, GatewayPublicationRevision,
    GatewayPublicationV1, PublicationRecordV1, TransactionPlanV1, WorkspaceId,
};
use serde_json::Value;

fn preview(
    port: &dyn RoutingFactsPort,
    change: &PlanContentChangeV2,
) -> Result<PlanContentPreviewV2, ErrorCode> {
    let state = port
        .plan_authoring_snapshot(&WorkspaceId::default(), change)
        .map_err(map_control_error)?;
    preview_plan_content(change, &state).map_err(map_preview_error)
}
fn map_preview_error(error: PlanPreviewError) -> ErrorCode {
    match error {
        PlanPreviewError::Stale => ErrorCode::ChangePreviewStale,
        PlanPreviewError::Compiler(
            crate::compiler::AgentPlanCompilerError::CandidateNotRoutable(_),
        ) => ErrorCode::ActionRequired,
        _ => ErrorCode::InvalidArguments,
    }
}
pub(super) fn dispatch_preview(
    service: &ApplicationService,
    request: LocalControlRequestV2,
) -> MachineEnvelopeV2<Value> {
    if request.protected_grant.is_some() {
        return failed(ErrorCode::CapabilityDenied, request.request_id);
    }
    let Some(port) = service.ports.as_ref().and_then(|p| p.routing.as_deref()) else {
        return failed(ErrorCode::DaemonUnavailable, request.request_id);
    };
    let payload: PlanContentPreviewRequestV2 = match serde_json::from_value(request.payload) {
        Ok(value) => value,
        Err(_) => return failed(ErrorCode::InvalidArguments, request.request_id),
    };
    match preview(port, &payload.change) {
        Ok(result) => succeeded(result, request.request_id),
        Err(error) => failed(error, request.request_id),
    }
}
pub(super) fn dispatch_apply(
    service: &ApplicationService,
    request: LocalControlRequestV2,
) -> MachineEnvelopeV2<Value> {
    if request.protected_grant.is_some() {
        return failed(ErrorCode::CapabilityDenied, request.request_id);
    }
    let Some(ports) = service.ports.as_ref() else {
        return failed(ErrorCode::DaemonUnavailable, request.request_id);
    };
    let (Some(port), Some(mutation)) = (ports.routing.as_ref(), ports.mutation.as_ref()) else {
        return failed(ErrorCode::DaemonUnavailable, request.request_id);
    };
    let payload: PlanContentApplyRequestV2 = match serde_json::from_value(request.payload) {
        Ok(value) => value,
        Err(_) => return failed(ErrorCode::InvalidArguments, request.request_id),
    };
    if payload.idempotency_key.is_empty() {
        return failed(ErrorCode::InvalidArguments, request.request_id);
    }
    if let Some(replay) = replay_apply(
        ports.control.as_ref(),
        PrincipalKind::InteractiveUser,
        "ApplyAgentPlanChange",
        &payload.idempotency_key,
        &payload.accept_digest,
        &request.request_id,
    ) {
        return replay;
    }
    let prepared = (|| {
        if plan_content_confirmation_digest(&payload.change, &payload.expected_revisions)
            .map_err(|_| ErrorCode::InvalidArguments)?
            != payload.accept_digest
        {
            return Err(ErrorCode::ChangePreviewStale);
        }
        let plan_id = match &payload.change.target {
            PlanContentTargetV2::Update { plan_id, .. } => plan_id.as_str().to_owned(),
            PlanContentTargetV2::Create { creation_key } => {
                crate::routing::created_plan_id(&WorkspaceId::default(), creation_key)
                    .map_err(map_preview_error)?
                    .as_str()
                    .to_owned()
            }
        };
        let spec = ChangeSpecV1 {
            schema_version: CHANGE_SPEC_SCHEMA_V1,
            command_id: "routing.apply".into(),
            resource_id: Some(format!("agent-plan/{plan_id}")),
            desired_state: serde_json::to_value(&payload.change)
                .map_err(|_| ErrorCode::InvalidArguments)?,
        };
        let apply = ApplyRequestV1 {
            schema_version: CHANGE_SPEC_SCHEMA_V1,
            spec: spec.clone(),
            accept_digest: payload.accept_digest.clone(),
            expected_revisions: payload.expected_revisions.clone(),
            idempotency_key: payload.idempotency_key.clone(),
            apply_capability: None,
        };
        let port = port.clone();
        crate::PreparedTransactionV1::for_plan_content(apply, move || {
            reproduce_plan(port.as_ref(), &payload, &spec)
                .map_err(crate::TransactionError::PlanContent)
        })
        .map_err(|e| e.error_code())
    })();
    let prepared = match prepared {
        Ok(value) => value,
        Err(error) => return failed(error, request.request_id),
    };
    match mutation.apply_local_prepared_change(prepared) {
        Ok(operation) => super::accepted_apply(operation, request.request_id),
        Err(error) => failed(error.error_code(), request.request_id),
    }
}

fn reproduce_plan(
    port: &dyn RoutingFactsPort,
    payload: &PlanContentApplyRequestV2,
    spec: &ChangeSpecV1,
) -> Result<TransactionPlanV1, ErrorCode> {
    let state = port
        .plan_authoring_snapshot(&WorkspaceId::default(), &payload.change)
        .map_err(map_control_error)?;
    let reproduced = preview_plan_content(&payload.change, &state).map_err(map_preview_error)?;
    if reproduced.change_digest != payload.accept_digest
        || reproduced.expected_revisions != payload.expected_revisions
    {
        return Err(ErrorCode::ChangePreviewStale);
    }
    let mut aliases = state.aliases.clone();
    let head = &reproduced.plan_head;
    if state.current_head.is_none() {
        let alias = if payload.change.editor.custom_alias.is_some() {
            aliases.allocate_custom(head.reference.plan_id.clone(), head.model_alias.clone())
        } else {
            aliases.allocate_named(
                head.reference.plan_id.clone(),
                reproduced.plan_version.configuration.display_name.as_str(),
            )
        }
        .map_err(|_| ErrorCode::ChangePreviewStale)?;
        if alias != head.model_alias {
            return Err(ErrorCode::ChangePreviewStale);
        }
    }
    let revision = GatewayPublicationRevision::new(
        state
            .active_publication
            .as_ref()
            .map(|p| p.publication_revision.get())
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(ErrorCode::InvalidArguments)?,
    )
    .map_err(|_| ErrorCode::InvalidArguments)?;
    let before_digest = state
        .active_publication
        .as_ref()
        .map(GatewayPublicationV1::digest)
        .transpose()
        .map_err(|_| ErrorCode::InvalidArguments)?;
    let base = match state.active_publication {
        Some(publication) => publication,
        None => GatewayPublicationV1::new(
            state.workspace.clone(),
            revision,
            AliasRegistryV1::default(),
            vec![],
        )
        .map_err(|_| ErrorCode::InvalidArguments)?,
    };
    let mut heads = state.plan_heads;
    heads.retain(|current| current.reference.plan_id != reproduced.plan_head.reference.plan_id);
    heads.push(reproduced.plan_head.clone());
    let publication = base
        .next_with_plan_content(
            revision,
            aliases,
            reproduced.plan_version.compiled.clone(),
            heads,
        )
        .map_err(|_| ErrorCode::InvalidArguments)?;
    let record = PublicationRecordV1::from_publication(state.workspace, &publication)
        .map_err(|_| ErrorCode::InvalidArguments)?;
    let plan = TransactionPlanV1::from_plan_content_planner(
        spec.clone(),
        reproduced.plan_version,
        reproduced.plan_head,
        reproduced.before_head,
        reproduced.consumed_draft.map(|d| ConsumedPlanDraftV1 {
            draft_id: d.draft_id,
            revision: d.revision,
        }),
        record,
        before_digest,
    )
    .map_err(|_| ErrorCode::InvalidArguments)?;
    let plan = if let Some(legacy) = reproduced.legacy_source {
        plan.with_legacy_plan_source(legacy)
            .map_err(|_| ErrorCode::InvalidArguments)?
    } else {
        plan
    };
    Ok(plan)
}
