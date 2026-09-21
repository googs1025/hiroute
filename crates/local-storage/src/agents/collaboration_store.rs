//! Agent grant/checkpoint persistence inside the existing control DB and Operation writer.
//! No file or Worker IO occurs in these transactions. Durable revocation is never compensated.
use super::{ControlStore, port};

impl hiroute_domain::AgentCollaborationRevocationStorePort for ControlStore {
    fn persist_collaboration_revocation(
        &self,
        checkpoint: &AgentCollaborationRevocation,
    ) -> PortResult<()> {
        self.record_collaboration_revocation(checkpoint)
    }
}
use hiroute_domain::{
    AgentCollaborationGrant, AgentCollaborationRevocation, OperationId, PortErrorCode, PortResult,
    WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

impl ControlStore {
    /// Bounded protected-credential lookup for caller-free Worker requests. The credential is
    /// compared only through each grant's domain-separated verifier and is never returned.
    pub fn collaboration_principal_for_credential(
        &self,
        material: &hiroute_domain::AgentCollaborationCredential,
    ) -> PortResult<hiroute_domain::VerifiedCollaborationPrincipal> {
        let workspace = WorkspaceId::default();
        let connection = self.connection.borrow();
        let mut statement = connection
            .prepare(
                "SELECT grant_json FROM agent_collaboration_grants
                 WHERE workspace_id=?1 AND generation>0 ORDER BY grant_id LIMIT 258",
            )
            .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.grant.scan"))?;
        let rows = statement
            .query_map(params![workspace.as_str()], |row| row.get::<_, String>(0))
            .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.grant.scan.rows"))?;
        let mut matched = None;
        let mut count = 0usize;
        for row in rows {
            count += 1;
            if count > 257 {
                return Err(port(
                    PortErrorCode::Unavailable,
                    "collaboration.grant.scan.bound",
                ));
            }
            let grant: AgentCollaborationGrant = serde_json::from_str(
                &row.map_err(|_| port(PortErrorCode::Corrupt, "collaboration.grant.scan.row"))?,
            )
            .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.grant.scan.decode"))?;
            grant
                .validate()
                .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.grant.scan.invalid"))?;
            if let Ok(principal) =
                grant.verify_bootstrap(&grant.context_id, grant.generation, material)
            {
                if matched.is_some() {
                    return Err(port(
                        PortErrorCode::PermissionDenied,
                        "collaboration.grant.scan.ambiguous",
                    ));
                }
                matched = Some(principal);
            }
        }
        matched.ok_or_else(|| {
            port(
                PortErrorCode::PermissionDenied,
                "collaboration.grant.scan.denied",
            )
        })
    }

    /// Only record the opaque receipt AFTER MVP-20 durably accepts the original cancel intent.
    /// This says nothing about whether the Worker process has finished.
    pub fn acknowledge_collaboration_cancel(
        &self,
        checkpoint: &AgentCollaborationRevocation,
        receipt_ref: &str,
    ) -> PortResult<()> {
        if receipt_ref.is_empty()
            || receipt_ref.len() > 256
            || receipt_ref.contains("..")
            || !receipt_ref
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_./:-".contains(&byte))
        {
            return Err(port(
                PortErrorCode::InvalidData,
                "collaboration.cancel.receipt",
            ));
        }
        self.complete_collaboration_recovery_part(checkpoint, Some(receipt_ref), false)
    }

    pub fn complete_collaboration_file_cleanup(
        &self,
        checkpoint: &AgentCollaborationRevocation,
    ) -> PortResult<()> {
        self.complete_collaboration_recovery_part(checkpoint, None, true)
    }

    fn complete_collaboration_recovery_part(
        &self,
        checkpoint: &AgentCollaborationRevocation,
        receipt_ref: Option<&str>,
        cleanup: bool,
    ) -> PortResult<()> {
        checkpoint
            .validate()
            .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.recovery.invalid"))?;
        let mut connection = self.connection.borrow_mut();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| {
                port(
                    PortErrorCode::Unavailable,
                    "collaboration.recovery.transaction",
                )
            })?;
        require_writer(
            &transaction,
            &checkpoint.operation_id,
            &checkpoint.after.workspace_id,
        )?;
        let json = serde_json::to_string(checkpoint)
            .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.recovery.encode"))?;
        let changed = transaction.execute(
            "UPDATE agent_collaboration_revocations SET cancel_receipt_ref=COALESCE(cancel_receipt_ref,?1),
             cleanup_complete=MAX(cleanup_complete,?2) WHERE operation_id=?3 AND grant_id=?4 AND checkpoint_json=?5
             AND (?1 IS NULL OR cancel_receipt_ref IS NULL OR cancel_receipt_ref=?1)",
            params![receipt_ref, cleanup, checkpoint.operation_id.as_str(), checkpoint.after.grant_id, json])
            .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.recovery.update"))?;
        if changed != 1 {
            return Err(port(
                PortErrorCode::Conflict,
                "collaboration.recovery.changed",
            ));
        }
        transaction
            .commit()
            .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.recovery.commit"))
    }

    pub fn collaboration_grant(
        &self,
        workspace: &WorkspaceId,
        grant_id: &str,
    ) -> PortResult<Option<AgentCollaborationGrant>> {
        read_grant(&self.connection.borrow(), workspace, grant_id)
            .map(|row| row.map(|(grant, _)| grant))
    }

    /// Activate only after the original Operation has prepared the Skill/credential/permit.
    /// The current generation must match the accepted Preview. This is not a public auth API.
    pub fn store_collaboration_grant(
        &self,
        operation: &OperationId,
        expected_generation: u64,
        grant: &AgentCollaborationGrant,
    ) -> PortResult<()> {
        grant
            .validate()
            .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.grant.invalid"))?;
        if !grant.enabled || expected_generation.checked_add(1) != Some(grant.generation) {
            return Err(port(
                PortErrorCode::InvalidData,
                "collaboration.grant.generation",
            ));
        }
        let mut connection = self.connection.borrow_mut();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| {
                port(
                    PortErrorCode::Unavailable,
                    "collaboration.grant.transaction",
                )
            })?;
        require_writer(&transaction, operation, &grant.workspace_id)?;
        let current = read_grant(&transaction, &grant.workspace_id, &grant.grant_id)?;
        if let Some((current, owner)) = &current {
            if owner == operation.as_str() {
                return if current == grant {
                    Ok(())
                } else {
                    Err(port(PortErrorCode::Conflict, "collaboration.grant.replay"))
                };
            }
            if current.context_id != grant.context_id || current.generation != expected_generation {
                return Err(port(PortErrorCode::Conflict, "collaboration.grant.changed"));
            }
        } else if expected_generation != 0 {
            return Err(port(PortErrorCode::Conflict, "collaboration.grant.missing"));
        }
        write_grant(&transaction, operation, grant)?;
        transaction
            .commit()
            .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.grant.commit"))
    }

    /// Called with MVP-20 deny already installed under the shared gate. Saves the after grant
    /// and original-Operation cancellation intent in ONE short control transaction.
    pub fn record_collaboration_revocation(
        &self,
        checkpoint: &AgentCollaborationRevocation,
    ) -> PortResult<()> {
        checkpoint
            .validate()
            .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.revoke.invalid"))?;
        let mut connection = self.connection.borrow_mut();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| {
                port(
                    PortErrorCode::Unavailable,
                    "collaboration.revoke.transaction",
                )
            })?;
        require_writer(
            &transaction,
            &checkpoint.operation_id,
            &checkpoint.after.workspace_id,
        )?;
        let previous: Option<String> = transaction.query_row(
            "SELECT checkpoint_json FROM agent_collaboration_revocations WHERE operation_id=?1 AND grant_id=?2",
            params![checkpoint.operation_id.as_str(), checkpoint.after.grant_id], |row| row.get(0))
            .optional().map_err(|_| port(PortErrorCode::Unavailable, "collaboration.revoke.read"))?;
        if let Some(previous) = previous {
            let previous: AgentCollaborationRevocation = serde_json::from_str(&previous)
                .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.revoke.decode"))?;
            return if &previous == checkpoint {
                Ok(())
            } else {
                Err(port(PortErrorCode::Conflict, "collaboration.revoke.replay"))
            };
        }
        let (current, _) = read_grant(
            &transaction,
            &checkpoint.after.workspace_id,
            &checkpoint.after.grant_id,
        )?
        .ok_or_else(|| port(PortErrorCode::NotFound, "collaboration.revoke.grant"))?;
        let expected = current
            .plan_revocation(
                checkpoint.operation_id.clone(),
                checkpoint.through_generation,
            )
            .map_err(|_| port(PortErrorCode::Conflict, "collaboration.revoke.generation"))?;
        if &expected != checkpoint {
            return Err(port(PortErrorCode::Conflict, "collaboration.revoke.intent"));
        }
        let json = serde_json::to_string(checkpoint)
            .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.revoke.encode"))?;
        transaction.execute(
            "INSERT INTO agent_collaboration_revocations(workspace_id,operation_id,grant_id,through_generation,checkpoint_json) VALUES(?1,?2,?3,?4,?5)",
            params![checkpoint.after.workspace_id.as_str(), checkpoint.operation_id.as_str(), checkpoint.after.grant_id,
                checkpoint.through_generation, json])
            .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.revoke.insert"))?;
        write_grant(&transaction, &checkpoint.operation_id, &checkpoint.after)?;
        transaction
            .commit()
            .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.revoke.commit"))
    }

    /// Startup must rebuild deny floors from ALL checkpoints, including completed ones, before
    /// opening admission. Completion of cleanup never invalidates a durable revocation floor.
    pub fn collaboration_revocations(
        &self,
        workspace: &WorkspaceId,
    ) -> PortResult<Vec<(AgentCollaborationRevocation, Option<String>, bool)>> {
        let connection = self.connection.borrow();
        let mut statement = connection.prepare(
            "SELECT checkpoint_json,cancel_receipt_ref,cleanup_complete FROM agent_collaboration_revocations WHERE workspace_id=?1 ORDER BY grant_id,through_generation")
            .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.recovery.query"))?;
        let rows = statement
            .query_map(params![workspace.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })
            .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.recovery.rows"))?;
        let mut checkpoints = Vec::new();
        for row in rows {
            let (json, receipt, cleanup) =
                row.map_err(|_| port(PortErrorCode::Corrupt, "collaboration.recovery.row"))?;
            let checkpoint: AgentCollaborationRevocation = serde_json::from_str(&json)
                .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.recovery.decode"))?;
            checkpoint
                .validate()
                .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.recovery.invalid"))?;
            if &checkpoint.after.workspace_id != workspace {
                return Err(port(
                    PortErrorCode::Corrupt,
                    "collaboration.recovery.workspace",
                ));
            }
            checkpoints.push((checkpoint, receipt, cleanup));
        }
        Ok(checkpoints)
    }
}

