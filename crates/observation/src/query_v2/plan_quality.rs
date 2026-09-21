use hiroute_domain::{
    AgentTurnAttributionV1, CanonicalDigest, PlanQualityAssessment, PlanQualitySample,
    PlanQualitySamplesPage, PlanQualitySamplesQuery,
};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};

use super::{ObservationReaderContext, ObservationV2Error, QueryDeadline, identifier};
use crate::LocalObservationStore;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    schema: String,
    binding: String,
    generation: u64,
    last_at_ms: i64,
    last_segment_id: String,
}

impl LocalObservationStore {
    pub fn observed_plan_quality_samples(
        &self,
        reader: &ObservationReaderContext,
        query: &PlanQualitySamplesQuery,
        now_ms: i64,
    ) -> Result<PlanQualitySamplesPage, ObservationV2Error> {
        reader.check(now_ms, false, false)?;
        validate(query)?;
        let _permit = self.query_permit()?;
        let mut normalized = query.clone();
        normalized.cursor = None;
        let binding = CanonicalDigest::of(&(reader.binding()?, normalized))
            .map_err(|_| ObservationV2Error::Invalid)?
            .to_string();
        let mut connection =
            Connection::open_with_flags(&self.activity_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let _deadline = QueryDeadline::start_for(&connection, reader)?;
        let transaction = connection.transaction()?;
        let generation: u64 = transaction.query_row(
            "SELECT CAST(value AS INTEGER) FROM observation_meta
             WHERE key='plan_quality_generation'",
            [],
            |row| row.get(0),
        )?;
        let cursor = match &query.cursor {
            Some(encoded) => {
                let cursor: Cursor = self.decode_observation_cursor(encoded)?;
                if cursor.schema != "plan-quality/v1"
                    || cursor.binding != binding
                    || cursor.generation != generation
                {
                    return Err(ObservationV2Error::Stale);
                }
                cursor
            }
            None => Cursor {
                schema: "plan-quality/v1".into(),
                binding,
                generation,
                last_at_ms: i64::MAX,
                last_segment_id: String::new(),
            },
        };
        let retained_from = now_ms
            .saturating_sub(crate::managed_text::RETENTION_MS)
            .saturating_add(1)
            .max(0);
        let from_ms = query.from_ms.unwrap_or(retained_from).max(retained_from);
        let to_ms = query.to_ms.unwrap_or_else(|| now_ms.saturating_add(1));
        if from_ms >= to_ms {
            return Err(ObservationV2Error::Invalid);
        }
        let runs = reader
            .allowed_runs()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| ObservationV2Error::Invalid)?;
        let mut rows = {
            let mut statement = transaction.prepare(
                "SELECT q.segment_id,q.session_id,q.plan_id,q.plan_revision,
                        q.selected_branch_id,q.executed_branch_id,q.model_configuration_id,
                        q.profile_digest,q.attribution,q.first_turn_id,q.first_turn_ordinal,
                        q.last_observed_turn_id,q.last_observed_turn_ordinal,q.first_at_ms,
                        q.last_at_ms,q.history_partial,q.first_request_id,q.last_request_id,
                        EXISTS(SELECT 1 FROM logical_requests first_req
                          WHERE first_req.workspace_id=q.workspace_id
                            AND first_req.request_id=q.first_request_id)
                          AND EXISTS(SELECT 1 FROM logical_requests last_req
                          WHERE last_req.workspace_id=q.workspace_id
                            AND last_req.request_id=q.last_request_id),
                        CASE WHEN COALESCE(l.conflicted,0)=0 THEN json_extract(l.body_json,'$.task_id') END,
                        CASE WHEN COALESCE(l.conflicted,0)=0 THEN l.run_id END,
                        q.assessment_event_id,q.assessment_trigger_request_id,q.assessed_at_ms,
                        q.target_from_turn_id,q.target_through_turn_id,q.target_from_ordinal,
                        q.target_through_ordinal,q.score,q.assessment_partial,
                        CASE WHEN q.reason_present=1
                          THEN json_extract(p.body_json,'$.fact.reason') END,
                        q.assessment_event_id IS NOT NULL AND EXISTS(
                          SELECT 1 FROM logical_requests trigger_req
                          WHERE trigger_req.workspace_id=q.workspace_id
                            AND trigger_req.request_id=q.assessment_trigger_request_id)
                 FROM plan_quality_segments q
                 JOIN sessions s ON s.workspace_id=q.workspace_id AND s.session_id=q.session_id
                 LEFT JOIN observation_run_links l ON l.workspace_id=q.workspace_id
                    AND l.request_id=q.first_request_id
                 LEFT JOIN execution_fact_events e ON e.workspace_id=q.workspace_id
                    AND e.event_id=q.assessment_event_id
                 LEFT JOIN observation_sensitive_payloads_v2 p
                    ON p.id='fact:'||e.envelope_digest
                 WHERE q.workspace_id=?1 AND q.last_at_ms>=?2 AND q.last_at_ms<?3
                   AND q.last_at_ms IS NOT NULL AND q.selected_branch_id IS NOT NULL
                   AND (?4 IS NULL OR q.plan_id=?4)
                   AND (?5 IS NULL OR q.session_id=?5)
                   AND (?6 IS NULL OR q.segment_id=?6)
                   AND (?7 IS NULL OR q.plan_revision=?7)
                   AND (?8 IS NULL OR q.model_configuration_id=?8)
                   AND (?9 IS NULL OR q.score>?9)
                   AND (?10 IS NULL OR q.score<?10)
                   AND (q.last_at_ms<?11 OR
                        (q.last_at_ms=?11 AND q.segment_id>?12))
                   AND (?13 IS NULL OR (l.conflicted=0 AND
                        l.run_id IN(SELECT value FROM json_each(?13))))
                   AND (s.tombstone_reason IS NULL OR EXISTS(
                        SELECT 1 FROM observation_tombstones t
                        WHERE t.workspace_id=q.workspace_id AND t.session_id=q.session_id
                          AND t.delete_scope='content_only'))
                 ORDER BY q.last_at_ms DESC,q.segment_id
                 LIMIT ?14",
            )?;
            statement
                .query_map(
                    params![
                        reader.workspace().as_str(),
                        from_ms,
                        to_ms,
                        query.plan_id,
                        query.session_id,
                        query.segment_id,
                        query
                            .plan_revision
                            .and_then(|value| i64::try_from(value).ok()),
                        query.model_configuration_id,
                        query.score_gt,
                        query.score_lt,
                        cursor.last_at_ms,
                        cursor.last_segment_id,
                        runs,
                        u64::from(query.limit) + 1,
                    ],
                    |row| {
                        let attribution = match row.get::<_, String>(8)?.as_str() {
                            "single" => AgentTurnAttributionV1::Single,
                            "mixed" => AgentTurnAttributionV1::Mixed,
                            "unknown" => AgentTurnAttributionV1::Unknown,
                            _ => return Err(rusqlite::Error::InvalidQuery),
                        };
                        let assessment_event_id: Option<String> = row.get(21)?;
                        let assessment = assessment_event_id
                            .map(|event_id| {
                                Ok::<PlanQualityAssessment, rusqlite::Error>(
                                    PlanQualityAssessment {
                                        event_id,
                                        trigger_request_id: row.get(22)?,
                                        assessed_at_ms: row.get(23)?,
                                        target_from_turn_id: row.get(24)?,
                                        target_through_turn_id: row.get(25)?,
                                        target_from_ordinal: sql_u64(row.get(26)?)?,
                                        target_through_ordinal: sql_u64(row.get(27)?)?,
                                        score: row.get(28)?,
                                        partial: row.get(29)?,
                                        reason: row.get(30)?,
                                        evidence_available: row.get(31)?,
                                    },
                                )
                            })
                            .transpose()?;
                        Ok(PlanQualitySample {
                            segment_id: row.get(0)?,
                            session_id: row.get(1)?,
                            plan_id: row.get(2)?,
                            plan_revision: sql_u64(row.get(3)?)?,
                            selected_branch_id: row.get(4)?,
                            executed_branch_id: row.get(5)?,
                            model_configuration_id: row.get(6)?,
                            profile_digest: row.get(7)?,
                            attribution,
                            first_turn_id: row.get(9)?,
                            first_turn_ordinal: sql_u64(row.get(10)?)?,
                            last_observed_turn_id: row.get(11)?,
                            last_observed_turn_ordinal: sql_u64(row.get(12)?)?,
                            first_at_ms: row.get(13)?,
                            last_at_ms: row.get(14)?,
                            history_partial: row.get(15)?,
                            first_request_id: row.get(16)?,
                            last_request_id: row.get(17)?,
                            execution_evidence_available: row.get(18)?,
                            task_id: row.get(19)?,
                            run_id: row.get(20)?,
                            assessment,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let more = rows.len() > usize::from(query.limit);
        rows.truncate(usize::from(query.limit));
        let next_cursor = if more {
            let last = rows.last().ok_or(ObservationV2Error::Unavailable)?;
            Some(self.encode_observation_cursor(&Cursor {
                last_at_ms: last.last_at_ms,
                last_segment_id: last.segment_id.clone(),
                ..cursor
            })?)
        } else {
            None
        };
        transaction.commit()?;
        let current: u64 = connection.query_row(
            "SELECT CAST(value AS INTEGER) FROM observation_meta
             WHERE key='plan_quality_generation'",
            [],
            |row| row.get(0),
        )?;
        if current != generation {
            return Err(ObservationV2Error::Stale);
        }
        reader.check(now_ms, false, false)?;
        Ok(PlanQualitySamplesPage {
            samples: rows,
            next_cursor,
        })
    }
}

fn validate(query: &PlanQualitySamplesQuery) -> Result<(), ObservationV2Error> {
    if query.plan_id.is_none() && query.session_id.is_none()
        || query.limit == 0
        || query.limit > 200
        || query.plan_revision == Some(0)
        || query.from_ms.is_some_and(|value| value < 0)
        || query.to_ms.is_some_and(|value| value < 0)
        || query
            .from_ms
            .zip(query.to_ms)
            .is_some_and(|(from, to)| from >= to)
        || [
            &query.plan_id,
            &query.session_id,
            &query.segment_id,
            &query.model_configuration_id,
        ]
        .into_iter()
        .flatten()
        .any(|value| !identifier(value))
        || [query.score_gt, query.score_lt]
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        || query
            .score_gt
            .zip(query.score_lt)
            .is_some_and(|(gt, lt)| gt >= lt)
        || query
            .plan_revision
            .is_some_and(|value| i64::try_from(value).is_err())
    {
        Err(ObservationV2Error::Invalid)
    } else {
        Ok(())
    }
}

fn sql_u64(value: i64) -> rusqlite::Result<u64> {
    value.try_into().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}
