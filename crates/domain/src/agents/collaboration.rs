//! Main-Agent collaboration grants are independent of model grants and Worker run credentials.
use crate::{AgentAccessGrantMaterial, AgentPlanId, CanonicalDigest, OperationId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

const GRANT_SCHEMA: &str = "hiroute.agent-collaboration-grant/v1";

/// One short transaction in the existing control store. The caller holds the original
/// Operation writer and shared admission guard; implementations must not do external IO.
pub trait AgentCollaborationRevocationStorePort {
    fn persist_collaboration_revocation(
        &self,
        checkpoint: &AgentCollaborationRevocation,
    ) -> crate::PortResult<()>;
}

/// Uses the existing protected random-material primitive, with a separate verifier domain and
/// separate storage record. This is never a model bearer, a run token, or a management capability.
pub struct AgentCollaborationCredential(AgentAccessGrantMaterial);
impl AgentCollaborationCredential {
    pub fn from_csprng_entropy(entropy: [u8; 32]) -> Self {
        Self(AgentAccessGrantMaterial::from_csprng_entropy(entropy))
    }
    pub fn from_authenticated_storage(bytes: Vec<u8>) -> Result<Self, CollaborationGrantError> {
        AgentAccessGrantMaterial::from_authenticated_storage(bytes)
            .map(Self)
            .map_err(|_| CollaborationGrantError::InvalidCredential)
    }
    pub fn expose(&self) -> &[u8] {
        self.0.expose()
    }
    fn verifier(
        &self,
        workspace: &WorkspaceId,
        context: &str,
        grant: &str,
        generation: u64,
    ) -> CanonicalDigest {
        let mut input = Zeroizing::new(format!("hiroute.collaboration-credential/v1\0{workspace}\0{context}\0{grant}\0{generation}\0").into_bytes());
        input.extend_from_slice(self.expose());
        CanonicalDigest::of_bytes(&input)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCollaborationGrant {
    pub schema: String,
    pub workspace_id: WorkspaceId,
    pub context_id: String,
    pub grant_id: String,
    pub generation: u64,
    pub enabled: bool,
    pub allowed_plan_ids: BTreeSet<AgentPlanId>,
    /// A domain-separated verifier, never the protected credential bytes.
    pub credential_verifier: Option<CanonicalDigest>,
}
impl AgentCollaborationGrant {
    pub fn issue(
        workspace_id: WorkspaceId,
        context_id: String,
        grant_id: String,
        generation: u64,
        allowed_plan_ids: BTreeSet<AgentPlanId>,
        material: &AgentCollaborationCredential,
    ) -> Result<Self, CollaborationGrantError> {
        let credential_verifier =
            Some(material.verifier(&workspace_id, &context_id, &grant_id, generation));
        let grant = Self {
            schema: GRANT_SCHEMA.into(),
            workspace_id,
            context_id,
            grant_id,
            generation,
            enabled: true,
            allowed_plan_ids,
            credential_verifier,
        };
        grant.validate()?;
        Ok(grant)
    }
    pub fn validate(&self) -> Result<(), CollaborationGrantError> {
        if self.schema != GRANT_SCHEMA
            || WorkspaceId::parse(self.workspace_id.as_str()).is_err()
            || !identity(&self.context_id)
            || !identity(&self.grant_id)
            || !self.grant_id.starts_with("collaboration-grant/")
            || self.generation == 0
            || self.allowed_plan_ids.len() > 256
            || self
                .allowed_plan_ids
                .iter()
                .any(|id| AgentPlanId::parse(id.as_str()).is_err())
            || self.enabled != self.credential_verifier.is_some()
            || self
                .credential_verifier
                .as_ref()
                .is_some_and(|digest| CanonicalDigest::parse(digest.as_str().to_owned()).is_err())
        {
            return Err(CollaborationGrantError::InvalidGrant);
        }
        Ok(())
    }
    /// Only after reading the authoritative current grant. A context selector itself conveys
    /// no authority. Admission still checks the current grant and the new run's task-scoped
    /// authorization in the same shared gate.
    pub fn verify_bootstrap(
        &self,
        context: &str,
        generation: u64,
        material: &AgentCollaborationCredential,
    ) -> Result<VerifiedCollaborationPrincipal, CollaborationGrantError> {
        self.validate()?;
        if !self.enabled || self.context_id != context || self.generation != generation {
            return Err(CollaborationGrantError::Denied);
        }
        let provided = material.verifier(&self.workspace_id, context, &self.grant_id, generation);
        let expected = self
            .credential_verifier
            .as_ref()
            .ok_or(CollaborationGrantError::Denied)?;
        if !bool::from(
            provided
                .as_str()
                .as_bytes()
                .ct_eq(expected.as_str().as_bytes()),
        ) {
            return Err(CollaborationGrantError::Denied);
        }
        Ok(VerifiedCollaborationPrincipal {
            workspace_id: self.workspace_id.clone(),
            context_id: self.context_id.clone(),
            grant_id: self.grant_id.clone(),
            generation,
            allowed_plan_ids: self.allowed_plan_ids.clone(),
        })
    }
    /// Reconstructs the same bounded principal only after a separate native management
    /// capability has authenticated the exact operation and request digest.  A caller must not
    /// use this method for ambient Local Control traffic: the selectors remain non-secret data
    /// and convey no authority by themselves.
    pub fn verify_management_selection(
        &self,
        workspace: &WorkspaceId,
        context: &str,
        grant_id: &str,
        generation: u64,
    ) -> Result<VerifiedCollaborationPrincipal, CollaborationGrantError> {
        self.validate()?;
        if !self.enabled
            || &self.workspace_id != workspace
            || self.context_id != context
            || self.grant_id != grant_id
            || self.generation != generation
        {
            return Err(CollaborationGrantError::Denied);
        }
        Ok(VerifiedCollaborationPrincipal {
            workspace_id: self.workspace_id.clone(),
            context_id: self.context_id.clone(),
            grant_id: self.grant_id.clone(),
            generation,
            allowed_plan_ids: self.allowed_plan_ids.clone(),
        })
    }
    /// A prepared value, not a storage commit. The original Operation installs MVP-20's deny
    /// under the shared gate, then persists this checkpoint and its cancellation intent together.
    pub fn plan_revocation(
        &self,
        operation_id: OperationId,
        expected_generation: u64,
    ) -> Result<AgentCollaborationRevocation, CollaborationGrantError> {
        self.validate()?;
        if self.generation != expected_generation || !self.enabled {
            return Err(CollaborationGrantError::Conflict);
        }
        let mut after = self.clone();
        after.generation = after
            .generation
            .checked_add(1)
            .ok_or(CollaborationGrantError::InvalidGrant)?;
        after.enabled = false;
        after.credential_verifier = None;
        let checkpoint = AgentCollaborationRevocation {
            operation_id,
            through_generation: self.generation,
            after,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }
}

/// No Deserialize or public constructor: IPC callers cannot turn claimed context IDs into this.
pub struct VerifiedCollaborationPrincipal {
    workspace_id: WorkspaceId,
    context_id: String,
    grant_id: String,
    generation: u64,
    allowed_plan_ids: BTreeSet<AgentPlanId>,
}
impl VerifiedCollaborationPrincipal {
    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }
    pub fn context_id(&self) -> &str {
        &self.context_id
    }
    pub fn grant_id(&self) -> &str {
        &self.grant_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn allowed_plan_ids(&self) -> &BTreeSet<AgentPlanId> {
        &self.allowed_plan_ids
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCollaborationRevocation {
    pub operation_id: OperationId,
    pub through_generation: u64,
    pub after: AgentCollaborationGrant,
}
impl AgentCollaborationRevocation {
    pub fn validate(&self) -> Result<(), CollaborationGrantError> {
        self.after.validate()?;
        if OperationId::parse(self.operation_id.as_str()).is_err()
            || self.after.enabled
            || self.through_generation == 0
            || self.through_generation.checked_add(1) != Some(self.after.generation)
        {
            return Err(CollaborationGrantError::InvalidGrant);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CollaborationGrantError {
    #[error("collaboration grant is invalid")]
    InvalidGrant,
    #[error("collaboration credential is invalid")]
    InvalidCredential,
    #[error("collaboration authority is denied")]
    Denied,
    #[error("collaboration grant generation changed")]
    Conflict,
}
fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('/')
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./:-".contains(&byte))
}

#[cfg(test)]
mod tests;
