use hiroute_domain::{
    CanonicalDigest, ComputeControlProjectionV1, ComputeInventorySnapshotV1,
    ComputeProjectionExpectationV1, ComputeSourceV1, CredentialPoolIdentityV1, CredentialPoolV1,
    OperationId, PortError, PortErrorCode, PortResult, SourceBindingV1,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{decode, encode};

const EFFECT_STATE_SCHEMA: &str = "hiroute.compute-projection-effect-state/v1";

mod upgrade;

use upgrade::{
    continued_alias_semantics_match, derived_binding_id, inventory_row, legacy_semantics_match,
    legacy_v7_source_id, matches_binding_expectation, normalize_projection, normalize_v7_state,
    rekey_legacy_pool, stable_binding_id, stable_source_id, v7_lineage,
};

impl crate::control::ControlStore {
    /// Looks up a pre-v8 graph only when the trusted discovery adapter supplies its exact legacy
    /// Source ID. Callers without that provenance must use the conservative legacy-blind entry.
    pub fn compute_projection_expectation_with_legacy_lineage(
        &self,
        source_id: &str,
        binding_id: &str,
        endpoint_profile_id: &str,
        legacy_v7_source_id: &str,
    ) -> PortResult<ComputeProjectionExpectationV1> {
        expectation_with_legacy_lineage(
            &self.connection.borrow(),
            source_id,
            binding_id,
            endpoint_profile_id,
            Some(legacy_v7_source_id),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProjectionEffectStateV1 {
    schema: String,
    source_id: String,
    binding_id: String,
    endpoint_profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<ComputeSourceV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding: Option<SourceBindingV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    other_bindings: Vec<SourceBindingV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    credential_pools: Vec<CredentialPoolV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inventory: Option<ComputeInventorySnapshotV1>,
}

impl ProjectionEffectStateV1 {
    fn current(
        connection: &rusqlite::Connection,
        source_id: &str,
        binding_id: &str,
        endpoint_profile_id: &str,
    ) -> PortResult<Self> {
        let source = json_row(
            connection,
            "SELECT source_json FROM compute_sources WHERE source_id=?1",
            source_id,
        )?;
        let binding = json_row(
            connection,
            "SELECT binding_json FROM source_bindings WHERE binding_id=?1 AND active=1",
            binding_id,
        )?;
        let mut other_bindings = binding_rows(connection, source_id, binding_id)?;
        other_bindings.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
        let mut credential_pools = pool_rows(connection, source_id)?;
        credential_pools.sort_by(|left, right| left.pool_id.cmp(&right.pool_id));
        let inventory = connection
            .query_row(
                "SELECT inventory_revision, inventory_digest, observed_models_json, captured_at
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
            .map(|(revision, digest, models, captured_at)| {
                Ok(ComputeInventorySnapshotV1 {
                    source_id: source_id.to_owned(),
                    endpoint_profile_id: endpoint_profile_id.to_owned(),
                    inventory_revision: revision,
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
            .transpose()?;
        let state = Self {
            schema: EFFECT_STATE_SCHEMA.into(),
            source_id: source_id.to_owned(),
            binding_id: binding_id.to_owned(),
            endpoint_profile_id: endpoint_profile_id.to_owned(),
            source,
            binding,
            other_bindings,
            credential_pools,
            inventory,
        };
        state.validate()?;
        Ok(state)
    }

    fn desired(projection: &ComputeControlProjectionV1, before: &Self) -> PortResult<Self> {
        let credential_pools = projection
            .credential_pool_identity
            .as_ref()
            .and_then(|identity| {
                before
                    .credential_pools
                    .iter()
                    .find(|pool| pool.pool_id == identity.pool_id)
                    .or_else(|| {
                        (before.binding_id != projection.binding.binding_id
                            && before
                                .source
                                .as_ref()
                                .zip(before.binding.as_ref())
                                .is_some_and(|(source, binding)| v7_lineage(source, binding)))
                        .then(|| before.credential_pools.first())
                        .flatten()
                    })
                    .map(|pool| (pool, identity))
            })
            .map(|(pool, identity)| {
                let rebound = if pool.pool_id == identity.pool_id {
                    pool.rebind_registered_identity(identity).map_err(|_| {
                        port(PortErrorCode::Conflict, "compute.projection.pool_rebind")
                    })?
                } else {
                    rekey_legacy_pool(pool, identity)?
                };
                rebound
                    .validate_against_binding(&projection.binding)
                    .map_err(|_| {
                        port(PortErrorCode::Conflict, "compute.projection.pool_binding")
                    })?;
                Ok(rebound)
            })
            .transpose()?
            .into_iter()
            .collect();
        let state = Self {
            schema: EFFECT_STATE_SCHEMA.into(),
            source_id: projection.source.source_id.clone(),
            binding_id: projection.binding.binding_id.clone(),
            endpoint_profile_id: projection.inventory.endpoint_profile_id.clone(),
            source: Some(projection.source.clone()),
            binding: Some(projection.binding.clone()),
            other_bindings: Vec::new(),
            credential_pools,
            inventory: Some(projection.inventory.clone()),
        };
        state.validate()?;
        Ok(state)
    }

    fn validate(&self) -> PortResult<()> {
        if self.schema != EFFECT_STATE_SCHEMA
            || self.source_id.is_empty()
            || self.binding_id.is_empty()
            || self.endpoint_profile_id.is_empty()
            || self
                .source
                .as_ref()
                .is_some_and(|source| source.source_id != self.source_id)
            || self
                .binding
                .as_ref()
                .is_some_and(|binding| binding.binding_id != self.binding_id)
            || self
                .other_bindings
                .iter()
                .any(|binding| binding.binding_id == self.binding_id)
            || self.inventory.as_ref().is_some_and(|inventory| {
                inventory.source_id != self.source_id
                    || inventory.endpoint_profile_id != self.endpoint_profile_id
            })
        {
            return Err(port(
                PortErrorCode::Corrupt,
                "compute.projection.effect_schema",
            ));
        }
        if let Some(source) = &self.source {
            source
                .validate_shape()
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.source"))?;
        }
        if let Some(binding) = &self.binding {
            binding
                .validate_shape()
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.binding"))?;
        }
        let mut binding_ids = std::collections::BTreeSet::new();
        if let Some(binding) = &self.binding {
            binding_ids.insert(binding.binding_id.as_str());
        }
        for binding in &self.other_bindings {
            binding
                .validate_shape()
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.binding"))?;
            if !binding_ids.insert(binding.binding_id.as_str()) {
                return Err(port(
                    PortErrorCode::Corrupt,
                    "compute.projection.binding_identity",
                ));
            }
        }
        if let Some(inventory) = &self.inventory {
            inventory
                .validate()
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.inventory"))?;
        }
        if self
            .binding
            .iter()
            .chain(&self.other_bindings)
            .any(|binding| {
                self.source.as_ref().is_none_or(|source| {
                    binding.source_id != source.source_id
                        || binding.source_revision != source.revision
                        || binding.source_identity_digest != source.identity_digest
                })
            })
            || self.inventory.as_ref().is_some_and(|inventory| {
                self.source.as_ref().is_none_or(|source| {
                    inventory.source_id != source.source_id
                        || inventory.endpoint_profile_id != source.identity.endpoint_profile_id
                })
            })
            || self.credential_pools.iter().any(|pool| {
                pool.validate().is_err()
                    || self.source.as_ref().is_none_or(|source| {
                        pool.source_id != source.source_id
                            || pool.source_revision != source.revision
                            || pool.source_identity_digest != source.identity_digest
                            || pool.connection_option_id != source.connection_option_id
                    })
                    || self
                        .binding
                        .iter()
                        .chain(&self.other_bindings)
                        .find(|binding| binding.binding_id == pool.binding_id)
                        .is_none_or(|binding| pool.validate_against_binding(binding).is_err())
            })
        {
            return Err(port(
                PortErrorCode::Corrupt,
                "compute.projection.effect_cross_reference",
            ));
        }
        Ok(())
    }

    fn matches_expectation(&self, expected: &ComputeProjectionExpectationV1) -> PortResult<bool> {
        Ok(matches_source_expectation(
            self.source.as_ref().map(|value| value.revision),
            self.source.as_ref(),
            expected.source_revision,
            expected.source_digest.as_ref(),
        )? && matches_binding_expectation(
            self.binding.as_ref().map(|value| value.revision),
            self.binding.as_ref(),
            self.source.as_ref(),
            expected.binding_revision,
            expected.binding_digest.as_ref(),
        )? && matches_revision_digest(
            self.inventory
                .as_ref()
                .map(|value| value.inventory_revision),
            self.inventory.as_ref(),
            expected.inventory_revision,
            expected.inventory_digest.as_ref(),
        )?)
    }
}

pub(super) fn stage_states(
    connection: &rusqlite::Connection,
    operation_id: &OperationId,
    projection: &ComputeControlProjectionV1,
    expected: &ComputeProjectionExpectationV1,
) -> PortResult<(Option<String>, String, String)> {
    let projection = normalize_projection(projection)?;
    let mapped_source_id = physical_source_id(connection, &projection.source.source_id)?;
    let exact = ProjectionEffectStateV1::current(
        connection,
        &mapped_source_id,
        &projection.binding.binding_id,
        &projection.inventory.endpoint_profile_id,
    )?;
    let before = if exact.source.is_none() && expected.source_revision > 0 {
        legacy_state_for_projection(connection, &projection)?
            .ok_or_else(|| port(PortErrorCode::Conflict, "compute.projection.legacy_missing"))?
    } else {
        exact
    };
    if !before.matches_expectation(expected)? {
        return Err(port(PortErrorCode::Conflict, "compute.projection.cas"));
    }
    if before.source_id != projection.source.source_id {
        register_identity_alias(connection, operation_id, &projection, &before)?;
    }
    let physical = physicalize_projection(&projection, &before.source_id)?;
    let after = ProjectionEffectStateV1::desired(&physical, &before)?;
    Ok((
        Some(encode(&before)?),
        encode(&after)?,
        after.source_id.clone(),
    ))
}

pub(super) fn expectation(
    connection: &rusqlite::Connection,
    canonical_source_id: &str,
    canonical_binding_id: &str,
    endpoint_profile_id: &str,
) -> PortResult<ComputeProjectionExpectationV1> {
    expectation_with_legacy_lineage(
        connection,
        canonical_source_id,
        canonical_binding_id,
        endpoint_profile_id,
        None,
    )
}

fn expectation_with_legacy_lineage(
    connection: &rusqlite::Connection,
    canonical_source_id: &str,
    canonical_binding_id: &str,
    endpoint_profile_id: &str,
    legacy_v7_source_id: Option<&str>,
) -> PortResult<ComputeProjectionExpectationV1> {
    let physical_source_id = physical_source_id(connection, canonical_source_id)?;
    let source: Option<ComputeSourceV1> = json_row(
        connection,
        "SELECT source_json FROM compute_sources WHERE source_id=?1",
        &physical_source_id,
    )?;
    let binding: Option<SourceBindingV1> = json_row(
        connection,
        "SELECT binding_json FROM source_bindings WHERE binding_id=?1 AND active=1",
        canonical_binding_id,
    )?;
    let inventory = inventory_row(connection, &physical_source_id, endpoint_profile_id)?;
    let alias_pending = physical_source_id != canonical_source_id && binding.is_none();
    let no_exact_graph = source.is_none() && binding.is_none() && inventory.is_none();
    if (alias_pending || no_exact_graph)
        && let Some(legacy) = legacy_state_for_ids(
            connection,
            canonical_source_id,
            canonical_binding_id,
            endpoint_profile_id,
            (physical_source_id != canonical_source_id).then_some(physical_source_id.as_str()),
            legacy_v7_source_id,
        )?
    {
        return expectation_from_state(&legacy);
    }
    if source.is_none() && (binding.is_some() || inventory.is_some())
        || binding
            .as_ref()
            .is_some_and(|value| value.source_id != physical_source_id)
    {
        return Err(port(
            PortErrorCode::Corrupt,
            "compute.projection.partial_identity",
        ));
    }
    Ok(ComputeProjectionExpectationV1 {
        source_revision: source.as_ref().map_or(0, |value| value.revision),
        source_digest: source
            .as_ref()
            .map(CanonicalDigest::of)
            .transpose()
            .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.source_digest"))?,
        binding_revision: binding.as_ref().map_or(0, |value| value.revision),
        binding_digest: binding
            .as_ref()
            .map(CanonicalDigest::of)
            .transpose()
            .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.binding_digest"))?,
        inventory_revision: inventory
            .as_ref()
            .map_or(0, |value| value.inventory_revision),
        inventory_digest: inventory
            .as_ref()
            .map(CanonicalDigest::of)
            .transpose()
            .map_err(|_| {
                port(
                    PortErrorCode::Corrupt,
                    "compute.projection.inventory_digest",
                )
            })?,
    })
}

pub(super) fn physical_source_id(
    connection: &rusqlite::Connection,
    canonical_source_id: &str,
) -> PortResult<String> {
    connection
        .query_row(
            "SELECT physical_source_id FROM compute_source_identity_aliases
             WHERE canonical_source_id=?1",
            params![canonical_source_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map(|value| value.unwrap_or_else(|| canonical_source_id.to_owned()))
        .map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.alias_read"))
}

fn expectation_from_state(
    state: &ProjectionEffectStateV1,
) -> PortResult<ComputeProjectionExpectationV1> {
    let source = state
        .source
        .as_ref()
        .ok_or_else(|| port(PortErrorCode::Corrupt, "compute.projection.legacy_source"))?;
    let binding = state
        .binding
        .as_ref()
        .ok_or_else(|| port(PortErrorCode::Corrupt, "compute.projection.legacy_binding"))?;
    let inventory = state.inventory.as_ref().ok_or_else(|| {
        port(
            PortErrorCode::Corrupt,
            "compute.projection.legacy_inventory",
        )
    })?;
    Ok(ComputeProjectionExpectationV1 {
        source_revision: source.revision,
        source_digest: Some(
            CanonicalDigest::of(source)
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.source_digest"))?,
        ),
        binding_revision: binding.revision,
        binding_digest: Some(
            CanonicalDigest::of(binding)
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.binding_digest"))?,
        ),
        inventory_revision: inventory.inventory_revision,
        inventory_digest: Some(CanonicalDigest::of(inventory).map_err(|_| {
            port(
                PortErrorCode::Corrupt,
                "compute.projection.inventory_digest",
            )
        })?),
    })
}

fn legacy_state_for_projection(
    connection: &rusqlite::Connection,
    projection: &ComputeControlProjectionV1,
) -> PortResult<Option<ProjectionEffectStateV1>> {
    let physical = physical_source_id(connection, &projection.source.source_id)?;
    let candidate = legacy_state_for_ids(
        connection,
        &projection.source.source_id,
        &projection.binding.binding_id,
        &projection.inventory.endpoint_profile_id,
        (physical != projection.source.source_id).then_some(physical.as_str()),
        Some(&legacy_v7_source_id(projection)?),
    )?;
    candidate
        .map(|candidate| {
            if !legacy_semantics_match(&candidate, projection)? {
                return Err(port(
                    PortErrorCode::Conflict,
                    "compute.projection.legacy_identity",
                ));
            }
            Ok(candidate)
        })
        .transpose()
}

fn legacy_state_for_ids(
    connection: &rusqlite::Connection,
    canonical_source_id: &str,
    canonical_binding_id: &str,
    endpoint_profile_id: &str,
    physical_hint: Option<&str>,
    legacy_v7_source_id: Option<&str>,
) -> PortResult<Option<ProjectionEffectStateV1>> {
    if !stable_source_id(canonical_source_id) || !stable_binding_id(canonical_binding_id) {
        return Ok(None);
    }
    let mut statement = connection
        .prepare(
            "SELECT s.source_json,b.binding_json
             FROM compute_sources s
             JOIN source_bindings b ON b.source_id=s.source_id AND b.active=1
             JOIN source_inventory_snapshots i ON i.source_id=s.source_id
             WHERE i.endpoint_profile_id=?1
             ORDER BY s.source_id,b.binding_id",
        )
        .map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "compute.projection.legacy_prepare",
            )
        })?;
    let rows = statement
        .query_map(params![endpoint_profile_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "compute.projection.legacy_query",
            )
        })?;
    if let Some(legacy_v7_source_id) = legacy_v7_source_id
        && !stable_source_id(legacy_v7_source_id)
    {
        return Err(port(
            PortErrorCode::InvalidData,
            "compute.projection.legacy_lineage",
        ));
    }
    let expected_physical = physical_hint.or(legacy_v7_source_id);
    let mut match_ids = Vec::new();
    let mut unproven_related = false;
    for row in rows {
        let (source_json, binding_json) =
            row.map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_row"))?;
        let source: ComputeSourceV1 = decode(&source_json)?;
        let binding: SourceBindingV1 = decode(&binding_json)?;
        source
            .validate_shape()
            .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_source"))?;
        binding
            .validate_shape()
            .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_binding"))?;
        if !v7_lineage(&source, &binding)
            || derived_binding_id(canonical_source_id, &binding)? != canonical_binding_id
        {
            continue;
        }
        if expected_physical == Some(source.source_id.as_str()) {
            match_ids.push((source.source_id, binding.binding_id));
        } else {
            unproven_related = true;
        }
    }
    if match_ids.len() > 1 {
        return Err(port(
            PortErrorCode::Conflict,
            "compute.projection.legacy_collision",
        ));
    }
    if let Some((source, binding)) = match_ids.pop() {
        return ProjectionEffectStateV1::current(
            connection,
            &source,
            &binding,
            endpoint_profile_id,
        )
        .map(Some);
    }
    if unproven_related {
        return Err(port(
            PortErrorCode::Conflict,
            "compute.projection.legacy_lineage_required",
        ));
    }
    Ok(None)
}

fn physicalize_projection(
    desired: &ComputeControlProjectionV1,
    physical_source_id: &str,
) -> PortResult<ComputeControlProjectionV1> {
    if desired.source.source_id == physical_source_id {
        return Ok(desired.clone());
    }
    let mut physical = desired.clone();
    physical.source.source_id = physical_source_id.to_owned();
    physical.binding.source_id = physical_source_id.to_owned();
    physical.inventory.source_id = physical_source_id.to_owned();
    if let Some(pool) = &mut physical.credential_pool_identity {
        pool.source_id = physical_source_id.to_owned();
        pool.binding_digest = CanonicalDigest::of(&physical.binding)
            .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.binding_digest"))?;
    }
    physical
        .validate_shape()
        .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.physical_shape"))?;
    Ok(physical)
}

fn register_identity_alias(
    connection: &rusqlite::Connection,
    operation_id: &OperationId,
    desired: &ComputeControlProjectionV1,
    before: &ProjectionEffectStateV1,
) -> PortResult<()> {
    let physical_source_id = before
        .source
        .as_ref()
        .map(|source| source.source_id.as_str());
    if physical_source_id != Some(before.source_id.as_str()) {
        return Err(port(
            PortErrorCode::Conflict,
            "compute.projection.alias_identity",
        ));
    }
    let existing = connection
        .query_row(
            "SELECT canonical_source_id,physical_source_id,canonical_identity_digest
             FROM compute_source_identity_aliases
             WHERE canonical_source_id=?1 OR physical_source_id=?2",
            params![desired.source.source_id, before.source_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "compute.projection.alias_lookup",
            )
        })?;
    let exact = (
        desired.source.source_id.clone(),
        before.source_id.clone(),
        desired.source.identity_digest.to_string(),
    );
    if let Some(existing) = existing {
        if existing != exact || !continued_alias_semantics_match(before, desired)? {
            return Err(port(
                PortErrorCode::Conflict,
                "compute.projection.alias_collision",
            ));
        }
        return Ok(());
    }
    if !legacy_semantics_match(before, desired)? {
        return Err(port(
            PortErrorCode::Conflict,
            "compute.projection.alias_identity",
        ));
    }
    connection
        .execute(
            "INSERT INTO compute_source_identity_aliases(
                canonical_source_id,physical_source_id,canonical_identity_digest,
                owner_operation_id,created_at
             ) VALUES (?1,?2,?3,?4,unixepoch())",
            params![
                desired.source.source_id,
                before.source_id,
                desired.source.identity_digest.as_str(),
                operation_id.as_str(),
            ],
        )
        .map(|_| ())
        .map_err(|_| port(PortErrorCode::Conflict, "compute.projection.alias_insert"))
}

pub(in crate::control) fn is_projection_state(encoded: &str) -> bool {
    decode_state(encoded).is_ok()
}

pub(in crate::control) fn current_matches(
    connection: &rusqlite::Connection,
    source_id: &str,
    after_json: &str,
) -> PortResult<bool> {
    let after = decode_state(after_json)?;
    if after.source_id != source_id {
        return Err(port(PortErrorCode::Corrupt, "compute.projection.identity"));
    }
    Ok(ProjectionEffectStateV1::current(
        connection,
        &after.source_id,
        &after.binding_id,
        &after.endpoint_profile_id,
    )? == after)
}

pub(in crate::control) fn activate(
    transaction: &rusqlite::Transaction<'_>,
    source_id: &str,
    expected_source_revision: u64,
    before_json: Option<&str>,
    after_json: &str,
) -> PortResult<()> {
    let before = before_json
        .map(decode_state)
        .transpose()?
        .ok_or_else(|| port(PortErrorCode::Corrupt, "compute.projection.before"))?;
    let after = decode_state(after_json)?;
    if after.source_id != source_id
        || before.source.as_ref().map_or(0, |value| value.revision) != expected_source_revision
        || ProjectionEffectStateV1::current(
            transaction,
            &before.source_id,
            &before.binding_id,
            &before.endpoint_profile_id,
        )? != before
    {
        return Err(port(
            PortErrorCode::Conflict,
            "compute.projection.activate_cas",
        ));
    }
    write_state(transaction, &after)
}

pub(in crate::control) fn compensate(
    transaction: &rusqlite::Transaction<'_>,
    source_id: &str,
    before_json: Option<&str>,
    after_json: &str,
) -> PortResult<bool> {
    let before = before_json
        .map(decode_state)
        .transpose()?
        .ok_or_else(|| port(PortErrorCode::Corrupt, "compute.projection.before"))?;
    let after = decode_state(after_json)?;
    if after.source_id != source_id {
        return Err(port(PortErrorCode::Corrupt, "compute.projection.identity"));
    }
    if ProjectionEffectStateV1::current(
        transaction,
        &after.source_id,
        &after.binding_id,
        &after.endpoint_profile_id,
    )? != after
    {
        return Ok(false);
    }
    write_state(transaction, &before)?;
    Ok(true)
}

fn write_state(
    transaction: &rusqlite::Transaction<'_>,
    state: &ProjectionEffectStateV1,
) -> PortResult<()> {
    if state.source.is_none() {
        if state.binding.is_some()
            || !state.other_bindings.is_empty()
            || !state.credential_pools.is_empty()
            || state.inventory.is_some()
        {
            return Err(port(
                PortErrorCode::Corrupt,
                "compute.projection.orphan_before_state",
            ));
        }
        transaction
            .execute(
                "DELETE FROM source_inventory_snapshots
                 WHERE source_id=?1 AND endpoint_profile_id=?2",
                params![state.source_id, state.endpoint_profile_id],
            )
            .map_err(|_| {
                port(
                    PortErrorCode::Unavailable,
                    "compute.projection.inventory_delete",
                )
            })?;
        transaction
            .execute(
                "DELETE FROM credential_pools WHERE source_id=?1",
                params![state.source_id],
            )
            .map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.pool_delete"))?;
        transaction
            .execute(
                "DELETE FROM source_bindings WHERE source_id=?1",
                params![state.source_id],
            )
            .map_err(|_| {
                port(
                    PortErrorCode::Unavailable,
                    "compute.projection.binding_delete",
                )
            })?;
        delete(
            transaction,
            "compute_sources",
            "source_id",
            &state.source_id,
        )?;
        return Ok(());
    }
    match &state.source {
        Some(source) => super::write_compute_source_in(transaction, source)?,
        None => unreachable!("missing source handled above"),
    }
    transaction
        .execute(
            "UPDATE source_bindings SET active=0 WHERE source_id=?1",
            params![state.source_id],
        )
        .map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "compute.projection.binding_disable",
            )
        })?;
    transaction
        .execute(
            "UPDATE credential_pools SET active=0 WHERE source_id=?1",
            params![state.source_id],
        )
        .map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "compute.projection.pool_disable",
            )
        })?;
    if let Some(binding) = &state.binding {
        write_binding(transaction, binding)?;
    }
    for binding in &state.other_bindings {
        write_binding(transaction, binding)?;
    }
    for pool in &state.credential_pools {
        super::write_credential_pool_in(transaction, pool)?;
    }
    match &state.inventory {
        Some(inventory) => {
            transaction
                .execute(
                    "INSERT INTO source_inventory_snapshots(
                       source_id,endpoint_profile_id,inventory_revision,inventory_digest,
                       observed_models_json,captured_at)
                     VALUES (?1,?2,?3,?4,?5,?6)
                     ON CONFLICT(source_id,endpoint_profile_id) DO UPDATE SET
                       inventory_revision=excluded.inventory_revision,
                       inventory_digest=excluded.inventory_digest,
                       observed_models_json=excluded.observed_models_json,
                       captured_at=excluded.captured_at",
                    params![
                        inventory.source_id,
                        inventory.endpoint_profile_id,
                        inventory.inventory_revision,
                        inventory.inventory_digest.as_str(),
                        encode(&inventory.observed_models)?,
                        inventory.captured_at,
                    ],
                )
                .map_err(|_| {
                    port(
                        PortErrorCode::Unavailable,
                        "compute.projection.inventory_write",
                    )
                })?;
        }
        None => {
            transaction
                .execute(
                    "DELETE FROM source_inventory_snapshots
                     WHERE source_id=?1 AND endpoint_profile_id=?2",
                    params![state.source_id, state.endpoint_profile_id],
                )
                .map_err(|_| {
                    port(
                        PortErrorCode::Unavailable,
                        "compute.projection.inventory_delete",
                    )
                })?;
        }
    }
    Ok(())
}

