//! Operation-owned preparation and a domain-separated sealed delivery artifact.
use super::{KEY_VERSION, LocalSecretStore, NONCE_BYTES, port};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use hiroute_domain::{
    AgentCollaborationCredential, AgentCollaborationGrant, AgentPlanId, OperationId, PortErrorCode,
    PortResult, WorkspaceId,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use zeroize::{Zeroize, Zeroizing};

const AUDIENCE: &str = "hiroute.local.collaboration";
const SCHEMA: &str = "hiroute.sealed-collaboration-bootstrap/v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: String,
    key_version: u32,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    audience: String,
    installation_id: String,
    grant: AgentCollaborationGrant,
    material: Vec<u8>,
}
impl Drop for Content {
    fn drop(&mut self) {
        self.material.zeroize();
    }
}

impl LocalSecretStore {
    /// The current original Operation writer serializes callers. A crash recovers the same
    /// random credential and full authenticated grant from the existing encrypted row.
    pub fn prepare_settings_collaboration(
        &self,
        operation: &OperationId,
        workspace: &WorkspaceId,
        context: &str,
        expected_generation: u64,
        plans: BTreeSet<AgentPlanId>,
    ) -> PortResult<(AgentCollaborationGrant, AgentCollaborationCredential)> {
        let id = format!("collaboration-grant/{context}");
        let generation = expected_generation
            .checked_add(1)
            .ok_or_else(|| port(PortErrorCode::Conflict, "collaboration.generation"))?;
        let prepared: Option<String> = self.connection.borrow().query_row(
            "SELECT prepared_grant_json FROM agent_collaboration_credentials WHERE owner_operation_id=?1 AND grant_id=?2",
            params![operation.as_str(), id], |row| row.get(0)).optional().map_err(|_| port(PortErrorCode::Unavailable, "collaboration.prepared"))?;
        if let Some(prepared) = prepared {
            let grant: AgentCollaborationGrant = serde_json::from_str(&prepared)
                .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.prepared"))?;
            if grant.workspace_id != *workspace
                || grant.context_id != context
                || grant.grant_id != id
                || grant.generation != generation
                || grant.allowed_plan_ids != plans
            {
                return Err(port(
                    PortErrorCode::Conflict,
                    "collaboration.prepared.binding",
                ));
            }
            let material = self.recover_prepared_collaboration_credential(operation, &grant)?;
            return Ok((grant, material));
        }
        let mut entropy = [0u8; 32];
        getrandom::fill(&mut entropy)
            .map_err(|_| port(PortErrorCode::Crypto, "collaboration.entropy"))?;
        let material = AgentCollaborationCredential::from_csprng_entropy(entropy);
        entropy.zeroize();
        let grant = AgentCollaborationGrant::issue(
            workspace.clone(),
            context.into(),
            id,
            generation,
            plans,
            &material,
        )
        .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.grant"))?;
        self.prepare_collaboration_credential(operation, &grant, &material)?;
        Ok((grant, material))
    }

