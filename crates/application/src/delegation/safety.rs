//! Nonblocking revocation projection, shared by every admitted run and Gateway Attempt.
//! This is neither an authorization database nor the admission lock. Writers must hold
//! the one injected MVP-13 gate; the original Operation owns durable revoke/cancel intent.
use crate::publication::admission::{AdmissionGuard, AdmissionSubject, SharedAdmissionGate};
use arc_swap::ArcSwap;
use hiroute_domain::delegation::DelegationErrorV1;
use hiroute_domain::{OperationId, WorkspaceId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunAuthorizationScope {
    Grant { id: String, through_generation: u64 },
    Permit { id: String, through_generation: u64 },
}

impl RunAuthorizationScope {
    fn subject(&self) -> AdmissionSubject {
        match self {
            Self::Grant { id, .. } => AdmissionSubject::Grant(id.clone()),
            Self::Permit { id, .. } => AdmissionSubject::Permit(id.clone()),
        }
    }
    fn generation(&self) -> u64 {
        match self {
            Self::Grant {
                through_generation, ..
            }
            | Self::Permit {
                through_generation, ..
            } => *through_generation,
        }
    }
}

/// These are fixed values from a committed run lease, not caller-supplied header metadata.
#[derive(Clone, Debug)]
pub struct RunSafetyBinding {
    pub workspace: WorkspaceId,
    pub daemon_epoch: String,
    pub permit_id: String,
    pub permit_generation: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Default)]
struct Denial {
    through_generation: u64,
    unresolved: BTreeSet<OperationId>,
}

type Projection = BTreeMap<(WorkspaceId, AdmissionSubject), Denial>;

pub struct RunSafetyProjection {
    gate: Arc<SharedAdmissionGate>,
    daemon_epoch: String,
    recovering: AtomicBool,
    denied: ArcSwap<Projection>,
}

impl RunSafetyProjection {
    pub fn new(
        gate: Arc<SharedAdmissionGate>,
        daemon_epoch: String,
    ) -> Result<Self, DelegationErrorV1> {
        if daemon_epoch.is_empty() || daemon_epoch.len() > 128 {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        Ok(Self {
            gate,
            daemon_epoch,
            recovering: AtomicBool::new(true),
            denied: ArcSwap::from_pointee(BTreeMap::new()),
        })
    }

    /// Daemon startup only: finish after restoring all persisted revocations and unresolved
    /// Operations, before accepting runs. Normal mutations never toggle global recovery.
    pub fn finish_startup_recovery(&self) {
        self.recovering.store(false, Ordering::Release);
    }

    /// First step under the same writer/gate, before durable revocation. A storage error must
    /// leave this affected scope denied. It does not block unrelated grants or workspaces.
    pub fn install_deny(
        &self,
        guard: &AdmissionGuard<'_>,
        operation: &OperationId,
        scopes: &[RunAuthorizationScope],
    ) -> Result<(), DelegationErrorV1> {
        self.validate_guard(guard, operation, scopes)?;
        let mut next = (**self.denied.load()).clone();
        for scope in scopes {
            let denial = next
                .entry((guard.workspace().clone(), scope.subject()))
                .or_default();
            denial.through_generation = denial.through_generation.max(scope.generation());
            denial.unresolved.insert(operation.clone());
        }
        self.denied.store(Arc::new(next));
        Ok(())
    }

    /// Only after the original Operation's revoke generation AND idempotent cancel intents
    /// are durable. This clears uncertainty for that Operation, never its revocation floor.
    /// Filesystem compensation or a failed process cancellation must not undo this floor.
    pub fn mark_recorded(
        &self,
        guard: &AdmissionGuard<'_>,
        operation: &OperationId,
        scopes: &[RunAuthorizationScope],
    ) -> Result<(), DelegationErrorV1> {
        self.validate_guard(guard, operation, scopes)?;
        let mut next = (**self.denied.load()).clone();
        for scope in scopes {
            let Some(denial) = next.get_mut(&(guard.workspace().clone(), scope.subject())) else {
                return Err(DelegationErrorV1::Conflict);
            };
            if denial.through_generation < scope.generation() {
                return Err(DelegationErrorV1::Conflict);
            }
            denial.unresolved.remove(operation);
        }
        self.denied.store(Arc::new(next));
        Ok(())
    }

    /// Called afresh for every model request, every Attempt and each permission request.
    /// No SQLite, writer lock or admission gate. Cached bindings never cache this verdict.
    pub fn check(&self, binding: &RunSafetyBinding, now_ms: u64) -> Result<(), DelegationErrorV1> {
        if self.recovering.load(Ordering::Acquire)
            || binding.daemon_epoch != self.daemon_epoch
            || binding.permit_generation == 0
            || binding.permit_id.is_empty()
            || now_ms >= binding.expires_at_ms
        {
            return Err(DelegationErrorV1::PermissionDenied);
        }
        let current = self.denied.load();
        for (subject, generation) in [(
            AdmissionSubject::Permit(binding.permit_id.clone()),
            binding.permit_generation,
        )] {
            if let Some(denial) = current.get(&(binding.workspace.clone(), subject))
                && (!denial.unresolved.is_empty() || generation <= denial.through_generation)
            {
                return Err(DelegationErrorV1::PermissionDenied);
            }
        }
        Ok(())
    }

    fn validate_guard(
        &self,
        guard: &AdmissionGuard<'_>,
        operation: &OperationId,
        scopes: &[RunAuthorizationScope],
    ) -> Result<(), DelegationErrorV1> {
        if guard.action_id() != operation.as_str()
            || !guard.belongs_to(&self.gate)
            || scopes.is_empty()
            || scopes.len() > 1024
            || scopes
                .iter()
                .any(|scope| scope.generation() == 0 || !guard.covers(&scope.subject()))
        {
            return Err(DelegationErrorV1::PermissionDenied);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