fn write_binding(
    transaction: &rusqlite::Transaction<'_>,
    binding: &SourceBindingV1,
) -> PortResult<()> {
    transaction
        .execute(
            "INSERT INTO source_bindings(
                binding_id,revision,source_id,binding_json,active,updated_at
             ) VALUES (?1,?2,?3,?4,1,unixepoch())
             ON CONFLICT(binding_id) DO UPDATE SET revision=excluded.revision,
               source_id=excluded.source_id,binding_json=excluded.binding_json,
               active=1,updated_at=excluded.updated_at",
            params![
                binding.binding_id,
                binding.revision,
                binding.source_id,
                encode(binding)?
            ],
        )
        .map(|_| ())
        .map_err(|_| {
            port(
                PortErrorCode::Unavailable,
                "compute.projection.binding_write",
            )
        })
}

fn decode_state(encoded: &str) -> PortResult<ProjectionEffectStateV1> {
    let mut state: ProjectionEffectStateV1 = decode(encoded)?;
    normalize_v7_state(&mut state)?;
    state.validate()?;
    Ok(state)
}

fn matches_source_expectation(
    current_revision: Option<u64>,
    current: Option<&ComputeSourceV1>,
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
        .map(|source| {
            let mut legacy = source.clone();
            legacy.identity_digest = CanonicalDigest::of(&legacy.identity)
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_source"))?;
            CanonicalDigest::of(&legacy)
                .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.legacy_source"))
        })
        .transpose()?;
    Ok(current_revision.unwrap_or(0) == expected_revision
        && legacy_digest.as_ref() == expected_digest)
}

