use super::{SessionDeletionOutcomeV1, SessionDeletionPreviewV1};
use crate::{CanonicalDigest, WorkspaceId};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedScopeDeletionV2 {
    pub workspace_id: WorkspaceId,
    pub task_id: String,
    pub run_id: String,
    pub visibility_generation: u64,
    pub reference_count: u64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionDeletionPreviewV2 {
    pub session: SessionDeletionPreviewV1,
    pub through_ms: i64,
    pub managed_scopes: Vec<ManagedScopeDeletionV2>,
    pub change_digest: CanonicalDigest,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionDeletionOutcomeV2 {
    pub session: SessionDeletionOutcomeV1,
    pub managed_native_cleanup_pending: bool,
    pub managed_object_cleanup_pending: bool,
    /// Store-wide ordinary blob GC backlog; may include earlier deletion jobs.
    pub object_cleanup_pending: bool,
}
