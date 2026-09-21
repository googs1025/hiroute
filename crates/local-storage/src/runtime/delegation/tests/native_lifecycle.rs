use super::*;

fn cleanup_job(
    workspace: &WorkspaceId,
    task_id: &str,
    run_id: &str,
    generation: u64,
) -> DelegationNativeCleanupJobV1 {
    DelegationNativeCleanupJobV1 {
        workspace_id: workspace.clone(),
        task_id: task_id.to_owned(),
        run_id: run_id.to_owned(),
        visibility_generation: generation,
        through_ms: 10_000,
    }
}

fn make_resumable(
    store: &RuntimeStore,
    input: &DelegationAcceptanceV1,
    until_ms: u64,
) -> DelegationRunV1 {
    let mut run = store.accept(input).unwrap();
    commit_ready_root(store, &run.workspace_id, &run.task_id);
    run = advance(store, &run, RunEventV1::Preparing);
    run = store
        .checkpoint(
            &run.workspace_id,
            &run.run_id,
            run.progress.revision,
            "native-test-spawn",
            &DelegationCheckpointV1::ProcessSpawned {
                binding: DelegationProcessBindingV1 {
                    launch_nonce: run.launch_nonce.clone(),
                    handle_id: format!("handle-{}", run.run_id),
                    creation_identity: format!("identity-{}", run.run_id),
                },
            },
        )
        .unwrap();
    let session = DelegationSessionBindingV1 {
        acp_session_id: format!("acp-{}", run.run_id),
        native_session_id: Some(format!("native-{}", run.run_id)),
    };
    run = store
        .checkpoint(
            &run.workspace_id,
            &run.run_id,
            run.progress.revision,
            "native-test-session",
            &DelegationCheckpointV1::SessionBound { binding: session },
        )
        .unwrap();
    run = advance(store, &run, RunEventV1::PromptSendIntent);
    let result = DelegationBodyRefV1 {
        opaque_id: format!("result-{}", run.run_id),
        scope_run_id: run.run_id.clone(),
        visibility_generation: 1,
        original_retention_deadline_ms: i64::try_from(until_ms).unwrap(),
    };
    run = store
        .checkpoint(
            &run.workspace_id,
            &run.run_id,
            run.progress.revision,
            "native-test-result",
            &DelegationCheckpointV1::ResultRecorded {
                body: Some(result.clone()),
                incomplete: false,
            },
        )
        .unwrap();
    run = advance(store, &run, RunEventV1::PromptCompleted);
    run = store
        .checkpoint(
            &run.workspace_id,
            &run.run_id,
            run.progress.revision,
            "native-test-stop",
            &DelegationCheckpointV1::ProcessStopped {
                evidence: RunStopEvidenceV1 {
                    scope: RunStopScopeV1::ProcessGroup,
                    observation: RunProcessObservationV1::Exited { code: Some(0) },
                    scope_stopped: true,
                    residual_unknown: false,
                },
            },
        )
        .unwrap();
    let mut bodies = input.task.body_refs.clone();
    bodies.push(result);
    store
        .set_resume_materials(
            &run.workspace_id,
            &run.task_id,
            &run.run_id,
            until_ms,
            &bodies,
            &["history.jsonl".into()],
        )
        .unwrap();
    run
}

