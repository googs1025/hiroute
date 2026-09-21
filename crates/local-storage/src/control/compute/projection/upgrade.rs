use super::*;

pub(super) fn legacy_v7_source_id(projection: &ComputeControlProjectionV1) -> PortResult<String> {
    let agent_id = projection
        .source
        .identity
        .account_subject_ref
        .strip_prefix("account/agent/")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| port(PortErrorCode::Conflict, "compute.projection.legacy_lineage"))?;
    let digest = CanonicalDigest::of(&(
        "hiroute.compute-projection-identity/v1",
        agent_id,
        &projection.scanner.scanner_id,
        &projection.scanner.scanner_version,
        &projection.scanner.discovered_source_ref,
        &projection.source.connection_option_id,
        &projection.source.identity.endpoint_profile_id,
        projection.source.identity.endpoint_profile_revision,
        &projection.binding.upstream_model_id,
        &projection.binding.model_configuration_id,
    ))
    .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_lineage"))?;
    let suffix = digest
        .as_str()
        .strip_prefix("sha256:")
        .and_then(|value| value.get(..24))
        .ok_or_else(|| port(PortErrorCode::Corrupt, "compute.projection.legacy_lineage"))?;
    Ok(format!("source/agent-{suffix}"))
}

pub(super) fn v7_lineage(source: &ComputeSourceV1, binding: &SourceBindingV1) -> bool {
    let Some(suffix) = source.source_id.strip_prefix("source/agent-") else {
        return false;
    };
    suffix.len() == 24
        && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        && source.identity.account_subject_ref == format!("account/agent-{suffix}")
        && binding.binding_id == format!("binding/agent-{suffix}")
        && binding.source_id == source.source_id
}

