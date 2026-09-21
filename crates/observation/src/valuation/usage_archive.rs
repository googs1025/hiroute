//! Retain basic usage totals without retaining session or run reverse links.
use super::{
    CACHE_HIT_ARCHIVE_SCHEMA, CACHE_HIT_ELIGIBLE_ATTEMPTS, CACHE_HIT_INPUT_TOKENS,
    CACHE_HIT_INVALID_ATTEMPTS, CACHE_HIT_MISSING_ATTEMPTS, CACHE_HIT_READ_TOKENS,
    CACHE_HIT_TOTAL_ATTEMPTS, CACHE_HIT_ZERO_INPUT_ATTEMPTS, ValuationError,
};
use rusqlite::{OptionalExtension, Transaction, params};
pub(super) fn archive(
    tx: &Transaction<'_>,
    workspace: &str,
    session: &str,
    request: Option<&str>,
) -> Result<(), ValuationError> {
    let mut stmt = tx.prepare("SELECT r.started_ms/86400000,r.plan_id,CASE WHEN length(i.usage_json)<=4096 THEN i.usage_json END FROM valuation_attempt_inputs_v2 i JOIN valuation_requests_v2 r ON r.workspace_id=i.workspace_id AND r.request_id=i.request_id JOIN logical_requests q ON q.workspace_id=r.workspace_id AND q.request_id=r.request_id WHERE r.workspace_id=?1 AND r.session_id=?2 AND (?3 IS NULL OR r.request_id=?3) AND q.traffic_kind='normal'")?;
    let rows = stmt.query_map(params![workspace, session, request], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (day, plan, body) = row?;
        let usage = body.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        archive_cache_hit(tx, workspace, day, plan.as_deref(), usage.as_ref())?;
        for (metric, field) in [
            ("input", "input"),
            ("output", "output"),
            ("cache_read", "read"),
            ("cache_write", "write"),
            ("reasoning", "reasoning"),
        ] {
            let known = usage
                .as_ref()
                .and_then(|v| v.get(field))
                .and_then(|v| v.as_u64());
            add_metric(
                tx,
                workspace,
                day,
                plan.as_deref(),
                metric,
                known,
                i64::from(known.is_none()),
            )?;
        }
    }
    Ok(())
}

fn archive_cache_hit(
    tx: &Transaction<'_>,
    workspace: &str,
    day: i64,
    plan: Option<&str>,
    usage: Option<&serde_json::Value>,
) -> Result<(), ValuationError> {
    let marked: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM observation_usage_archives_v2
         WHERE workspace_id=?1 AND day=?2 AND plan_id IS ?3 AND metric=?4)",
        params![workspace, day, plan, CACHE_HIT_ARCHIVE_SCHEMA],
        |row| row.get(0),
    )?;
    if !marked {
        let legacy_usage: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM observation_usage_archives_v2
             WHERE workspace_id=?1 AND day=?2 AND plan_id IS ?3
               AND metric IN ('input','cache_read'))",
            params![workspace, day, plan],
            |row| row.get(0),
        )?;
        if legacy_usage {
            // Do not migrate or mix historical unpaired totals with the current paired
            // contract. The query leaves the whole affected archive day explicitly partial.
            return Ok(());
        }
        tx.execute(
            "INSERT OR IGNORE INTO observation_usage_archives_v2
             (workspace_id,day,plan_id,metric,known_sum,missing_count)
             VALUES(?1,?2,?3,?4,1,0)",
            params![workspace, day, plan, CACHE_HIT_ARCHIVE_SCHEMA],
        )?;
    }

    add_metric(
        tx,
        workspace,
        day,
        plan,
        CACHE_HIT_TOTAL_ATTEMPTS,
        Some(1),
        0,
    )?;
    let pair =
        usage.and_then(|value| Some((value.get("input")?.as_u64()?, value.get("read")?.as_u64()?)));
    let Some((input, read)) = pair else {
        return add_metric(
            tx,
            workspace,
            day,
            plan,
            CACHE_HIT_MISSING_ATTEMPTS,
            Some(1),
            0,
        );
    };
    if read > input {
        return add_metric(
            tx,
            workspace,
            day,
            plan,
            CACHE_HIT_INVALID_ATTEMPTS,
            Some(1),
            0,
        );
    }
    if input == 0 {
        return add_metric(
            tx,
            workspace,
            day,
            plan,
            CACHE_HIT_ZERO_INPUT_ATTEMPTS,
            Some(1),
            0,
        );
    }
    for (metric, value) in [
        (CACHE_HIT_ELIGIBLE_ATTEMPTS, 1),
        (CACHE_HIT_INPUT_TOKENS, input),
        (CACHE_HIT_READ_TOKENS, read),
    ] {
        add_metric(tx, workspace, day, plan, metric, Some(value), 0)?;
    }
    Ok(())
}

fn add_metric(
    tx: &Transaction<'_>,
    workspace: &str,
    day: i64,
    plan: Option<&str>,
    metric: &str,
    known: Option<u64>,
    missing: i64,
) -> Result<(), ValuationError> {
    // SQLite INTEGER is signed. Treat its 64 bits as the current u64 archive
    // representation: ordinary values remain positive, while high-bit values
    // are stored negative and restored with `as u64` by the query readers.
    // Keep addition in Rust so SQLite never turns an unsigned total into REAL.
    // A negative missing_count encodes an overflowed bucket as -(count + 1).
    // This preserves the real missing count while making overflow sticky across
    // later requests, without a new table or a second archive representation.
    let previous: Option<(Option<i64>, i64)> = tx
        .query_row(
            "SELECT known_sum,missing_count FROM observation_usage_archives_v2
             WHERE workspace_id=?1 AND day=?2 AND plan_id IS ?3 AND metric=?4",
            params![workspace, day, plan, metric],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (sum, stored_missing) = match previous {
        Some((previous, stored_missing)) => {
            let already_overflowed = stored_missing < 0;
            let previous_missing = if already_overflowed {
                (-i128::from(stored_missing) - 1) as i64
            } else {
                stored_missing
            };
            let previous_sum = previous.map(|value| value as u64);
            let next_sum = match (previous_sum, known) {
                (Some(left), Some(right)) => left.checked_add(right),
                (Some(left), None) => Some(left),
                (None, Some(right)) => Some(right),
                (None, None) => None,
            };
            let overflow = already_overflowed
                || (previous_sum.is_some() && known.is_some() && next_sum.is_none());
            let missing_total = previous_missing
                .checked_add(missing)
                .ok_or(ValuationError::Storage)?;
            let stored_missing = if overflow {
                (-(i128::from(missing_total) + 1)) as i64
            } else {
                missing_total
            };
            (if overflow { None } else { next_sum }, stored_missing)
        }
        None => (known, missing),
    };
    tx.execute(
        "INSERT INTO observation_usage_archives_v2
         (workspace_id,day,plan_id,metric,known_sum,missing_count)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT DO UPDATE SET
           known_sum=excluded.known_sum,missing_count=excluded.missing_count",
        params![
            workspace,
            day,
            plan,
            metric,
            sum.map(|value| value as i64),
            stored_missing
        ],
    )?;
    Ok(())
}
