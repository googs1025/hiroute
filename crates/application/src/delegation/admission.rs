//! Instance-scoped Worker admission and collaboration bootstrap verification.
use super::safety::{RunSafetyBinding, RunSafetyProjection};
use crate::publication::admission::{
    AdmissionAction, AdmissionGuard, AdmissionSubject, SharedAdmissionGate,
};
use hiroute_domain::delegation::*;
use hiroute_domain::{AgentCollaborationCredential, VerifiedCollaborationPrincipal, WorkspaceId};
use std::collections::BTreeSet;
use std::sync::Arc;

type Result<T> = std::result::Result<T, DelegationErrorV1>;
pub fn verify_bootstrap(
    authority: &dyn DelegationGrantAuthorityPort,
    workspace: &WorkspaceId,
    grant_id: &str,
    context: &str,
    generation: u64,
    material: &AgentCollaborationCredential,
) -> Result<VerifiedCollaborationPrincipal> {
    authority
        .current_grant(workspace, grant_id)?
        .ok_or(DelegationErrorV1::PermissionDenied)?
        .verify_bootstrap(context, generation, material)
        .map_err(|_| DelegationErrorV1::PermissionDenied)
}

pub struct DelegationAdmission<'a> {
    pub gate: Arc<SharedAdmissionGate>,
    pub safety: &'a RunSafetyProjection,
    pub runtime: &'a dyn DelegationRuntimePort,
}
impl DelegationAdmission<'_> {
    /// Admit a Worker run selected by the local same-UID channel. The daemon callback must
    /// re-read current Plan policy while this shared gate is held before acquiring the exact
    /// Plan version. Run configuration is execution metadata, not caller or management authority.
    pub fn accept(
        &self,
        input: &DelegationAcceptanceV1,
        check_policy_exact_and_resume: impl FnOnce(
            &AdmissionGuard<'_>,
            &DelegationAcceptanceV1,
        ) -> Result<()>,
    ) -> Result<DelegationRunV1> {
        let run = &input.run;
        if input.task.workspace_id != run.workspace_id || input.task.task_id != run.task_id {
            return Err(DelegationErrorV1::PermissionDenied);
        }
        let scopes = BTreeSet::from([
            AdmissionSubject::Plan(input.task.plan.plan_id.clone()),
            AdmissionSubject::Permit(run.permit_id.clone()),
        ]);
        let guard = self
            .gate
            .enter(
                &run.workspace_id,
                &scopes,
                if run.continued_from.is_some() {
                    AdmissionAction::Continue
                } else {
                    AdmissionAction::Start
                },
                &run.run_id,
            )
            .map_err(|_| DelegationErrorV1::PermissionDenied)?;
        let permit = run.profile_permit()?;
        let deadline =
            permit.authorize(&run.execution, input.admitted_at_ms, run.permit_generation)?;
        if deadline != run.deadline_ms {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        self.safety.check(
            &RunSafetyBinding {
                workspace: run.workspace_id.clone(),
                daemon_epoch: run.daemon_epoch.clone(),
                permit_id: run.permit_id.clone(),
                permit_generation: run.permit_generation,
                expires_at_ms: run.deadline_ms,
            },
            input.admitted_at_ms,
        )?;
        check_policy_exact_and_resume(&guard, input)?;
        self.runtime.accept(input)
    }
}
