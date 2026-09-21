use std::collections::{BTreeMap, BTreeSet};

use hiroute_domain::{
    DeletionDataClass, FactsCompleteness, ObservationQueryError, SessionDeletionSpecV1,
};
use rusqlite::{Transaction, params};

const DAY_MILLIS: i64 = 86_400_000;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BucketKey {
    agent_plan_id: String,
    day_number: i64,
    currency: String,
    billing_unit: String,
}

#[derive(Clone, Copy, Debug)]
struct OptionalSum {
    sum: i64,
    known: bool,
}

impl Default for OptionalSum {
    fn default() -> Self {
        Self {
            sum: 0,
            known: true,
        }
    }
}

impl OptionalSum {
    fn add(&mut self, value: Option<i64>) -> Result<(), ObservationQueryError> {
        match value {
            Some(value) if self.known => {
                self.sum = self
                    .sum
                    .checked_add(value)
                    .ok_or(ObservationQueryError::Corrupt)?;
            }
            Some(_) => {}
            None => self.known = false,
        }
        Ok(())
    }

    fn value(self) -> Option<i64> {
        self.known.then_some(self.sum)
    }
}

#[derive(Clone, Debug)]
struct BucketValues {
    tokens: [i64; 5],
    amounts: [OptionalSum; 6],
    price_version_refs: BTreeSet<String>,
    price_override_revision_refs: BTreeSet<String>,
    facts_completeness: FactsCompleteness,
}

impl Default for BucketValues {
    fn default() -> Self {
        Self {
            tokens: [0; 5],
            amounts: [OptionalSum::default(); 6],
            price_version_refs: BTreeSet::new(),
            price_override_revision_refs: BTreeSet::new(),
            facts_completeness: FactsCompleteness::Complete,
        }
    }
}

impl BucketValues {
    fn add(
        &mut self,
        tokens: [i64; 5],
        amounts: [Option<i64>; 6],
        price_version_refs: BTreeSet<String>,
        price_override_revision_refs: BTreeSet<String>,
        facts_completeness: FactsCompleteness,
    ) -> Result<(), ObservationQueryError> {
        for (total, value) in self.tokens.iter_mut().zip(tokens) {
            if value < 0 {
                return Err(ObservationQueryError::Corrupt);
            }
            *total = total
                .checked_add(value)
                .ok_or(ObservationQueryError::Corrupt)?;
        }
        for (total, value) in self.amounts.iter_mut().zip(amounts) {
            total.add(value)?;
        }
        self.price_version_refs.extend(price_version_refs);
        self.price_override_revision_refs
            .extend(price_override_revision_refs);
        self.facts_completeness = combine_facts(self.facts_completeness, facts_completeness);
        Ok(())
    }
}

pub(super) fn archive_session_value(
    transaction: &Transaction<'_>,
    spec: &SessionDeletionSpecV1,
) -> Result<(), ObservationQueryError> {
    archive_selected_value(transaction, spec, None, None)
}

pub(super) fn archive_request_value(
    transaction: &Transaction<'_>,
    spec: &SessionDeletionSpecV1,
    request: &str,
    archive_id: hiroute_domain::SessionId,
) -> Result<(), ObservationQueryError> {
    archive_selected_value(transaction, spec, Some(request), Some(archive_id))
}

