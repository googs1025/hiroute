use super::*;
use crate::valuation::{
    CACHE_HIT_ARCHIVE_SCHEMA, CACHE_HIT_ELIGIBLE_ATTEMPTS, CACHE_HIT_INPUT_TOKENS,
    CACHE_HIT_INVALID_ATTEMPTS, CACHE_HIT_MISSING_ATTEMPTS, CACHE_HIT_READ_TOKENS,
    CACHE_HIT_TOTAL_ATTEMPTS, CACHE_HIT_ZERO_INPUT_ATTEMPTS,
};
use hiroute_domain::{
    ObservationCacheHitStateV2, ObservationCacheHitSummaryV2, ObservationMetricCoverageV2,
    ObservationReaderContext, ObservationValueQueryV2,
};
use rusqlite::{Transaction, params};
use std::collections::BTreeMap;

#[derive(Default)]
struct AttemptTotals {
    read: u64,
    input: u64,
    eligible: u64,
    total: u64,
    zero_input: u64,
    missing: u64,
    invalid: u64,
    arithmetic_overflow: bool,
    archive_coverage_partial: bool,
}

#[derive(Default)]
struct ArchivedMetric {
    sum: u64,
    known: bool,
    incomplete: bool,
}

pub(super) fn aggregate(
    tx: &Transaction<'_>,
    reader: &ObservationReaderContext,
    query: &ObservationValueQueryV2,
    retained_from: i64,
    pending_requests: u64,
    provisional_requests: u64,
    unknown_traffic_requests: u64,
) -> Result<ObservationCacheHitSummaryV2, ObservationV2Error> {
    let runs = reader
        .allowed_runs()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| ObservationV2Error::Invalid)?;
    let retention_boundary_partial = retained_from > query.from_ms;
    let scope = "WITH selected AS (
        SELECT r.* FROM logical_requests r
        WHERE r.workspace_id=?1 AND r.started_at_ms>=?2 AND r.started_at_ms<?3
          AND (?4 IS NULL OR r.session_id=?4)
          AND (?5 IS NULL OR EXISTS(
              SELECT 1 FROM valuation_requests_v2 v
              WHERE v.workspace_id=r.workspace_id AND v.request_id=r.request_id AND v.plan_id=?5))
          AND (?6 IS NULL OR EXISTS(
              SELECT 1 FROM observation_run_links l
              WHERE l.workspace_id=r.workspace_id AND l.request_id=r.request_id
                AND l.conflicted=0 AND l.run_id IN(SELECT value FROM json_each(?6))))
    )";
    let sql = format!(
        "{scope} SELECT i.usage_json
         FROM valuation_attempt_inputs_v2 i
         JOIN selected r ON r.workspace_id=i.workspace_id AND r.request_id=i.request_id
         WHERE r.traffic_kind='normal'"
    );
    let mut totals = AttemptTotals::default();
    let mut statement = tx.prepare(&sql)?;
    let rows = statement.query_map(
        params![
            reader.workspace().as_str(),
            retained_from,
            query.to_ms,
            query.session_id,
            query.plan_id,
            runs,
        ],
        |row| row.get::<_, Option<String>>(0),
    )?;
    for row in rows {
        checked_add(&mut totals.total, 1, &mut totals.arithmetic_overflow);
        let usage = row?.and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok());
        let pair = usage
            .as_ref()
            .and_then(|value| Some((value.get("input")?.as_u64()?, value.get("read")?.as_u64()?)));
        record_pair(&mut totals, pair);
    }
    drop(statement);

    if reader.allowed_runs().is_none() && query.session_id.is_none() {
        merge_archives(tx, reader, query, &mut totals)?;
    }

    let paired_sums_available = totals.eligible > 0 && !totals.arithmetic_overflow;
    let (ratio_basis_points, cache_read_tokens, total_input_tokens) =
        if paired_sums_available && totals.input > 0 {
            let numerator = u128::from(totals.read)
                .checked_mul(10_000)
                .and_then(|value| value.checked_add(u128::from(totals.input / 2)));
            let ratio = numerator
                .map(|value| value / u128::from(totals.input))
                .and_then(|value| u32::try_from(value).ok());
            match ratio {
                Some(ratio) => (Some(ratio), Some(totals.read), Some(totals.input)),
                None => {
                    totals.arithmetic_overflow = true;
                    (None, None, None)
                }
            }
        } else {
            (None, None, None)
        };
    let only_zero_input = totals.zero_input > 0
        && totals.zero_input == totals.total
        && totals.eligible == 0
        && totals.missing == 0
        && totals.invalid == 0
        && !totals.arithmetic_overflow
        && !totals.archive_coverage_partial
        && !retention_boundary_partial
        && pending_requests == 0
        && provisional_requests == 0
        && unknown_traffic_requests == 0;
    let state = if ratio_basis_points.is_some() {
        ObservationCacheHitStateV2::Available
    } else if only_zero_input {
        ObservationCacheHitStateV2::NotApplicable
    } else {
        ObservationCacheHitStateV2::Unknown
    };
    let has_known_basis = ratio_basis_points.is_some() || totals.zero_input > 0;
    let has_gap = totals.missing > 0
        || totals.invalid > 0
        || totals.arithmetic_overflow
        || totals.archive_coverage_partial
        || retention_boundary_partial
        || pending_requests > 0
        || provisional_requests > 0
        || unknown_traffic_requests > 0;
    let coverage = match (has_known_basis, has_gap) {
        (false, _) => ObservationMetricCoverageV2::Unknown,
        (true, false) => ObservationMetricCoverageV2::Complete,
        (true, true) => ObservationMetricCoverageV2::Partial,
    };
    Ok(ObservationCacheHitSummaryV2 {
        state,
        ratio_basis_points,
        cache_read_tokens,
        total_input_tokens,
        eligible_attempt_count: totals.eligible,
        total_attempt_count: totals.total,
        zero_input_attempt_count: totals.zero_input,
        missing_attempt_count: totals.missing,
        invalid_attempt_count: totals.invalid,
        arithmetic_overflow: totals.arithmetic_overflow,
        archive_coverage_partial: totals.archive_coverage_partial,
        coverage,
    })
}

