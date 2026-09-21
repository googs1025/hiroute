use hiroute_domain::{
    AGENT_ACCESS_GRANT_EFFECT_SCHEMA_V1, CanonicalDigest, OwnedEffectKind, OwnedEffectV1,
    PortErrorCode, PortResult,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::json;

use super::super::port;

#[derive(Clone, Eq, PartialEq)]
pub(super) struct GrantHead {
    pub(super) connection_id: String,
    pub(super) owner_scope: String,
    pub(super) generation: u64,
    pub(super) active_version_generation: Option<u64>,
    pub(super) owner_operation_id: String,
}

pub(super) struct GrantVersion {
    pub(super) connection_id: String,
    pub(super) generation: u64,
    pub(super) grant_id: String,
    pub(super) owner_scope: String,
    pub(super) scope_json: String,
    pub(super) scope_hash: String,
    pub(super) ciphertext: Vec<u8>,
    pub(super) nonce: Vec<u8>,
    pub(super) aad_schema: String,
    pub(super) key_version: u32,
    pub(super) material_sha256: String,
    pub(super) owner_operation_id: String,
}

pub(super) struct AfterMetadata {
    pub(super) grant_id: String,
    pub(super) scope_hash: String,
    pub(super) material_sha256: String,
}

impl AfterMetadata {
    pub(super) fn from_version(version: &GrantVersion) -> Self {
        Self {
            grant_id: version.grant_id.clone(),
            scope_hash: version.scope_hash.clone(),
            material_sha256: version.material_sha256.clone(),
        }
    }
}

pub(super) struct GrantEffect {
    pub(super) operation_id: String,
    pub(super) connection_id: String,
    pub(super) action: String,
    pub(super) owner_scope: String,
    pub(super) before_generation: u64,
    pub(super) before_active_version_generation: Option<u64>,
    pub(super) before_owner_operation_id: Option<String>,
    pub(super) after_generation: u64,
    pub(super) after_active_version_generation: Option<u64>,
    pub(super) after_grant_id: Option<String>,
    pub(super) after_scope_hash: Option<String>,
    pub(super) after_material_sha256: Option<String>,
    pub(super) compensated: bool,
    pub(super) activated: bool,
}

impl GrantEffect {
    pub(super) fn before_head(&self) -> Option<GrantHead> {
        self.before_owner_operation_id
            .as_ref()
            .map(|owner| GrantHead {
                connection_id: self.connection_id.clone(),
                owner_scope: self.owner_scope.clone(),
                generation: self.before_generation,
                active_version_generation: self.before_active_version_generation,
                owner_operation_id: owner.clone(),
            })
    }

    pub(super) fn after_head(&self) -> Option<GrantHead> {
        Some(self.after_head_with_owner(&self.operation_id))
    }

    pub(super) fn after_head_with_owner(&self, owner: &str) -> GrantHead {
        GrantHead {
            connection_id: self.connection_id.clone(),
            owner_scope: self.owner_scope.clone(),
            generation: self.after_generation,
            active_version_generation: self.after_active_version_generation,
            owner_operation_id: owner.to_owned(),
        }
    }

    pub(super) fn owned_effect(&self) -> OwnedEffectV1 {
        OwnedEffectV1 {
            effect_id: format!("agent-access-grant:{}", self.connection_id),
            kind: OwnedEffectKind::Secret,
            target: self.connection_id.clone(),
            before_fingerprint: None,
            after_fingerprint: self
                .after_material_sha256
                .as_ref()
                .and_then(|digest| CanonicalDigest::parse(digest.clone()).ok()),
            compensation: json!({
                "schema": AGENT_ACCESS_GRANT_EFFECT_SCHEMA_V1,
                "operation_id": self.operation_id,
                "connection_id": self.connection_id,
                "owner_scope": self.owner_scope,
                "grant_id": self.after_grant_id,
                "generation": self.after_generation,
                "scope_hash": self.after_scope_hash,
                "material_sha256": self.after_material_sha256,
            })
            .into(),
        }
    }
}

pub(super) fn read_head(
    connection: &Connection,
    connection_id: &str,
) -> PortResult<Option<GrantHead>> {
    connection
        .query_row(
            "SELECT owner_scope, generation, active_version_generation, owner_operation_id
             FROM agent_access_grant_heads WHERE connection_id = ?1",
            params![connection_id],
            |row| {
                Ok(GrantHead {
                    connection_id: connection_id.to_owned(),
                    owner_scope: row.get(0)?,
                    generation: row.get(1)?,
                    active_version_generation: row.get(2)?,
                    owner_operation_id: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|_| port(PortErrorCode::Unavailable, "agent_access_grant.head.read"))
}

pub(super) fn read_version(
    connection: &Connection,
    connection_id: &str,
    generation: u64,
) -> PortResult<Option<GrantVersion>> {
    connection
        .query_row(
            "SELECT grant_id, owner_scope, scope_json, scope_hash, ciphertext, nonce,
                    aad_schema, key_version, material_sha256, owner_operation_id
             FROM agent_access_grant_versions
             WHERE connection_id = ?1 AND generation = ?2",
            params![connection_id, generation],
            |row| {
                Ok(GrantVersion {
                    connection_id: connection_id.to_owned(),
                    generation,
                    grant_id: row.get(0)?,
                    owner_scope: row.get(1)?,
                    scope_json: row.get(2)?,
                    scope_hash: row.get(3)?,
                    ciphertext: row.get(4)?,
                    nonce: row.get(5)?,
                    aad_schema: row.get(6)?,
                    key_version: row.get(7)?,
                    material_sha256: row.get(8)?,
                    owner_operation_id: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "agent_access_grant.version.read",
            )
        })
}

pub(super) fn insert_version(
    transaction: &Transaction<'_>,
    version: &GrantVersion,
) -> PortResult<()> {
    transaction
        .execute(
            "INSERT INTO agent_access_grant_versions(
                connection_id, generation, grant_id, owner_scope, scope_json, scope_hash,
                ciphertext, nonce, aad_schema, key_version, material_sha256,
                owner_operation_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, unixepoch())",
            params![
                version.connection_id,
                version.generation,
                version.grant_id,
                version.owner_scope,
                version.scope_json,
                version.scope_hash,
                version.ciphertext,
                version.nonce,
                version.aad_schema,
                version.key_version,
                version.material_sha256,
                version.owner_operation_id,
            ],
        )
        .map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "agent_access_grant.version.insert",
            )
        })?;
    Ok(())
}

pub(super) fn read_effect(
    connection: &Connection,
    operation_id: &str,
    connection_id: &str,
) -> PortResult<Option<GrantEffect>> {
    connection
        .query_row(
            "SELECT action, owner_scope, before_generation, before_active_version_generation,
                    before_owner_operation_id, after_generation, after_active_version_generation,
                    after_grant_id, after_scope_hash, after_material_sha256, compensated, activated
             FROM agent_access_grant_effects
             WHERE operation_id = ?1 AND connection_id = ?2",
            params![operation_id, connection_id],
            |row| {
                Ok(GrantEffect {
                    operation_id: operation_id.to_owned(),
                    connection_id: connection_id.to_owned(),
                    action: row.get(0)?,
                    owner_scope: row.get(1)?,
                    before_generation: row.get(2)?,
                    before_active_version_generation: row.get(3)?,
                    before_owner_operation_id: row.get(4)?,
                    after_generation: row.get(5)?,
                    after_active_version_generation: row.get(6)?,
                    after_grant_id: row.get(7)?,
                    after_scope_hash: row.get(8)?,
                    after_material_sha256: row.get(9)?,
                    compensated: row.get(10)?,
                    activated: row.get(11)?,
                })
            },
        )
        .optional()
        .map_err(|_| port(PortErrorCode::Unavailable, "agent_access_grant.effect.read"))
}

pub(super) fn insert_effect(transaction: &Transaction<'_>, effect: &GrantEffect) -> PortResult<()> {
    transaction
        .execute(
            "INSERT INTO agent_access_grant_effects(
                operation_id, connection_id, action, owner_scope, before_generation,
                before_active_version_generation, before_owner_operation_id, after_generation,
                after_active_version_generation, after_grant_id, after_scope_hash,
                after_material_sha256, compensated, activated
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, 0)",
            params![
                effect.operation_id,
                effect.connection_id,
                effect.action,
                effect.owner_scope,
                effect.before_generation,
                effect.before_active_version_generation,
                effect.before_owner_operation_id,
                effect.after_generation,
                effect.after_active_version_generation,
                effect.after_grant_id,
                effect.after_scope_hash,
                effect.after_material_sha256,
            ],
        )
        .map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "agent_access_grant.effect.insert",
            )
        })?;
    Ok(())
}

