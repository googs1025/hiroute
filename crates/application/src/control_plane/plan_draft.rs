//! Draft mutation uses the same protected routing command and single recoverable writer.
use super::*;
use crate::control::RoutingFactsPort;
use crate::routing::{PlanPreviewError, preview_plan_draft};
use hiroute_application_api::*;
use hiroute_domain::{ChangeSpecV1, TransactionPlanV1, WorkspaceId};
use serde_json::Value;

fn preview(
    port: &dyn RoutingFactsPort,
    change: &hiroute_domain::PlanDraftChangeV1,
) -> Result<PlanDraftPreviewV1, ErrorCode> {
    let state = port
        .plan_draft_snapshot(&WorkspaceId::default(), change)
        .map_err(map_control_error)?;
    preview_plan_draft(change, &state).map_err(map_preview_error)
}
fn map_preview_error(error: PlanPreviewError) -> ErrorCode {
    match error {
        PlanPreviewError::Stale => ErrorCode::ChangePreviewStale,
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
    let payload: PlanDraftPreviewRequestV1 = match serde_json::from_value(request.payload) {
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
    let payload: PlanDraftApplyRequestV1 = match serde_json::from_value(request.payload) {
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
        let state = port
            .plan_draft_snapshot(&WorkspaceId::default(), &payload.change)
            .map_err(map_control_error)?;
        let reproduced = preview_plan_draft(&payload.change, &state).map_err(map_preview_error)?;
        if reproduced.change_digest != payload.accept_digest
            || reproduced.expected_revisions != payload.expected_revisions
        {
            return Err(ErrorCode::ChangePreviewStale);
        }
        let spec = ChangeSpecV1 {
            schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
            command_id: "routing.apply".into(),
            resource_id: Some(format!("plan-draft/{}", payload.change.draft_id)),
            desired_state: serde_json::to_value(&payload.change)
                .map_err(|_| ErrorCode::InvalidArguments)?,
        };
        let mut plan = TransactionPlanV1::from_plan_draft_planner(spec.clone())
            .map_err(|_| ErrorCode::InvalidArguments)?;
        if let Some(legacy) = reproduced.legacy_source {
            plan = plan
                .with_draft_legacy_source(legacy)
                .map_err(|_| ErrorCode::InvalidArguments)?;
        }
        let apply = ApplyRequestV1 {
            schema_version: CHANGE_SPEC_SCHEMA_V1,
            spec,
            accept_digest: payload.accept_digest.clone(),
            expected_revisions: payload.expected_revisions.clone(),
            idempotency_key: payload.idempotency_key,
            apply_capability: None,
        };
        let prepared =
            crate::PreparedTransactionV1::for_setup(apply, payload.accept_digest.clone(), plan)
                .map_err(|e| e.error_code())?;
        let port = port.clone();
        Ok(prepared.with_revalidation(move || {
            let current = preview(port.as_ref(), &payload.change)
                .map_err(|_| crate::TransactionError::ChangePreviewStale)?;
            if current.change_digest != payload.accept_digest
                || current.expected_revisions != payload.expected_revisions
            {
                return Err(crate::TransactionError::ChangePreviewStale);
            }
            Ok(())
        }))
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