fn read_grant(
    connection: &Connection,
    workspace: &WorkspaceId,
    grant_id: &str,
) -> PortResult<Option<(AgentCollaborationGrant, String)>> {
    let row: Option<(String, String, u64, String)> = connection.query_row(
        "SELECT grant_json,owner_operation_id,generation,context_id FROM agent_collaboration_grants WHERE workspace_id=?1 AND grant_id=?2",
        params![workspace.as_str(), grant_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .optional().map_err(|_| port(PortErrorCode::Unavailable, "collaboration.grant.read"))?;
    let Some((json, owner, generation, context)) = row else {
        return Ok(None);
    };
    let grant: AgentCollaborationGrant = serde_json::from_str(&json)
        .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.grant.decode"))?;
    grant
        .validate()
        .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.grant.stored"))?;
    if &grant.workspace_id != workspace
        || grant.grant_id != grant_id
        || grant.generation != generation
        || grant.context_id != context
    {
        return Err(port(PortErrorCode::Corrupt, "collaboration.grant.identity"));
    }
    Ok(Some((grant, owner)))
}
fn write_grant(
    connection: &Connection,
    operation: &OperationId,
    grant: &AgentCollaborationGrant,
) -> PortResult<()> {
    let json = serde_json::to_string(grant)
        .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.grant.encode"))?;
    connection.execute(
        "INSERT INTO agent_collaboration_grants(workspace_id,grant_id,context_id,generation,grant_json,owner_operation_id)
         VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(workspace_id,grant_id) DO UPDATE SET
         generation=excluded.generation,grant_json=excluded.grant_json,owner_operation_id=excluded.owner_operation_id",
        params![grant.workspace_id.as_str(), grant.grant_id, grant.context_id, grant.generation, json, operation.as_str()])
        .map_err(|_| port(PortErrorCode::Conflict, "collaboration.grant.write"))?;
    Ok(())
}
pub(super) fn require_writer(
    connection: &Connection,
    operation: &OperationId,
    workspace: &WorkspaceId,
) -> PortResult<()> {
    let authorized: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM writer_claim w JOIN operations o ON o.operation_id=w.operation_id
         WHERE w.singleton=1 AND w.operation_id=?1 AND o.workspace_id=?2)",
        params![operation.as_str(), workspace.as_str()], |row| row.get(0))
        .map_err(|_| port(PortErrorCode::Unavailable, "collaboration.writer.read"))?;
    if !authorized {
        return Err(port(
            PortErrorCode::PermissionDenied,
            "collaboration.writer.required",
        ));
    }
    Ok(())
}
