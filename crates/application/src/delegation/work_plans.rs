//! Authenticated, current collaboration discovery. Management catalogs are not an authority.
use crate::{ApplicationService, failed, succeeded};
use hiroute_application_api::*;
use hiroute_domain::delegation::DelegationGrantAuthorityPort;
use hiroute_domain::{AgentCollaborationCredential, VerifiedCollaborationPrincipal, WorkspaceId};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Narrow data projection at the fixed MVP-13 composition boundary.
#[derive(Clone)]
pub struct WorkPlanMetadataV1 {
    pub agent_plan_id: AgentPlanId,
    pub alias: String,
    pub display_name: String,
    pub purpose: String,
    pub published: bool,
    pub work: Option<(
        hiroute_domain::delegation::WorkerHarnessV1,
        hiroute_domain::AgentIngressProtocolV1,
    )>,
    pub availability: WorkPlanAvailabilityV1,
    pub reason: Option<String>,
}

/// The integration owner reads the current published head, never unpublished editor content.
/// Availability must be evaluated for the Worker protocol, independently of the main Agent.
/// Missing publication or Harness returns no row. Unknown and unavailable rows are retained.
pub trait WorkPlanMetadataPort: Send + Sync {
    fn current_plans(
        &self,
        workspace: &WorkspaceId,
        allowed: &BTreeSet<AgentPlanId>,
    ) -> Result<Vec<WorkPlanMetadataV1>, ErrorCode>;
}

/// Explicit composition gap until the fixed MVP-13 current-metadata interface is integrated.
pub struct UnavailableWorkPlanMetadata;
impl WorkPlanMetadataPort for UnavailableWorkPlanMetadata {
    fn current_plans(
        &self,
        _: &WorkspaceId,
        _: &BTreeSet<AgentPlanId>,
    ) -> Result<Vec<WorkPlanMetadataV1>, ErrorCode> {
        Err(ErrorCode::CapabilityUnavailable)
    }
}

pub struct WorkPlanDirectory {
    authority: Arc<dyn DelegationGrantAuthorityPort + Send + Sync>,
    metadata: Arc<dyn WorkPlanMetadataPort>,
}
impl WorkPlanDirectory {
    pub fn new(
        authority: Arc<dyn DelegationGrantAuthorityPort + Send + Sync>,
        metadata: Arc<dyn WorkPlanMetadataPort>,
    ) -> Self {
        Self {
            authority,
            metadata,
        }
    }

    pub fn list(
        &self,
        request: &WorkPlanListRequestV1,
        material: &AgentCollaborationCredential,
    ) -> Result<WorkPlanListV1, ErrorCode> {
        self.list_authorized(request, || {
            let grant = self
                .authority
                .current_grant(&request.workspace_id, &request.grant_id)
                .map_err(|_| ErrorCode::CapabilityUnavailable)?
                .ok_or(ErrorCode::CapabilityDenied)?;
            if grant.workspace_id != request.workspace_id || grant.grant_id != request.grant_id {
                return Err(ErrorCode::CapabilityDenied);
            }
            grant
                .verify_bootstrap(&request.context_id, grant.generation, material)
                .map_err(|_| ErrorCode::CapabilityDenied)
        })
    }

    fn list_management(
        &self,
        request: &WorkPlanListRequestV1,
    ) -> Result<WorkPlanListV1, ErrorCode> {
        self.list_authorized(request, || {
            let grant = self
                .authority
                .current_grant(&request.workspace_id, &request.grant_id)
                .map_err(|_| ErrorCode::CapabilityUnavailable)?
                .ok_or(ErrorCode::CapabilityDenied)?;
            grant
                .verify_management_selection(
                    &request.workspace_id,
                    &request.context_id,
                    &request.grant_id,
                    grant.generation,
                )
                .map_err(|_| ErrorCode::CapabilityDenied)
        })
    }

