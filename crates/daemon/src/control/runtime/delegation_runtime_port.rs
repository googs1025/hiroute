//! Thin production bridge from the delegation domain port to the durable runtime store.

use hiroute_domain::delegation::{
    DelegationAcceptanceV1, DelegationBodyRefV1, DelegationContinuationReleaseV1,
    DelegationErrorV1, DelegationNativeCleanupClaimV1, DelegationNativeCleanupFailureKindV1,
    DelegationNativeRootReadyV1, DelegationNativeRootSnapshotV1, DelegationRuntimePort,
    DelegationTaskMaintenanceCursorV1, DelegationTaskV1, WorkerConcurrencySettingsV1,
};
use hiroute_domain::{OperationId, WorkspaceId};

use super::LocalControlAdapter;

impl DelegationRuntimePort for LocalControlAdapter {
    fn worker_concurrency_settings(
        &self,
    ) -> Result<WorkerConcurrencySettingsV1, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .worker_concurrency_settings()
    }

    fn set_worker_concurrency_settings(
        &self,
        settings: WorkerConcurrencySettingsV1,
    ) -> Result<WorkerConcurrencySettingsV1, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .set_worker_concurrency_settings(settings)
    }

    fn task(
        &self,
        workspace: &WorkspaceId,
        task_id: &str,
    ) -> Result<Option<DelegationTaskV1>, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .task(workspace, task_id)
    }

    fn run(
        &self,
        workspace: &WorkspaceId,
        run_id: &str,
    ) -> Result<Option<hiroute_domain::delegation::DelegationRunV1>, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .run(workspace, run_id)
    }

    fn find_submission(
        &self,
        workspace: &WorkspaceId,
        continuation: bool,
        key: &str,
    ) -> Result<Option<hiroute_domain::delegation::DelegationRunV1>, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .find_submission(workspace, continuation, key)
    }

    fn list_latest_runs(
        &self,
        workspace: &WorkspaceId,
        title_lookup_key: Option<&str>,
        before_sequence: Option<u64>,
        limit: u16,
    ) -> Result<Vec<hiroute_domain::delegation::DelegationRunV1>, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .list_latest_runs(workspace, title_lookup_key, before_sequence, limit)
    }

    fn resumable_tasks(
        &self,
        workspace: &WorkspaceId,
        now_ms: u64,
    ) -> Result<Vec<DelegationTaskV1>, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .resumable_tasks(workspace, now_ms)
    }

    fn accept(
        &self,
        acceptance: &DelegationAcceptanceV1,
    ) -> Result<hiroute_domain::delegation::DelegationRunV1, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .accept(acceptance)
    }

    fn checkpoint(
        &self,
        workspace: &WorkspaceId,
        run_id: &str,
        expected_revision: u64,
        event_id: &str,
        event: &hiroute_domain::delegation::DelegationCheckpointV1,
    ) -> Result<hiroute_domain::delegation::DelegationRunV1, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .checkpoint(workspace, run_id, expected_revision, event_id, event)
    }

    fn request_cancel(
        &self,
        workspace: &WorkspaceId,
        run_id: &str,
        operation: &OperationId,
        reason: &str,
    ) -> Result<hiroute_domain::delegation::DelegationCancelReceiptV1, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .request_cancel(workspace, run_id, operation, reason)
    }

    fn unreconciled(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<hiroute_domain::delegation::DelegationRunV1>, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .unreconciled(workspace)
    }

    fn set_resume_materials(
        &self,
        workspace: &WorkspaceId,
        task_id: &str,
        latest_run_id: &str,
        until_ms: u64,
        body_refs: &[DelegationBodyRefV1],
        native_history_paths: &[String],
    ) -> Result<(), DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .set_resume_materials(
                workspace,
                task_id,
                latest_run_id,
                until_ms,
                body_refs,
                native_history_paths,
            )
    }

    fn native_root(
        &self,
        workspace: &WorkspaceId,
        task_id: &str,
    ) -> Result<Option<DelegationNativeRootSnapshotV1>, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .native_root(workspace, task_id)
    }

    fn commit_native_root_ready(
        &self,
        ready: &DelegationNativeRootReadyV1,
    ) -> Result<DelegationNativeRootSnapshotV1, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .commit_native_root_ready(ready)
    }

    fn mark_native_root_unknown(
        &self,
        workspace: &WorkspaceId,
        task_id: &str,
        root_generation: u64,
        creation_nonce: &str,
    ) -> Result<(), DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .mark_native_root_unknown(workspace, task_id, root_generation, creation_nonce)
    }

    fn claim_native_cleanup(
        &self,
        workspace: &WorkspaceId,
        task_id: &str,
        claim: &DelegationNativeCleanupClaimV1,
    ) -> Result<DelegationNativeRootSnapshotV1, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .claim_native_cleanup(workspace, task_id, claim)
    }

    fn record_native_cleanup_batch(
        &self,
        workspace: &WorkspaceId,
        task_id: &str,
        root_generation: u64,
        claim_id: &str,
    ) -> Result<(), DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .record_native_cleanup_batch(workspace, task_id, root_generation, claim_id)
    }

    fn record_native_cleanup_failure(
        &self,
        workspace: &WorkspaceId,
        task_id: &str,
        root_generation: u64,
        claim_id: &str,
        kind: DelegationNativeCleanupFailureKindV1,
        attempted_at_ms: i64,
    ) -> Result<(), DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .record_native_cleanup_failure(
                workspace,
                task_id,
                root_generation,
                claim_id,
                kind,
                attempted_at_ms,
            )
    }

    fn complete_native_cleanup(
        &self,
        workspace: &WorkspaceId,
        task_id: &str,
        root_generation: u64,
        claim_id: &str,
    ) -> Result<DelegationNativeRootSnapshotV1, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .complete_native_cleanup(workspace, task_id, root_generation, claim_id)
    }

    fn maintenance_tasks(
        &self,
        after: Option<&DelegationTaskMaintenanceCursorV1>,
        limit: u16,
    ) -> Result<Vec<DelegationTaskV1>, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .maintenance_tasks(after, limit)
    }

    fn clear_task_title(&self, expected: &DelegationTaskV1) -> Result<bool, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .clear_task_title(expected)
    }

    fn claim_continuation_release(
        &self,
        expected: &DelegationTaskV1,
    ) -> Result<DelegationContinuationReleaseV1, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .claim_continuation_release(expected)
    }

    fn pending_continuation_releases(
        &self,
        limit: u16,
    ) -> Result<Vec<DelegationContinuationReleaseV1>, DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .pending_continuation_releases(limit)
    }

    fn complete_continuation_release(
        &self,
        release: &DelegationContinuationReleaseV1,
    ) -> Result<(), DelegationErrorV1> {
        self.stores_lock()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?
            .runtime()
            .complete_continuation_release(release)
    }
}
