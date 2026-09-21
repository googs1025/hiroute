use super::ValuationError;
use rusqlite::{Transaction, params};

/// Called inside the existing exact-scope deletion transaction, before request
/// rows disappear. Archives have no request/run reverse index.
pub(crate) fn archive_session(
    transaction: &Transaction<'_>,
    workspace: &str,
    session: &str,
    keep_totals: bool,
) -> Result<(), ValuationError> {
    archive_selected(transaction, workspace, session, None, keep_totals)
}

pub(crate) fn archive_request(
    transaction: &Transaction<'_>,
    workspace: &str,
    session: &str,
    request: &str,
) -> Result<(), ValuationError> {
    super::worker::settle_one(transaction, workspace, request)?;
    archive_selected(transaction, workspace, session, Some(request), true)
}

fn archive_selected(
    transaction: &Transaction<'_>,
    workspace: &str,
    session: &str,
    request: Option<&str>,
    keep_totals: bool,
) -> Result<(), ValuationError> {
    if keep_totals {
        super::usage_archive::archive(transaction, workspace, session, request)?;
        let mut statement=transaction.prepare(
            "SELECT c.day,c.plan_id,c.currency,CASE WHEN q.traffic_kind='normal' THEN c.valuation_kind ELSE 'unclassified:'||c.valuation_kind END,c.known_micros,c.coverage FROM valuation_contributions_v2 c
             JOIN valuation_requests_v2 r ON r.workspace_id=c.workspace_id AND r.request_id=c.request_id
             JOIN logical_requests q ON q.workspace_id=r.workspace_id AND q.request_id=r.request_id
             WHERE r.workspace_id=?1 AND r.session_id=?2 AND (?3 IS NULL OR r.request_id=?3)",
        )?;
        let rows = statement.query_map(params![workspace, session, request], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (day, plan, currency, kind, known, coverage) = row?;
            transaction.execute(
                "INSERT INTO valuation_archives_v2(workspace_id,day,plan_id,currency,valuation_kind,known_micros,partial_count)
                 VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT DO UPDATE SET
                    known_micros=CASE WHEN valuation_archives_v2.known_micros IS NULL THEN excluded.known_micros WHEN excluded.known_micros IS NULL THEN valuation_archives_v2.known_micros
                         WHEN valuation_archives_v2.known_micros>9223372036854775807-excluded.known_micros THEN NULL
                         ELSE valuation_archives_v2.known_micros+excluded.known_micros END,
                    partial_count=valuation_archives_v2.partial_count+excluded.partial_count+COALESCE(valuation_archives_v2.known_micros>9223372036854775807-excluded.known_micros,0)",
                params![workspace,day,plan,currency,kind,known,coverage!="\"complete\""],
            )?;
        }
    }
    for table in [
        "valuation_attempt_inputs_v2",
        "valuation_pending_v2",
        "valuation_records_v2",
        "valuation_contributions_v2",
    ] {
        transaction.execute(
            &format!(
                "DELETE FROM {table} WHERE workspace_id=?1 AND request_id IN
            (SELECT request_id FROM valuation_requests_v2 WHERE workspace_id=?1 AND session_id=?2 AND (?3 IS NULL OR request_id=?3))"
            ),
            params![workspace, session, request],
        )?;
    }
    transaction.execute(
        "DELETE FROM valuation_requests_v2 WHERE workspace_id=?1 AND session_id=?2 AND (?3 IS NULL OR request_id=?3)",
        params![workspace, session, request],
    )?;
    Ok(())
}
