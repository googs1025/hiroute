//! Permit state and its original settings Operation checkpoint in control.db.
use super::*;
use hiroute_domain::delegation::*;

type Result<T> = std::result::Result<T, DelegationErrorV1>;
fn unavailable(_: rusqlite::Error) -> DelegationErrorV1 {
    DelegationErrorV1::StorageUnavailable
}

impl DelegationGrantAuthorityPort for ControlStore {
    fn principal_for_credential(
        &self,
        material: &hiroute_domain::AgentCollaborationCredential,
    ) -> Result<hiroute_domain::VerifiedCollaborationPrincipal> {
        self.collaboration_principal_for_credential(material)
            .map_err(|error| match error.code {
                hiroute_domain::PortErrorCode::PermissionDenied => {
                    DelegationErrorV1::PermissionDenied
                }
                _ => DelegationErrorV1::StorageUnavailable,
            })
    }

    fn current_grant(
        &self,
        workspace: &WorkspaceId,
        id: &str,
    ) -> Result<Option<hiroute_domain::AgentCollaborationGrant>> {
        self.collaboration_grant(workspace, id)
            .map_err(|_| DelegationErrorV1::StorageUnavailable)
    }
}
impl DelegationPermitStorePort for ControlStore {
    fn permit(
        &self,
        workspace: &WorkspaceId,
        id: &str,
    ) -> Result<Option<WorkspaceExecutionPermitV1>> {
        read(&self.connection.borrow(), workspace, id)
    }
    fn prepare_permit(&self, change: &DelegationPermitMutationV1) -> Result<()> {
        self.write_permit(change, false)
    }
    fn commit_permit(&self, change: &DelegationPermitMutationV1) -> Result<()> {
        self.write_permit(change, true)
    }
    fn permit_mutations(&self, workspace: &WorkspaceId) -> Result<Vec<DelegationPermitMutationV1>> {
        self.read_permit_mutations(workspace, true)
    }
    fn pending_permit_mutations(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<DelegationPermitMutationV1>> {
        self.read_permit_mutations(workspace, false)
    }
}
impl ControlStore {
    fn write_permit(&self, change: &DelegationPermitMutationV1, apply: bool) -> Result<()> {
        change.validate()?;
        let mut conn = self.connection.borrow_mut();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let writer: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM writer_claim w JOIN operations o ON o.operation_id=w.operation_id WHERE w.singleton=1 AND w.operation_id=?1 AND o.workspace_id=?2)",params![change.operation.as_str(), change.workspace.as_str()], |r|r.get(0)).map_err(unavailable)?;
        if !writer {
            return Err(DelegationErrorV1::PermissionDenied);
        }
        let encoded =
            serde_json::to_string(change).map_err(|_| DelegationErrorV1::InvalidArguments)?;
        let prior: Option<(String,bool)> = tx.query_row("SELECT checkpoint_json,applied FROM delegation_permit_operations WHERE operation_id=?1 AND permit_id=?2",params![change.operation.as_str(),change.after.permit_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(unavailable)?;
        if let Some((prior, applied)) = &prior {
            if prior != &encoded {
                return Err(DelegationErrorV1::Conflict);
            }
            if *applied || !apply {
                return Ok(());
            }
        }
        if read(&tx, &change.workspace, &change.after.permit_id)? != change.before {
            return Err(DelegationErrorV1::Conflict);
        }
        if prior.is_none() {
            tx.execute("INSERT INTO delegation_permit_operations(operation_id,workspace_id,permit_id,checkpoint_json,applied) VALUES(?1,?2,?3,?4,0)",params![change.operation.as_str(),change.workspace.as_str(),change.after.permit_id,encoded]).map_err(unavailable)?;
        }
        if !apply {
            return tx.commit().map_err(unavailable);
        }
        let record = serde_json::to_string(&change.after)
            .map_err(|_| DelegationErrorV1::InvalidArguments)?;
        tx.execute("INSERT INTO delegation_permits(workspace_id,permit_id,record_json) VALUES(?1,?2,?3) ON CONFLICT(workspace_id,permit_id) DO UPDATE SET record_json=excluded.record_json",params![change.workspace.as_str(),change.after.permit_id,record]).map_err(unavailable)?;
        tx.execute("UPDATE delegation_permit_operations SET applied=1 WHERE operation_id=?1 AND permit_id=?2",params![change.operation.as_str(),change.after.permit_id]).map_err(unavailable)?;
        tx.commit().map_err(unavailable)
    }
    fn read_permit_mutations(
        &self,
        workspace: &WorkspaceId,
        applied: bool,
    ) -> Result<Vec<DelegationPermitMutationV1>> {
        let conn = self.connection.borrow();
        let mut stmt = conn.prepare("SELECT checkpoint_json FROM delegation_permit_operations WHERE workspace_id=?1 AND applied=?2 ORDER BY rowid LIMIT 10001").map_err(unavailable)?;
        let rows = stmt
            .query_map(params![workspace.as_str(), applied], |r| {
                r.get::<_, String>(0)
            })
            .map_err(unavailable)?;
        let mut result = vec![];
        for row in rows {
            let change: DelegationPermitMutationV1 =
                serde_json::from_str(&row.map_err(unavailable)?)
                    .map_err(|_| DelegationErrorV1::StorageUnavailable)?;
            change.validate()?;
            if &change.workspace != workspace || result.len() == 10000 {
                return Err(DelegationErrorV1::StorageUnavailable);
            }
            result.push(change);
        }
        Ok(result)
    }
}
fn read(
    conn: &Connection,
    workspace: &WorkspaceId,
    id: &str,
) -> Result<Option<WorkspaceExecutionPermitV1>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT record_json FROM delegation_permits WHERE workspace_id=?1 AND permit_id=?2",
            params![workspace.as_str(), id],
            |r| r.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    json.map(|json| {
        let permit: WorkspaceExecutionPermitV1 =
            serde_json::from_str(&json).map_err(|_| DelegationErrorV1::StorageUnavailable)?;
        permit.validate()?;
        if permit.permit_id != id {
            return Err(DelegationErrorV1::StorageUnavailable);
        }
        Ok(permit)
    })
    .transpose()
}
