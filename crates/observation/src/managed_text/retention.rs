use rusqlite::{OptionalExtension, params};

use super::storage::barrier;
use super::*;
use crate::LocalObservationStore;

impl LocalObservationStore {
    /// Discover expired scopes from durable state after restart. Native cleanup
    /// must be journaled even when nobody attempts to read the expired reference.
    pub fn managed_text_expire_pending(
        &self,
        now_ms: i64,
        limit: usize,
    ) -> Result<usize, ManagedTextError> {
        if now_ms < 0 || limit == 0 || limit > 200 {
            return Err(ManagedTextError::Invalid);
        }
        let scopes = {
            let connection = self.connection.lock();
            let mut statement = connection.prepare(
                "SELECT DISTINCT scope FROM managed_text_refs WHERE deadline_ms<=?1 AND state!='deleted' ORDER BY scope LIMIT ?2"
            )?;
            statement
                .query_map(params![now_ms, limit], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut expired = 0;
        for encoded in scopes {
            let scope: ManagedTextScope =
                serde_json::from_str(&encoded).map_err(|_| ManagedTextError::Storage)?;
            match self.managed_text_expire_scope(&scope, now_ms) {
                Ok(Some(_)) => expired += 1,
                Ok(None) | Err(ManagedTextError::Stale) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(expired)
    }
    /// The retention worker schedules exact task scopes. Expiry uses the same
    /// durable native-cache cleanup protocol as an explicit deletion.
    pub fn managed_text_expire_scope(
        &self,
        scope: &ManagedTextScope,
        now_ms: i64,
    ) -> Result<Option<ManagedTextDeleteResult>, ManagedTextError> {
        let Some(through_ms) = now_ms.checked_sub(RETENTION_MS).filter(|value| *value >= 0) else {
            return Ok(None);
        };
        let preview = self.managed_text_delete_preview(scope, through_ms, now_ms)?;
        if preview.reference_count == 0 {
            return Ok(None);
        }
        self.managed_text_delete_apply(&preview).map(Some)
    }

    /// Recovery polling for the trusted task service. Notifications are an
    /// optimization; unacknowledged work persists through either service restart.
    pub fn managed_text_pending_native_cleanup(
        &self,
        scope: &ManagedTextScope,
        after_generation: u64,
        limit: usize,
    ) -> Result<Vec<ManagedTextNativeCleanup>, ManagedTextError> {
        if limit == 0 || limit > 200 || after_generation > i64::MAX as u64 {
            return Err(ManagedTextError::Invalid);
        }
        let key = scope.key()?;
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT generation,through_ms FROM managed_text_delete_jobs
             WHERE scope=?1 AND generation>?2 AND native_gc_pending=1 ORDER BY generation LIMIT ?3",
        )?;
        let jobs = statement
            .query_map(params![key, after_generation, limit], |row| {
                Ok(ManagedTextNativeCleanup {
                    scope: scope.clone(),
                    visibility_generation: row.get(0)?,
                    through_ms: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(jobs)
    }

    /// Instance-wide bounded recovery page.  `(scope,generation)` is the existing durable key;
    /// generations from different scopes are never compared as a global sequence.
    pub fn managed_text_pending_native_cleanup_page(
        &self,
        after: Option<&ManagedTextNativeCleanupCursor>,
        limit: usize,
    ) -> Result<Vec<ManagedTextNativeCleanup>, ManagedTextError> {
        if limit == 0 || limit > 200 {
            return Err(ManagedTextError::Invalid);
        }
        let (after_scope, after_generation) = match after {
            Some(cursor) if cursor.visibility_generation <= i64::MAX as u64 => {
                (Some(cursor.scope.key()?), cursor.visibility_generation)
            }
            Some(_) => return Err(ManagedTextError::Invalid),
            None => (None, 0),
        };
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT scope,generation,through_ms FROM managed_text_delete_jobs
             WHERE native_gc_pending=1
               AND (?1 IS NULL OR scope>?1 OR (scope=?1 AND generation>?2))
             ORDER BY scope,generation LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![after_scope.as_deref(), after_generation, limit],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        let mut jobs = Vec::new();
        for row in rows {
            let (scope, visibility_generation, through_ms) = row?;
            jobs.push(ManagedTextNativeCleanup {
                scope: serde_json::from_str(&scope).map_err(|_| ManagedTextError::Storage)?,
                visibility_generation,
                through_ms,
            });
        }
        Ok(jobs)
    }

    pub fn managed_text_scope_cleanup_state(
        &self,
        scope: &ManagedTextScope,
        now_ms: i64,
    ) -> Result<Option<ManagedTextScopeCleanupState>, ManagedTextError> {
        if now_ms < 0 {
            return Err(ManagedTextError::Invalid);
        }
        let key = scope.key()?;
        let connection = self.connection.lock();
        let scope_row: Option<(u64, i64)> = connection
            .query_row(
                "SELECT generation,deleted_through_ms FROM managed_text_scopes WHERE scope=?1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((visibility_generation, deleted_through_ms)) = scope_row else {
            return Ok(None);
        };
        let (reference_count, visible_reference_count, latest_created_ms) = connection.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN state!='deleted' AND deadline_ms>?2 THEN 1 ELSE 0 END),0),
                    MAX(created_ms)
             FROM managed_text_refs WHERE scope=?1",
            params![key, now_ms],
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )?;
        Ok(Some(ManagedTextScopeCleanupState {
            visibility_generation,
            deleted_through_ms,
            reference_count,
            visible_reference_count,
            latest_created_ms,
        }))
    }

    pub fn managed_text_scope_contains_refs(
        &self,
        scope: &ManagedTextScope,
        opaque_ids: &[String],
    ) -> Result<bool, ManagedTextError> {
        if opaque_ids.is_empty()
            || opaque_ids.len() > 32
            || opaque_ids.iter().any(|id| !valid_id(id))
            || opaque_ids
                .iter()
                .enumerate()
                .any(|(index, id)| opaque_ids[..index].contains(id))
        {
            return Err(ManagedTextError::Invalid);
        }
        let key = scope.key()?;
        let connection = self.connection.lock();
        for id in opaque_ids {
            let present: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM managed_text_refs WHERE scope=?1 AND id=?2)",
                params![key, id],
                |row| row.get(0),
            )?;
            if !present {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Internal operation: the application owns the distinct management grant
    /// and exact confirmation, including its operation idempotency identity.
    pub fn managed_text_delete_preview(
        &self,
        scope: &ManagedTextScope,
        through_ms: i64,
        now_ms: i64,
    ) -> Result<ManagedTextDeletePreview, ManagedTextError> {
        if through_ms < 0 || through_ms > now_ms {
            return Err(ManagedTextError::Invalid);
        }
        let key = scope.key()?;
        let connection = self.connection.lock();
        let generation = connection
            .query_row(
                "SELECT generation FROM managed_text_scopes WHERE scope=?1",
                [&key],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let count = connection.query_row(
            "SELECT count(*) FROM managed_text_refs WHERE scope=?1 AND created_ms<=?2 AND state!='deleted'",
            params![key,through_ms], |row| row.get(0),
        )?;
        Ok(ManagedTextDeletePreview {
            scope: scope.clone(),
            through_ms,
            visibility_generation: generation,
            reference_count: count,
        })
    }

    pub fn managed_text_delete_apply(
        &self,
        preview: &ManagedTextDeletePreview,
    ) -> Result<ManagedTextDeleteResult, ManagedTextError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let result = apply_managed_delete(&transaction, preview)?;
        transaction.commit()?;
        Ok(result)
    }

    /// Bounded resumable GC. Tombstones remain so delayed writes cannot resurrect
    /// old events. Physical failure leaves the chunk queued for the next pass.
    pub fn managed_text_gc(&self, now_ms: i64, limit: usize) -> Result<usize, ManagedTextError> {
        if limit == 0 || limit > 200 {
            return Err(ManagedTextError::Invalid);
        }
        let connection = self.connection.lock();
        let chunks = {
            let mut statement = connection.prepare(
                "SELECT c.ref_id,c.ordinal FROM managed_text_chunks c
                 JOIN managed_text_refs r ON r.id=c.ref_id
                 WHERE r.state='deleted' OR r.deadline_ms<=?1 ORDER BY c.ref_id,c.ordinal LIMIT ?2",
            )?;
            statement
                .query_map(params![now_ms, limit], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut removed = 0;
        for (id, ordinal) in chunks {
            match std::fs::remove_file(self.managed_chunk_path(&id, ordinal)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => continue,
            }
            connection.execute(
                "DELETE FROM managed_text_chunks WHERE ref_id=?1 AND ordinal=?2",
                params![id, ordinal],
            )?;
            removed += 1;
        }
        Ok(removed)
    }

    pub fn managed_text_native_gc_ack(
        &self,
        scope: &ManagedTextScope,
        generation: u64,
    ) -> Result<(), ManagedTextError> {
        let key = scope.key()?;
        let connection = self.connection.lock();
        let changed = connection.execute(
            "UPDATE managed_text_delete_jobs SET native_gc_pending=0 WHERE scope=?1 AND generation=?2",
            params![key,generation],
        )?;
        if changed == 0 {
            return Err(ManagedTextError::Unavailable);
        }
        Ok(())
    }

    pub fn managed_text_native_gc_ack_exact(
        &self,
        job: &ManagedTextNativeCleanup,
    ) -> Result<(), ManagedTextError> {
        let key = job.scope.key()?;
        let connection = self.connection.lock();
        let changed = connection.execute(
            "UPDATE managed_text_delete_jobs SET native_gc_pending=0
             WHERE scope=?1 AND generation=?2 AND through_ms=?3 AND native_gc_pending=1",
            params![key, job.visibility_generation, job.through_ms],
        )?;
        if changed == 1 {
            return Ok(());
        }
        let current: Option<(i64, bool)> = connection
            .query_row(
                "SELECT through_ms,native_gc_pending FROM managed_text_delete_jobs
                 WHERE scope=?1 AND generation=?2",
                params![key, job.visibility_generation],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if current == Some((job.through_ms, false)) {
            Ok(())
        } else {
            Err(ManagedTextError::Unavailable)
        }
    }
}

fn has_chunks(
    connection: &rusqlite::Connection,
    key: &str,
    through: i64,
) -> Result<bool, ManagedTextError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM managed_text_chunks c JOIN managed_text_refs r ON r.id=c.ref_id
         WHERE r.scope=?1 AND r.created_ms<=?2 AND r.state='deleted')",
        params![key,through], |row| row.get(0),
    )?)
}

pub(crate) fn apply_managed_delete(
    transaction: &rusqlite::Transaction<'_>,
    preview: &ManagedTextDeletePreview,
) -> Result<ManagedTextDeleteResult, ManagedTextError> {
    let key = preview.scope.key()?;
    let next = preview
        .visibility_generation
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or(ManagedTextError::Invalid)?;
    transaction.execute(
        "INSERT OR IGNORE INTO managed_text_scopes(scope) VALUES(?1)",
        [&key],
    )?;
    let prior: Option<(i64,u64,bool)> = transaction.query_row(
            "SELECT through_ms,hidden_count,native_gc_pending FROM managed_text_delete_jobs WHERE scope=?1 AND generation=?2",
            params![key,next], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
    if let Some((through, count, native_pending)) = prior {
        if through != preview.through_ms || count != preview.reference_count {
            return Err(ManagedTextError::Stale);
        }
        return Ok(ManagedTextDeleteResult {
            visibility_generation: next,
            logically_hidden: count,
            object_gc_pending: has_chunks(transaction, &key, preview.through_ms)?,
            native_gc_pending: native_pending,
        });
    }
    let (generation, cutoff) = barrier(transaction, &key)?;
    let count: u64 = transaction.query_row(
            "SELECT count(*) FROM managed_text_refs WHERE scope=?1 AND created_ms<=?2 AND state!='deleted'",
            params![key,preview.through_ms], |row| row.get(0),
        )?;
    if generation != preview.visibility_generation || count != preview.reference_count {
        return Err(ManagedTextError::Stale);
    }
    transaction.execute(
        "UPDATE managed_text_scopes SET generation=?2,deleted_through_ms=?3 WHERE scope=?1",
        params![key, next, cutoff.max(preview.through_ms)],
    )?;
    transaction.execute(
        "UPDATE managed_text_refs SET state='deleted' WHERE scope=?1 AND created_ms<=?2",
        params![key, preview.through_ms],
    )?;
    transaction.execute(
            "INSERT INTO managed_text_delete_jobs(scope,generation,through_ms,hidden_count) VALUES(?1,?2,?3,?4)",
            params![key,next,preview.through_ms,count],
        )?;
    let pending = has_chunks(transaction, &key, preview.through_ms)?;
    crate::store::increment_store_revision(transaction).map_err(|_| ManagedTextError::Storage)?;
    Ok(ManagedTextDeleteResult {
        visibility_generation: next,
        logically_hidden: count,
        object_gc_pending: pending,
        native_gc_pending: true,
    })
}
