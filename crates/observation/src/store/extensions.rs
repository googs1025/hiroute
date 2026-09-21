//! One atomic extension migration. The branch-local version is independent of
//! the coordinator's activity schema number; unknown future data fails closed.
use crate::writer::ObservationStoreError as Error;
use rusqlite::{Connection, OptionalExtension};

pub(super) fn migrate(connection: &mut Connection) -> Result<(), Error> {
    let tx = connection
        .transaction()
        .map_err(|_| Error::ActivityUnavailable)?;
    let version: Option<String> = tx
        .query_row(
            "SELECT value FROM observation_meta WHERE key='observation_extension_version'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Corrupt)?;
    if version
        .as_deref()
        .is_some_and(|version| version != "1" && version != "2" && version != "3" && version != "4")
    {
        return Err(Error::Corrupt);
    }
    crate::managed_text::migrate(&tx)?;
    crate::query_v2::migrate(&tx)?;
    crate::text_index::migrate(&tx)?;
    super::sensitive::migrate(&tx)?;
    crate::valuation::migrate(&tx)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS observation_delete_jobs_v2(digest TEXT PRIMARY KEY,outcome_json TEXT NOT NULL); CREATE TABLE IF NOT EXISTS observation_session_barriers_v2(workspace_id TEXT NOT NULL,session_id TEXT NOT NULL,through_ms INTEGER NOT NULL,PRIMARY KEY(workspace_id,session_id));").map_err(|_|Error::ActivityUnavailable)?;
    if !matches!(version.as_deref(), Some("3" | "4")) {
        // Preserve pre-extension deletion watermarks across upgrade. Existing
        // retained facts receive request barriers before sessions may revive.
        tx.execute_batch("INSERT INTO observation_session_barriers_v2(workspace_id,session_id,through_ms) SELECT workspace_id,session_id,MAX(deleted_at_ms) FROM observation_tombstones GROUP BY workspace_id,session_id ON CONFLICT(workspace_id,session_id) DO UPDATE SET through_ms=MAX(through_ms,excluded.through_ms); INSERT OR IGNORE INTO observation_request_tombstones_v2(workspace_id,request_id,deleted_ms) SELECT r.workspace_id,r.request_id,b.through_ms FROM logical_requests r JOIN observation_session_barriers_v2 b ON b.workspace_id=r.workspace_id AND b.session_id=r.session_id WHERE r.started_at_ms<=b.through_ms;").map_err(|_|Error::ActivityUnavailable)?;
    }
    tx.execute("INSERT OR REPLACE INTO observation_meta(key,value) VALUES('observation_extension_version','4')",[]).map_err(|_|Error::ActivityUnavailable)?;
    tx.commit().map_err(|_| Error::ActivityUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_future_extension_and_failed_migration_preserve_old_state() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE observation_meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO observation_meta VALUES('observation_extension_version','999');").unwrap();
        assert!(migrate(&mut db).is_err());
        let count: u32 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='managed_text_meta'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        db.execute_batch("DELETE FROM observation_meta; CREATE TABLE managed_text_meta(version INTEGER); INSERT INTO managed_text_meta VALUES(999);").unwrap();
        assert!(migrate(&mut db).is_err());
        let marker: Option<String> = db
            .query_row(
                "SELECT value FROM observation_meta WHERE key='observation_extension_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert!(marker.is_none());

        db.execute("UPDATE managed_text_meta SET version=2", [])
            .unwrap();
        assert!(migrate(&mut db).is_err());
        let progress_tables: u32 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='managed_text_progress'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(progress_tables, 0);
    }
}
