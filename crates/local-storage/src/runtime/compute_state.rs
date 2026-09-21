use hiroute_domain::{
    COMPUTE_RUNTIME_STATE_SCHEMA_V1, ComputeRuntimeStateStoreV1, PortError, PortErrorCode,
    PortResult, RuntimeProbeAcquireOutcomeV1, RuntimeProbeLeaseRequestV1, RuntimeProbeLeaseV1,
    RuntimeStateIdentityV1, RuntimeStateV1,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::RuntimeStore;

const STATE_TABLE: &str = "compute_runtime_state_v1";

impl ComputeRuntimeStateStoreV1 for RuntimeStore {
    fn runtime_state(
        &self,
        identity: &RuntimeStateIdentityV1,
    ) -> PortResult<Option<RuntimeStateV1>> {
        validate_identity(identity)?;
        read_state(&self.connection.borrow(), identity)
    }

    fn compare_and_set_runtime_state(
        &self,
        expected_generation: u64,
        state: &RuntimeStateV1,
    ) -> PortResult<()> {
        validate_state_input(state)?;
        if expected_generation.checked_add(1) != Some(state.generation()) {
            return Err(conflict("runtime.compute.cas.next_generation"));
        }
        let mut connection = self.connection.borrow_mut();
        let transaction = begin(&mut connection, "runtime.compute.cas.begin")?;
        let current = read_state(&transaction, state.identity())?;
        match &current {
            Some(current) => {
                if current.generation() != expected_generation {
                    return Err(conflict("runtime.compute.cas.generation"));
                }
                current
                    .validate_direct_successor(state)
                    .map_err(|_| conflict("runtime.compute.cas.transition"))?;
            }
            None => {
                if expected_generation != 0 {
                    return Err(conflict("runtime.compute.cas.missing"));
                }
                let sampled_at = hiroute_domain::RuntimeClockSampleV1::from_unix_millis(
                    state.updated_at_unix_millis(),
                )
                .map_err(|_| invalid("runtime.compute.cas.clock"))?;
                let initial = RuntimeStateV1::ready(state.identity().clone(), 0, sampled_at)
                    .map_err(|_| invalid("runtime.compute.cas.initial"))?;
                initial
                    .validate_direct_successor(state)
                    .map_err(|_| conflict("runtime.compute.cas.transition"))?;
            }
        }
        write_state(
            &transaction,
            current.is_some(),
            expected_generation,
            state,
            "runtime.compute.cas.write",
        )?;
        transaction
            .commit()
            .map_err(|_| unavailable("runtime.compute.cas.commit"))
    }

    fn acquire_runtime_probe(
        &self,
        identity: &RuntimeStateIdentityV1,
        expected_generation: u64,
        request: &RuntimeProbeLeaseRequestV1,
    ) -> PortResult<RuntimeProbeAcquireOutcomeV1> {
        validate_identity(identity)?;
        request
            .validate()
            .map_err(|_| invalid("runtime.compute.lease.request"))?;
        let mut connection = self.connection.borrow_mut();
        let transaction = begin(&mut connection, "runtime.compute.lease.begin")?;
        let Some(current) = read_state(&transaction, identity)? else {
            return Ok(RuntimeProbeAcquireOutcomeV1::Conflict);
        };
        if current.generation() != expected_generation {
            return Ok(RuntimeProbeAcquireOutcomeV1::Conflict);
        }
        if request.sampled_at().unix_millis() < current.updated_at_unix_millis() {
            return Ok(RuntimeProbeAcquireOutcomeV1::Conflict);
        }
        let Some(cooldown_until) = current.cooldown_until_unix_millis() else {
            return Ok(RuntimeProbeAcquireOutcomeV1::Busy);
        };
        if !request.sampled_at().has_reached(cooldown_until)
            || current
                .probe_lease()
                .is_some_and(|lease| !lease.is_expired_at(request.sampled_at()))
        {
            return Ok(RuntimeProbeAcquireOutcomeV1::Busy);
        }
        let next = match current.acquire_probe(expected_generation, request) {
            Ok(next) => next,
            Err(_) => return Ok(RuntimeProbeAcquireOutcomeV1::Conflict),
        };
        write_state(
            &transaction,
            true,
            expected_generation,
            &next,
            "runtime.compute.lease.write",
        )?;
        transaction
            .commit()
            .map_err(|_| unavailable("runtime.compute.lease.commit"))?;
        Ok(RuntimeProbeAcquireOutcomeV1::Acquired(next))
    }

    fn complete_runtime_probe(
        &self,
        lease: &RuntimeProbeLeaseV1,
        state: &RuntimeStateV1,
    ) -> PortResult<()> {
        lease
            .validate()
            .map_err(|_| invalid("runtime.compute.complete.lease"))?;
        validate_state_input(state)?;
        if lease.fence_generation().checked_add(1) != Some(state.generation()) {
            return Err(conflict("runtime.compute.complete.next_generation"));
        }
        let mut connection = self.connection.borrow_mut();
        let transaction = begin(&mut connection, "runtime.compute.complete.begin")?;
        let current = read_state(&transaction, state.identity())?
            .ok_or_else(|| conflict("runtime.compute.complete.missing"))?;
        current
            .validate_probe_successor(lease, state)
            .map_err(|_| conflict("runtime.compute.complete.fence"))?;
        write_state(
            &transaction,
            true,
            lease.fence_generation(),
            state,
            "runtime.compute.complete.write",
        )?;
        transaction
            .commit()
            .map_err(|_| unavailable("runtime.compute.complete.commit"))
    }
}

fn validate_identity(identity: &RuntimeStateIdentityV1) -> PortResult<()> {
    identity
        .validate()
        .map_err(|_| invalid("runtime.compute.identity"))?;
    identity
        .canonical_key()
        .map_err(|_| invalid("runtime.compute.identity_key"))?;
    Ok(())
}

fn validate_state_input(state: &RuntimeStateV1) -> PortResult<()> {
    state
        .validate()
        .map_err(|_| invalid("runtime.compute.state"))?;
    validate_identity(state.identity())
}

fn begin<'connection>(
    connection: &'connection mut Connection,
    context: &'static str,
) -> PortResult<Transaction<'connection>> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| unavailable(context))
}

