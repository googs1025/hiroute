use rusqlite::{OptionalExtension, Transaction};

use crate::LocalStorageError;

pub(super) const CONTROL_COMPUTE_MANAGEMENT: &str =
    "CREATE TABLE IF NOT EXISTS compute_management_sources (
        workspace_id TEXT NOT NULL,
        source_id TEXT NOT NULL,
        lineage_digest TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK(revision > 0),
        source_json TEXT NOT NULL,
        owner_operation_id TEXT NOT NULL REFERENCES operations(operation_id),
        updated_at INTEGER NOT NULL,
        PRIMARY KEY(workspace_id, source_id),
        UNIQUE(workspace_id, lineage_digest)
     );
     CREATE INDEX IF NOT EXISTS compute_management_sources_updated_idx
        ON compute_management_sources(workspace_id, updated_at, source_id);
     CREATE TABLE IF NOT EXISTS compute_management_effects (
        operation_id TEXT PRIMARY KEY REFERENCES operations(operation_id),
        workspace_id TEXT NOT NULL,
        source_id TEXT NOT NULL,
        expected_revision INTEGER NOT NULL CHECK(expected_revision >= 0),
        before_owner_operation_id TEXT,
        desired_digest TEXT NOT NULL
     );";

pub(super) const SECRETS_V14_COLLABORATION_PREPARATION: &str =
    "ALTER TABLE agent_collaboration_credentials ADD COLUMN prepared_grant_json TEXT;";

pub(super) fn ensure_collaboration_preparation_column(
    transaction: &Transaction<'_>,
) -> Result<(), LocalStorageError> {
    let prepared_grant_present = transaction
        .query_row(
            "SELECT 1 FROM pragma_table_info('agent_collaboration_credentials')
             WHERE name = 'prepared_grant_json'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !prepared_grant_present {
        transaction.execute_batch(SECRETS_V14_COLLABORATION_PREPARATION)?;
    }
    Ok(())
}
