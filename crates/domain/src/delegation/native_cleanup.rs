//! Durable ownership facts for task-scoped native session roots.
//!
//! These records authorize no caller and contain no native history bytes.  They let the daemon
//! distinguish a root it created from an arbitrary path, keep every accepted run as a user of
//! that root, and make cleanup/Continue exclusion a runtime-store CAS rather than a path guess.

use serde::{Deserialize, Serialize};

use super::{DelegationErrorV1, DelegationTaskV1, WorkerHarnessV1, valid_delegation_id};
use crate::WorkspaceId;

pub const DELEGATION_NATIVE_ROOT_GENERATION_V1: u64 = 1;
pub const DELEGATION_NATIVE_ROOT_MARKER_FILE_V1: &str = ".hiroute-native-root-v1.json";
pub const DELEGATION_NATIVE_ROOT_MARKER_SCHEMA_V1: &str = "hiroute.delegation-native-root/v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationNativeRootStateV1 {
    Creating,
    Ready,
    /// Durable proof that profile creation never created this generation.
    NoNative,
    /// Creation may have changed the filesystem but did not publish a verifiable identity.
    Unknown,
    Deleting,
    Removed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationNativeUseStateV1 {
    Accepted,
    /// Persisted immediately before the platform launch call.  A restart cannot turn this into
    /// `NeverSpawned` merely because no process handle survived.
    MayHaveSpawned,
    Stopped,
    NeverSpawned,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationNativeCleanupFailureKindV1 {
    UnsupportedPlatform,
    UnsupportedIdentity,
    BaseUnavailable,
    IdentityMismatch,
    UnsafeEntry,
    IoUnavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationNativeCleanupFailureV1 {
    pub kind: DelegationNativeCleanupFailureKindV1,
    pub attempted_at_ms: i64,
    pub attempts: u64,
}

impl DelegationNativeUseStateV1 {
    pub fn ended(self) -> bool {
        matches!(self, Self::Stopped | Self::NeverSpawned)
    }
}

/// Unix cleanup uses an already-open base/root and compares these identities before every batch.
/// Other platforms may persist a different scheme later; an unsupported scheme remains pending.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationNativeFilesystemIdentityV1 {
    pub scheme: String,
    pub base_device: u64,
    pub base_inode: u64,
    pub root_device: u64,
    pub root_inode: u64,
    /// The ownership marker remains present until the final deletion batch.  Binding its file
    /// identity as well as its contents prevents an immediately reused root inode from adopting
    /// an unrelated replacement directory.
    pub marker_device: u64,
    pub marker_inode: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationNativeRootMarkerV1 {
    pub schema: String,
    pub workspace_id: WorkspaceId,
    pub task_id: String,
    pub root_generation: u64,
    pub creation_nonce: String,
    pub harness: WorkerHarnessV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationNativeCleanupJobV1 {
    pub workspace_id: WorkspaceId,
    pub task_id: String,
    pub run_id: String,
    pub visibility_generation: u64,
    pub through_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationNativeCleanupClaimV1 {
    pub claim_id: String,
    pub root_generation: u64,
    pub expected_use_revision: u64,
    pub checked_at_ms: i64,
    pub jobs: Vec<DelegationNativeCleanupJobV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationNativeRootV1 {
    pub workspace_id: WorkspaceId,
    pub task_id: String,
    pub root_generation: u64,
    pub harness: WorkerHarnessV1,
    pub workspace_root_identity: String,
    /// A single portable component below the daemon-owned sessions base.
    pub relative_root: String,
    pub creation_nonce: String,
    pub state: DelegationNativeRootStateV1,
    pub use_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_base_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_identity: Option<DelegationNativeFilesystemIdentityV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_claim: Option<DelegationNativeCleanupClaimV1>,
    #[serde(default)]
    pub deletion_batches: u64,
    /// One bounded diagnostic fact for a still-pending claim; repeated failures overwrite the
    /// timestamp and increment the counter instead of growing an unbounded retry log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cleanup_failure: Option<DelegationNativeCleanupFailureV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationNativeUseV1 {
    pub workspace_id: WorkspaceId,
    pub task_id: String,
    pub root_generation: u64,
    pub run_id: String,
    pub lease_id: String,
    pub daemon_epoch: String,
    pub accepted_at_ms: u64,
    pub state: DelegationNativeUseStateV1,
    /// The exact managed-text scope has the same workspace/task plus this run id.
    pub scope_run_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationNativeRootSnapshotV1 {
    pub root: DelegationNativeRootV1,
    pub uses: Vec<DelegationNativeUseV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationNativeRootReadyV1 {
    pub workspace_id: WorkspaceId,
    pub task_id: String,
    pub root_generation: u64,
    pub creation_nonce: String,
    pub managed_base_path: String,
    pub filesystem_identity: DelegationNativeFilesystemIdentityV1,
}

/// Process-local keyset position for bounded task metadata maintenance.  It is never persisted
/// and grants no authority; every mutation re-reads an exact durable task snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationTaskMaintenanceCursorV1 {
    pub workspace_id: WorkspaceId,
    pub task_id: String,
}

/// Durable handoff between the runtime transaction that disables one exact continuation and the
/// control-store release of its Task/Continuation Plan hold.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationContinuationReleaseV1 {
    pub workspace_id: WorkspaceId,
    pub task_id: String,
    pub latest_run_id: String,
    pub resume_until_ms: u64,
    pub task: DelegationTaskV1,
}

impl DelegationContinuationReleaseV1 {
    pub fn validate(&self) -> Result<(), DelegationErrorV1> {
        if WorkspaceId::parse(self.workspace_id.as_str()).is_err()
            || !valid_delegation_id(&self.task_id)
            || !valid_delegation_id(&self.latest_run_id)
            || self.resume_until_ms == 0
            || self.task.workspace_id != self.workspace_id
            || self.task.task_id != self.task_id
            || self.task.latest_run_id != self.latest_run_id
            || self.task.resume_until_ms != self.resume_until_ms
        {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        Ok(())
    }
}

impl DelegationNativeRootV1 {
    pub fn ownership_marker(&self) -> DelegationNativeRootMarkerV1 {
        DelegationNativeRootMarkerV1 {
            schema: DELEGATION_NATIVE_ROOT_MARKER_SCHEMA_V1.to_owned(),
            workspace_id: self.workspace_id.clone(),
            task_id: self.task_id.clone(),
            root_generation: self.root_generation,
            creation_nonce: self.creation_nonce.clone(),
            harness: self.harness,
        }
    }

    pub fn validate(&self) -> Result<(), DelegationErrorV1> {
        if WorkspaceId::parse(self.workspace_id.as_str()).is_err()
            || !valid_delegation_id(&self.task_id)
            || self.root_generation == 0
            || !valid_delegation_id(&self.workspace_root_identity)
            || !valid_delegation_id(&self.creation_nonce)
            || self.relative_root.is_empty()
            || self.relative_root.len() > 256
            || self.relative_root.contains(['/', '\\', '\0', '\r', '\n'])
            || self.use_revision == 0
            || self
                .last_cleanup_failure
                .as_ref()
                .is_some_and(|failure| failure.attempted_at_ms < 0 || failure.attempts == 0)
            || self
                .managed_base_path
                .as_deref()
                .is_some_and(|path| path.is_empty() || path.len() > 4096 || path.contains('\0'))
            || self.filesystem_identity.as_ref().is_some_and(|identity| {
                !matches!(
                    identity.scheme.as_str(),
                    "unix-dev-inode-v1" | "windows-volume-file-index-v1"
                )
            })
            || self
                .cleanup_claim
                .as_ref()
                .is_some_and(|claim| claim.validate_for(self).is_err())
        {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        let ready_identity = self.managed_base_path.is_some() && self.filesystem_identity.is_some();
        match self.state {
            DelegationNativeRootStateV1::Creating
            | DelegationNativeRootStateV1::NoNative
            | DelegationNativeRootStateV1::Unknown => {
                if ready_identity
                    || self.cleanup_claim.is_some()
                    || self.last_cleanup_failure.is_some()
                {
                    return Err(DelegationErrorV1::InvalidArguments);
                }
            }
            DelegationNativeRootStateV1::Ready => {
                if !ready_identity
                    || self.cleanup_claim.is_some()
                    || self.last_cleanup_failure.is_some()
                {
                    return Err(DelegationErrorV1::InvalidArguments);
                }
            }
            DelegationNativeRootStateV1::Deleting => {
                if self.cleanup_claim.is_none()
                    || (self.filesystem_identity.is_some() != self.managed_base_path.is_some())
                {
                    return Err(DelegationErrorV1::InvalidArguments);
                }
            }
            DelegationNativeRootStateV1::Removed => {
                if self.cleanup_claim.is_none() || self.last_cleanup_failure.is_some() {
                    return Err(DelegationErrorV1::InvalidArguments);
                }
            }
        }
        Ok(())
    }
}

impl DelegationNativeUseV1 {
    pub fn validate(&self) -> Result<(), DelegationErrorV1> {
        if WorkspaceId::parse(self.workspace_id.as_str()).is_err()
            || !valid_delegation_id(&self.task_id)
            || self.root_generation == 0
            || !valid_delegation_id(&self.run_id)
            || !valid_delegation_id(&self.lease_id)
            || !valid_delegation_id(&self.daemon_epoch)
            || self.accepted_at_ms == 0
            || self.scope_run_id != self.run_id
        {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        Ok(())
    }
}

impl DelegationNativeCleanupClaimV1 {
    pub fn validate_for(&self, root: &DelegationNativeRootV1) -> Result<(), DelegationErrorV1> {
        if !valid_delegation_id(&self.claim_id)
            || self.root_generation != root.root_generation
            || self.expected_use_revision != root.use_revision
            || self.checked_at_ms < 0
            || self.jobs.is_empty()
            || self.jobs.len() > 200
        {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        for (index, job) in self.jobs.iter().enumerate() {
            if job.workspace_id != root.workspace_id
                || job.task_id != root.task_id
                || !valid_delegation_id(&job.run_id)
                || job.visibility_generation == 0
                || job.through_ms < 0
                || self.jobs[..index].iter().any(|prior| {
                    prior.run_id == job.run_id
                        && prior.visibility_generation == job.visibility_generation
                })
            {
                return Err(DelegationErrorV1::InvalidArguments);
            }
        }
        Ok(())
    }
}
