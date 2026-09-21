//! Trusted same-daemon version port for 20. This is not a public historical execution API.
use super::admission::AdmissionGuard;
use hiroute_domain::{
    AgentPlanId, PlanExecutionRef, PlanHeadV1, PlanVersionError, PlanVersionV1, VersionOwnerRefV1,
    VersionReservationV1, WorkspaceId,
};
use std::sync::Arc;

pub trait ExactPlanVersionPort: Send + Sync {
    /// Caller checks current enabled/ref/grant/permit/limits under the SAME guard and keeps it
    /// alive until its runtime acceptance commits. This read does not constitute admission.
    fn current_plan(
        &self,
        guard: &AdmissionGuard<'_>,
        plan: &AgentPlanId,
    ) -> Result<PlanHeadV1, PlanVersionError>;
    fn acquire_exact(
        &self,
        guard: &AdmissionGuard<'_>,
        reservation: &VersionReservationV1,
    ) -> Result<Arc<PlanVersionV1>, PlanVersionError>;
    /// Internal authorized lookup; does not retain content or provide a run credential.
    fn lookup_exact(
        &self,
        reference: &PlanExecutionRef,
    ) -> Result<Arc<PlanVersionV1>, PlanVersionError>;
    fn renew(
        &self,
        workspace: &WorkspaceId,
        owner: &VersionOwnerRefV1,
        expires_at_unix: i64,
    ) -> Result<(), PlanVersionError>;
    fn release(
        &self,
        workspace: &WorkspaceId,
        owner: &VersionOwnerRefV1,
    ) -> Result<(), PlanVersionError>;
    /// Complete authoritative 20 owner set, never a page. Run before opening admission at
    /// startup; later reconciles participate in the shared gate protocol. No runtime transaction
    /// may remain open while this method enters the control store.
    fn reconcile(
        &self,
        workspace: &WorkspaceId,
        owners: &[VersionReservationV1],
        now_unix: i64,
    ) -> Result<(), PlanVersionError>;
}
