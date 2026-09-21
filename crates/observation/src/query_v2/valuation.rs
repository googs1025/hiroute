use super::{ObservationReaderContext, ObservationV2Error, relation};
use crate::LocalObservationStore;
use hiroute_domain::{
    LogicalRequestId, ObservationQueryError as Error, ObservationValuationAmountV2,
    ObservationValuationSummaryV2,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

impl LocalObservationStore {
    /// Facts-only current-value lookup. No raw receipt, matching text, content
    /// reference, current price lookup, or cross-run aggregate is expanded here.
    pub fn observed_valuation_summary(
        &self,
        reader: &ObservationReaderContext,
        request: &LogicalRequestId,
        now_ms: i64,
    ) -> Result<ObservationValuationSummaryV2, Error> {
        reader.check(now_ms, false, false)?;
        let _permit = self
            .query_permit()
            .map_err(ObservationV2Error::into_domain)?;
        let mut connection =
            Connection::open_with_flags(&self.activity_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| Error::Unavailable)?;
        let _deadline = super::QueryDeadline::start_for(&connection, reader)
            .map_err(ObservationV2Error::into_domain)?;
        let transaction = connection.transaction().map_err(|_| Error::Unavailable)?;
        if reader.allowed_runs().is_some() {
            relation::authorized_link(&transaction, reader, request, now_ms)
                .map_err(ObservationV2Error::into_domain)?;
        }
        let metadata: Option<(u64, bool, bool, bool)> = transaction.query_row(
            "SELECT v.input_revision,v.terminal,v.partial,EXISTS(
                SELECT 1 FROM valuation_pending_v2 p WHERE p.workspace_id=v.workspace_id AND p.request_id=v.request_id)
             FROM valuation_requests_v2 v JOIN logical_requests r
                ON r.workspace_id=v.workspace_id AND r.request_id=v.request_id
             JOIN sessions s ON s.workspace_id=r.workspace_id AND s.session_id=r.session_id
             WHERE v.workspace_id=?1 AND v.request_id=?2 AND r.started_at_ms>?3
                AND (s.tombstone_reason IS NULL OR s.content_completeness='deleted')",
            params![reader.workspace().as_str(),request.as_str(),now_ms.saturating_sub(crate::managed_text::RETENTION_MS)],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional().map_err(|_| Error::Unavailable)?;
        let Some((revision, terminal, partial, queued)) = metadata else {
            return Err(Error::NotFound);
        };
        // Extract only the bounded safe summary, never the full per-attempt quote
        // graph. The JSON length guard runs before SQLite hands text to Rust.
        let settled: Option<(u64,u64,Option<String>)> = transaction.query_row(
            "SELECT revision,json_extract(body_json,'$.unknown_attempt_count'),
                CASE WHEN length(json_extract(body_json,'$.amounts'))<=65536
                THEN json_extract(body_json,'$.amounts') ELSE NULL END
             FROM valuation_records_v2 WHERE workspace_id=?1 AND request_id=?2 ORDER BY revision DESC LIMIT 1",
            params![reader.workspace().as_str(),request.as_str()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional().map_err(|_| Error::Unavailable)?;
        let (settled_revision, unknown_attempt_count, amounts) = match settled {
            Some((settled, unknown, Some(encoded))) => {
                let amounts: Vec<ObservationValuationAmountV2> =
                    serde_json::from_str(&encoded).map_err(|_| Error::Corrupt)?;
                if amounts.len() > 200 {
                    return Err(Error::Unavailable);
                }
                (Some(settled), Some(unknown), amounts)
            }
            Some(_) => return Err(Error::Unavailable),
            None => (None, None, Vec::new()),
        };
        transaction.commit().map_err(|_| Error::Unavailable)?;
        reader.check(now_ms, false, false)?;
        Ok(ObservationValuationSummaryV2 {
            schema: "hiroute.observation.valuation-summary/v2".into(),
            request_id: request.clone(),
            input_revision: revision,
            settled_revision,
            pending: queued || settled_revision != Some(revision),
            terminal_observed: terminal,
            facts_partial: partial,
            unknown_attempt_count,
            amounts,
        })
    }
}