fn read_state(
    connection: &Connection,
    identity: &RuntimeStateIdentityV1,
) -> PortResult<Option<RuntimeStateV1>> {
    let identity_key = identity
        .canonical_key()
        .map_err(|_| invalid("runtime.compute.read.identity_key"))?;
    let row: Option<(String, String, String, i64, i64)> = connection
        .query_row(
            &format!(
                "SELECT contract_schema,identity_json,state_json,generation,updated_at_unix_millis
                 FROM {STATE_TABLE} WHERE identity_key=?1"
            ),
            params![identity_key],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(|_| unavailable("runtime.compute.read"))?;
    let Some((contract_schema, identity_json, state_json, generation, updated_at)) = row else {
        return Ok(None);
    };
    let stored_identity: RuntimeStateIdentityV1 = serde_json::from_str(&identity_json)
        .map_err(|_| corrupt("runtime.compute.read.identity_decode"))?;
    let state: RuntimeStateV1 = serde_json::from_str(&state_json)
        .map_err(|_| corrupt("runtime.compute.read.state_decode"))?;
    stored_identity
        .validate()
        .map_err(|_| corrupt("runtime.compute.read.identity"))?;
    state
        .validate()
        .map_err(|_| corrupt("runtime.compute.read.state"))?;
    let stored_key = stored_identity
        .canonical_key()
        .map_err(|_| corrupt("runtime.compute.read.identity_key"))?;
    if contract_schema != COMPUTE_RUNTIME_STATE_SCHEMA_V1
        || stored_identity != *identity
        || state.identity() != identity
        || stored_key != identity_key
        || i64::try_from(state.generation()).ok() != Some(generation)
        || state.updated_at_unix_millis() != updated_at
    {
        return Err(corrupt("runtime.compute.read.binding"));
    }
    Ok(Some(state))
}

fn write_state(
    transaction: &Transaction<'_>,
    exists: bool,
    expected_generation: u64,
    state: &RuntimeStateV1,
    context: &'static str,
) -> PortResult<()> {
    let identity_key = state
        .identity()
        .canonical_key()
        .map_err(|_| invalid("runtime.compute.write.identity_key"))?;
    let identity_json = serde_json::to_string(state.identity())
        .map_err(|_| invalid("runtime.compute.write.identity_encode"))?;
    let state_json =
        serde_json::to_string(state).map_err(|_| invalid("runtime.compute.write.state_encode"))?;
    let changed = if exists {
        transaction.execute(
            &format!(
                "UPDATE {STATE_TABLE}
                 SET contract_schema=?2,identity_json=?3,state_json=?4,generation=?5,
                     updated_at_unix_millis=?6
                 WHERE identity_key=?1 AND generation=?7"
            ),
            params![
                identity_key,
                COMPUTE_RUNTIME_STATE_SCHEMA_V1,
                identity_json,
                state_json,
                state.generation(),
                state.updated_at_unix_millis(),
                expected_generation,
            ],
        )
    } else {
        transaction.execute(
            &format!(
                "INSERT INTO {STATE_TABLE}(
                    identity_key,contract_schema,identity_json,state_json,generation,
                    updated_at_unix_millis
                 ) VALUES (?1,?2,?3,?4,?5,?6)"
            ),
            params![
                identity_key,
                COMPUTE_RUNTIME_STATE_SCHEMA_V1,
                identity_json,
                state_json,
                state.generation(),
                state.updated_at_unix_millis(),
            ],
        )
    }
    .map_err(|_| unavailable(context))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(conflict("runtime.compute.write.cas"))
    }
}

fn conflict(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::Conflict, context)
}

fn corrupt(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::Corrupt, context)
}

fn invalid(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::InvalidData, context)
}

fn unavailable(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::Unavailable, context)
}
