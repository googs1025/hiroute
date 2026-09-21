use super::*;

impl ComputeSourceControlPort for ControlStore {
    fn apply_compute_source(
        &self,
        operation_id: &OperationId,
        workspace: &WorkspaceId,
        expected_target_revision: u64,
        mutation: &ComputeSourceMutationV1,
    ) -> PortResult<OwnedEffectV1> {
        let mut connection = self.connection.borrow_mut();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| port("compute.source.effect.begin"))?;
        let existing: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM control_effects WHERE operation_id=?1)",
                params![operation_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|_| port("compute.source.effect.lookup"))?;
        if existing {
            drop(transaction);
            drop(connection);
            return match self.observe_control(operation_id, workspace)? {
                EffectReconciliation::Staged(effect) | EffectReconciliation::Applied(effect) => {
                    Ok(effect)
                }
                EffectReconciliation::Missing => Err(conflict("compute.source.effect.compensated")),
                EffectReconciliation::OwnershipLost(_) => {
                    Err(conflict("compute.source.effect.ownership"))
                }
            };
        }

        let current: Option<ComputeSourceV1> = transaction
            .query_row(
                "SELECT source_json FROM compute_sources WHERE source_id=?1",
                params![mutation.source_id()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| port("compute.source.effect.current"))?
            .map(|encoded| decode(&encoded))
            .transpose()?;
        mutation
            .validate_against(current.as_ref())
            .map_err(|_| conflict("compute.source.effect.cas"))?;
        let current_target = super::super::read_control_head(&transaction, workspace)?;
        if current_target != expected_target_revision {
            return Err(conflict("compute.source.effect.target_revision"));
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
            .map_err(|_| port("compute.source.effect.before"))?;
        let control = json!({"compute_source_mutation": mutation});
        let staged_json = serde_json::to_string(&json!({
            "schema": "hiroute.control-desired/v1",
            "value": control,
        }))
        .map_err(|_| invalid("compute.source.effect.encode"))?;
        let after_digest =
            CanonicalDigest::of(&control).map_err(|_| invalid("compute.source.effect.digest"))?;
        let after_revision = expected_target_revision
            .checked_add(1)
            .ok_or_else(|| conflict("compute.source.effect.target_overflow"))?;
        let (before_source_json, after_source_json, effect_source_id) = match (
            mutation.expected_projection(),
            mutation.desired_projection(),
        ) {
            (Some(expected), Some(projection)) => {
                super::projection::stage_states(&transaction, operation_id, projection, expected)?
            }
            (None, None) => (
                current.as_ref().map(encode).transpose()?,
                encode(mutation.desired())?,
                mutation.source_id().to_owned(),
            ),
            _ => return Err(invalid("compute.source.effect.projection")),
        };
        transaction
            .execute(
                "INSERT INTO control_effects(
                    operation_id, workspace_id, before_exists, before_json, before_revision,
                    before_digest, before_owner_operation_id, after_revision, after_digest,
                    compensated, staged_json, activated, compute_source_id,
                    compute_source_expected_revision, compute_source_before_json,
                    compute_source_after_json
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
                    effect_source_id,
                    mutation.expected_revision(),
                    before_source_json,
                    after_source_json,
                ],
            )
            .map_err(|_| port("compute.source.effect.journal"))?;
        transaction
            .commit()
            .map_err(|_| port("compute.source.effect.commit"))?;
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

pub(in crate::control) fn write_compute_source_in(
    transaction: &rusqlite::Transaction<'_>,
    source: &ComputeSourceV1,
) -> PortResult<()> {
    source
        .validate_shape()
        .map_err(|_| invalid("compute.source.effect.desired"))?;
    let encoded = encode(source)?;
    transaction
        .execute(
            "INSERT INTO compute_sources(source_id,revision,identity_digest,source_json,updated_at)
             VALUES (?1,?2,?3,?4,unixepoch())
             ON CONFLICT(source_id) DO UPDATE SET revision=excluded.revision,
                identity_digest=excluded.identity_digest, source_json=excluded.source_json,
                updated_at=excluded.updated_at",
            params![
                source.source_id,
                source.revision,
                source.identity_digest.as_str(),
                encoded,
            ],
        )
        .map_err(|_| port("compute.source.effect.write"))?;
    Ok(())
}
