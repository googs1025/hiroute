//! Single storage authority shared with Plan publication; no second version database/cache.
use super::{Arc, LocalControlAdapter};
use hiroute_application::publication::admission::{AdmissionGuard, AdmissionSubject};
use hiroute_application::publication::versions::ExactPlanVersionPort;
use hiroute_domain::{
    AgentPlanId, PlanExecutionRef, PlanHeadV1, PlanVersionError, PlanVersionV1, VersionOwnerRefV1,
    VersionReservationV1, WorkspaceId,
};

impl LocalControlAdapter {
    fn verify_plan_guard(
        &self,
        guard: &AdmissionGuard<'_>,
        workspace: &WorkspaceId,
        plan: &AgentPlanId,
    ) -> Result<(), PlanVersionError> {
        if !guard.belongs_to(&self.plan_admission)
            || guard.workspace() != workspace
            || !guard.covers(&AdmissionSubject::Plan(plan.clone()))
        {
            return Err(PlanVersionError::Conflict);
        }
        Ok(())
    }
}

impl ExactPlanVersionPort for LocalControlAdapter {
    fn current_plan(
        &self,
        guard: &AdmissionGuard<'_>,
        plan: &AgentPlanId,
    ) -> Result<PlanHeadV1, PlanVersionError> {
        self.verify_plan_guard(guard, guard.workspace(), plan)?;
        self.stores
            .lock()
            .map_err(|_| PlanVersionError::StorageUnavailable)?
            .control()
            .plan_head(guard.workspace(), plan)?
            .ok_or(PlanVersionError::Unavailable)
    }

    fn acquire_exact(
        &self,
        guard: &AdmissionGuard<'_>,
        reservation: &VersionReservationV1,
    ) -> Result<Arc<PlanVersionV1>, PlanVersionError> {
        self.verify_plan_guard(
            guard,
            &reservation.reference.workspace_id,
            &reservation.reference.plan_id,
        )?;
        self.stores
            .lock()
            .map_err(|_| PlanVersionError::StorageUnavailable)?
            .control()
            .acquire_exact_plan_version(reservation)
            .map(Arc::new)
    }

    fn lookup_exact(
        &self,
        reference: &PlanExecutionRef,
    ) -> Result<Arc<PlanVersionV1>, PlanVersionError> {
        self.stores
            .lock()
            .map_err(|_| PlanVersionError::StorageUnavailable)?
            .control()
            .lookup_exact_plan_version(reference)
            .map(Arc::new)
    }

    fn renew(
        &self,
        workspace: &WorkspaceId,
        owner: &VersionOwnerRefV1,
        expiry: i64,
    ) -> Result<(), PlanVersionError> {
        self.stores
            .lock()
            .map_err(|_| PlanVersionError::StorageUnavailable)?
            .control()
            .renew_plan_version(workspace, owner, expiry)
    }

    fn release(
        &self,
        workspace: &WorkspaceId,
        owner: &VersionOwnerRefV1,
    ) -> Result<(), PlanVersionError> {
        self.stores
            .lock()
            .map_err(|_| PlanVersionError::StorageUnavailable)?
            .control()
            .release_plan_version(workspace, owner)
    }

    fn reconcile(
        &self,
        workspace: &WorkspaceId,
        owners: &[VersionReservationV1],
        now: i64,
    ) -> Result<(), PlanVersionError> {
        self.stores
            .lock()
            .map_err(|_| PlanVersionError::StorageUnavailable)?
            .control()
            .reconcile_plan_versions(workspace, owners, now)
    }
}
