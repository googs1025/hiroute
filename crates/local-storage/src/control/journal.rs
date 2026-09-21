//! Update mutable journal fields while retaining the current immutable Operation JSON format.
use super::*;

pub(super) fn save(transaction: &Transaction<'_>, operation: &OperationV1) -> PortResult<u64> {
    let update = operation
        .journal_update()
        .map_err(|_| port(PortErrorCode::InvalidData, "control.journal.inputs"))?;
    let mut expression = String::from(
        "json_set(operation_json, '$.state', ?2, '$.generation', ?3, '$.safe_error_code', ?4",
    );
    let mut values: Vec<rusqlite::types::Value> = vec![
        operation.operation_id.as_str().to_owned().into(),
        operation.state.as_str().to_owned().into(),
        i64::try_from(update.next_generation)
            .map_err(|_| {
                port(
                    PortErrorCode::InvalidData,
                    "control.journal.generation_range",
                )
            })?
            .into(),
        operation
            .safe_error_code
            .clone()
            .map(Into::into)
            .unwrap_or(rusqlite::types::Value::Null),
        i64::try_from(update.expected_generation)
            .map_err(|_| {
                port(
                    PortErrorCode::InvalidData,
                    "control.journal.generation_range",
                )
            })?
            .into(),
        update.plan_json.to_owned().into(),
        operation.request_digest.as_str().to_owned().into(),
        operation.accepted_digest.as_str().to_owned().into(),
        operation.workspace_id.as_str().to_owned().into(),
        operation.idempotency.principal.clone().into(),
        operation.idempotency.operation_kind.clone().into(),
        operation.idempotency.key.clone().into(),
    ];
    for step in &update.changed_steps {
        let encoded = serde_json::to_string(step)
            .map_err(|_| port(PortErrorCode::InvalidData, "control.step.encode"))?;
        values.push(encoded.into());
        expression.push_str(&format!(
            ", '$.steps[{}]', json(?{})",
            step.sequence,
            values.len()
        ));
        store_step(transaction, operation, step)?;
    }
    expression.push(')');
    // Same writer generation and exact immutable plan are checked in SQLite, without rebuilding
    // a Rust JSON tree or re-running typed planners. Independent recovery still fully decodes.
    let sql = format!(
        "UPDATE operations SET state=?2, generation=?3, operation_json={expression}, updated_at=unixepoch()
         WHERE operation_id=?1 AND generation=?5 AND json_extract(operation_json, '$.plan')=?6
         AND request_digest=?7 AND accepted_change_digest=?8 AND workspace_id=?9
         AND principal=?10 AND operation_kind=?11 AND idempotency_key=?12"
    );
    let updated = transaction
        .execute(&sql, rusqlite::params_from_iter(values))
        .map_err(|_| port(PortErrorCode::Unavailable, "control.journal.update"))?;
    if updated != 1 {
        return Err(port(PortErrorCode::Conflict, "control.journal.generation"));
    }
    Ok(update.next_generation)
}

pub(super) fn is_current(connection: &Connection, operation: &OperationV1) -> PortResult<bool> {
    if !operation
        .journal_is_committed()
        .map_err(|_| port(PortErrorCode::InvalidData, "control.current.checkpoint"))?
    {
        return Ok(false);
    }
    let checkpoint = operation
        .journal_update()
        .map_err(|_| port(PortErrorCode::InvalidData, "control.current.inputs"))?;
    let steps = serde_json::to_string(&operation.steps)
        .map_err(|_| port(PortErrorCode::InvalidData, "control.current.steps"))?;
    let revisions = serde_json::to_string(&operation.expected_revisions)
        .map_err(|_| port(PortErrorCode::InvalidData, "control.current.revisions"))?;
    // Step rows are written through json! (Value), whose object ordering depends on the
    // serde_json feature set. Use the same representation, not struct field order.
    let step_rows = serde_json::to_value(&operation.steps)
        .map_err(|_| port(PortErrorCode::InvalidData, "control.current.step-rows"))?
        .to_string();
    // A sealed settings service releases its writer claim while the native-file tail waits for
    // explicit retry. The coordinator verifies the receipt before doing any tail work and must
    // still be able to observe this exact committed journal during startup recovery.
    let parked_settings_tail = operation.state == OperationState::Activating
        && operation
            .step(hiroute_domain::OperationStepKind::Activate)
            .terminal_result
            .as_deref()
            .and_then(hiroute_domain::SettingsServiceCompletionV1::parse)
            .is_some();
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE operation_id=?1 AND generation=?2
         AND json_extract(operation_json, '$.plan')=?3 AND request_digest=?4
         AND accepted_change_digest=?5 AND workspace_id=?6 AND principal=?7
         AND operation_kind=?8 AND idempotency_key=?9 AND state=?10
         AND json_extract(operation_json, '$.operation_id')=?1
         AND json_extract(operation_json, '$.workspace_id')=?6
         AND json_extract(operation_json, '$.request_digest')=?4
         AND json_extract(operation_json, '$.accepted_digest')=?5
         AND json_extract(operation_json, '$.idempotency.principal')=?7
         AND json_extract(operation_json, '$.idempotency.operation_kind')=?8
         AND json_extract(operation_json, '$.idempotency.key')=?9
         AND json_extract(operation_json, '$.steps')=json(?11)
         AND json_extract(operation_json, '$.safe_error_code') IS ?12
         AND json_extract(operation_json, '$.expected_revisions')=json(?13)
         AND json_extract(operation_json, '$.schema_version')=?14
         AND json_extract(operation_json, '$.generation')=?2
         AND json_extract(operation_json, '$.state')=?10
         AND (state IN ('succeeded', 'rolled_back', 'needs_attention')
              OR EXISTS(SELECT 1 FROM writer_claim WHERE operation_id=?1)
              OR (?17 AND NOT EXISTS(SELECT 1 FROM writer_claim)))
         AND (SELECT count(*) FROM operation_steps WHERE operation_id=?1)=?15
         AND NOT EXISTS(SELECT 1 FROM operation_steps s WHERE s.operation_id=?1
             AND (s.step_no IS NOT json_extract(s.step_json, '$.step.sequence')
                  OR s.step_kind IS NOT json_extract(s.step_json, '$.step.kind')
                  OR s.state IS NOT json_extract(s.step_json, '$.step.status')
                  OR json_extract(s.step_json, '$.step') IS NOT
                     json_extract(?16, '$[' || s.step_no || ']'))))",
            params![
                operation.operation_id.as_str(),
                operation.generation,
                checkpoint.plan_json,
                operation.request_digest.as_str(),
                operation.accepted_digest.as_str(),
                operation.workspace_id.as_str(),
                operation.idempotency.principal,
                operation.idempotency.operation_kind,
                operation.idempotency.key,
                operation.state.as_str(),
                steps,
                operation.safe_error_code,
                revisions,
                operation.schema_version,
                operation.steps.len() as u64,
                step_rows,
                parked_settings_tail,
            ],
            |row| row.get(0),
        )
        .map_err(|_| port(PortErrorCode::Unavailable, "control.current.read"))
}
