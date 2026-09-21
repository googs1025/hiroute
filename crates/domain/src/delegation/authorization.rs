//! Typed values for the original settings Operation; no value here grants authority.
use super::*;
use crate::{AgentCollaborationGrant, CanonicalDigest, OperationId, WorkspaceId};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DelegationAuthorizationScopeV1 {
    Grant { id: String, through_generation: u64 },
    Permit { id: String, through_generation: u64 },
}
impl DelegationAuthorizationScopeV1 {
    pub fn validate(&self) -> Result<(), DelegationErrorV1> {
        let (id, generation) = match self {
            Self::Grant {
                id,
                through_generation,
            }
            | Self::Permit {
                id,
                through_generation,
            } => (id, through_generation),
        };
        if !valid_delegation_id(id) || *generation == 0 {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        Ok(())
    }
    pub fn matches(&self, run: &DelegationRunV1) -> bool {
        match self {
            Self::Grant {
                id: _,
                through_generation: _,
            } => false,
            Self::Permit {
                id,
                through_generation,
            } => &run.permit_id == id && run.permit_generation <= *through_generation,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationPermitMutationV1 {
    pub workspace: WorkspaceId,
    pub operation: OperationId,
    pub before: Option<WorkspaceExecutionPermitV1>,
    pub after: WorkspaceExecutionPermitV1,
}
impl DelegationPermitMutationV1 {
    pub fn validate(&self) -> Result<(), DelegationErrorV1> {
        self.after.validate()?;
        if WorkspaceId::parse(self.workspace.as_str()).is_err()
            || OperationId::parse(self.operation.as_str()).is_err()
            || !valid_delegation_id(&self.after.permit_id)
            || self
                .before
                .as_ref()
                .map_or(0, |p| p.generation)
                .checked_add(1)
                != Some(self.after.generation)
        {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        if let Some(before) = &self.before {
            before.validate()?;
            if before.permit_id != self.after.permit_id {
                return Err(DelegationErrorV1::InvalidArguments);
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<CanonicalDigest, DelegationErrorV1> {
        self.validate()?;
        CanonicalDigest::of(self).map_err(|_| DelegationErrorV1::InvalidArguments)
    }
    pub fn revoked_scope(&self) -> Option<DelegationAuthorizationScopeV1> {
        self.before
            .as_ref()
            .map(|p| DelegationAuthorizationScopeV1::Permit {
                id: p.permit_id.clone(),
                through_generation: p.generation,
            })
    }
}

pub trait DelegationPermitStorePort {
    fn permit(
        &self,
        workspace: &WorkspaceId,
        id: &str,
    ) -> Result<Option<WorkspaceExecutionPermitV1>, DelegationErrorV1>;
    fn prepare_permit(
        &self,
        mutation: &DelegationPermitMutationV1,
    ) -> Result<(), DelegationErrorV1>;
    fn pending_permit_mutations(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<DelegationPermitMutationV1>, DelegationErrorV1>;
    /// Existing writer must belong to mutation.operation. Atomic current record + original
    /// Operation checkpoint, idempotent by operation/permit and exact content, never rollback.
    fn commit_permit(&self, mutation: &DelegationPermitMutationV1)
    -> Result<(), DelegationErrorV1>;
    fn permit_mutations(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<DelegationPermitMutationV1>, DelegationErrorV1>;
}

pub trait DelegationGrantAuthorityPort {
    fn open_sealed_bootstrap(
        &self,
        _encoded: &str,
    ) -> Result<(AgentCollaborationGrant, crate::AgentCollaborationCredential), DelegationErrorV1>
    {
        Err(DelegationErrorV1::PermissionDenied)
    }
    fn current_grant(
        &self,
        workspace: &WorkspaceId,
        id: &str,
    ) -> Result<Option<AgentCollaborationGrant>, DelegationErrorV1>;

    /// Resolve a protected Skill credential without accepting caller-provided workspace, grant,
    /// or generation selectors. Implementations must reject no-match and ambiguous matches.
    fn principal_for_credential(
        &self,
        _material: &crate::AgentCollaborationCredential,
    ) -> Result<crate::VerifiedCollaborationPrincipal, DelegationErrorV1> {
        Err(DelegationErrorV1::PermissionDenied)
    }
}

/// Whole-scope intent is committed atomically under the admission guard. The returned stable
/// reference identifies durable acceptance, not dispatch or stopping. Zero affected runs is valid.
pub trait DelegationScopeCancellationPort {
    fn cancel_scope(
        &self,
        workspace: &WorkspaceId,
        operation: &OperationId,
        scope: &DelegationAuthorizationScopeV1,
    ) -> Result<String, DelegationErrorV1>;
}
