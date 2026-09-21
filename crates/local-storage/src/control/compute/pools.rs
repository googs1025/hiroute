use hiroute_domain::{CredentialPoolIdentityV1, MaterializationState};

use super::*;

impl ControlStore {
    pub fn put_source_binding(
        &self,
        expected_revision: u64,
        binding: &SourceBindingV1,
        registry: &ConnectorRegistryBundleV1,
        model_data: &ModelDataBundleV1,
    ) -> PortResult<()> {
        registry
            .validate()
            .map_err(|_| invalid("compute.registry.validate"))?;
        model_data
            .validate_against(registry)
            .map_err(|_| invalid("compute.model_data.validate"))?;
        let source = self
            .compute_source(&binding.source_id)?
            .ok_or_else(|| invalid("compute.binding.source"))?;
        binding
            .validate(&source, model_data)
            .map_err(|_| invalid("compute.binding.validate"))?;
        let resolved = registry
            .resolve_option(&source.connection_option_id)
            .map_err(|_| invalid("compute.binding.connection_option"))?;
        match (
            resolved.connector.authentication,
            binding.credential_pool_id.as_deref(),
        ) {
            (AuthenticationKind::None, None) => {}
            (
                AuthenticationKind::ProviderApiKey | AuthenticationKind::ConnectorOwnedOpaque,
                Some(pool_id),
            ) => match self.credential_pool(pool_id)? {
                Some(pool) => {
                    pool.validate_against_source(&source, registry, model_data)
                        .map_err(|_| invalid("compute.binding.pool"))?;
                    pool.validate_against_binding(binding)
                        .map_err(|_| invalid("compute.binding.identity"))?;
                }
                None if source.state == MaterializationState::NeedsCredential => {
                    CredentialPoolIdentityV1::for_registered_binding(
                        binding, &source, registry, model_data,
                    )
                    .map_err(|_| invalid("compute.binding.pool_identity"))?;
                }
                None => return Err(invalid("compute.binding.pool")),
            },
            _ => return Err(invalid("compute.binding.pool")),
        }
        put_revisioned(
            &mut self.connection.borrow_mut(),
            "source_bindings",
            "binding_id",
            &binding.binding_id,
            "binding_json",
            binding.revision,
            expected_revision,
            binding,
            Some(("source_id", binding.source_id.as_str())),
        )
    }

    pub fn source_binding(&self, binding_id: &str) -> PortResult<Option<SourceBindingV1>> {
        validate_storage_id(binding_id)?;
        let binding: Option<SourceBindingV1> = self
            .connection
            .borrow()
            .query_row(
                "SELECT binding_json FROM source_bindings
                 WHERE binding_id=?1 AND active=1",
                params![binding_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| port("compute.binding.read"))?
            .map(|encoded| decode(&encoded))
            .transpose()?;
        binding
            .map(|binding| {
                if binding.binding_id != binding_id {
                    return Err(invalid("compute.binding.identity"));
                }
                binding
                    .validate_shape()
                    .map_err(|_| invalid("compute.binding.validate"))?;
                Ok(binding)
            })
            .transpose()
    }

    pub fn put_credential_pool(
        &self,
        expected_revision: u64,
        pool: &CredentialPoolV1,
        registry: &ConnectorRegistryBundleV1,
        model_data: &ModelDataBundleV1,
    ) -> PortResult<()> {
        registry
            .validate()
            .map_err(|_| invalid("compute.registry.validate"))?;
        let source = self
            .compute_source(&pool.source_id)?
            .ok_or_else(|| invalid("compute.pool.source"))?;
        let binding = self
            .source_binding(&pool.binding_id)?
            .ok_or_else(|| invalid("compute.pool.binding"))?;
        model_data
            .validate_against(registry)
            .map_err(|_| invalid("compute.model_data.validate"))?;
        binding
            .validate(&source, model_data)
            .map_err(|_| invalid("compute.pool.binding"))?;
        pool.validate_against_source(&source, registry, model_data)
            .and_then(|()| pool.validate_against_binding(&binding))
            .map_err(|_| invalid("compute.pool.validate"))?;
        let mut connection = self.connection.borrow_mut();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| port("compute.pool.begin"))?;
        let current: Option<u64> = transaction
            .query_row(
                "SELECT revision FROM credential_pools WHERE pool_id = ?1",
                params![pool.pool_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| port("compute.pool.lookup"))?;
        if current.unwrap_or(0) != expected_revision
            || expected_revision.checked_add(1) != Some(pool.revision)
        {
            return Err(conflict("compute.pool.revision"));
        }
        write_pool(&transaction, pool, "compute.pool.write")?;
        transaction
            .commit()
            .map_err(|_| port("compute.pool.commit"))
    }

    pub fn credential_pool(&self, pool_id: &str) -> PortResult<Option<CredentialPoolV1>> {
        validate_storage_id(pool_id)?;
        let pool: Option<CredentialPoolV1> = self
            .connection
            .borrow()
            .query_row(
                "SELECT pool_json FROM credential_pools WHERE pool_id=?1 AND active=1",
                params![pool_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| port("compute.pool.read"))?
            .map(|encoded| decode(&encoded))
            .transpose()?;
        pool.map(|pool| {
            if pool.pool_id != pool_id {
                return Err(invalid("compute.pool.identity"));
            }
            pool.validate()
                .map_err(|_| invalid("compute.pool.validate"))?;
            Ok(pool)
        })
        .transpose()
    }
}

pub(super) fn write_pool(
    transaction: &rusqlite::Transaction<'_>,
    pool: &CredentialPoolV1,
    error_code: &'static str,
) -> PortResult<()> {
    let encoded = encode(pool)?;
    let identity_digest = CanonicalDigest::of(&pool.identity())
        .map_err(|_| invalid("compute.pool.identity_digest"))?;
    transaction
        .execute(
            "INSERT INTO credential_pools(
                pool_id, binding_id, binding_revision, binding_digest, source_id,
                source_revision, connection_option_id, offer_ref, offer_revision,
                offer_evidence_digest, billing_class, model_configuration_id, revision,
                homogeneous_identity_digest, authentication_kind, pool_json, active, updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,1,unixepoch())
             ON CONFLICT(pool_id) DO UPDATE SET binding_id=excluded.binding_id,
                binding_revision=excluded.binding_revision, binding_digest=excluded.binding_digest,
                source_id=excluded.source_id, source_revision=excluded.source_revision,
                connection_option_id=excluded.connection_option_id, offer_ref=excluded.offer_ref,
                offer_revision=excluded.offer_revision,
                offer_evidence_digest=excluded.offer_evidence_digest,
                billing_class=excluded.billing_class,
                model_configuration_id=excluded.model_configuration_id,
                revision=excluded.revision,
                homogeneous_identity_digest=excluded.homogeneous_identity_digest,
                authentication_kind=excluded.authentication_kind,
                pool_json=excluded.pool_json, active=1, updated_at=excluded.updated_at",
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
                billing_class(pool.billing_class),
                pool.model_configuration_id,
                pool.revision,
                identity_digest.as_str(),
                authentication_kind(pool.authentication),
                encoded,
            ],
        )
        .map_err(|_| port(error_code))?;
    Ok(())
}
