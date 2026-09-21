//! The single daemon-local control admission gate shared by Plan, grant and run owners.
//!
//! Lock order: the existing Operation writer (when needed), this gate, then sequential short
//! storage transactions. Never acquire the writer or hold two database transactions under a
//! guard. Compilation, body/file IO, process/ACP work and network waits belong outside it.
//! Gateway request/Attempt authorization does not acquire this gate.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

use hiroute_domain::{AgentPlanId, WorkspaceId};
use thiserror::Error;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AdmissionSubject {
    Plan(AgentPlanId),
    Grant(String),
    Permit(String),
}

impl AdmissionSubject {
    fn valid(&self) -> bool {
        match self {
            Self::Plan(id) => AgentPlanId::parse(id.as_str()).is_ok(),
            Self::Grant(id) | Self::Permit(id) => valid_identity(id),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionAction {
    Start,
    Continue,
    PlanChange,
    AuthorizationChange,
    /// Replays the existing durable Operation with the same stable action ID.
    Recovery,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AdmissionGateError {
    #[error("the affected scope requires Operation recovery")]
    RecoveryRequired,
    #[error("admission identity or scope is invalid")]
    InvalidIdentity,
    #[error("the admission gate is unavailable")]
    Unavailable,
    #[error("this admission action cannot change recovery state")]
    InvalidAction,
}

#[derive(Default)]
struct GateState {
    // A bounded projection of existing unfinished Operations, not another journal or grant DB.
    pending: BTreeMap<(WorkspaceId, AdmissionSubject), BTreeSet<String>>,
}

/// Construct once in daemon composition and inject the same Arc into 13/14/20. The gate does
/// not own grant/permit/run data or confer execution authority. After restart, owners restore
/// unfinished scope barriers from their journals before enabling admission.
#[derive(Default)]
pub struct SharedAdmissionGate {
    state: Mutex<GateState>,
}

impl SharedAdmissionGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enter<'a>(
        &'a self,
        workspace: &WorkspaceId,
        affected_scope: &BTreeSet<AdmissionSubject>,
        action: AdmissionAction,
        stable_action_id: &str,
    ) -> Result<AdmissionGuard<'a>, AdmissionGateError> {
        if WorkspaceId::parse(workspace.as_str()).is_err()
            || affected_scope.is_empty()
            || affected_scope.len() > 1024
            || !affected_scope.iter().all(AdmissionSubject::valid)
            || !valid_identity(stable_action_id)
        {
            return Err(AdmissionGateError::InvalidIdentity);
        }
        let state = self
            .state
            .lock()
            .map_err(|_| AdmissionGateError::Unavailable)?;
        for subject in affected_scope {
            if let Some(owners) = state.pending.get(&(workspace.clone(), subject.clone()))
                && owners
                    .iter()
                    .any(|owner| action != AdmissionAction::Recovery || owner != stable_action_id)
            {
                return Err(AdmissionGateError::RecoveryRequired);
            }
        }
        Ok(AdmissionGuard {
            gate: self,
            state,
            workspace: workspace.clone(),
            scope: affected_scope.clone(),
            action,
            action_id: stable_action_id.to_owned(),
        })
    }
}

/// Keep alive from current-state validation through the runtime DB acceptance commit (20), or
/// the effective Plan/grant/permit transition (13/14/20). A reservation is not acceptance.
/// The MutexGuard is deliberately not transferable across threads or an async suspension.
pub struct AdmissionGuard<'a> {
    gate: &'a SharedAdmissionGate,
    state: MutexGuard<'a, GateState>,
    workspace: WorkspaceId,
    scope: BTreeSet<AdmissionSubject>,
    action: AdmissionAction,
    action_id: String,
}

impl AdmissionGuard<'_> {
    pub fn action_id(&self) -> &str {
        &self.action_id
    }

    pub fn belongs_to(&self, gate: &SharedAdmissionGate) -> bool {
        std::ptr::eq(self.gate, gate)
    }

    pub fn workspace(&self) -> &WorkspaceId {
        &self.workspace
    }

    pub fn covers(&self, subject: &AdmissionSubject) -> bool {
        self.scope.contains(subject)
    }

    /// Call for the same recorded Operation before releasing an uncertain transition. This
    /// controls new task admission only; 20 must first install its own nonblocking Gateway
    /// safety barrier for revocation. Persisted revocation is never undone by clearing this.
    pub fn mark_recovery_required(&mut self) -> Result<(), AdmissionGateError> {
        self.require_mutation()?;
        for subject in &self.scope {
            self.state
                .pending
                .entry((self.workspace.clone(), subject.clone()))
                .or_default()
                .insert(self.action_id.clone());
        }
        Ok(())
    }

    /// The caller must have reconciled the original Operation and the current security
    /// projection. Removes only this action's scope; it cannot clear another owner's barrier.
    pub fn complete_recovery(&mut self) -> Result<(), AdmissionGateError> {
        self.require_mutation()?;
        for subject in &self.scope {
            let key = (self.workspace.clone(), subject.clone());
            if let Some(owners) = self.state.pending.get_mut(&key) {
                owners.remove(&self.action_id);
                if owners.is_empty() {
                    self.state.pending.remove(&key);
                }
            }
        }
        Ok(())
    }

    fn require_mutation(&self) -> Result<(), AdmissionGateError> {
        if matches!(
            self.action,
            AdmissionAction::Start | AdmissionAction::Continue
        ) {
            Err(AdmissionGateError::InvalidAction)
        } else {
            Ok(())
        }
    }
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':')
        })
}

#[cfg(test)]
mod tests;