fn archive_selected_value(
    transaction: &Transaction<'_>,
    spec: &SessionDeletionSpecV1,
    request: Option<&str>,
    archive_id: Option<hiroute_domain::SessionId>,
) -> Result<(), ObservationQueryError> {
    if spec.data_class != DeletionDataClass::FactsAndContent {
        return Ok(());
    }
    let mut statement = transaction
        .prepare(
            "SELECT agent_plan_id, frozen_at_ms, currency, billing_unit,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    reasoning_tokens, baseline_api_equivalent_cost_micros,
                    chosen_api_equivalent_cost_micros, actual_incremental_cost_micros,
                    routing_savings_micros, entitlement_savings_micros,
                    estimated_total_savings_micros, price_version,
                    price_override_revision, facts_completeness
             FROM value_ledger_entries
             WHERE workspace_id=?1 AND session_id=?2 AND (?3 IS NULL OR request_id=?3)
               AND NOT EXISTS(SELECT 1 FROM valuation_attempt_inputs_v2 v WHERE v.workspace_id=value_ledger_entries.workspace_id AND v.request_id=value_ledger_entries.request_id AND v.pricing_json IS NOT NULL)
             ORDER BY frozen_at_ms, request_id",
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let rows = statement
        .query_map(
            params![
                spec.workspace_id.as_str(),
                spec.session_id.as_str(),
                request
            ],
            |row| {
                Ok((
                    BucketKey {
                        agent_plan_id: row.get(0)?,
                        day_number: row.get::<_, i64>(1)?.div_euclid(DAY_MILLIS),
                        currency: row.get(2)?,
                        billing_unit: row.get(3)?,
                    },
                    [
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ],
                    [
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                        row.get(14)?,
                    ],
                    row.get::<_, String>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, String>(17)?,
                ))
            },
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let mut buckets = BTreeMap::<BucketKey, BucketValues>::new();
    for row in rows {
        let (key, tokens, amounts, price_version, override_revision, facts) =
            row.map_err(|_| ObservationQueryError::Corrupt)?;
        let mut price_refs = BTreeSet::new();
        price_refs.insert(price_version);
        let override_refs = override_revision.into_iter().collect();
        buckets.entry(key).or_default().add(
            tokens,
            amounts,
            price_refs,
            override_refs,
            parse_facts(&facts)?,
        )?;
    }
    drop(statement);

    let mut target = spec.clone();
    if let Some(id) = archive_id {
        target.session_id = id;
    }
    for (key, values) in &buckets {
        write_contribution(transaction, &target, key, values)?;
        recompute_bucket(transaction, &spec.workspace_id, key)?;
    }
    Ok(())
}

pub(super) fn delete_session_value_rollups(
    transaction: &Transaction<'_>,
    spec: &SessionDeletionSpecV1,
) -> Result<(), ObservationQueryError> {
    let keys = read_session_bucket_keys(transaction, spec)?;
    transaction
        .execute(
            "DELETE FROM session_value_rollup_contributions
             WHERE workspace_id=?1 AND session_id=?2",
            params![spec.workspace_id.as_str(), spec.session_id.as_str()],
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    for key in &keys {
        recompute_bucket(transaction, &spec.workspace_id, key)?;
    }
    Ok(())
}

fn read_session_bucket_keys(
    transaction: &Transaction<'_>,
    spec: &SessionDeletionSpecV1,
) -> Result<Vec<BucketKey>, ObservationQueryError> {
    let mut statement = transaction
        .prepare(
            "SELECT agent_plan_id, day_number, currency, billing_unit
             FROM session_value_rollup_contributions
             WHERE workspace_id=?1 AND session_id=?2",
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    statement
        .query_map(
            params![spec.workspace_id.as_str(), spec.session_id.as_str()],
            |row| {
                Ok(BucketKey {
                    agent_plan_id: row.get(0)?,
                    day_number: row.get(1)?,
                    currency: row.get(2)?,
                    billing_unit: row.get(3)?,
                })
            },
        )
        .map_err(|_| ObservationQueryError::Unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ObservationQueryError::Corrupt)
}

fn write_contribution(
    transaction: &Transaction<'_>,
    spec: &SessionDeletionSpecV1,
    key: &BucketKey,
    values: &BucketValues,
) -> Result<(), ObservationQueryError> {
    transaction
        .execute(
            "INSERT OR REPLACE INTO session_value_rollup_contributions
             (workspace_id, session_id, agent_plan_id, day_number, currency, billing_unit,
              input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
              baseline_api_equivalent_cost_micros, chosen_api_equivalent_cost_micros,
              actual_incremental_cost_micros, routing_savings_micros,
              entitlement_savings_micros, estimated_total_savings_micros,
              price_version_refs_json, price_override_revision_refs_json, facts_completeness)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                spec.workspace_id.as_str(),
                spec.session_id.as_str(),
                key.agent_plan_id,
                key.day_number,
                key.currency,
                key.billing_unit,
                values.tokens[0],
                values.tokens[1],
                values.tokens[2],
                values.tokens[3],
                values.tokens[4],
                values.amounts[0].value(),
                values.amounts[1].value(),
                values.amounts[2].value(),
                values.amounts[3].value(),
                values.amounts[4].value(),
                values.amounts[5].value(),
                encode_refs(&values.price_version_refs)?,
                encode_refs(&values.price_override_revision_refs)?,
                facts_name(values.facts_completeness),
            ],
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    Ok(())
}

fn recompute_bucket(
    transaction: &Transaction<'_>,
    workspace_id: &hiroute_domain::WorkspaceId,
    key: &BucketKey,
) -> Result<(), ObservationQueryError> {
    let mut statement = transaction
        .prepare(
            "SELECT input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    reasoning_tokens, baseline_api_equivalent_cost_micros,
                    chosen_api_equivalent_cost_micros, actual_incremental_cost_micros,
                    routing_savings_micros, entitlement_savings_micros,
                    estimated_total_savings_micros, price_version_refs_json,
                    price_override_revision_refs_json, facts_completeness
             FROM session_value_rollup_contributions
             WHERE workspace_id=?1 AND agent_plan_id=?2 AND day_number=?3
               AND currency=?4 AND billing_unit=?5",
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let rows = statement
        .query_map(
            params![
                workspace_id.as_str(),
                key.agent_plan_id,
                key.day_number,
                key.currency,
                key.billing_unit,
            ],
            |row| {
                Ok((
                    [
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ],
                    [
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ],
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                ))
            },
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let mut values = BucketValues::default();
    let mut count = 0_u64;
    for row in rows {
        let (tokens, amounts, price_refs, override_refs, facts) =
            row.map_err(|_| ObservationQueryError::Corrupt)?;
        values.add(
            tokens,
            amounts,
            decode_refs(&price_refs)?,
            decode_refs(&override_refs)?,
            parse_facts(&facts)?,
        )?;
        count = count.checked_add(1).ok_or(ObservationQueryError::Corrupt)?;
    }
    drop(statement);
    if count == 0 {
        transaction
            .execute(
                "DELETE FROM daily_value_rollups
                 WHERE workspace_id=?1 AND agent_plan_id=?2 AND day_number=?3
                   AND currency=?4 AND billing_unit=?5",
                params![
                    workspace_id.as_str(),
                    key.agent_plan_id,
                    key.day_number,
                    key.currency,
                    key.billing_unit,
                ],
            )
            .map_err(|_| ObservationQueryError::Unavailable)?;
        return Ok(());
    }
    transaction
        .execute(
            "INSERT OR REPLACE INTO daily_value_rollups
             (workspace_id, agent_plan_id, day_number, currency, billing_unit,
              input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
              baseline_api_equivalent_cost_micros, chosen_api_equivalent_cost_micros,
              actual_incremental_cost_micros, routing_savings_micros,
              entitlement_savings_micros, estimated_total_savings_micros,
              price_version_refs_json, price_override_revision_refs_json, facts_completeness)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19)",
            params![
                workspace_id.as_str(),
                key.agent_plan_id,
                key.day_number,
                key.currency,
                key.billing_unit,
                values.tokens[0],
                values.tokens[1],
                values.tokens[2],
                values.tokens[3],
                values.tokens[4],
                values.amounts[0].value(),
                values.amounts[1].value(),
                values.amounts[2].value(),
                values.amounts[3].value(),
                values.amounts[4].value(),
                values.amounts[5].value(),
                encode_refs(&values.price_version_refs)?,
                encode_refs(&values.price_override_revision_refs)?,
                facts_name(values.facts_completeness),
            ],
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    Ok(())
}

pub(super) fn session_rollup_count(
    transaction: &rusqlite::Connection,
    spec: &SessionDeletionSpecV1,
) -> Result<u64, ObservationQueryError> {
    let count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM session_value_rollup_contributions
             WHERE workspace_id=?1 AND session_id=?2",
            params![spec.workspace_id.as_str(), spec.session_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    count.try_into().map_err(|_| ObservationQueryError::Corrupt)
}

pub(super) fn session_tombstone_count(
    transaction: &rusqlite::Connection,
    spec: &SessionDeletionSpecV1,
) -> Result<u64, ObservationQueryError> {
    let count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM observation_tombstones
             WHERE workspace_id=?1 AND session_id=?2",
            params![spec.workspace_id.as_str(), spec.session_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    count.try_into().map_err(|_| ObservationQueryError::Corrupt)
}

pub(super) fn delete_session_tombstones(
    transaction: &Transaction<'_>,
    spec: &SessionDeletionSpecV1,
) -> Result<(), ObservationQueryError> {
    transaction
        .execute(
            "DELETE FROM observation_tombstones WHERE workspace_id=?1 AND session_id=?2",
            params![spec.workspace_id.as_str(), spec.session_id.as_str()],
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    Ok(())
}

fn encode_refs(refs: &BTreeSet<String>) -> Result<String, ObservationQueryError> {
    serde_json::to_string(refs).map_err(|_| ObservationQueryError::Corrupt)
}

fn decode_refs(value: &str) -> Result<BTreeSet<String>, ObservationQueryError> {
    serde_json::from_str(value).map_err(|_| ObservationQueryError::Corrupt)
}

fn parse_facts(value: &str) -> Result<FactsCompleteness, ObservationQueryError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| ObservationQueryError::Corrupt)
}

fn facts_name(value: FactsCompleteness) -> &'static str {
    match value {
        FactsCompleteness::Complete => "complete",
        FactsCompleteness::Partial => "partial",
        FactsCompleteness::Unknown => "unknown",
    }
}

const fn combine_facts(left: FactsCompleteness, right: FactsCompleteness) -> FactsCompleteness {
    match (left, right) {
        (FactsCompleteness::Unknown, _) | (_, FactsCompleteness::Unknown) => {
            FactsCompleteness::Unknown
        }
        (FactsCompleteness::Partial, _) | (_, FactsCompleteness::Partial) => {
            FactsCompleteness::Partial
        }
        _ => FactsCompleteness::Complete,
    }
}