pub(super) fn write_head(transaction: &Transaction<'_>, head: &GrantHead) -> PortResult<()> {
    transaction
        .execute(
            "INSERT INTO agent_access_grant_heads(
                connection_id, owner_scope, generation, active_version_generation,
                owner_operation_id, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())
             ON CONFLICT(connection_id) DO UPDATE SET
                owner_scope = excluded.owner_scope, generation = excluded.generation,
                active_version_generation = excluded.active_version_generation,
                owner_operation_id = excluded.owner_operation_id,
                updated_at = excluded.updated_at",
            params![
                head.connection_id,
                head.owner_scope,
                head.generation,
                head.active_version_generation,
                head.owner_operation_id,
            ],
        )
        .map_err(|_| port(PortErrorCode::Unavailable, "agent_access_grant.head.write"))?;
    Ok(())
}

pub(super) fn restore_head(
    transaction: &Transaction<'_>,
    connection_id: &str,
    before: Option<&GrantHead>,
) -> PortResult<()> {
    if let Some(before) = before {
        write_head(transaction, before)
    } else {
        transaction
            .execute(
                "DELETE FROM agent_access_grant_heads WHERE connection_id = ?1",
                params![connection_id],
            )
            .map_err(|_| port(PortErrorCode::Unavailable, "agent_access_grant.head.delete"))?;
        Ok(())
    }
}

pub(super) fn head_matches(actual: Option<&GrantHead>, expected: Option<&GrantHead>) -> bool {
    actual == expected
}
