//! Physical `activity.db` plus an external content-object directory.
mod backfill;
mod extensions;
mod unified_retention;

pub(crate) mod fact_log;
mod gap;
mod ingest;
mod protocol;
mod request_retention;
mod retention;
mod safe_facts;
mod schema;
pub(crate) mod sensitive;
#[cfg(test)]
pub(crate) mod tests;
mod value_rollup;

use std::fs;
use std::path::{Path, PathBuf};

use hiroute_domain::{ContentId, RetentionPolicyV1, WorkspaceId};
use parking_lot::Mutex;
use rusqlite::Connection;

use crate::DigestAuthority;
use crate::writer::ObservationStoreError;

pub struct LocalObservationStore {
    pub(crate) maintenance_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) index_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) maintenance_errors: std::sync::atomic::AtomicU64,
    pub(crate) query_workers: std::sync::atomic::AtomicUsize,
    pub(crate) connection: Mutex<Connection>,
    pub(crate) activity_path: PathBuf,
    pub(crate) staging_root: PathBuf,
    pub(crate) blob_root: PathBuf,
    pub(crate) authority: DigestAuthority,
    pub(crate) retention: RetentionPolicyV1,
}

impl LocalObservationStore {
    pub fn open(
        root: impl AsRef<Path>,
        authority: DigestAuthority,
    ) -> Result<Self, ObservationStoreError> {
        let root = root.as_ref();
        let content_root = root.join("conversation-content");
        let staging_root = content_root.join("staging");
        let blob_root = content_root.join("blobs");
        fs::create_dir_all(&staging_root).map_err(|_| ObservationStoreError::Io)?;
        fs::create_dir_all(&blob_root).map_err(|_| ObservationStoreError::Io)?;
        let activity_path = root.join("activity.db");
        let mut connection = Connection::open(&activity_path)
            .map_err(|_| ObservationStoreError::ActivityUnavailable)?;
        schema::migrate(&mut connection)?;
        extensions::migrate(&mut connection)?;
        Ok(Self {
            maintenance_running: Default::default(),
            index_running: Default::default(),
            maintenance_errors: std::sync::atomic::AtomicU64::new(0),
            query_workers: std::sync::atomic::AtomicUsize::new(0),
            connection: Mutex::new(connection),
            activity_path,
            staging_root,
            blob_root,
            authority,
            retention: RetentionPolicyV1::default(),
        })
    }

    pub fn retention_policy(&self) -> &RetentionPolicyV1 {
        &self.retention
    }

    pub(crate) fn staging_directory(
        &self,
        workspace_id: &WorkspaceId,
        content_id: &ContentId,
    ) -> PathBuf {
        self.staging_root.join(portable_hash(&format!(
            "{}\0{}",
            workspace_id.as_str(),
            content_id.as_str()
        )))
    }

    pub(crate) fn blob_path(&self, digest: &str) -> PathBuf {
        self.blob_root.join(portable_hash(digest))
    }
}

fn portable_hash(value: &str) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(value.as_bytes());
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

pub(crate) fn store_revision(connection: &Connection) -> Result<u64, ObservationStoreError> {
    let value: String = connection
        .query_row(
            "SELECT value FROM observation_meta WHERE key='store_revision'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| ObservationStoreError::Corrupt)?;
    value.parse().map_err(|_| ObservationStoreError::Corrupt)
}

pub(crate) fn increment_store_revision(
    connection: &Connection,
) -> Result<u64, ObservationStoreError> {
    let next = store_revision(connection)?
        .checked_add(1)
        .ok_or(ObservationStoreError::Corrupt)?;
    connection
        .execute(
            "UPDATE observation_meta SET value=?1 WHERE key='store_revision'",
            [next.to_string()],
        )
        .map_err(|_| ObservationStoreError::ActivityUnavailable)?;
    Ok(next)
}
