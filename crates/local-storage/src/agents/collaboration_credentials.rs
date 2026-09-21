//! Encrypted preparation/recovery for an original Agent settings Operation.
//! These methods are daemon storage primitives, NOT a context-id-to-token bootstrap API.
use super::{KEY_VERSION, LocalSecretStore, NONCE_BYTES, port};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use hiroute_domain::{
    AgentCollaborationCredential, AgentCollaborationGrant, OperationId, PortErrorCode, PortResult,
};
use rusqlite::{OptionalExtension, params};

const SCHEMA: &str = "hiroute.agent-collaboration-credential/v1";

impl LocalSecretStore {
    /// Caller owns the original Operation writer. Persist before activating its control grant
    /// or writing the protected native slot; replay authenticates the exact original material.
    pub fn prepare_collaboration_credential(
        &self,
        operation: &OperationId,
        grant: &AgentCollaborationGrant,
        material: &AgentCollaborationCredential,
    ) -> PortResult<()> {
        grant
            .verify_bootstrap(&grant.context_id, grant.generation, material)
            .map_err(|_| {
                port(
                    PortErrorCode::InvalidData,
                    "collaboration.credential.binding",
                )
            })?;
        let aad = self.collaboration_aad(operation, grant)?;
        let mut connection = self.connection.borrow_mut();
        let transaction = connection.transaction().map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "collaboration.credential.transaction",
            )
        })?;
        let existing = read(&transaction, grant)?;
        if let Some(row) = existing {
            self.decrypt_collaboration(operation, grant, &row)?;
            return Ok(());
        }
        let mut nonce = [0u8; NONCE_BYTES];
        getrandom::fill(&mut nonce)
            .map_err(|_| port(PortErrorCode::Crypto, "collaboration.credential.entropy"))?;
        let cipher = Aes256Gcm::new_from_slice(self.keys.encryption())
            .map_err(|_| port(PortErrorCode::Crypto, "collaboration.credential.key"))?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: material.expose(),
                    aad: &aad,
                },
            )
            .map_err(|_| port(PortErrorCode::Crypto, "collaboration.credential.encrypt"))?;
        transaction.execute("INSERT INTO agent_collaboration_credentials(workspace_id,context_id,grant_id,generation,owner_operation_id,ciphertext,nonce,aad_schema,key_version,prepared_grant_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![grant.workspace_id.as_str(), grant.context_id, grant.grant_id, grant.generation, operation.as_str(), ciphertext, nonce.as_slice(), SCHEMA, KEY_VERSION, serde_json::to_string(grant).map_err(|_| port(PortErrorCode::InvalidData, "collaboration.grant.encode"))?])
            .map_err(|_| port(PortErrorCode::Conflict, "collaboration.credential.insert"))?;
        transaction.commit().map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "collaboration.credential.commit",
            )
        })
    }

    /// Recover only using the complete original sealed grant and Operation. The control-layer
    /// caller must reject superseded/revoked settings before materializing a native slot.
    /// Having an opaque context selector alone never reaches this API from CLI or WebView.
    pub fn recover_prepared_collaboration_credential(
        &self,
        operation: &OperationId,
        grant: &AgentCollaborationGrant,
    ) -> PortResult<AgentCollaborationCredential> {
        let row = read(&self.connection.borrow(), grant)?
            .ok_or_else(|| port(PortErrorCode::NotFound, "collaboration.credential.missing"))?;
        self.decrypt_collaboration(operation, grant, &row)
    }

    fn collaboration_aad(
        &self,
        operation: &OperationId,
        grant: &AgentCollaborationGrant,
    ) -> PortResult<Vec<u8>> {
        grant
            .validate()
            .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.credential.grant"))?;
        if !grant.enabled {
            return Err(port(
                PortErrorCode::PermissionDenied,
                "collaboration.credential.revoked",
            ));
        }
        OperationId::parse(operation.as_str()).map_err(|_| {
            port(
                PortErrorCode::InvalidData,
                "collaboration.credential.operation",
            )
        })?;
        serde_json::to_vec(&(SCHEMA, KEY_VERSION, &self.keys.store_uuid, operation, grant))
            .map_err(|_| port(PortErrorCode::InvalidData, "collaboration.credential.aad"))
    }
    fn decrypt_collaboration(
        &self,
        operation: &OperationId,
        grant: &AgentCollaborationGrant,
        row: &EncryptedCredential,
    ) -> PortResult<AgentCollaborationCredential> {
        if row.context != grant.context_id
            || row.operation != operation.as_str()
            || row.schema != SCHEMA
            || row.key_version != KEY_VERSION
            || row.nonce.len() != NONCE_BYTES
        {
            return Err(port(
                PortErrorCode::Corrupt,
                "collaboration.credential.identity",
            ));
        }
        let aad = self.collaboration_aad(operation, grant)?;
        let cipher = Aes256Gcm::new_from_slice(self.keys.encryption())
            .map_err(|_| port(PortErrorCode::Crypto, "collaboration.credential.key"))?;
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&row.nonce),
                Payload {
                    msg: &row.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| {
                port(
                    PortErrorCode::Corrupt,
                    "collaboration.credential.authenticate",
                )
            })?;
        let material = AgentCollaborationCredential::from_authenticated_storage(plaintext)
            .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.credential.material"))?;
        grant
            .verify_bootstrap(&grant.context_id, grant.generation, &material)
            .map_err(|_| port(PortErrorCode::Corrupt, "collaboration.credential.verifier"))?;
        Ok(material)
    }
}
struct EncryptedCredential {
    context: String,
    operation: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
    schema: String,
    key_version: u32,
}
fn read(
    connection: &rusqlite::Connection,
    grant: &AgentCollaborationGrant,
) -> PortResult<Option<EncryptedCredential>> {
    connection.query_row("SELECT context_id,owner_operation_id,ciphertext,nonce,aad_schema,key_version FROM agent_collaboration_credentials WHERE workspace_id=?1 AND grant_id=?2 AND generation=?3",
        params![grant.workspace_id.as_str(), grant.grant_id, grant.generation], |row| Ok(EncryptedCredential { context: row.get(0)?, operation: row.get(1)?, ciphertext: row.get(2)?, nonce: row.get(3)?, schema: row.get(4)?, key_version: row.get(5)? }))
        .optional().map_err(|_| port(PortErrorCode::Unavailable, "collaboration.credential.read"))
}

#[cfg(test)]
#[path = "collaboration_credentials_tests.rs"]
mod tests;
