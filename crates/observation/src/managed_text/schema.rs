use rusqlite::Connection;

use crate::writer::ObservationStoreError;

/// Branch-local current schema intent; the coordinator owns the integrated
/// activity schema number. MVP deliberately rejects older managed-text layouts instead of
/// carrying migration readers or dual schema paths.
pub(crate) fn migrate(transaction: &Connection) -> Result<(), ObservationStoreError> {
    let has_meta: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='managed_text_meta')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| ObservationStoreError::Corrupt)?;
    let version: Option<i64> = if has_meta {
        Some(
            transaction
                .query_row("SELECT version FROM managed_text_meta", [], |row| {
                    row.get(0)
                })
                .map_err(|_| ObservationStoreError::Corrupt)?,
        )
    } else {
        None
    };
    if version.is_some_and(|version| version != 3) {
        return Err(ObservationStoreError::Corrupt);
    }
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS managed_text_meta(
            version INTEGER NOT NULL CHECK(version=3));
         INSERT INTO managed_text_meta(version) SELECT 3
            WHERE NOT EXISTS(SELECT 1 FROM managed_text_meta);
         CREATE TABLE IF NOT EXISTS managed_text_scopes(
            scope TEXT PRIMARY KEY, generation INTEGER NOT NULL DEFAULT 0,
            deleted_through_ms INTEGER NOT NULL DEFAULT -1);
         CREATE TABLE IF NOT EXISTS managed_text_refs(
            id TEXT PRIMARY KEY, scope TEXT NOT NULL REFERENCES managed_text_scopes(scope),
            input_digest TEXT NOT NULL, created_ms INTEGER NOT NULL, deadline_ms INTEGER NOT NULL,
            state TEXT NOT NULL, chunk_count INTEGER NOT NULL DEFAULT 0,
            UNIQUE(scope, input_digest));
         CREATE INDEX IF NOT EXISTS managed_text_scope_time ON managed_text_refs(scope,created_ms,id);
         CREATE TABLE IF NOT EXISTS managed_text_chunks(
            ref_id TEXT NOT NULL REFERENCES managed_text_refs(id), ordinal INTEGER NOT NULL,
            digest TEXT NOT NULL, size INTEGER NOT NULL,
            ready INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(ref_id,ordinal));
         CREATE TABLE IF NOT EXISTS managed_text_delete_jobs(
            scope TEXT NOT NULL, generation INTEGER NOT NULL, through_ms INTEGER NOT NULL,
            hidden_count INTEGER NOT NULL, native_gc_pending INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY(scope,generation));
         CREATE TABLE IF NOT EXISTS managed_text_progress(
            scope TEXT PRIMARY KEY REFERENCES managed_text_scopes(scope),
            created_ms INTEGER NOT NULL CHECK(created_ms>=0),
            segment INTEGER NOT NULL CHECK(segment>=0),
            head INTEGER NOT NULL CHECK(head>=0),
            end INTEGER NOT NULL CHECK(end>=head));
         CREATE TABLE IF NOT EXISTS managed_text_progress_blocks(
            scope TEXT NOT NULL REFERENCES managed_text_progress(scope),
            segment INTEGER NOT NULL CHECK(segment>=0),
            start INTEGER NOT NULL CHECK(start>=0),
            end INTEGER NOT NULL CHECK(end>start),
            bytes BLOB NOT NULL CHECK(length(bytes)>0 AND length(bytes)<=65536),
            digest TEXT NOT NULL,
            order_seq INTEGER NOT NULL UNIQUE CHECK(order_seq>=0),
            CHECK(end-start=length(bytes)),
            PRIMARY KEY(scope,segment,start));
         CREATE INDEX IF NOT EXISTS managed_text_progress_oldest
            ON managed_text_progress_blocks(order_seq);
         CREATE TABLE IF NOT EXISTS managed_text_progress_meta(
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            next_order_seq INTEGER NOT NULL CHECK(next_order_seq>=0));
         INSERT INTO managed_text_progress_meta(singleton,next_order_seq) SELECT 1,0
            WHERE NOT EXISTS(SELECT 1 FROM managed_text_progress_meta);",
    ).map_err(|_| ObservationStoreError::ActivityUnavailable)?;
    Ok(())
}