pub(super) fn stable_source_id(value: &str) -> bool {
    value.strip_prefix("source/agent-").is_some_and(|suffix| {
        suffix.len() == 24 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

pub(super) fn stable_binding_id(value: &str) -> bool {
    value.strip_prefix("binding/agent-").is_some_and(|suffix| {
        suffix.len() == 24 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

pub(super) fn derived_binding_id(
    canonical_source_id: &str,
    binding: &SourceBindingV1,
) -> PortResult<String> {
    let digest = CanonicalDigest::of(&(
        "hiroute.compute-source-binding-identity/v1",
        canonical_source_id,
        &binding.model_configuration_id,
        &binding.upstream_model_id,
        &binding.offer_ref,
        &binding.capability_id,
    ))
    .map_err(|_| {
        port(
            PortErrorCode::Corrupt,
            "compute.projection.binding_identity",
        )
    })?;
    let suffix = digest
        .as_str()
        .strip_prefix("sha256:")
        .and_then(|value| value.get(..24))
        .ok_or_else(|| {
            port(
                PortErrorCode::Corrupt,
                "compute.projection.binding_identity",
            )
        })?;
    Ok(format!("binding/agent-{suffix}"))
}

pub(super) fn legacy_semantics_match(
    before: &ProjectionEffectStateV1,
    desired: &ComputeControlProjectionV1,
) -> PortResult<bool> {
    let (Some(source), Some(binding), Some(inventory)) =
        (&before.source, &before.binding, &before.inventory)
    else {
        return Ok(false);
    };
    let owner_matches =
        v7_lineage(source, binding) && legacy_v7_source_id(desired)? == source.source_id;
    Ok(owner_matches && shared_semantics_match(before, desired, source, binding, inventory))
}

pub(super) fn continued_alias_semantics_match(
    before: &ProjectionEffectStateV1,
    desired: &ComputeControlProjectionV1,
) -> PortResult<bool> {
    let (Some(source), Some(binding), Some(inventory)) =
        (&before.source, &before.binding, &before.inventory)
    else {
        return Ok(false);
    };
    let owner_matches = source.identity.account_subject_ref
        == desired.source.identity.account_subject_ref
        || (v7_lineage(source, binding) && legacy_v7_source_id(desired)? == source.source_id);
    Ok(owner_matches && shared_semantics_match(before, desired, source, binding, inventory))
}

fn shared_semantics_match(
    before: &ProjectionEffectStateV1,
    desired: &ComputeControlProjectionV1,
    source: &ComputeSourceV1,
    binding: &SourceBindingV1,
    inventory: &ComputeInventorySnapshotV1,
) -> bool {
    let left = &source.identity;
    let right = &desired.source.identity;
    before.other_bindings.is_empty()
        && source.connection_option_id == desired.source.connection_option_id
        && source.connector_id == desired.source.connector_id
        && source.origin == desired.source.origin
        && source.billing_class == desired.source.billing_class
        && left.provider_platform_id == right.provider_platform_id
        && left.service_offering_id == right.service_offering_id
        && left.entitlement_id == right.entitlement_id
        && left.usage_scope == right.usage_scope
        && left.endpoint_profile_id == right.endpoint_profile_id
        && left.region_id == right.region_id
        && binding.model_configuration_id == desired.binding.model_configuration_id
        && binding.upstream_model_id == desired.binding.upstream_model_id
        && binding.offer_ref == desired.binding.offer_ref
        && binding.capability_id == desired.binding.capability_id
        && binding.billing_class == desired.binding.billing_class
        && inventory.endpoint_profile_id == desired.inventory.endpoint_profile_id
}

pub(super) fn rekey_legacy_pool(
    pool: &CredentialPoolV1,
    identity: &CredentialPoolIdentityV1,
) -> PortResult<CredentialPoolV1> {
    if pool.connection_option_id != identity.connection_option_id
        || pool.offer_ref != identity.offer_ref
        || pool.billing_class != identity.billing_class
        || pool.model_configuration_id != identity.model_configuration_id
        || pool.authentication != identity.authentication
        || pool.source_id != identity.source_id
        || pool
            .credentials
            .iter()
            .any(|entry| entry.credential.owner_scope() != format!("source/{}", identity.source_id))
    {
        return Err(port(
            PortErrorCode::Conflict,
            "compute.projection.pool_identity_transition",
        ));
    }
    let mut rebound = pool.clone();
    rebound.pool_id = identity.pool_id.clone();
    rebound.binding_id = identity.binding_id.clone();
    rebound.binding_revision = identity.binding_revision;
    rebound.binding_digest = identity.binding_digest.clone();
    rebound.source_revision = identity.source_revision;
    rebound.source_identity_digest = identity.source_identity_digest.clone();
    rebound.offer_revision = identity.offer_revision;
    rebound.offer_evidence_digest = identity.offer_evidence_digest.clone();
    rebound.revision = rebound
        .revision
        .checked_add(1)
        .ok_or_else(|| port(PortErrorCode::Conflict, "compute.projection.pool_revision"))?;
    rebound
        .validate()
        .map_err(|_| port(PortErrorCode::Conflict, "compute.projection.pool_rekey"))?;
    Ok(rebound)
}

pub(super) fn normalize_projection(
    projection: &ComputeControlProjectionV1,
) -> PortResult<ComputeControlProjectionV1> {
    if projection.validate_shape().is_ok() {
        return Ok(projection.clone());
    }
    let mut normalized = projection.clone();
    let legacy_digest = CanonicalDigest::of(&normalized.source.identity)
        .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_identity"))?;
    let legacy_binding_digest = CanonicalDigest::of(&normalized.binding)
        .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_binding"))?;
    if normalized.source.identity_digest != legacy_digest
        || normalized.binding.source_identity_digest != legacy_digest
        || normalized
            .credential_pool_identity
            .as_ref()
            .is_some_and(|pool| {
                pool.source_identity_digest != legacy_digest
                    || pool.binding_digest != legacy_binding_digest
            })
    {
        return Err(port(
            PortErrorCode::InvalidData,
            "compute.projection.desired",
        ));
    }
    let semantic_digest = normalized
        .source
        .identity
        .digest()
        .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_identity"))?;
    normalized.source.identity_digest = semantic_digest.clone();
    normalized.binding.source_identity_digest = semantic_digest.clone();
    if let Some(pool) = &mut normalized.credential_pool_identity {
        pool.source_identity_digest = semantic_digest;
        pool.binding_digest = CanonicalDigest::of(&normalized.binding)
            .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.binding_digest"))?;
    }
    normalized
        .validate_shape()
        .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_shape"))?;
    Ok(normalized)
}

pub(super) fn normalize_v7_state(state: &mut ProjectionEffectStateV1) -> PortResult<()> {
    let Some(source) = &mut state.source else {
        return Ok(());
    };
    if source.validate_shape().is_ok() {
        return Ok(());
    }
    let legacy_digest = CanonicalDigest::of(&source.identity)
        .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_identity"))?;
    if source.identity_digest != legacy_digest {
        return Err(port(
            PortErrorCode::Corrupt,
            "compute.projection.legacy_identity",
        ));
    }
    let mut raw_binding_digests = std::collections::BTreeMap::new();
    for binding in state.binding.iter().chain(&state.other_bindings) {
        if binding.source_identity_digest != legacy_digest {
            return Err(port(
                PortErrorCode::Corrupt,
                "compute.projection.legacy_binding",
            ));
        }
        raw_binding_digests.insert(
            binding.binding_id.clone(),
            CanonicalDigest::of(binding)
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_binding"))?,
        );
    }
    for pool in &state.credential_pools {
        if pool.source_identity_digest != legacy_digest
            || raw_binding_digests
                .get(&pool.binding_id)
                .is_none_or(|digest| digest != &pool.binding_digest)
        {
            return Err(port(
                PortErrorCode::Corrupt,
                "compute.projection.legacy_pool",
            ));
        }
    }
    let semantic_digest = source
        .identity
        .digest()
        .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_identity"))?;
    source.identity_digest = semantic_digest.clone();
    for binding in state.binding.iter_mut().chain(&mut state.other_bindings) {
        binding.source_identity_digest = semantic_digest.clone();
    }
    let binding_digests = state
        .binding
        .iter()
        .chain(&state.other_bindings)
        .map(|binding| {
            CanonicalDigest::of(binding)
                .map(|digest| (binding.binding_id.clone(), digest))
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.binding_digest"))
        })
        .collect::<PortResult<std::collections::BTreeMap<_, _>>>()?;
    for pool in &mut state.credential_pools {
        pool.source_identity_digest = semantic_digest.clone();
        pool.binding_digest = binding_digests
            .get(&pool.binding_id)
            .cloned()
            .ok_or_else(|| port(PortErrorCode::Corrupt, "compute.projection.pool_binding"))?;
    }
    Ok(())
}

pub(super) fn matches_binding_expectation(
    current_revision: Option<u64>,
    current: Option<&SourceBindingV1>,
    source: Option<&ComputeSourceV1>,
    expected_revision: u64,
    expected_digest: Option<&CanonicalDigest>,
) -> PortResult<bool> {
    if matches_revision_digest(
        current_revision,
        current,
        expected_revision,
        expected_digest,
    )? {
        return Ok(true);
    }
    let legacy_digest = current
        .zip(source)
        .map(|(binding, source)| {
            let mut legacy = binding.clone();
            legacy.source_identity_digest = CanonicalDigest::of(&source.identity)
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_binding"))?;
            CanonicalDigest::of(&legacy)
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_binding"))
        })
        .transpose()?;
    Ok(current_revision.unwrap_or(0) == expected_revision
        && legacy_digest.as_ref() == expected_digest)
}

pub(super) fn inventory_row(
    connection: &rusqlite::Connection,
    source_id: &str,
    endpoint_profile_id: &str,
) -> PortResult<Option<ComputeInventorySnapshotV1>> {
    connection
        .query_row(
            "SELECT inventory_revision,inventory_digest,observed_models_json,captured_at
             FROM source_inventory_snapshots
             WHERE source_id=?1 AND endpoint_profile_id=?2",
            params![source_id, endpoint_profile_id],
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.inventory"))?
        .map(|(inventory_revision, digest, models, captured_at)| {
            Ok(ComputeInventorySnapshotV1 {
                source_id: source_id.to_owned(),
                endpoint_profile_id: endpoint_profile_id.to_owned(),
                inventory_revision,
                inventory_digest: CanonicalDigest::parse(digest).map_err(|_| {
                    port(
                        PortErrorCode::Corrupt,
                        "compute.projection.inventory_digest",
                    )
                })?,
                observed_models: decode(&models)?,
                captured_at,
            })
        })
        .transpose()
}
