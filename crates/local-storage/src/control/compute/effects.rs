use super::*;

impl CredentialPoolControlPort for ControlStore {
    fn apply_credential_pool(
        &self,
        operation_id: &OperationId,
        workspace: &WorkspaceId,
        expected_target_revision: u64,
        mutation: &CredentialPoolMutationV1,
    ) -> PortResult<OwnedEffectV1> {
        mutation
            .validate_shape()
            .map_err(|_| invalid("compute.pool.effect.shape"))?;
        let mut connection = self.connection.borrow_mut();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| port("compute.pool.effect.begin"))?;
        let existing: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM control_effects WHERE operation_id=?1)",
                params![operation_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|_| port("compute.pool.effect.lookup"))?;
        if existing {
            drop(transaction);
            drop(connection);
            return match self.observe_control(operation_id, workspace)? {
                EffectReconciliation::Staged(effect) | EffectReconciliation::Applied(effect) => {
                    Ok(effect)
                }
                EffectReconciliation::Missing => Err(conflict("compute.pool.effect.compensated")),
                EffectReconciliation::OwnershipLost(_) => {
                    Err(conflict("compute.pool.effect.ownership"))
                }
            };
        }

        let current: Option<CredentialPoolV1> = transaction
            .query_row(
                "SELECT pool_json FROM credential_pools WHERE pool_id=?1 AND active=1",
                params![mutation.desired().pool_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| port("compute.pool.effect.current"))?
            .map(|encoded| decode(&encoded))
            .transpose()?;
        mutation
            .validate_against(current.as_ref())
            .map_err(|_| conflict("compute.pool.effect.cas"))?;
        validate_credential_pool_references_in(&transaction, mutation.desired())?;
        let current_target = super::super::read_control_head(&transaction, workspace)?;
        if current_target != expected_target_revision {
            return Err(conflict("compute.pool.effect.target_revision"));
        }
        let before = transaction
            .query_row(
                "SELECT desired_json, desired_digest, owner_operation_id
                 FROM workspace_state WHERE workspace_id=?1",
                params![workspace.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| port("compute.pool.effect.before"))?;
        let control = json!({"credential_pool_mutation": mutation});
        let staged_json = serde_json::to_string(&json!({
            "schema": "hiroute.control-desired/v1",
            "value": control,
        }))
        .map_err(|_| invalid("compute.pool.effect.encode"))?;
        let after_digest =
            CanonicalDigest::of(&control).map_err(|_| invalid("compute.pool.effect.digest"))?;
        let after_revision = expected_target_revision
            .checked_add(1)
            .ok_or_else(|| conflict("compute.pool.effect.target_overflow"))?;
        let before_pool_json = current.as_ref().map(encode).transpose()?;
        let after_pool_json = encode(mutation.desired())?;
        transaction
            .execute(
                "INSERT INTO control_effects(
                    operation_id, workspace_id, before_exists, before_json, before_revision,
                    before_digest, before_owner_operation_id, after_revision, after_digest,
                    compensated, staged_json, activated, compute_pool_id,
                    compute_pool_expected_revision, compute_pool_before_json,
                    compute_pool_after_json
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10,0,?11,?12,?13,?14)",
                params![
                    operation_id.as_str(),
                    workspace.as_str(),
                    i64::from(before.is_some()),
                    before.as_ref().map(|value| value.0.as_str()),
                    current_target,
                    before.as_ref().map(|value| value.1.as_str()),
                    before.as_ref().and_then(|value| value.2.as_deref()),
                    after_revision,
                    after_digest.as_str(),
                    staged_json,
                    mutation.desired().pool_id,
                    mutation.expected_revision(),
                    before_pool_json,
                    after_pool_json,
                ],
            )
            .map_err(|_| port("compute.pool.effect.journal"))?;
        transaction
            .commit()
            .map_err(|_| port("compute.pool.effect.commit"))?;
        Ok(super::super::control_effect(
            operation_id,
            workspace,
            before
                .as_ref()
                .and_then(|value| super::super::parse_digest(&value.1)),
            after_digest,
            after_revision,
        ))
    }
}

pub(in crate::control) fn write_credential_pool_in(
    transaction: &rusqlite::Transaction<'_>,
    pool: &CredentialPoolV1,
) -> PortResult<()> {
    pool.validate()
        .map_err(|_| invalid("compute.pool.effect.desired"))?;
    validate_credential_pool_references_in(transaction, pool)?;
    super::pools::write_pool(transaction, pool, "compute.pool.effect.write")
}

fn validate_credential_pool_references_in(
    transaction: &rusqlite::Transaction<'_>,
    pool: &CredentialPoolV1,
) -> PortResult<()> {
    let source: ComputeSourceV1 = transaction
        .query_row(
            "SELECT source_json FROM compute_sources WHERE source_id=?1",
            params![pool.source_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| port("compute.pool.effect.source"))?
        .map(|encoded| decode(&encoded))
        .transpose()?
        .ok_or_else(|| invalid("compute.pool.effect.source_missing"))?;
    let binding: SourceBindingV1 = transaction
        .query_row(
            "SELECT binding_json FROM source_bindings
             WHERE binding_id=?1 AND active=1",
            params![pool.binding_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| port("compute.pool.effect.binding"))?
        .map(|encoded| decode(&encoded))
        .transpose()?
        .ok_or_else(|| invalid("compute.pool.effect.binding_missing"))?;
    source
        .validate_shape()
        .map_err(|_| invalid("compute.pool.effect.source_invalid"))?;
    binding
        .validate_shape()
        .map_err(|_| invalid("compute.pool.effect.binding_invalid"))?;
    if source.state == MaterializationState::Disabled
        || source.source_id != pool.source_id
        || source.revision != pool.source_revision
        || source.identity_digest != pool.source_identity_digest
        || source.connection_option_id != pool.connection_option_id
        || binding.source_id != source.source_id
        || binding.source_revision != source.revision
        || binding.source_identity_digest != source.identity_digest
        || pool.validate_against_binding(&binding).is_err()
    {
        return Err(conflict("compute.pool.effect.reference"));
    }
    Ok(())
}