#[test]
fn native_lifecycle_records_preparing_then_proven_pre_spawn_failure() {
    let dir = crate::test_tempdir().unwrap();
    let store = open(dir.path());
    let run = store
        .accept(&sample("native-task", "native-run", "native-root"))
        .unwrap();
    let accepted = store
        .native_root(&run.workspace_id, &run.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(accepted.root.state, DelegationNativeRootStateV1::Creating);
    assert_eq!(accepted.uses.len(), 1);
    assert_eq!(accepted.uses[0].state, DelegationNativeUseStateV1::Accepted);

    let preparing = advance(&store, &run, RunEventV1::Preparing);
    let prepared = store
        .native_root(&run.workspace_id, &run.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        prepared.uses[0].state,
        DelegationNativeUseStateV1::MayHaveSpawned
    );

    advance(&store, &preparing, RunEventV1::LaunchFailedBeforeSpawn);
    let failed = store
        .native_root(&run.workspace_id, &run.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(failed.root.state, DelegationNativeRootStateV1::NoNative);
    assert_eq!(
        failed.uses[0].state,
        DelegationNativeUseStateV1::NeverSpawned
    );
}

#[test]
fn native_cleanup_claim_is_a_use_revision_cas_against_continue() {
    let dir = crate::test_tempdir().unwrap();
    let store = open(dir.path());
    let input = sample("cas-task", "cas-run", "cas-root");
    let run = make_resumable(&store, &input, 5_000);
    let before = store
        .native_root(&run.workspace_id, &run.task_id)
        .unwrap()
        .unwrap();
    let stale_claim = DelegationNativeCleanupClaimV1 {
        claim_id: "claim-stale".into(),
        root_generation: before.root.root_generation,
        expected_use_revision: before.root.use_revision,
        checked_at_ms: 10_000,
        jobs: vec![cleanup_job(&run.workspace_id, &run.task_id, &run.run_id, 1)],
    };

    let mut continuation = sample("cas-task", "cas-next", "cas-root");
    continuation.task = store
        .task(&run.workspace_id, &run.task_id)
        .unwrap()
        .unwrap();
    continuation.task.latest_run_id = continuation.run.run_id.clone();
    let next_body = DelegationBodyRefV1 {
        opaque_id: "cas-next-body".into(),
        scope_run_id: continuation.run.run_id.clone(),
        visibility_generation: 1,
        original_retention_deadline_ms: 5_000,
    };
    continuation
        .task
        .required_body_ids
        .push(next_body.opaque_id.clone());
    continuation.task.body_refs.push(next_body);
    continuation.run.ordinal = 2;
    continuation.run.continued_from = Some(run.run_id.clone());
    continuation.expected_latest_run_id = Some(run.run_id.clone());
    continuation.title_lookup_key = None;
    let next = store.accept(&continuation).unwrap();
    assert_eq!(
        store.claim_native_cleanup(&run.workspace_id, &run.task_id, &stale_claim),
        Err(DelegationErrorV1::Conflict)
    );

    release_slot(&store, &next);
    let current = store
        .native_root(&run.workspace_id, &run.task_id)
        .unwrap()
        .unwrap();
    let claim = DelegationNativeCleanupClaimV1 {
        claim_id: "claim-current".into(),
        root_generation: current.root.root_generation,
        expected_use_revision: current.root.use_revision,
        checked_at_ms: 10_000,
        jobs: vec![
            cleanup_job(&run.workspace_id, &run.task_id, &run.run_id, 1),
            cleanup_job(&run.workspace_id, &run.task_id, &next.run_id, 1),
        ],
    };
    let claimed = store
        .claim_native_cleanup(&run.workspace_id, &run.task_id, &claim)
        .unwrap();
    assert_eq!(claimed.root.state, DelegationNativeRootStateV1::Deleting);
    for attempted_at_ms in [10_001, 10_002] {
        store
            .record_native_cleanup_failure(
                &run.workspace_id,
                &run.task_id,
                claimed.root.root_generation,
                "claim-current",
                DelegationNativeCleanupFailureKindV1::IdentityMismatch,
                attempted_at_ms,
            )
            .unwrap();
    }
    let blocked = store
        .native_root(&run.workspace_id, &run.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(blocked.root.state, DelegationNativeRootStateV1::Deleting);
    assert_eq!(
        blocked.root.last_cleanup_failure,
        Some(DelegationNativeCleanupFailureV1 {
            kind: DelegationNativeCleanupFailureKindV1::IdentityMismatch,
            attempted_at_ms: 10_002,
            attempts: 2,
        })
    );
    assert!(
        store.accept(&continuation).is_ok(),
        "same key remains replayable"
    );
}

#[test]
fn continuation_release_and_title_cleanup_are_exact_and_survive_reopen() {
    let dir = crate::test_tempdir().unwrap();
    let workspace = WorkspaceId::parse("workspace").unwrap();
    let release;
    {
        let store = open(dir.path());
        let input = sample("release-task", "release-run", "release-root");
        let run = make_resumable(&store, &input, 5_000);
        let page = store.maintenance_tasks(None, 1).unwrap();
        assert_eq!(page.len(), 1);
        let expected = page[0].clone();
        release = store.claim_continuation_release(&expected).unwrap();
        let disabled = store.task(&workspace, "release-task").unwrap().unwrap();
        assert_eq!(disabled.resume_until_ms, 0);
        assert!(
            store
                .set_resume_materials(
                    &workspace,
                    "release-task",
                    &run.run_id,
                    6_000,
                    &input.task.body_refs,
                    &["history.jsonl".into()],
                )
                .is_err()
        );
        assert!(store.clear_task_title(&disabled).unwrap());
        assert!(
            store
                .list_latest_runs(&workspace, Some("title-release-task"), None, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.pending_continuation_releases(10).unwrap(),
            vec![release.clone()]
        );
    }
    let reopened = open(dir.path());
    assert_eq!(
        reopened.pending_continuation_releases(10).unwrap(),
        vec![release.clone()]
    );
    reopened.complete_continuation_release(&release).unwrap();
    reopened.complete_continuation_release(&release).unwrap();
    assert!(
        reopened
            .pending_continuation_releases(10)
            .unwrap()
            .is_empty()
    );
}
