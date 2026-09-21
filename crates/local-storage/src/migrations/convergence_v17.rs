use rusqlite::{OptionalExtension, Transaction};

use super::DatabaseKind;
use crate::LocalStorageError;

const WORKER_AUTHORIZATION_FORMAT: &str = r#"
CREATE TABLE IF NOT EXISTS worker_authorization_format (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    format_version INTEGER NOT NULL CHECK(format_version = 1)
);
INSERT OR IGNORE INTO worker_authorization_format(singleton, format_version) VALUES(1, 1);
"#;

pub(super) fn converge_worker_authorization_format(
    transaction: &Transaction<'_>,
) -> Result<(), LocalStorageError> {
    transaction.execute_batch(WORKER_AUTHORIZATION_FORMAT)?;
    validate_worker_authorization_format(transaction)
}

pub(super) fn converge_subscription_validation(
    transaction: &Transaction<'_>,
) -> Result<(), LocalStorageError> {
    transaction.execute_batch(super::subscription_v16::CONTROL)?;
    validate_subscription_validation(transaction)
}

pub(super) fn validate_current(
    transaction: &Transaction<'_>,
    kind: DatabaseKind,
) -> Result<(), LocalStorageError> {
    validate_worker_authorization_format(transaction)?;
    if kind == DatabaseKind::Control {
        validate_subscription_validation(transaction)?;
    }
    Ok(())
}

fn validate_worker_authorization_format(
    transaction: &Transaction<'_>,
) -> Result<(), LocalStorageError> {
    validate_columns(
        transaction,
        "worker_authorization_format",
        &[
            ("singleton", "INTEGER", false, 1),
            ("format_version", "INTEGER", true, 0),
        ],
    )?;
    let marker = transaction
        .query_row(
            "SELECT format_version FROM worker_authorization_format WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let count: i64 = transaction.query_row(
        "SELECT count(*) FROM worker_authorization_format",
        [],
        |row| row.get(0),
    )?;
    if marker != Some(1) || count != 1 {
        return Err(LocalStorageError::InvalidData);
    }
    Ok(())
}

fn validate_subscription_validation(
    transaction: &Transaction<'_>,
) -> Result<(), LocalStorageError> {
    validate_columns(
        transaction,
        "compute_subscription_validations",
        &[
            ("operation_id", "TEXT", false, 1),
            ("candidate_ref", "TEXT", true, 0),
            ("candidate_revision", "INTEGER", true, 0),
            ("record_json", "TEXT", true, 0),
            ("state", "TEXT", true, 0),
            ("save_operation_id", "TEXT", false, 0),
            ("updated_at", "INTEGER", true, 0),
        ],
    )?;
    let foreign_key: Option<(String, String, String)> = transaction
        .query_row(
            "SELECT \"table\", \"from\", \"to\"
             FROM pragma_foreign_key_list('compute_subscription_validations')
             WHERE \"from\" = 'operation_id'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if foreign_key
        != Some((
            "operations".to_owned(),
            "operation_id".to_owned(),
            "operation_id".to_owned(),
        ))
    {
        return Err(LocalStorageError::InvalidData);
    }
    let index_columns = pragma_index_columns(
        transaction,
        "compute_subscription_validations_candidate_idx",
    )?;
    if index_columns != ["candidate_ref", "candidate_revision", "updated_at"] {
        return Err(LocalStorageError::InvalidData);
    }
    Ok(())
}

fn validate_columns(
    transaction: &Transaction<'_>,
    table: &str,
    expected: &[(&str, &str, bool, i64)],
) -> Result<(), LocalStorageError> {
    let mut statement = transaction.prepare(&format!("PRAGMA table_info('{table}')"))?;
    let actual = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if actual.len() != expected.len()
        || actual.iter().zip(expected).any(|(actual, expected)| {
            actual.0 != expected.0
                || !actual.1.eq_ignore_ascii_case(expected.1)
                || actual.2 != expected.2
                || actual.3 != expected.3
        })
    {
        return Err(LocalStorageError::InvalidData);
    }
    Ok(())
}

fn pragma_index_columns(
    transaction: &Transaction<'_>,
    index: &str,
) -> Result<Vec<String>, LocalStorageError> {
    let mut statement = transaction.prepare(&format!("PRAGMA index_info('{index}')"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(2))?
        .collect::<Result<Vec<_>, _>>()?)
}