fn record_pair(totals: &mut AttemptTotals, pair: Option<(u64, u64)>) {
    let Some((input, read)) = pair else {
        checked_add(&mut totals.missing, 1, &mut totals.arithmetic_overflow);
        return;
    };
    if read > input {
        checked_add(&mut totals.invalid, 1, &mut totals.arithmetic_overflow);
    } else if input == 0 {
        checked_add(&mut totals.zero_input, 1, &mut totals.arithmetic_overflow);
    } else {
        checked_add(&mut totals.eligible, 1, &mut totals.arithmetic_overflow);
        checked_add(&mut totals.input, input, &mut totals.arithmetic_overflow);
        checked_add(&mut totals.read, read, &mut totals.arithmetic_overflow);
    }
}

fn merge_archives(
    tx: &Transaction<'_>,
    reader: &ObservationReaderContext,
    query: &ObservationValueQueryV2,
    totals: &mut AttemptTotals,
) -> Result<(), ObservationV2Error> {
    let mut metrics = BTreeMap::<String, ArchivedMetric>::new();
    let mut statement = tx.prepare(
        "SELECT metric,known_sum,missing_count FROM observation_usage_archives_v2
         WHERE workspace_id=?1 AND day*86400000>=?2 AND (day+1)*86400000<=?3
           AND (?4 IS NULL OR plan_id=?4) AND metric LIKE 'cache_hit_%'",
    )?;
    let rows = statement.query_map(
        params![
            reader.workspace().as_str(),
            query.from_ms,
            query.to_ms,
            query.plan_id,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    for row in rows {
        let (name, value, missing) = row?;
        let metric = metrics.entry(name).or_default();
        if missing != 0 {
            metric.incomplete = true;
        }
        match value.map(|value| value as u64) {
            Some(value) => {
                metric.known = true;
                if let Some(sum) = metric.sum.checked_add(value) {
                    metric.sum = sum;
                } else {
                    metric.incomplete = true;
                }
            }
            None => metric.incomplete = true,
        }
    }
    drop(statement);

    let unmarked_legacy: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM observation_usage_archives_v2 a
             WHERE a.workspace_id=?1 AND a.day*86400000>=?2 AND (a.day+1)*86400000<=?3
               AND (?4 IS NULL OR a.plan_id=?4)
               AND a.metric IN ('input','cache_read')
               AND NOT EXISTS(
                   SELECT 1 FROM observation_usage_archives_v2 marker
                   WHERE marker.workspace_id=a.workspace_id AND marker.day=a.day
                     AND marker.plan_id IS a.plan_id AND marker.metric=?5))",
        params![
            reader.workspace().as_str(),
            query.from_ms,
            query.to_ms,
            query.plan_id,
            CACHE_HIT_ARCHIVE_SCHEMA,
        ],
        |row| row.get(0),
    )?;
    totals.archive_coverage_partial = unmarked_legacy;

    merge_count(
        &metrics,
        CACHE_HIT_TOTAL_ATTEMPTS,
        &mut totals.total,
        &mut totals.arithmetic_overflow,
    );
    merge_count(
        &metrics,
        CACHE_HIT_ELIGIBLE_ATTEMPTS,
        &mut totals.eligible,
        &mut totals.arithmetic_overflow,
    );
    merge_count(
        &metrics,
        CACHE_HIT_ZERO_INPUT_ATTEMPTS,
        &mut totals.zero_input,
        &mut totals.arithmetic_overflow,
    );
    merge_count(
        &metrics,
        CACHE_HIT_MISSING_ATTEMPTS,
        &mut totals.missing,
        &mut totals.arithmetic_overflow,
    );
    merge_count(
        &metrics,
        CACHE_HIT_INVALID_ATTEMPTS,
        &mut totals.invalid,
        &mut totals.arithmetic_overflow,
    );
    merge_count(
        &metrics,
        CACHE_HIT_READ_TOKENS,
        &mut totals.read,
        &mut totals.arithmetic_overflow,
    );
    merge_count(
        &metrics,
        CACHE_HIT_INPUT_TOKENS,
        &mut totals.input,
        &mut totals.arithmetic_overflow,
    );
    Ok(())
}

fn merge_count(
    metrics: &BTreeMap<String, ArchivedMetric>,
    name: &str,
    target: &mut u64,
    overflow: &mut bool,
) {
    let Some(metric) = metrics.get(name) else {
        return;
    };
    if metric.incomplete || !metric.known {
        *overflow = true;
        return;
    }
    checked_add(target, metric.sum, overflow);
}

fn checked_add(target: &mut u64, value: u64, overflow: &mut bool) {
    match target.checked_add(value) {
        Some(sum) => *target = sum,
        None => *overflow = true,
    }
}
