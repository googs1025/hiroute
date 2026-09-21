//! Task detail/cancel/recovery use the existing authenticated control boundary.
//! No management authority is inferred from a task ID, same UID, or Worker query credential.
use hiroute_domain::delegation::*;
use hiroute_domain::{OperationId, WorkspaceId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelegationControlAction {
    Read,
    Cancel,
    ConfirmResidual,
}

/// A per-request adapter over the real authenticated caller and current grants. Returning
/// an actor identifier is for the operation record only; it does not create a capability.
/// ConfirmResidual requires an explicit local user action, never a Worker's self-query token.
pub trait DelegationControlAccess {
    fn authorize(
        &self,
        run: &DelegationRunV1,
        action: DelegationControlAction,
    ) -> Result<String, DelegationErrorV1>;
}

/// Inject the active run-token revoker. Denial happens before durable intent and never rolls
/// back if storage fails. This is separate from process termination and does not await ACP.
pub trait DelegationRunDenial {
    fn deny(&self, run: &DelegationRunV1) -> Result<(), DelegationErrorV1>;
}

pub struct DelegationControl<'a> {
    pub runtime: &'a dyn DelegationRuntimePort,
    pub access: &'a dyn DelegationControlAccess,
    pub denial: &'a dyn DelegationRunDenial,
}

impl DelegationControl<'_> {
    pub fn read(
        &self,
        workspace: &WorkspaceId,
        run_id: &str,
    ) -> Result<DelegationRunV1, DelegationErrorV1> {
        let run = self.load(workspace, run_id)?;
        self.access.authorize(&run, DelegationControlAction::Read)?;
        Ok(run)
    }

    pub fn cancel(
        &self,
        workspace: &WorkspaceId,
        run_id: &str,
        operation: &OperationId,
        reason: &str,
    ) -> Result<DelegationCancelReceiptV1, DelegationErrorV1> {
        let run = self.load(workspace, run_id)?;
        self.access
            .authorize(&run, DelegationControlAction::Cancel)?;
        self.denial.deny(&run)?;
        self.runtime
            .request_cancel(workspace, run_id, operation, reason)
    }

    pub fn confirm_residual(
        &self,
        workspace: &WorkspaceId,
        run_id: &str,
        expected_revision: u64,
        operation: &OperationId,
    ) -> Result<DelegationRunV1, DelegationErrorV1> {
        let run = self.load(workspace, run_id)?;
        let actor_id = self
            .access
            .authorize(&run, DelegationControlAction::ConfirmResidual)?;
        self.denial.deny(&run)?;
        // The store's revision/event-key transaction both rejects known-live confirmation
        // and makes a repeated user action idempotent. It never emits another prompt.
        self.runtime.checkpoint(
            workspace,
            run_id,
            expected_revision,
            &format!("residual:{operation}"),
            &DelegationCheckpointV1::ResidualConfirmed {
                operation_id: operation.clone(),
                actor_id,
            },
        )
    }

    fn load(
        &self,
        workspace: &WorkspaceId,
        run_id: &str,
    ) -> Result<DelegationRunV1, DelegationErrorV1> {
        self.runtime
            .run(workspace, run_id)?
            .ok_or(DelegationErrorV1::PermissionDenied)
    }
}

#[cfg(test)]
mod tests;
