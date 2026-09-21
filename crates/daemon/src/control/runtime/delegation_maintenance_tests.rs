use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use hiroute_domain::WorkspaceId;
use hiroute_domain::delegation::{
    DELEGATION_NATIVE_ROOT_MARKER_FILE_V1, DelegationBodyRefV1, DelegationCheckpointV1,
    DelegationNativeCleanupFailureKindV1, DelegationNativeRootReadyV1, DelegationNativeRootStateV1,
    DelegationRuntimePort, RunEventV1,
};
use hiroute_observation::managed_text::{
    ManagedTextInput, ManagedTextNativeCleanup, ManagedTextPurpose, ManagedTextScope,
};

use super::*;
use crate::control::runtime::ProductionControlRuntime;

const BLOCKED_TEST: &str = "control::runtime::delegation_maintenance::tests::blocked_native_deletion_remains_claimed_and_unacknowledged";
const SUCCESS_TEST: &str = "control::runtime::delegation_maintenance::tests::owned_native_deletion_commits_removed_before_exact_ack";

#[derive(Clone, Copy)]
enum BlockedFixture {
    ReplacedIdentity,
    NonPrivatePermissions,
    MarkerFinalizationInterrupted,
}

struct CleanupFixture {
    runtime: ProductionControlRuntime,
    _directory: tempfile::TempDir,
    workspace_id: WorkspaceId,
    task_id: String,
    native_root: PathBuf,
    job: ManagedTextNativeCleanup,
}

#[test]
fn blocked_native_deletion_remains_claimed_and_unacknowledged() {
    if crate::test_support::isolated_agent_home(BLOCKED_TEST) {
        return;
    }
    assert_blocked_cleanup(BlockedFixture::ReplacedIdentity, "identity");
    assert_blocked_cleanup(BlockedFixture::NonPrivatePermissions, "permissions");
    assert_blocked_cleanup(
        BlockedFixture::MarkerFinalizationInterrupted,
        "marker-interrupted",
    );
}

