//! Backend-only material access for the original file-staging Operation.
use super::*;

impl LocalSecretStore {
    /// Generation remains authoritative after revocation, when no active reference exists.
    pub fn agent_access_grant_generation(
        &self,
        owner_scope: &str,
        connection_id: &str,
    ) -> PortResult<u64> {
        let connection = self.connection.borrow();
        let head = read_head(&connection, connection_id)?;
        validate_head_owner(&head, owner_scope)?;
        Ok(head.map_or(0, |head| head.generation))
    }

    /// Resolve the exact staged model grant before activation. This is deliberately separate
    /// from the active-grant helper API: no public context selector can use it. The daemon must
    /// obtain `mutation` from the original admitted Operation, never from request JSON.
    pub fn resolve_prepared_agent_access_grant(
        &self,
        operation: &OperationId,
        mutation: &AgentAccessGrantMutationV1,
    ) -> PortResult<AgentAccessGrantMaterial> {
        mutation.validate().map_err(|_| {
            port(
                PortErrorCode::InvalidData,
                "agent_access_grant.prepared.mutation",
            )
        })?;
        if mutation.kind() != AgentAccessGrantMutationKindV1::Ensure {
            return Err(port(
                PortErrorCode::PermissionDenied,
                "agent_access_grant.prepared.kind",
            ));
        }
        let effect = match observe(self, operation, mutation)? {
            EffectReconciliation::Staged(effect) | EffectReconciliation::Applied(effect) => effect,
            _ => {
                return Err(port(
                    PortErrorCode::Conflict,
                    "agent_access_grant.prepared.ownership",
                ));
            }
        };
        let expected =
            AgentAccessGrantRefV1::from_ensure_effect(&effect, mutation).map_err(|_| {
                port(
                    PortErrorCode::Corrupt,
                    "agent_access_grant.prepared.reference",
                )
            })?;
        let connection = self.connection.borrow();
        let version = read_version(&connection, mutation.connection_id(), expected.generation())?
            .ok_or_else(|| {
            port(
                PortErrorCode::NotFound,
                "agent_access_grant.prepared.version",
            )
        })?;
        let (reference, material) = authenticate_version(self, &version)?;
        if reference != expected {
            return Err(port(
                PortErrorCode::Corrupt,
                "agent_access_grant.prepared.binding",
            ));
        }
        Ok(material)
    }
}
