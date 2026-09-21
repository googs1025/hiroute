use hiroute_domain::{
    CanonicalDigest, ComputeInventorySnapshotV1, ComputeSourceV1, CredentialPoolV1, SourceBindingV1,
};
use rusqlite::params;

use crate::LocalStorageError;

pub(super) fn migrate_semantic_identities(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), LocalStorageError> {
    let source_rows = {
        let mut statement = transaction.prepare(
            "SELECT source_id,revision,identity_digest,source_json
             FROM compute_sources ORDER BY source_id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut sources = std::collections::BTreeMap::new();
    let mut semantic_identities = std::collections::BTreeMap::new();
    for (source_id, revision, stored_identity_digest, encoded) in source_rows {
        let mut source: ComputeSourceV1 =
            serde_json::from_str(&encoded).map_err(|_| LocalStorageError::InvalidData)?;
        let legacy_digest =
            CanonicalDigest::of(&source.identity).map_err(|_| LocalStorageError::InvalidData)?;
        let semantic_digest = source
            .identity
            .digest()
            .map_err(|_| LocalStorageError::InvalidData)?;
        if source.source_id != source_id
            || source.revision != revision
            || source.identity_digest.as_str() != stored_identity_digest
            || (source.identity_digest != legacy_digest
                && source.identity_digest != semantic_digest)
            || semantic_identities
                .insert(semantic_digest.to_string(), source_id.clone())
                .is_some()
        {
            return Err(LocalStorageError::InvalidData);
        }
        source.identity_digest = semantic_digest;
        source
            .validate_shape()
            .map_err(|_| LocalStorageError::InvalidData)?;
        sources.insert(source.source_id.clone(), (source, encoded));
    }

    let binding_rows = {
        let mut statement = transaction.prepare(
            "SELECT binding_id,revision,source_id,binding_json
             FROM source_bindings ORDER BY binding_id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut bindings = std::collections::BTreeMap::new();
    for (binding_id, revision, stored_source_id, encoded) in binding_rows {
        let mut binding: SourceBindingV1 =
            serde_json::from_str(&encoded).map_err(|_| LocalStorageError::InvalidData)?;
        let (source, _) = sources
            .get(&binding.source_id)
            .ok_or(LocalStorageError::InvalidData)?;
        let legacy_source_digest =
            CanonicalDigest::of(&source.identity).map_err(|_| LocalStorageError::InvalidData)?;
        if binding.binding_id != binding_id
            || binding.revision != revision
            || binding.source_id != stored_source_id
            || binding.source_revision != source.revision
            || (binding.source_identity_digest != legacy_source_digest
                && binding.source_identity_digest != source.identity_digest)
        {
            return Err(LocalStorageError::InvalidData);
        }
        binding.source_identity_digest = source.identity_digest.clone();
        binding
            .validate_shape()
            .map_err(|_| LocalStorageError::InvalidData)?;
        bindings.insert(binding.binding_id.clone(), (binding, encoded));
    }

    let inventory_rows = {
        let mut statement = transaction.prepare(
            "SELECT source_id,endpoint_profile_id,inventory_revision,inventory_digest,
                    observed_models_json,captured_at
             FROM source_inventory_snapshots ORDER BY source_id,endpoint_profile_id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (source_id, endpoint_profile_id, revision, digest, observed, captured_at) in inventory_rows
    {
        let (source, _) = sources
            .get(&source_id)
            .ok_or(LocalStorageError::InvalidData)?;
        let inventory = ComputeInventorySnapshotV1 {
            source_id,
            endpoint_profile_id,
            inventory_revision: revision,
            inventory_digest: CanonicalDigest::parse(digest)
                .map_err(|_| LocalStorageError::InvalidData)?,
            observed_models: serde_json::from_str(&observed)
                .map_err(|_| LocalStorageError::InvalidData)?,
            captured_at,
        };
        if inventory.endpoint_profile_id != source.identity.endpoint_profile_id
            || inventory.validate().is_err()
        {
            return Err(LocalStorageError::InvalidData);
        }
    }

    let pool_rows = {
        let mut statement = transaction.prepare(
            "SELECT pool_id,binding_id,binding_revision,binding_digest,source_id,
                    source_revision,connection_option_id,offer_ref,offer_revision,
                    offer_evidence_digest,billing_class,model_configuration_id,revision,
                    homogeneous_identity_digest,authentication_kind,pool_json
             FROM credential_pools ORDER BY pool_id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, u64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, u64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, u64>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut pools = Vec::new();
    for (
        pool_id,
        stored_binding_id,
        stored_binding_revision,
        stored_binding_digest,
        stored_source_id,
        stored_source_revision,
        stored_connection_option_id,
        stored_offer_ref,
        stored_offer_revision,
        stored_offer_evidence_digest,
        stored_billing_class,
        stored_model_configuration_id,
        stored_revision,
        stored_identity_digest,
        stored_authentication,
        encoded,
    ) in pool_rows
    {
        let mut pool: CredentialPoolV1 =
            serde_json::from_str(&encoded).map_err(|_| LocalStorageError::InvalidData)?;
        let (binding, raw_binding) = bindings
            .get(&pool.binding_id)
            .ok_or(LocalStorageError::InvalidData)?;
        let (source, _) = sources
            .get(&pool.source_id)
            .ok_or(LocalStorageError::InvalidData)?;
        let raw_binding: SourceBindingV1 =
            serde_json::from_str(raw_binding).map_err(|_| LocalStorageError::InvalidData)?;
        let raw_binding_digest =
            CanonicalDigest::of(&raw_binding).map_err(|_| LocalStorageError::InvalidData)?;
        let raw_pool_identity_digest =
            CanonicalDigest::of(&pool.identity()).map_err(|_| LocalStorageError::InvalidData)?;
        if pool.pool_id != pool_id
            || pool.binding_id != stored_binding_id
            || pool.binding_revision != stored_binding_revision
            || pool.binding_digest.as_str() != stored_binding_digest
            || pool.source_id != stored_source_id
            || pool.source_revision != stored_source_revision
            || pool.connection_option_id != stored_connection_option_id
            || pool.offer_ref != stored_offer_ref
            || pool.offer_revision != stored_offer_revision
            || pool.offer_evidence_digest.as_str() != stored_offer_evidence_digest
            || enum_token(pool.billing_class)? != stored_billing_class
            || pool.model_configuration_id != stored_model_configuration_id
            || pool.revision != stored_revision
            || raw_pool_identity_digest.as_str() != stored_identity_digest
            || enum_token(pool.authentication)? != stored_authentication
            || pool.binding_digest != raw_binding_digest
            || binding.source_id != pool.source_id
            || binding.source_revision != pool.source_revision
            || source.revision != pool.source_revision
            || (pool.source_identity_digest
                != CanonicalDigest::of(&source.identity)
                    .map_err(|_| LocalStorageError::InvalidData)?
                && pool.source_identity_digest != source.identity_digest)
        {
            return Err(LocalStorageError::InvalidData);
        }
        pool.binding_revision = binding.revision;
        pool.binding_digest =
            CanonicalDigest::of(binding).map_err(|_| LocalStorageError::InvalidData)?;
        pool.source_revision = source.revision;
        pool.source_identity_digest = source.identity_digest.clone();
        pool.validate_against_binding(binding)
            .map_err(|_| LocalStorageError::InvalidData)?;
        pools.push((pool, encoded));
    }

    for (source, encoded) in sources.values() {
        let rewritten =
            serde_json::to_string(source).map_err(|_| LocalStorageError::InvalidData)?;
        if rewritten != *encoded {
            transaction.execute(
                "UPDATE compute_sources
                 SET identity_digest=?2,source_json=?3,updated_at=unixepoch()
                 WHERE source_id=?1",
                params![source.source_id, source.identity_digest.as_str(), rewritten],
            )?;
        }
    }
    for (binding, encoded) in bindings.values() {
        let rewritten =
            serde_json::to_string(binding).map_err(|_| LocalStorageError::InvalidData)?;
        if rewritten != *encoded {
            transaction.execute(
                "UPDATE source_bindings SET binding_json=?2,updated_at=unixepoch()
                 WHERE binding_id=?1",
                params![binding.binding_id, rewritten],
            )?;
        }
    }
    for (pool, encoded) in pools {
        let rewritten = serde_json::to_string(&pool).map_err(|_| LocalStorageError::InvalidData)?;
        if rewritten != encoded {
            transaction.execute(
                "UPDATE credential_pools
                 SET binding_revision=?2,binding_digest=?3,source_revision=?4,
                     homogeneous_identity_digest=?5,pool_json=?6,updated_at=unixepoch()
                 WHERE pool_id=?1",
                params![
                    pool.pool_id,
                    pool.binding_revision,
                    pool.binding_digest.as_str(),
                    pool.source_revision,
                    CanonicalDigest::of(&pool.identity())
                        .map_err(|_| LocalStorageError::InvalidData)?
                        .as_str(),
                    rewritten,
                ],
            )?;
        }
    }
    Ok(())
}

fn enum_token<T: serde::Serialize>(value: T) -> Result<String, LocalStorageError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or(LocalStorageError::InvalidData)
}

#[cfg(test)]
mod tests {
    use hiroute_domain::{
        AuthenticationKind, BillingClass, COMPUTE_STATE_SCHEMA_V1, CredentialRefV1,
        MaterializationState, PoolCredentialV1, SourceIdentityV1, SourceOrigin,
    };
    use rusqlite::Connection;

    use super::*;

    type GraphSnapshot = (
        (String, String, i64),
        (String, i64),
        (String, u64, String, i64),
    );

    #[test]
    fn v8_graph_migration_preserves_activity_is_idempotent_and_rejects_corruption() {
        let mut connection = Connection::open_in_memory().unwrap();
        for sql in [
            super::super::CONTROL_V1,
            super::super::CONTROL_V2,
            super::super::CONTROL_V3,
            super::super::CONTROL_V4,
            super::super::CONTROL_V5,
            super::super::NOOP_V6,
            super::super::CONTROL_V7,
        ] {
            connection.execute_batch(sql).unwrap();
        }
        let identity = SourceIdentityV1 {
            identity_revision: 7,
            provider_platform_id: "provider.legacy".into(),
            service_offering_id: "offering.legacy".into(),
            entitlement_id: "entitlement.legacy".into(),
            usage_scope: "account".into(),
            endpoint_profile_id: "endpoint.legacy".into(),
            endpoint_profile_revision: 9,
            region_id: "test".into(),
            account_subject_ref: "account.legacy".into(),
            evidence_refs: vec![CanonicalDigest::of_bytes(b"legacy-scanner")],
        };
        let legacy_digest = CanonicalDigest::of(&identity).unwrap();
        let semantic_digest = identity.digest().unwrap();
        assert_ne!(legacy_digest, semantic_digest);
        let source = ComputeSourceV1 {
            schema: COMPUTE_STATE_SCHEMA_V1.into(),
            source_id: "source.legacy".into(),
            revision: 7,
            connection_option_id: "option.legacy".into(),
            connector_id: "connector.legacy".into(),
            connector_revision: 1,
            origin: SourceOrigin::NativeApi,
            identity,
            identity_digest: legacy_digest.clone(),
            billing_class: BillingClass::Paid,
            state: MaterializationState::NeedsCredential,
        };
        let binding = SourceBindingV1 {
            binding_id: "binding.legacy".into(),
            revision: 4,
            source_id: source.source_id.clone(),
            source_revision: source.revision,
            source_identity_digest: legacy_digest.clone(),
            model_data_bundle_version: "models.legacy".into(),
            capability_slice_version: "capabilities.legacy".into(),
            offer_ref: "offer.legacy".into(),
            offer_evidence_digest: CanonicalDigest::of_bytes(b"offer-legacy"),
            billing_class: BillingClass::Paid,
            model_configuration_id: "model.legacy".into(),
            upstream_model_id: "upstream.legacy".into(),
            capability_id: "capability.legacy".into(),
            credential_pool_id: Some("pool.legacy".into()),
        };
        let credential = CredentialRefV1::new(
            "credential.legacy",
            "source/source.legacy",
            "hirouted",
            "provider-auth",
            ["connection-option/option.legacy".into()],
            1,
        )
        .unwrap();
        let pool = CredentialPoolV1 {
            pool_id: "pool.legacy".into(),
            binding_id: binding.binding_id.clone(),
            binding_revision: binding.revision,
            binding_digest: CanonicalDigest::of(&binding).unwrap(),
            source_id: source.source_id.clone(),
            source_revision: source.revision,
            connection_option_id: source.connection_option_id.clone(),
            source_identity_digest: legacy_digest,
            offer_ref: binding.offer_ref.clone(),
            offer_revision: 1,
            offer_evidence_digest: binding.offer_evidence_digest.clone(),
            billing_class: BillingClass::Paid,
            model_configuration_id: binding.model_configuration_id.clone(),
            authentication: AuthenticationKind::ProviderApiKey,
            revision: 3,
            credentials: vec![PoolCredentialV1 {
                credential,
                fingerprint: CanonicalDigest::of_bytes(b"legacy-fingerprint"),
                ordinal: 0,
                enabled: true,
            }],
        };
        pool.validate().unwrap();
        connection
            .execute(
                "INSERT INTO compute_sources(
                    source_id,revision,identity_digest,source_json,updated_at
                 ) VALUES (?1,?2,?3,?4,1)",
                params![
                    source.source_id,
                    source.revision,
                    source.identity_digest.as_str(),
                    serde_json::to_string(&source).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_bindings(
                    binding_id,revision,source_id,binding_json,updated_at
                 ) VALUES (?1,?2,?3,?4,1)",
                params![
                    binding.binding_id,
                    binding.revision,
                    binding.source_id,
                    serde_json::to_string(&binding).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO credential_pools(
                    pool_id,binding_id,binding_revision,binding_digest,source_id,
                    source_revision,connection_option_id,offer_ref,offer_revision,
                    offer_evidence_digest,billing_class,model_configuration_id,revision,
                    homogeneous_identity_digest,authentication_kind,pool_json,updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'paid',?11,?12,?13,
                           'provider_api_key',?14,1)",
                params![
                    pool.pool_id,
                    pool.binding_id,
                    pool.binding_revision,
                    pool.binding_digest.as_str(),
                    pool.source_id,
                    pool.source_revision,
                    pool.connection_option_id,
                    pool.offer_ref,
                    pool.offer_revision,
                    pool.offer_evidence_digest.as_str(),
                    pool.model_configuration_id,
                    pool.revision,
                    CanonicalDigest::of(&pool.identity()).unwrap().as_str(),
                    serde_json::to_string(&pool).unwrap(),
                ],
            )
            .unwrap();

        connection.execute_batch(super::super::CONTROL_V8).unwrap();
        let transaction = connection.transaction().unwrap();
        migrate_semantic_identities(&transaction).unwrap();
        transaction.commit().unwrap();

        let rewritten_source: ComputeSourceV1 = serde_json::from_str(
            &connection
                .query_row(
                    "SELECT source_json FROM compute_sources WHERE source_id='source.legacy'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        rewritten_source.validate_shape().unwrap();
        assert_eq!(rewritten_source.identity_digest, semantic_digest);
        let (binding_json, binding_active): (String, bool) = connection
            .query_row(
                "SELECT binding_json, active FROM source_bindings
                 WHERE binding_id='binding.legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let rewritten_binding: SourceBindingV1 = serde_json::from_str(&binding_json).unwrap();
        assert_eq!(rewritten_binding.source_identity_digest, semantic_digest);
        assert!(binding_active);
        let (pool_json, pool_active): (String, bool) = connection
            .query_row(
                "SELECT pool_json, active FROM credential_pools WHERE pool_id='pool.legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let rewritten_pool: CredentialPoolV1 = serde_json::from_str(&pool_json).unwrap();
        rewritten_pool
            .validate_against_binding(&rewritten_binding)
            .unwrap();
        assert_eq!(rewritten_pool.source_identity_digest, semantic_digest);
        assert!(pool_active);

        let snapshot = graph_snapshot(&connection);
        let transaction = connection.transaction().unwrap();
        migrate_semantic_identities(&transaction).unwrap();
        transaction.commit().unwrap();
        assert_eq!(graph_snapshot(&connection), snapshot);

        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "DELETE FROM source_bindings WHERE binding_id='binding.legacy'",
                [],
            )
            .unwrap();
        assert!(matches!(
            migrate_semantic_identities(&transaction),
            Err(LocalStorageError::InvalidData)
        ));
        drop(transaction);

        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "UPDATE credential_pools SET binding_revision=99
                 WHERE pool_id='pool.legacy'",
                [],
            )
            .unwrap();
        assert!(matches!(
            migrate_semantic_identities(&transaction),
            Err(LocalStorageError::InvalidData)
        ));
        drop(transaction);

        let mut duplicate = rewritten_source;
        duplicate.source_id = "source.duplicate".into();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO compute_sources(
                    source_id,revision,identity_digest,source_json,updated_at
                 ) VALUES (?1,?2,?3,?4,1)",
                params![
                    duplicate.source_id,
                    duplicate.revision,
                    duplicate.identity_digest.as_str(),
                    serde_json::to_string(&duplicate).unwrap(),
                ],
            )
            .unwrap();
        assert!(matches!(
            migrate_semantic_identities(&transaction),
            Err(LocalStorageError::InvalidData)
        ));
        drop(transaction);
        assert_eq!(graph_snapshot(&connection), snapshot);
    }

    fn graph_snapshot(connection: &Connection) -> GraphSnapshot {
        let source = connection
            .query_row(
                "SELECT identity_digest,source_json,updated_at FROM compute_sources
                 WHERE source_id='source.legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let binding = connection
            .query_row(
                "SELECT binding_json,updated_at FROM source_bindings
                 WHERE binding_id='binding.legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let pool = connection
            .query_row(
                "SELECT pool_json,binding_revision,homogeneous_identity_digest,updated_at
                 FROM credential_pools WHERE pool_id='pool.legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        (source, binding, pool)
    }
}
