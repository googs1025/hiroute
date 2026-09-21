use super::*;
use hiroute_domain::{
    CanonicalDigest, ObservationFactsPageV2, ObservationFactsQueryV2, ObservationSafeFactV2,
};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FactsCursor {
    schema: u8,
    binding: String,
    visibility: u64,
    watermark: i64,
    after: i64,
}
impl crate::LocalObservationStore {
    pub fn observed_facts(
        &self,
        reader: &ObservationReaderContext,
        query: &ObservationFactsQueryV2,
        now_ms: i64,
    ) -> Result<ObservationFactsPageV2, ObservationV2Error> {
        reader.check(now_ms, false, false)?;
        let _permit = self.query_permit()?;
        if query.limit == 0 || query.limit > 200 {
            return Err(ObservationV2Error::Invalid);
        }
        let binding = CanonicalDigest::of(&(reader.binding()?, &query.request_id, query.limit))
            .map_err(|_| ObservationV2Error::Invalid)?
            .to_string();
        let mut connection =
            Connection::open_with_flags(&self.activity_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let _deadline = super::QueryDeadline::start_for(&connection, reader)?;
        let transaction = connection.transaction()?;
        if reader.allowed_runs().is_some() {
            relation::authorized_link(&transaction, reader, &query.request_id, now_ms)?;
        }
        let live:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM logical_requests WHERE workspace_id=?1 AND request_id=?2 AND started_at_ms>?3)",params![reader.workspace().as_str(),query.request_id.as_str(),now_ms.saturating_sub(crate::managed_text::RETENTION_MS)],|row|row.get(0))?;
        if !live {
            return Err(ObservationV2Error::Unavailable);
        }
        let visibility:u64=transaction.query_row("SELECT CAST(value AS INTEGER) FROM observation_meta WHERE key='query_visibility_generation'",[],|row|row.get(0))?;
        let mut cursor = match &query.cursor {
            Some(encoded) => {
                let cursor: FactsCursor = self.decode_observation_cursor(encoded)?;
                if cursor.schema != 2
                    || cursor.binding != binding
                    || cursor.visibility != visibility
                {
                    return Err(ObservationV2Error::Stale);
                }
                cursor
            }
            None => FactsCursor {
                schema: 2,
                binding,
                visibility,
                watermark: transaction.query_row(
                    "SELECT COALESCE(MAX(rowid),0) FROM observation_safe_facts_v2",
                    [],
                    |row| row.get(0),
                )?,
                after: 0,
            },
        };
        let projection_partial:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM execution_fact_events e WHERE e.workspace_id=?1 AND e.request_id=?2 AND NOT EXISTS(SELECT 1 FROM observation_safe_facts_v2 f WHERE f.workspace_id=e.workspace_id AND f.request_id=e.request_id AND f.original_digest=e.envelope_digest))",params![reader.workspace().as_str(),query.request_id.as_str()],|row|row.get(0))?;
        let rows = {
            let mut statement=transaction.prepare("SELECT f.rowid,f.body_json,NOT EXISTS(SELECT 1 FROM observation_sensitive_payloads_v2 p WHERE p.id='fact:'||f.original_digest) FROM observation_safe_facts_v2 f WHERE f.workspace_id=?1 AND f.request_id=?2 AND f.rowid>?3 AND f.rowid<=?4 ORDER BY f.rowid LIMIT ?5")?;
            statement
                .query_map(
                    params![
                        reader.workspace().as_str(),
                        query.request_id.as_str(),
                        cursor.after,
                        cursor.watermark,
                        u64::from(query.limit) + 1
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, bool>(2)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let more = rows.len() > usize::from(query.limit);
        let mut facts = Vec::new();
        let mut bytes = 0;
        for (id, json, deleted) in rows.into_iter().take(usize::from(query.limit)) {
            bytes += json.len();
            if bytes > 1024 * 1024 {
                return Err(ObservationV2Error::Unavailable);
            }
            let mut fact: ObservationSafeFactV2 =
                serde_json::from_str(&json).map_err(|_| ObservationV2Error::Unavailable)?;
            // Routing identity is an internal durable projection consumed by the request
            // timeline. Keep the public safe-facts surface unchanged.
            fact.routing_context = None;
            fact.sensitive_fields_deleted = deleted;
            facts.push(fact);
            cursor.after = id;
        }
        transaction.commit()?;
        Ok(ObservationFactsPageV2 {
            facts,
            projection_partial,
            next_cursor: if more {
                Some(self.encode_observation_cursor(&cursor)?)
            } else {
                None
            },
        })
    }
}
