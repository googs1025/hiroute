//! Original-Operation CAS persistence for shared Skill references. No file effects here.
use super::{ControlStore, collaboration_store::require_writer, port};
use hiroute_domain::{
    ManagedCollaborationSkill, OperationId, PortErrorCode, PortResult, WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

impl hiroute_domain::AgentSkillInstallationStorePort for ControlStore {
    fn skill_installation(
        &self,
        workspace: &WorkspaceId,
        root_ref: &str,
    ) -> PortResult<Option<ManagedCollaborationSkill>> {
        ControlStore::skill_installation(self, workspace, root_ref)
    }

    fn store_skill_installation(
        &self,
        workspace: &WorkspaceId,
        operation: &OperationId,
        expected_revision: u64,
        next: &ManagedCollaborationSkill,
    ) -> PortResult<()> {
        ControlStore::store_skill_installation(self, workspace, operation, expected_revision, next)
    }
}

impl ControlStore {
    pub fn skill_installation(
        &self,
        workspace: &WorkspaceId,
        root_ref: &str,
    ) -> PortResult<Option<ManagedCollaborationSkill>> {
        read(&self.connection.borrow(), workspace, root_ref)
            .map(|row| row.map(|(record, _)| record))
    }
    /// Call after authorization changes are durably recorded and before final file cleanup.
    /// Replaying the original Operation is idempotent; a cleanup error never restores old refs.
    pub fn store_skill_installation(
        &self,
        workspace: &WorkspaceId,
        operation: &OperationId,
        expected_revision: u64,
        next: &ManagedCollaborationSkill,
    ) -> PortResult<()> {
        next.validate()
            .map_err(|_| port(PortErrorCode::InvalidData, "agent.skill.record"))?;
        let mut connection = self.connection.borrow_mut();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| port(PortErrorCode::Unavailable, "agent.skill.transaction"))?;
        require_writer(&transaction, operation, workspace)?;
        if let Some((current, owner)) = read(&transaction, workspace, &next.root_ref)? {
            if &current == next
                && (owner == operation.as_str() || current.revision == expected_revision)
            {
                return Ok(());
            }
            if owner == operation.as_str() || current.revision != expected_revision {
                return Err(port(PortErrorCode::Conflict, "agent.skill.changed"));
            }
        } else if expected_revision != 0 {
            return Err(port(PortErrorCode::Conflict, "agent.skill.missing"));
        }
        if expected_revision.checked_add(1) != Some(next.revision) {
            return Err(port(PortErrorCode::InvalidData, "agent.skill.revision"));
        }
        let json = serde_json::to_string(next)
            .map_err(|_| port(PortErrorCode::InvalidData, "agent.skill.encode"))?;
        transaction.execute("INSERT INTO agent_skill_installations(workspace_id,root_ref,revision,record_json,owner_operation_id) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(workspace_id,root_ref) DO UPDATE SET revision=excluded.revision,record_json=excluded.record_json,owner_operation_id=excluded.owner_operation_id",
            params![workspace.as_str(), next.root_ref, next.revision, json, operation.as_str()]).map_err(|_| port(PortErrorCode::Conflict, "agent.skill.write"))?;
        transaction
            .commit()
            .map_err(|_| port(PortErrorCode::Unavailable, "agent.skill.commit"))
    }
}
fn read(
    connection: &Connection,
    workspace: &WorkspaceId,
    root_ref: &str,
) -> PortResult<Option<(ManagedCollaborationSkill, String)>> {
    let row: Option<(String,u64,String)> = connection.query_row("SELECT record_json,revision,owner_operation_id FROM agent_skill_installations WHERE workspace_id=?1 AND root_ref=?2",
        params![workspace.as_str(), root_ref], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(|_| port(PortErrorCode::Unavailable,"agent.skill.read"))?;
    let Some((json, revision, owner)) = row else {
        return Ok(None);
    };
    let record: ManagedCollaborationSkill = serde_json::from_str(&json)
        .map_err(|_| port(PortErrorCode::Corrupt, "agent.skill.decode"))?;
    record
        .validate()
        .map_err(|_| port(PortErrorCode::Corrupt, "agent.skill.invalid"))?;
    if record.root_ref != root_ref || record.revision != revision {
        return Err(port(PortErrorCode::Corrupt, "agent.skill.identity"));
    }
    Ok(Some((record, owner)))
}
