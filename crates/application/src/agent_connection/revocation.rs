//! A short effect inside the existing Operation, using the injected 13 gate and 20 safety view.
//! Filesystem cleanup, cancel delivery and waits belong to subsequent effects outside this call.
use crate::delegation::safety::{RunAuthorizationScope, RunSafetyProjection};
use crate::publication::admission::{AdmissionAction, AdmissionSubject, SharedAdmissionGate};
use hiroute_domain::{AgentCollaborationRevocation, AgentCollaborationRevocationStorePort};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CollaborationRevocationError {
    #[error("collaboration revocation checkpoint is invalid")]
    InvalidCheckpoint,
    #[error("collaboration revocation requires the original Operation to recover")]
    RecoveryRequired,
}

/// The existing Operation writer must already belong to this checkpoint's operation.
/// On uncertainty, both gate recovery and the dynamic deny stay in place for the affected scope.
pub fn record_collaboration_revocation(
    gate: &SharedAdmissionGate,
    safety: &RunSafetyProjection,
    store: &dyn AgentCollaborationRevocationStorePort,
    checkpoint: &AgentCollaborationRevocation,
    recovering: bool,
) -> Result<(), CollaborationRevocationError> {
    checkpoint
        .validate()
        .map_err(|_| CollaborationRevocationError::InvalidCheckpoint)?;
    let scope = BTreeSet::from([AdmissionSubject::Grant(checkpoint.after.grant_id.clone())]);
    let action = if recovering {
        AdmissionAction::Recovery
    } else {
        AdmissionAction::AuthorizationChange
    };
    let mut guard = gate
        .enter(
            &checkpoint.after.workspace_id,
            &scope,
            action,
            checkpoint.operation_id.as_str(),
        )
        .map_err(|_| CollaborationRevocationError::RecoveryRequired)?;
    guard
        .mark_recovery_required()
        .map_err(|_| CollaborationRevocationError::RecoveryRequired)?;
    let denied = [RunAuthorizationScope::Grant {
        id: checkpoint.after.grant_id.clone(),
        through_generation: checkpoint.through_generation,
    }];
    safety
        .install_deny(&guard, &checkpoint.operation_id, &denied)
        .map_err(|_| CollaborationRevocationError::RecoveryRequired)?;
    // The store commits the new generation and its ORIGINAL Operation cancellation intent
    // together. A failure may be an uncertain commit: never restore an old allow projection.
    store
        .persist_collaboration_revocation(checkpoint)
        .map_err(|_| CollaborationRevocationError::RecoveryRequired)?;
    safety
        .mark_recorded(&guard, &checkpoint.operation_id, &denied)
        .map_err(|_| CollaborationRevocationError::RecoveryRequired)?;
    guard
        .complete_recovery()
        .map_err(|_| CollaborationRevocationError::RecoveryRequired)?;
    Ok(())
}

#[cfg(test)]
mod tests;
