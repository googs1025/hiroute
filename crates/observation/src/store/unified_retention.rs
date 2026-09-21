use super::*;
use hiroute_domain::*;
use rusqlite::{OptionalExtension, params};

impl LocalObservationStore {
    pub fn preview_session_deletion(
        &self,
        principal: &ObservationPrincipalV1,
        spec: &SessionDeletionSpecV1,
    ) -> Result<SessionDeletionPreviewV1, ObservationQueryError> {
        let preview = self.preview_session_deletion_base(principal, spec)?;
        if !self.related_managed_scopes(spec, i64::MAX)?.is_empty() {
            return Err(ObservationQueryError::InvalidQuery);
        }
        Ok(preview)
    }
    pub fn preview_session_deletion_v2(
        &self,
        principal: &ObservationPrincipalV1,
        spec: &SessionDeletionSpecV1,
        through_ms: i64,
    ) -> Result<SessionDeletionPreviewV2, ObservationQueryError> {
        if through_ms < 0 {
            return Err(ObservationQueryError::InvalidQuery);
        }
        let session = self.preview_session_deletion_base(principal, spec)?;
        let managed_scopes = self.related_managed_scopes(spec, through_ms)?;
        let revision = store_revision(&self.connection.lock())
            .map_err(|_| ObservationQueryError::Unavailable)?;
        if revision != session.store_revision {
            return Err(ObservationQueryError::StalePreview);
        }
        let change_digest = CanonicalDigest::of(&(
            "unified-session-delete/v2",
            &session,
            through_ms,
            &managed_scopes,
        ))
        .map_err(|_| ObservationQueryError::Corrupt)?;
        Ok(SessionDeletionPreviewV2 {
            session,
            through_ms,
            managed_scopes,
            change_digest,
        })
    }
    pub fn apply_session_deletion_v2(
        &self,
        principal: &ObservationPrincipalV1,
        preview: &SessionDeletionPreviewV2,
        accepted_digest: &CanonicalDigest,
        now_ms: i64,
    ) -> Result<SessionDeletionOutcomeV2, ObservationQueryError> {
        if preview.through_ms > now_ms || accepted_digest != &preview.change_digest {
            return Err(ObservationQueryError::StalePreview);
        }
        super::retention::authorize_retention(principal, &preview.session.spec)?;
        let digest = CanonicalDigest::of(&(
            "unified-session-delete/v2",
            &preview.session,
            preview.through_ms,
            &preview.managed_scopes,
        ))
        .map_err(|_| ObservationQueryError::Corrupt)?;
        if digest != preview.change_digest {
            return Err(ObservationQueryError::StalePreview);
        }
        let previous: Option<String> = self
            .connection
            .lock()
            .query_row(
                "SELECT outcome_json FROM observation_delete_jobs_v2 WHERE digest=?1",
                [digest.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| ObservationQueryError::Unavailable)?;
        let session = if let Some(body) = previous {
            serde_json::from_str(&body).map_err(|_| ObservationQueryError::Corrupt)?
        } else {
            let current = self.preview_session_deletion_v2(
                principal,
                &preview.session.spec,
                preview.through_ms,
            )?;
            if &current != preview {
                return Err(ObservationQueryError::StalePreview);
            }
            self.apply_session_deletion_with_managed(
                principal,
                &preview.session.spec,
                preview.session.store_revision,
                &preview.session.change_digest,
                now_ms,
                Some(preview),
            )?
        };
        let mut native = false;
        let mut objects = false;
        let db = self.connection.lock();
        for scope in &preview.managed_scopes {
            let key = serde_json::to_string(&crate::managed_text::ManagedTextScope {
                workspace_id: scope.workspace_id.clone(),
                task_id: scope.task_id.clone(),
                run_id: scope.run_id.clone(),
            })
            .map_err(|_| ObservationQueryError::Corrupt)?;
            native|=db.query_row("SELECT EXISTS(SELECT 1 FROM managed_text_delete_jobs WHERE scope=?1 AND native_gc_pending=1)",[&key],|r|r.get::<_,bool>(0)).map_err(|_|ObservationQueryError::Unavailable)?;
            objects|=db.query_row("SELECT EXISTS(SELECT 1 FROM managed_text_chunks c JOIN managed_text_refs r ON r.id=c.ref_id WHERE r.scope=?1 AND r.state='deleted')",[&key],|r|r.get::<_,bool>(0)).map_err(|_|ObservationQueryError::Unavailable)?;
        }
        let object_cleanup_pending = db.query_row("SELECT EXISTS(SELECT 1 FROM content_blobs_v2 b WHERE NOT EXISTS(SELECT 1 FROM content_instances_v2 c WHERE c.workspace_id=b.workspace_id AND c.content_blob_digest=b.blob_digest AND c.state='complete'))", [], |r| r.get(0)).map_err(|_|ObservationQueryError::Unavailable)?;
        Ok(SessionDeletionOutcomeV2 {
            session,
            managed_native_cleanup_pending: native,
            managed_object_cleanup_pending: objects,
            object_cleanup_pending,
        })
    }
    fn related_managed_scopes(
        &self,
        spec: &SessionDeletionSpecV1,
        through: i64,
    ) -> Result<Vec<ManagedScopeDeletionV2>, ObservationQueryError> {
        let db = self.connection.lock();
        // Scope keys are typed JSON from ManagedTextScope, never caller SQL.
        let mut stmt=db.prepare("SELECT m.scope,m.generation,COUNT(r.id) FROM managed_text_scopes m JOIN managed_text_refs r ON r.scope=m.scope AND r.state!='deleted' AND r.created_ms<=?3 WHERE json_extract(m.scope,'$.workspace_id')=?1 AND EXISTS(SELECT 1 FROM observation_run_links l JOIN logical_requests q ON q.workspace_id=l.workspace_id AND q.request_id=l.request_id WHERE q.workspace_id=?1 AND q.session_id=?2 AND l.conflicted=0 AND l.run_id=json_extract(m.scope,'$.run_id') AND json_extract(l.body_json,'$.task_id')=json_extract(m.scope,'$.task_id')) GROUP BY m.scope,m.generation ORDER BY m.scope LIMIT 201").map_err(|_|ObservationQueryError::Unavailable)?;
        let rows = stmt
            .query_map(
                params![
                    spec.workspace_id.as_str(),
                    spec.session_id.as_str(),
                    through
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u64>(1)?,
                        r.get::<_, u64>(2)?,
                    ))
                },
            )
            .map_err(|_| ObservationQueryError::Unavailable)?;
        let mut scopes = Vec::new();
        for row in rows {
            let (key, generation, count) = row.map_err(|_| ObservationQueryError::Corrupt)?;
            if scopes.len() == 200 {
                return Err(ObservationQueryError::InvalidQuery);
            }
            let scope: crate::managed_text::ManagedTextScope =
                serde_json::from_str(&key).map_err(|_| ObservationQueryError::Corrupt)?;
            scopes.push(ManagedScopeDeletionV2 {
                workspace_id: scope.workspace_id,
                task_id: scope.task_id,
                run_id: scope.run_id,
                visibility_generation: generation,
                reference_count: count,
            });
        }
        Ok(scopes)
    }
}
