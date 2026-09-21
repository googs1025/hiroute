//! Original Operation adapters. Caller owns the existing writer before acquiring this gate.
use super::safety::{RunAuthorizationScope, RunSafetyProjection};
use crate::publication::admission::{AdmissionGuard, AdmissionSubject, SharedAdmissionGate};
use hiroute_domain::delegation::*;
use hiroute_domain::{
    AgentCollaborationRevocation, AgentCollaborationRevocationStorePort, CanonicalDigest,
    OperationId, WorkspaceId,
};
use std::sync::Arc;

type Result<T> = std::result::Result<T, DelegationErrorV1>;
pub struct DelegationAuthorization<'a> {
    pub gate: Arc<SharedAdmissionGate>,
    pub safety: &'a RunSafetyProjection,
    pub permits: &'a dyn DelegationPermitStorePort,
    pub cancellations: &'a dyn DelegationScopeCancellationPort,
}
impl DelegationAuthorization<'_> {
    pub fn preview_permit(
        &self,
        workspace: &WorkspaceId,
        operation: &OperationId,
        after: WorkspaceExecutionPermitV1,
    ) -> Result<(DelegationPermitMutationV1, CanonicalDigest)> {
        let change = DelegationPermitMutationV1 {
            workspace: workspace.clone(),
            operation: operation.clone(),
            before: self.permits.permit(workspace, &after.permit_id)?,
            after,
        };
        let digest = change.digest()?;
        Ok((change, digest))
    }
    pub fn commit_permit(
        &self,
        guard: &mut AdmissionGuard<'_>,
        change: &DelegationPermitMutationV1,
        accepted_digest: &CanonicalDigest,
    ) -> Result<()> {
        if &change.digest()? != accepted_digest {
            return Err(DelegationErrorV1::Conflict);
        }
        self.require_operation(guard, &change.operation)?;
        self.require(
            guard,
            &change.workspace,
            &AdmissionSubject::Permit(change.after.permit_id.clone()),
        )?;
        self.permits.prepare_permit(change)?;
        guard
            .mark_recovery_required()
            .map_err(|_| DelegationErrorV1::Conflict)?;
        if let Some(scope) = change.revoked_scope() {
            self.install(guard, &change.operation, &scope)?;
        }
        self.permits.commit_permit(change)?;
        if let Some(scope) = change.revoked_scope() {
            self.record(guard, &change.operation, &scope)?;
        }
        // The original settings owner clears its gate barrier only after reconciling ALL
        // of its side effects. A permit receipt cannot complete that larger Operation.
        Ok(())
    }
    pub fn revoke_grant(
        &self,
        guard: &mut AdmissionGuard<'_>,
        change: &AgentCollaborationRevocation,
        store: &dyn AgentCollaborationRevocationStorePort,
    ) -> Result<String> {
        self.require_operation(guard, &change.operation_id)?;
        change
            .validate()
            .map_err(|_| DelegationErrorV1::InvalidArguments)?;
        let scope = DelegationAuthorizationScopeV1::Grant {
            id: change.after.grant_id.clone(),
            through_generation: change.through_generation,
        };
        self.require(guard, &change.after.workspace_id, &subject(&scope))?;
        guard
            .mark_recovery_required()
            .map_err(|_| DelegationErrorV1::Conflict)?;
        self.install(guard, &change.operation_id, &scope)?;
        store
            .persist_collaboration_revocation(change)
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?;
        let receipt = self.record(guard, &change.operation_id, &scope)?;
        Ok(receipt)
    }
    /// Replays the original durable scope after restart, including completed revocation floors.
    /// Call before finish_startup_recovery; it never resurrects credentials or sends a prompt.
    pub fn recover_scope(
        &self,
        guard: &mut AdmissionGuard<'_>,
        operation: &OperationId,
        scope: &DelegationAuthorizationScopeV1,
    ) -> Result<String> {
        self.require_operation(guard, operation)?;
        self.require(guard, guard.workspace(), &subject(scope))?;
        guard
            .mark_recovery_required()
            .map_err(|_| DelegationErrorV1::Conflict)?;
        self.install(guard, operation, scope)?;
        let receipt = self.record(guard, operation, scope)?;
        Ok(receipt)
    }
    pub fn restore_pending_permit(
        &self,
        guard: &mut AdmissionGuard<'_>,
        change: &DelegationPermitMutationV1,
    ) -> Result<()> {
        change.validate()?;
        self.require_operation(guard, &change.operation)?;
        self.require(
            guard,
            &change.workspace,
            &AdmissionSubject::Permit(change.after.permit_id.clone()),
        )?;
        guard
            .mark_recovery_required()
            .map_err(|_| DelegationErrorV1::Conflict)?;
        if let Some(scope) = change.revoked_scope() {
            self.install(guard, &change.operation, &scope)?;
        }
        Ok(())
    }
    fn require_operation(&self, guard: &AdmissionGuard<'_>, operation: &OperationId) -> Result<()> {
        if guard.action_id() != operation.as_str() {
            return Err(DelegationErrorV1::PermissionDenied);
        }
        Ok(())
    }
    fn require(
        &self,
        guard: &AdmissionGuard<'_>,
        workspace: &WorkspaceId,
        scope: &AdmissionSubject,
    ) -> Result<()> {
        if !guard.belongs_to(&self.gate) || guard.workspace() != workspace || !guard.covers(scope) {
            return Err(DelegationErrorV1::PermissionDenied);
        }
        Ok(())
    }
    fn install(
        &self,
        guard: &AdmissionGuard<'_>,
        operation: &OperationId,
        scope: &DelegationAuthorizationScopeV1,
    ) -> Result<()> {
        scope.validate()?;
        self.safety
            .install_deny(guard, operation, &[projection(scope)])
    }
    fn record(
        &self,
        guard: &AdmissionGuard<'_>,
        operation: &OperationId,
        scope: &DelegationAuthorizationScopeV1,
    ) -> Result<String> {
        let receipt = self
            .cancellations
            .cancel_scope(guard.workspace(), operation, scope)?;
        self.safety
            .mark_recorded(guard, operation, &[projection(scope)])?;
        Ok(receipt)
    }
}
pub fn subject(scope: &DelegationAuthorizationScopeV1) -> AdmissionSubject {
    match scope {
        DelegationAuthorizationScopeV1::Grant { id, .. } => AdmissionSubject::Grant(id.clone()),
        DelegationAuthorizationScopeV1::Permit { id, .. } => AdmissionSubject::Permit(id.clone()),
    }
}
fn projection(scope: &DelegationAuthorizationScopeV1) -> RunAuthorizationScope {
    match scope {
        DelegationAuthorizationScopeV1::Grant {
            id,
            through_generation,
        } => RunAuthorizationScope::Grant {
            id: id.clone(),
            through_generation: *through_generation,
        },
        DelegationAuthorizationScopeV1::Permit {
            id,
            through_generation,
        } => RunAuthorizationScope::Permit {
            id: id.clone(),
            through_generation: *through_generation,
        },
    }
}
