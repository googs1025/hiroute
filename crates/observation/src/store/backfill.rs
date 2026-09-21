//! Supported activity-v4 inline carriers only. Preserve exact original bytes
//! and digests; malformed/oversized rows remain unavailable, not reinterpreted.
use super::*;
use hiroute_domain::{CanonicalDigest, ExecutionFactEnvelopeV1, RoutingReceiptV1};
use rusqlite::{OptionalExtension, params};

impl LocalObservationStore {
    pub fn backfill_inline_payloads(&self) -> Result<usize, ObservationStoreError> {
        let started = std::time::Instant::now();
        let mut db = self.connection.lock();
        let tx = db
            .transaction()
            .map_err(|_| ObservationStoreError::ActivityUnavailable)?;
        let mut completed = 0;
        let mut bytes = 0;
        for (table, column, digest_column, kind) in [
            (
                "execution_fact_events",
                "envelope_json",
                "envelope_digest",
                "fact",
            ),
            ("routing_receipts", "body_json", "body_digest", "receipt"),
        ] {
            let key = format!("inline-backfill-{kind}-rowid");
            let after: i64 = tx
                .query_row(
                    "SELECT CAST(value AS INTEGER) FROM observation_meta WHERE key=?1",
                    [&key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| ObservationStoreError::Corrupt)?
                .unwrap_or(0);
            let ids = {
                let mut stmt = tx
                    .prepare(&format!(
                        "SELECT rowid FROM {table} WHERE rowid>?1 ORDER BY rowid LIMIT 8"
                    ))
                    .map_err(|_| ObservationStoreError::ActivityUnavailable)?;
                stmt.query_map([after], |r| r.get::<_, i64>(0))
                    .map_err(|_| ObservationStoreError::ActivityUnavailable)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| ObservationStoreError::Corrupt)?
            };
            for id in ids {
                if bytes >= 8 * 1024 * 1024
                    || started.elapsed() >= std::time::Duration::from_millis(500)
                {
                    break;
                }
                let (workspace,session,request,digest,body):(String,String,String,String,String)=tx.query_row(&format!("SELECT workspace_id,session_id,request_id,{digest_column},CASE WHEN length(CAST({column} AS BLOB))<=1048576 THEN {column} ELSE '' END FROM {table} WHERE rowid=?1"),[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(|_|ObservationStoreError::Corrupt)?;
                bytes += body.len();
                if !body.contains("\"managed_sensitive_ref\"") {
                    let verified = if kind == "fact" {
                        serde_json::from_str::<ExecutionFactEnvelopeV1>(&body)
                            .ok()
                            .filter(|fact| {
                                fact.validate_persisted_contract().is_ok()
                                    && fact.correlation.workspace_id.as_str() == workspace
                                    && fact.correlation.conversation_id.as_str() == session
                                    && fact.correlation.request_id.as_str() == request
                                    && CanonicalDigest::of(fact).is_ok_and(|d| d.as_str() == digest)
                            })
                            .map(|fact| {
                                super::safe_facts::project(
                                    &tx,
                                    &fact,
                                    &CanonicalDigest::parse(&digest)
                                        .map_err(|_| ObservationStoreError::Corrupt)?,
                                )?;
                                super::sensitive::project_model(&tx, &fact)?;
                                fact.occurred_at_ms()
                                    .map_err(|_| ObservationStoreError::Corrupt)
                            })
                            .transpose()?
                    } else {
                        serde_json::from_str::<RoutingReceiptV1>(&body)
                            .ok()
                            .filter(|receipt| {
                                receipt.validate().is_ok()
                                    && receipt.workspace_id.as_str() == workspace
                                    && receipt.session_id.as_str() == session
                                    && receipt.request_id.as_str() == request
                                    && receipt.digest().is_ok_and(|d| d.as_str() == digest)
                            })
                            .map(|receipt| receipt.frozen_at_ms)
                    };
                    if let Some(created) = verified {
                        let marker = super::sensitive::put(
                            &tx, kind, &digest, &workspace, &session, &request, created, &body,
                        )?;
                        tx.execute(
                            &format!("UPDATE {table} SET {column}=?2 WHERE rowid=?1"),
                            params![id, marker],
                        )
                        .map_err(|_| ObservationStoreError::ActivityUnavailable)?;
                        completed += 1;
                    } else {
                        tx.execute("INSERT INTO observation_meta(key,value) VALUES('inline_backfill_errors','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1",[]).map_err(|_|ObservationStoreError::ActivityUnavailable)?;
                    }
                }
                tx.execute(
                    "INSERT OR REPLACE INTO observation_meta(key,value) VALUES(?1,?2)",
                    params![key, id.to_string()],
                )
                .map_err(|_| ObservationStoreError::ActivityUnavailable)?;
            }
        }
        if completed > 0 {
            crate::query_v2::invalidate_visibility(&tx)
                .map_err(|_| ObservationStoreError::ActivityUnavailable)?;
            increment_store_revision(&tx)?;
        }
        tx.commit()
            .map_err(|_| ObservationStoreError::ActivityUnavailable)?;
        Ok(completed)
    }
}