    pub fn seal_collaboration_bootstrap(
        &self,
        grant: &AgentCollaborationGrant,
        material: &AgentCollaborationCredential,
        installation_id: &str,
    ) -> PortResult<Zeroizing<String>> {
        grant
            .verify_bootstrap(&grant.context_id, grant.generation, material)
            .map_err(|_| port(PortErrorCode::PermissionDenied, "collaboration.seal"))?;
        let content = Content {
            audience: AUDIENCE.into(),
            installation_id: installation_id.into(),
            grant: grant.clone(),
            material: material.expose().to_vec(),
        };
        let bytes = Zeroizing::new(
            serde_json::to_vec(&content)
                .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.seal"))?,
        );
        let mut nonce = [0u8; NONCE_BYTES];
        getrandom::fill(&mut nonce)
            .map_err(|_| port(PortErrorCode::Crypto, "collaboration.nonce"))?;
        let cipher = Aes256Gcm::new_from_slice(self.keys.encryption())
            .map_err(|_| port(PortErrorCode::Crypto, "collaboration.key"))?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &bytes,
                    aad: SCHEMA.as_bytes(),
                },
            )
            .map_err(|_| port(PortErrorCode::Crypto, "collaboration.seal"))?;
        let encoded = serde_json::to_string(&Envelope {
            schema: SCHEMA.into(),
            key_version: KEY_VERSION,
            nonce: nonce.to_vec(),
            ciphertext,
        })
        .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.seal"))?;
        if encoded.len() > 4096 {
            return Err(port(PortErrorCode::InvalidData, "collaboration.seal.bound"));
        }
        Ok(Zeroizing::new(encoded))
    }

    pub fn open_collaboration_bootstrap(
        &self,
        encoded: &str,
    ) -> PortResult<(AgentCollaborationGrant, AgentCollaborationCredential)> {
        let denied = || port(PortErrorCode::PermissionDenied, "collaboration.bootstrap");
        if encoded.len() > 4096 {
            return Err(denied());
        }
        let envelope: Envelope = serde_json::from_str(encoded).map_err(|_| denied())?;
        if envelope.schema != SCHEMA
            || envelope.key_version != KEY_VERSION
            || envelope.nonce.len() != NONCE_BYTES
        {
            return Err(denied());
        }
        let cipher = Aes256Gcm::new_from_slice(self.keys.encryption()).map_err(|_| denied())?;
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(&envelope.nonce),
                    Payload {
                        msg: &envelope.ciphertext,
                        aad: SCHEMA.as_bytes(),
                    },
                )
                .map_err(|_| denied())?,
        );
        let mut content: Content = serde_json::from_slice(&plaintext).map_err(|_| denied())?;
        if content.audience != AUDIENCE || content.installation_id != content.grant.context_id {
            return Err(denied());
        }
        let material = AgentCollaborationCredential::from_authenticated_storage(std::mem::take(
            &mut content.material,
        ))
        .map_err(|_| denied())?;
        content
            .grant
            .verify_bootstrap(
                &content.grant.context_id,
                content.grant.generation,
                &material,
            )
            .map_err(|_| denied())?;
        Ok((content.grant.clone(), material))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collaboration_bootstrap_preparation_replays_and_seal_rejects_tampering() {
        let dir = crate::test_tempdir().unwrap();
        let store = LocalSecretStore::open(
            &crate::test_storage_authority(),
            dir.path().join("data/secrets.db"),
            dir.path().join("data/key"),
            dir.path().join("backups"),
        )
        .unwrap();
        let operation = OperationId::parse("op_00112233445566778899aabbccddeeff").unwrap();
        let workspace = WorkspaceId::default();
        let (grant, material) = store
            .prepare_settings_collaboration(
                &operation,
                &workspace,
                "context/one",
                0,
                BTreeSet::new(),
            )
            .unwrap();
        let (again, recovered) = store
            .prepare_settings_collaboration(
                &operation,
                &workspace,
                "context/one",
                0,
                BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(grant, again);
        assert_eq!(material.expose(), recovered.expose());
        let sealed = store
            .seal_collaboration_bootstrap(&grant, &material, "context/one")
            .unwrap();
        assert!(!sealed.contains(std::str::from_utf8(material.expose()).unwrap()));
        let (opened, secret) = store.open_collaboration_bootstrap(&sealed).unwrap();
        assert_eq!(opened, grant);
        assert_eq!(secret.expose(), material.expose());
        let mut tampered: Envelope = serde_json::from_str(&sealed).unwrap();
        tampered.ciphertext[0] ^= 1;
        assert!(
            store
                .open_collaboration_bootstrap(&serde_json::to_string(&tampered).unwrap())
                .is_err()
        );
        let wrong = store
            .seal_collaboration_bootstrap(&grant, &material, "context/two")
            .unwrap();
        assert!(store.open_collaboration_bootstrap(&wrong).is_err());
        assert!(
            store
                .open_collaboration_bootstrap(std::str::from_utf8(material.expose()).unwrap())
                .is_err()
        );
        assert!(
            store
                .prepare_settings_collaboration(
                    &operation,
                    &workspace,
                    "context/one",
                    1,
                    BTreeSet::new()
                )
                .is_err()
        );
    }
}
