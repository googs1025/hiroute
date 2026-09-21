//! Restore the existing control-store Operation checkpoints before opening run admission.
use hiroute_application::delegation::{
    authorization::{DelegationAuthorization, subject},
    safety::RunSafetyProjection,
};
use hiroute_application::publication::admission::{AdmissionAction, SharedAdmissionGate};
use hiroute_domain::delegation::*;
use hiroute_domain::{OperationId, WorkspaceId};
use hiroute_local_storage::LocalStorageSet;
use std::{collections::BTreeSet, sync::Arc};

/// The startup owner keeps RunSafetyProjection in recovery mode across ALL workspaces and
/// unresolved Operations. This function does not mark the whole settings Operation complete,
/// clear another owner's barrier or start an ACP process. After restoring every workspace,
/// the owner may finish_startup_recovery and dispatch_pending outside the gate.
pub fn recover_authorizations(
    stores: &LocalStorageSet,
    workspace: &WorkspaceId,
    gate: Arc<SharedAdmissionGate>,
    safety: &RunSafetyProjection,
) -> Result<(), DelegationErrorV1> {
    let auth = DelegationAuthorization {
        gate: gate.clone(),
        safety,
        permits: stores.control(),
        cancellations: stores.runtime(),
    };
    let mut checkpoints: Vec<(OperationId, DelegationAuthorizationScopeV1)> = Vec::new();
    for (change, _, _) in stores
        .control()
        .collaboration_revocations(workspace)
        .map_err(|_| DelegationErrorV1::StorageUnavailable)?
    {
        checkpoints.push((
            change.operation_id,
            DelegationAuthorizationScopeV1::Grant {
                id: change.after.grant_id,
                through_generation: change.through_generation,
            },
        ));
    }
    for (operation, scope) in checkpoints {
        let mut guard = gate
            .enter(
                workspace,
                &BTreeSet::from([subject(&scope)]),
                AdmissionAction::Recovery,
                operation.as_str(),
            )
            .map_err(|_| DelegationErrorV1::Conflict)?;
        auth.recover_scope(&mut guard, &operation, &scope)?;
        // This guard covers exactly the persisted authorization subject, not the original
        // settings Operation's other file/installation subjects.
        guard
            .complete_recovery()
            .map_err(|_| DelegationErrorV1::Conflict)?;
    }
    Ok(())
}
