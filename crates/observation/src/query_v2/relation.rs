use super::*;
use crate::LocalObservationStore;
use rusqlite::{Connection, OptionalExtension, params};

impl LocalObservationStore {
    /// Mark the exact request selected by the native live-check receipt verifier.
    /// Callers cannot use this as a general traffic classifier.
    pub fn mark_observed_connectivity_probe(
        &self,
        workspace: &hiroute_domain::WorkspaceId,
        request: &LogicalRequestId,
    ) -> Result<(), ObservationV2Error> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT traffic_kind FROM logical_requests
                 WHERE workspace_id=?1 AND request_id=?2",
                params![workspace.as_str(), request.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(ObservationV2Error::Unavailable);
        };
        if !matches!(
            current.as_str(),
            "unknown" | "normal" | "connectivity_probe"
        ) {
            return Err(ObservationV2Error::Unavailable);
        }
        if current != "connectivity_probe" {
            transaction.execute(
                "UPDATE logical_requests SET traffic_kind='connectivity_probe'
                 WHERE workspace_id=?1 AND request_id=?2",
                params![workspace.as_str(), request.as_str()],
            )?;
            super::invalidate_visibility(&transaction)?;
            transaction.execute(
                "UPDATE observation_meta SET value=CAST(value AS INTEGER)+1
                 WHERE key='store_revision'",
                [],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Persist only after Gateway has verified admission. This does not validate
    /// a run token, launch a task, or infer an association from matching metadata.
    pub fn link_observed_request(
        &self,
        link: &RunObservationLink,
    ) -> Result<(), ObservationV2Error> {
        for value in [
            &link.task_id,
            &link.run_id,
            &link.producer_epoch,
            &link.source_event_id,
            &link.plan_id,
            &link.plan_revision,
            &link.publication_ref,
            &link.harness_id,
            &link.protocol_kind,
        ] {
            if !identifier(value) {
                return Err(ObservationV2Error::Invalid);
            }
        }
        for value in [
            &link.native_session_id,
            &link.native_turn_id,
            &link.parent_context_ref,
            &link.continued_from_run_id,
        ]
        .into_iter()
        .flatten()
        {
            if !identifier(value) {
                return Err(ObservationV2Error::Invalid);
            }
        }
        let body = serde_json::to_string(link).map_err(|_| ObservationV2Error::Invalid)?;
        let digest =
            hiroute_domain::CanonicalDigest::of(link).map_err(|_| ObservationV2Error::Invalid)?;
        let mut semantic = serde_json::to_value(link).map_err(|_| ObservationV2Error::Invalid)?;
        let object = semantic
            .as_object_mut()
            .ok_or(ObservationV2Error::Invalid)?;
        object.remove("producer_epoch");
        object.remove("source_event_id");
        let relation_digest = hiroute_domain::CanonicalDigest::of(&semantic)
            .map_err(|_| ObservationV2Error::Invalid)?;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let previous:Option<(String,String)>=transaction.query_row(
            "SELECT request_id,body_digest FROM observation_link_events WHERE workspace_id=?1 AND producer_epoch=?2 AND event_id=?3",
            params![link.workspace_id.as_str(),link.producer_epoch,link.source_event_id], |row|Ok((row.get(0)?,row.get(1)?)),
        ).optional()?;
        let existing:Option<String>=transaction.query_row(
            "SELECT body_digest FROM observation_run_links WHERE workspace_id=?1 AND request_id=?2",
            params![link.workspace_id.as_str(),link.request_id.as_str()],|row|row.get(0),
        ).optional()?;
        let conflict = previous
            .as_ref()
            .is_some_and(|(_, old)| old != digest.as_str())
            || existing
                .as_ref()
                .is_some_and(|old| old != relation_digest.as_str());
        transaction.execute(
            "INSERT OR IGNORE INTO observation_run_links(workspace_id,request_id,run_id,body_json,body_digest) VALUES(?1,?2,?3,?4,?5)",
            params![link.workspace_id.as_str(),link.request_id.as_str(),link.run_id,body,relation_digest.as_str()],
        )?;
        if conflict {
            transaction.execute(
                "UPDATE observation_run_links SET conflicted=1 WHERE workspace_id=?1 AND (request_id=?2 OR request_id=?3)",
                params![link.workspace_id.as_str(),link.request_id.as_str(),previous.as_ref().map(|(request,_)|request.as_str())],
            )?;
            // A conflicting association can no longer prove normal delegated traffic. Keep
            // both affected requests visible, but remove them from trusted value totals.
            transaction.execute(
                "UPDATE logical_requests SET traffic_kind='unknown' WHERE workspace_id=?1 AND (request_id=?2 OR request_id=?3)",
                params![link.workspace_id.as_str(),link.request_id.as_str(),previous.as_ref().map(|(request,_)|request.as_str())],
            )?;
        } else {
            transaction.execute(
                "INSERT OR IGNORE INTO observation_link_events(workspace_id,producer_epoch,event_id,request_id,body_digest) VALUES(?1,?2,?3,?4,?5)",
                params![link.workspace_id.as_str(),link.producer_epoch,link.source_event_id,link.request_id.as_str(),digest.as_str()],
            )?;
            // This port is callable only after Gateway has verified delegated-run admission.
            // That authority is the normal-traffic classifier; connectivity probes never own a
            // RunObservationLink and therefore remain unclassified.
            transaction.execute(
                "UPDATE logical_requests SET traffic_kind='normal' WHERE workspace_id=?1 AND request_id=?2 AND traffic_kind='unknown'",
                params![link.workspace_id.as_str(),link.request_id.as_str()],
            )?;
        }
        if existing.is_none() || conflict {
            super::invalidate_visibility(&transaction)?;
            transaction.execute("UPDATE observation_meta SET value=CAST(value AS INTEGER)+1 WHERE key='store_revision'",[])?;
        }
        transaction.commit()?;
        if conflict {
            Err(ObservationV2Error::RelationshipConflict)
        } else {
            Ok(())
        }
    }

    pub fn observed_request_link(
        &self,
        reader: &ObservationReaderContext,
        request: &LogicalRequestId,
        now_ms: i64,
    ) -> Result<Option<RunObservationLink>, ObservationV2Error> {
        reader.check(now_ms, false, false)?;
        let _permit = self.query_permit()?;
        let connection = Connection::open_with_flags(
            &self.activity_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let _deadline = super::QueryDeadline::start_for(&connection, reader)?;
        authorized_link(&connection, reader, request, now_ms)
    }
}

pub(super) fn authorized_link(
    connection: &Connection,
    reader: &ObservationReaderContext,
    request: &LogicalRequestId,
    now_ms: i64,
) -> Result<Option<RunObservationLink>, ObservationV2Error> {
    let row: Option<(String, bool)> = connection
        .query_row(
            "SELECT l.body_json,l.conflicted FROM observation_run_links l JOIN logical_requests r
           ON r.workspace_id=l.workspace_id AND r.request_id=l.request_id
         WHERE l.workspace_id=?1 AND l.request_id=?2 AND r.started_at_ms>?3",
            params![
                reader.workspace().as_str(),
                request.as_str(),
                now_ms.saturating_sub(crate::managed_text::RETENTION_MS)
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((body, conflicted)) = row else {
        return if reader.allowed_runs().is_some() {
            Err(ObservationV2Error::Unauthorized)
        } else {
            Ok(None)
        };
    };
    let link: RunObservationLink =
        serde_json::from_str(&body).map_err(|_| ObservationV2Error::Unavailable)?;
    if reader
        .allowed_runs()
        .is_some_and(|runs| !runs.contains(&link.run_id))
    {
        return Err(ObservationV2Error::Unauthorized);
    }
    if conflicted {
        return Err(ObservationV2Error::RelationshipConflict);
    }
    Ok(Some(link))
}