fn matches_revision_digest<T: Serialize>(
    current_revision: Option<u64>,
    current: Option<&T>,
    expected_revision: u64,
    expected_digest: Option<&CanonicalDigest>,
) -> PortResult<bool> {
    Ok(current_revision.unwrap_or(0) == expected_revision
        && current
            .map(CanonicalDigest::of)
            .transpose()
            .map_err(|_| port(PortErrorCode::Corrupt, "compute.projection.current_digest"))?
            .as_ref()
            == expected_digest)
}

fn json_row<T: serde::de::DeserializeOwned>(
    connection: &rusqlite::Connection,
    query: &str,
    id: &str,
) -> PortResult<Option<T>> {
    connection
        .query_row(query, params![id], |row| row.get::<_, String>(0))
        .optional()
        .map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.read"))?
        .map(|encoded| decode(&encoded))
        .transpose()
}

fn binding_rows(
    connection: &rusqlite::Connection,
    source_id: &str,
    excluded_binding_id: &str,
) -> PortResult<Vec<SourceBindingV1>> {
    let mut statement = connection
        .prepare(
            "SELECT binding_json FROM source_bindings
             WHERE source_id=?1 AND binding_id<>?2 AND active=1
             ORDER BY binding_id",
        )
        .map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.bindings"))?;
    let rows = statement
        .query_map(params![source_id, excluded_binding_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.bindings"))?;
    rows.map(|row| {
        let encoded =
            row.map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.binding_row"))?;
        decode(&encoded)
    })
    .collect()
}

fn pool_rows(
    connection: &rusqlite::Connection,
    source_id: &str,
) -> PortResult<Vec<CredentialPoolV1>> {
    let mut statement = connection
        .prepare(
            "SELECT pool_json FROM credential_pools
             WHERE source_id=?1 AND active=1 ORDER BY pool_id",
        )
        .map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.pools"))?;
    let rows = statement
        .query_map(params![source_id], |row| row.get::<_, String>(0))
        .map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.pools"))?;
    rows.map(|row| {
        let encoded =
            row.map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.pool_row"))?;
        decode(&encoded)
    })
    .collect()
}

fn delete(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
    id: &str,
) -> PortResult<()> {
    transaction
        .execute(
            &format!("DELETE FROM {table} WHERE {column}=?1"),
            params![id],
        )
        .map(|_| ())
        .map_err(|_| port(PortErrorCode::Unavailable, "compute.projection.delete"))
}

fn port(code: PortErrorCode, context: &'static str) -> PortError {
    PortError::new(code, context)
}

#[cfg(test)]
#[path = "projection/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "projection/upgrade_tests.rs"]
mod upgrade_tests;