fn assert_blocked_cleanup(fixture: BlockedFixture, suffix: &str) {
    let cleanup = cleanup_fixture(suffix);
    let preserved_entry = match fixture {
        BlockedFixture::ReplacedIdentity => {
            std::fs::remove_file(
                cleanup
                    .native_root
                    .join(DELEGATION_NATIVE_ROOT_MARKER_FILE_V1),
            )
            .unwrap();
            std::fs::remove_dir(&cleanup.native_root).unwrap();
            private_directory(&cleanup.native_root);
            true
        }
        BlockedFixture::NonPrivatePermissions => {
            std::fs::set_permissions(&cleanup.native_root, std::fs::Permissions::from_mode(0o755))
                .unwrap();
            true
        }
        BlockedFixture::MarkerFinalizationInterrupted => {
            std::fs::remove_file(
                cleanup
                    .native_root
                    .join(DELEGATION_NATIVE_ROOT_MARKER_FILE_V1),
            )
            .unwrap();
            false
        }
    };
    if preserved_entry {
        std::fs::write(cleanup.native_root.join("must-remain"), b"preserved").unwrap();
    }

    let mut remaining = DELETE_BUDGET;
    cleanup
        .runtime
        .adapter
        .process_native_root_for_job(&cleanup.job, 101, &mut remaining)
        .unwrap();
    let blocked = DelegationRuntimePort::native_root(
        cleanup.runtime.adapter.as_ref(),
        &cleanup.workspace_id,
        &cleanup.task_id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(blocked.root.state, DelegationNativeRootStateV1::Deleting);
    let failure = blocked.root.last_cleanup_failure.unwrap();
    assert_eq!(
        failure.kind,
        DelegationNativeCleanupFailureKindV1::IdentityMismatch
    );
    assert_eq!(failure.attempted_at_ms, 101);
    assert_eq!(failure.attempts, 1);
    assert_eq!(remaining, DELETE_BUDGET);
    assert_eq!(
        cleanup
            .runtime
            .observation
            .managed_text_pending_native_cleanup_page(None, 10)
            .unwrap(),
        vec![cleanup.job]
    );
    assert!(cleanup.native_root.exists());
    if preserved_entry {
        assert_eq!(
            std::fs::read(cleanup.native_root.join("must-remain")).unwrap(),
            b"preserved"
        );
    }
}

#[test]
fn owned_native_deletion_commits_removed_before_exact_ack() {
    if crate::test_support::isolated_agent_home(SUCCESS_TEST) {
        return;
    }
    let cleanup = cleanup_fixture("success");
    std::fs::write(cleanup.native_root.join("history.jsonl"), b"native history").unwrap();
    let mut remaining = DELETE_BUDGET;
    cleanup
        .runtime
        .adapter
        .process_native_root_for_job(&cleanup.job, 101, &mut remaining)
        .unwrap();
    let removed = DelegationRuntimePort::native_root(
        cleanup.runtime.adapter.as_ref(),
        &cleanup.workspace_id,
        &cleanup.task_id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(removed.root.state, DelegationNativeRootStateV1::Removed);
    assert!(removed.root.last_cleanup_failure.is_none());
    assert!(remaining < DELETE_BUDGET);
    assert!(!cleanup.native_root.exists());
    assert!(
        cleanup
            .runtime
            .observation
            .managed_text_pending_native_cleanup_page(None, 10)
            .unwrap()
            .is_empty()
    );
}

fn cleanup_fixture(suffix: &str) -> CleanupFixture {
    let directory = tempfile::tempdir().unwrap();
    let storage = directory.path().join("storage");
    private_directory(&storage);
    let mut runtime = ProductionControlRuntime::open_with_release_catalog(
        &storage,
        crate::release_catalog::fixture_catalog(),
    )
    .unwrap();
    runtime._observation_maintenance.take();
    for _ in 0..100 {
        if !runtime.observation.maintenance_status().0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!runtime.observation.maintenance_status().0);

    let task_id = format!("cleanup-{suffix}-task");
    let run_id = format!("cleanup-{suffix}-run");
    let scope = ManagedTextScope {
        workspace_id: WorkspaceId::default(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
    };
    let input = ManagedTextInput {
        scope: scope.clone(),
        purpose: ManagedTextPurpose::Result,
        source_event_id: format!("cleanup-{suffix}-body"),
        source_revision: 1,
        original_created_at_ms: 10,
        import_origin: None,
    };
    let reference = runtime.observation.managed_text_put(&input, 100).unwrap();
    runtime
        .observation
        .managed_text_append(&scope, &reference, 0, b"result", 100)
        .unwrap();
    let reference = runtime
        .observation
        .managed_text_finish(&scope, &reference, 1, 100)
        .unwrap();
    let body = DelegationBodyRefV1 {
        opaque_id: reference.opaque_id,
        scope_run_id: run_id.clone(),
        visibility_generation: reference.visibility_generation,
        original_retention_deadline_ms: reference.original_retention_deadline_ms,
    };
    let acceptance = super::super::tests::worker_list_acceptance(&task_id, &run_id, body, 10);
    let accepted = DelegationRuntimePort::accept(runtime.adapter.as_ref(), &acceptance).unwrap();

    let native_base = std::fs::canonicalize(directory.path())
        .unwrap()
        .join("native");
    private_directory(&native_base);
    let creating = DelegationRuntimePort::native_root(
        runtime.adapter.as_ref(),
        &accepted.workspace_id,
        &accepted.task_id,
    )
    .unwrap()
    .unwrap();
    let session = crate::delegation::profile::TaskSessionRoot::prepare(
        &native_base,
        &accepted.workspace_id,
        &creating.root.workspace_root_identity,
        &accepted.task_id,
        creating.root.harness,
        crate::delegation::profile::SessionRootUse::New,
    )
    .unwrap();
    let identity = session
        .create_ownership_marker(&native_base, &creating.root)
        .unwrap();
    let native_root = session.path().to_owned();
    DelegationRuntimePort::commit_native_root_ready(
        runtime.adapter.as_ref(),
        &DelegationNativeRootReadyV1 {
            workspace_id: accepted.workspace_id.clone(),
            task_id: accepted.task_id.clone(),
            root_generation: creating.root.root_generation,
            creation_nonce: creating.root.creation_nonce,
            managed_base_path: native_base.to_str().unwrap().into(),
            filesystem_identity: identity,
        },
    )
    .unwrap();
    DelegationRuntimePort::checkpoint(
        runtime.adapter.as_ref(),
        &accepted.workspace_id,
        &accepted.run_id,
        accepted.progress.revision,
        &format!("cleanup-{suffix}-stopped"),
        &DelegationCheckpointV1::Progress {
            event: RunEventV1::LaunchFailedBeforeSpawn,
        },
    )
    .unwrap();

    let preview = runtime
        .observation
        .managed_text_delete_preview(&scope, 100, 100)
        .unwrap();
    let deleted = runtime
        .observation
        .managed_text_delete_apply(&preview)
        .unwrap();
    assert!(deleted.native_gc_pending);
    let mut jobs = runtime
        .observation
        .managed_text_pending_native_cleanup_page(None, 10)
        .unwrap();
    assert_eq!(jobs.len(), 1);
    CleanupFixture {
        runtime,
        _directory: directory,
        workspace_id: accepted.workspace_id,
        task_id: accepted.task_id,
        native_root,
        job: jobs.pop().unwrap(),
    }
}

fn private_directory(path: &std::path::Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