    fn list_authorized(
        &self,
        request: &WorkPlanListRequestV1,
        verify: impl Fn() -> Result<VerifiedCollaborationPrincipal, ErrorCode>,
    ) -> Result<WorkPlanListV1, ErrorCode> {
        let before = verify()?;
        let metadata = self
            .metadata
            .current_plans(&request.workspace_id, before.allowed_plan_ids())?;
        // A metadata read may race revocation or whitelist editing; the final current read is
        // the authorization point. An expansion racing this read is visible on the next query.
        let current = verify()?;
        if metadata.len() > 256 {
            return Err(ErrorCode::CapabilityUnavailable);
        }
        let mut plans: Vec<_> = metadata
            .into_iter()
            .filter_map(|p| {
                if !p.published || !current.allowed_plan_ids().contains(&p.agent_plan_id) {
                    return None;
                }
                let (harness, protocol) = p.work?;
                Some(WorkPlanViewV1 {
                    agent_plan_id: p.agent_plan_id,
                    alias: p.alias,
                    display_name: p.display_name,
                    purpose: p.purpose,
                    harness,
                    protocol,
                    availability: p.availability,
                    reason: p.reason,
                })
            })
            .collect();
        plans.sort_by(|a, b| a.agent_plan_id.cmp(&b.agent_plan_id));
        if plans
            .windows(2)
            .any(|w| w[0].agent_plan_id == w[1].agent_plan_id)
        {
            return Err(ErrorCode::CapabilityUnavailable);
        }
        Ok(WorkPlanListV1 {
            schema: "hiroute.work-plan-list/v1".into(),
            plans,
        })
    }
}

pub(crate) fn dispatch(
    service: &ApplicationService,
    mut request: LocalControlRequestV2,
) -> MachineEnvelopeV2<serde_json::Value> {
    let Some(mut grant) = request.protected_grant.take() else {
        return failed(ErrorCode::CapabilityDenied, request.request_id);
    };
    let Ok(query) = serde_json::from_value::<WorkPlanListRequestV1>(request.payload) else {
        return failed(ErrorCode::InvalidArguments, request.request_id);
    };
    let Some(directory) = service.ports.as_ref().and_then(|p| p.work_plans.as_ref()) else {
        return failed(ErrorCode::CapabilityUnavailable, request.request_id);
    };
    if grant.principal_kind == PrincipalKind::Desktop {
        let authenticate = || {
            let digest = CanonicalDigest::of(&query).map_err(|_| ErrorCode::InvalidArguments)?;
            let ports = service
                .ports
                .as_ref()
                .ok_or(ErrorCode::CapabilityUnavailable)?;
            let revisions = ports
                .control
                .snapshot(&query.workspace_id)
                .map_err(crate::map_control_error)?
                .revisions;
            ports
                .control
                .validate_protected_capability(
                    &grant.capability,
                    &query.workspace_id,
                    PrincipalKind::Desktop,
                    "ListWorkPlans",
                    &digest,
                    &revisions,
                )
                .map_err(crate::map_control_error)
        };
        let result = authenticate()
            .and_then(|()| directory.list_management(&query))
            .and_then(|result| authenticate().map(|()| result));
        return match result {
            Ok(result) => succeeded(result, request.request_id),
            Err(code) => failed(code, request.request_id),
        };
    }
    if !grant.principal_kind.is_collaboration() {
        return failed(ErrorCode::CapabilityDenied, request.request_id);
    }
    let material = if grant.principal_kind == PrincipalKind::SealedCollaboration {
        service
            .ports
            .as_ref()
            .and_then(|p| p.work_plans.as_ref())
            .ok_or(hiroute_domain::delegation::DelegationErrorV1::CapabilityUnavailable)
            .and_then(|directory| directory.authority.open_sealed_bootstrap(&grant.capability))
            .and_then(|(binding, material)| {
                if query.workspace_id != binding.workspace_id
                    || query.context_id != binding.context_id
                    || query.grant_id != binding.grant_id
                {
                    return Err(hiroute_domain::delegation::DelegationErrorV1::PermissionDenied);
                }
                Ok(material)
            })
    } else {
        AgentCollaborationCredential::from_authenticated_storage(
            std::mem::take(&mut grant.capability).into_bytes(),
        )
        .map_err(|_| hiroute_domain::delegation::DelegationErrorV1::PermissionDenied)
    };
    let Ok(material) = material else {
        return failed(ErrorCode::CapabilityDenied, request.request_id);
    };
    match directory.list(&query, &material) {
        Ok(result) => succeeded(result, request.request_id),
        Err(code) => failed(code, request.request_id),
    }
}

#[cfg(test)]
mod tests;
