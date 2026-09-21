use std::collections::BTreeSet;

use hiroute_domain::{
    AgentPlanId, DailyValueAggregateV1, FactsCompleteness, ObservationQueryError, ValueQueryV1,
    WorkspaceId,
};

pub(super) fn read_daily_aggregates(
    connection: &rusqlite::Connection,
    workspace_id: &WorkspaceId,
    query: &ValueQueryV1,
) -> Result<Vec<DailyValueAggregateV1>, ObservationQueryError> {
    let mut statement = connection
        .prepare(
            "SELECT day_number, billing_unit, input_tokens, output_tokens, cache_read_tokens,
                    cache_write_tokens, reasoning_tokens,
                    baseline_api_equivalent_cost_micros,
                    chosen_api_equivalent_cost_micros, actual_incremental_cost_micros,
                    routing_savings_micros, entitlement_savings_micros,
                    estimated_total_savings_micros, CASE WHEN length(price_version_refs_json)<=4096 THEN price_version_refs_json ELSE '' END,
                    CASE WHEN length(price_override_revision_refs_json)<=4096 THEN price_override_revision_refs_json ELSE '' END, facts_completeness
             FROM daily_value_rollups
             WHERE workspace_id=?1 AND agent_plan_id=?2 AND currency=?3 AND day_number>=?4 AND day_number<=?5
             ORDER BY day_number, billing_unit LIMIT 201",
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let rows = statement
        .query_map(
            rusqlite::params![
                workspace_id.as_str(),
                query.agent_plan_id.as_str(),
                query.currency,
                query.from_ms.div_euclid(86400000),
                (query.to_ms - 1).div_euclid(86400000),
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    [
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ],
                    [
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ],
                    [
                        row.get::<_, Option<i64>>(10)?,
                        row.get::<_, Option<i64>>(11)?,
                        row.get::<_, Option<i64>>(12)?,
                    ],
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                ))
            },
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let mut aggregates = Vec::new();
    for (index, row) in rows.enumerate() {
        if index >= 200 {
            return Err(ObservationQueryError::InvalidQuery);
        }
        let (day_number, billing_unit, tokens, costs, savings, price_refs, override_refs, facts) =
            row.map_err(|_| ObservationQueryError::Corrupt)?;
        let day_start = day_number
            .checked_mul(86_400_000)
            .ok_or(ObservationQueryError::Corrupt)?;
        let day_end = day_start
            .checked_add(86_400_000)
            .ok_or(ObservationQueryError::Corrupt)?;
        if day_end <= query.from_ms || day_start >= query.to_ms {
            continue;
        }
        aggregates.push(DailyValueAggregateV1 {
            agent_plan_id: AgentPlanId::parse(query.agent_plan_id.as_str())
                .map_err(|_| ObservationQueryError::Corrupt)?,
            day_number,
            currency: query.currency.clone(),
            billing_unit,
            input_tokens: nonnegative(tokens[0])?,
            output_tokens: nonnegative(tokens[1])?,
            cache_read_tokens: nonnegative(tokens[2])?,
            cache_write_tokens: nonnegative(tokens[3])?,
            reasoning_tokens: nonnegative(tokens[4])?,
            baseline_api_equivalent_cost_micros: optional_nonnegative(costs[0])?,
            chosen_api_equivalent_cost_micros: optional_nonnegative(costs[1])?,
            actual_incremental_cost_micros: optional_nonnegative(costs[2])?,
            routing_savings_micros: savings[0],
            entitlement_savings_micros: savings[1],
            estimated_total_savings_micros: savings[2],
            price_version_refs: parse_refs(&price_refs)?,
            price_override_revision_refs: parse_refs(&override_refs)?,
            facts_completeness: parse_facts(&facts)?,
            detail_available: false,
        });
    }
    Ok(aggregates)
}

fn nonnegative(value: i64) -> Result<u64, ObservationQueryError> {
    value.try_into().map_err(|_| ObservationQueryError::Corrupt)
}

fn optional_nonnegative(value: Option<i64>) -> Result<Option<u64>, ObservationQueryError> {
    value.map(nonnegative).transpose()
}

fn parse_refs(value: &str) -> Result<BTreeSet<String>, ObservationQueryError> {
    serde_json::from_str(value).map_err(|_| ObservationQueryError::Corrupt)
}

fn parse_facts(value: &str) -> Result<FactsCompleteness, ObservationQueryError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| ObservationQueryError::Corrupt)
}

pub(super) fn get_value(
    store: &crate::LocalObservationStore,
    workspace_id: &WorkspaceId,
    query: &ValueQueryV1,
) -> Result<hiroute_domain::ValueViewV1, ObservationQueryError> {
    use super::*;

    if query.from_ms >= query.to_ms || query.currency.is_empty() {
        return Err(ObservationQueryError::InvalidQuery);
    }
    let _permit = store
        .query_permit()
        .map_err(crate::query_v2::ObservationV2Error::into_domain)?;
    let connection = rusqlite::Connection::open_with_flags(
        &store.activity_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|_| ObservationQueryError::Unavailable)?;
    let _deadline = crate::query_v2::QueryDeadline::start(&connection)
        .map_err(crate::query_v2::ObservationV2Error::into_domain)?;
    let mut statement = connection
        .prepare(
            "SELECT session_id, frozen_at_ms, CASE WHEN length(CAST(body_json AS BLOB))<=4096 THEN body_json ELSE '' END, body_digest
                 FROM value_ledger_entries
                 WHERE workspace_id=?1 AND agent_plan_id=?2 AND currency=?3
                   AND frozen_at_ms>=?4 AND frozen_at_ms<?5
                   AND (?6 IS NULL OR session_id=?6)
                   AND NOT EXISTS(SELECT 1 FROM valuation_attempt_inputs_v2 v WHERE v.workspace_id=value_ledger_entries.workspace_id AND v.request_id=value_ledger_entries.request_id AND v.pricing_json IS NOT NULL)
                 ORDER BY frozen_at_ms, request_id LIMIT 201",
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let rows = statement
        .query_map(
            params![
                workspace_id.as_str(),
                query.agent_plan_id.as_str(),
                query.currency,
                query.from_ms,
                query.to_ms,
                query.session_id.as_ref().map(|id| id.as_str()),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let mut entries = Vec::new();
    for (candidate, row) in rows.enumerate() {
        if candidate >= 200 {
            return Err(ObservationQueryError::InvalidQuery);
        }
        let (session, _frozen_at, body, stored_digest) =
            row.map_err(|_| ObservationQueryError::Corrupt)?;
        if query
            .session_id
            .as_ref()
            .is_some_and(|value| value.as_str() != session)
        {
            continue;
        }
        let entry: ValueLedgerEntryV1 =
            serde_json::from_str(&body).map_err(|_| ObservationQueryError::Corrupt)?;
        entry
            .validate()
            .map_err(|_| ObservationQueryError::Corrupt)?;
        if entry.frozen.agent_plan_id != query.agent_plan_id
            || entry.frozen.currency != query.currency
        {
            return Err(ObservationQueryError::Corrupt);
        }
        if hiroute_domain::CanonicalDigest::of(&entry)
            .map_err(|_| ObservationQueryError::Corrupt)?
            .as_str()
            != stored_digest
        {
            return Err(ObservationQueryError::Corrupt);
        }
        entries.push(entry);
    }
    let daily_aggregates = if query.session_id.is_none() {
        value::read_daily_aggregates(&connection, workspace_id, query)?
    } else {
        Vec::new()
    };
    aggregate_entries(query, entries, daily_aggregates)
}
