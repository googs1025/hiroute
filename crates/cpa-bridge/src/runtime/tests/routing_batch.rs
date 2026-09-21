use super::*;

#[test]
fn routing_batch_probe_cost_is_constant_for_models_protocols_and_credentials() {
    for (plan_candidates, workspace_candidates) in [(1, 1), (5, 5), (10, 10), (1, 40)] {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(FakeBackend::default());
        let control = Arc::new(FakeControl::default());
        let mut codex = snapshot(CpaAccountKind::Codex, 'a', "gpt-5.5");
        let mut claude = snapshot(CpaAccountKind::Claude, 'b', "claude-sonnet-5");
        // These are the exact workspace candidate requests, not the size of the plan.
        // Extra inventory-only models are not candidates in this fixture.
        let requests = (0..workspace_candidates)
            .map(|n| {
                let model = format!("batch-model-{n}");
                let is_codex = n % 2 == 0;
                if is_codex {
                    codex.observed_model_ids.insert(model.clone());
                } else {
                    claude.observed_model_ids.insert(model.clone());
                }
                let protocol = if is_codex {
                    [
                        UpstreamProtocol::Responses,
                        UpstreamProtocol::ChatCompletions,
                        UpstreamProtocol::Messages,
                    ][n % 3]
                } else {
                    UpstreamProtocol::Messages
                };
                (is_codex, model, protocol)
            })
            .collect::<Vec<_>>();
        control.set_accounts(vec![codex, claude]);
        let runtime = fixture_runtime(&root, backend, Arc::clone(&control), 3);
        runtime.start().unwrap();
        let probes = control.probes.load(Ordering::SeqCst);
        let discoveries = control.discoveries.load(Ordering::SeqCst);
        let batch = runtime.begin_routing_batch().unwrap();
        let codex = batch
            .sources()
            .iter()
            .find(|s| s.source.connector_id == "connector.cpa.codex")
            .unwrap();
        let claude = batch
            .sources()
            .iter()
            .find(|s| s.source.connector_id == "connector.cpa.claude")
            .unwrap();
        let after_begin = control.probes.load(Ordering::SeqCst);
        let mut workspace = Vec::new();
        for (is_codex, model, protocol) in &requests {
            let (matching, wrong) = if *is_codex {
                (codex, claude)
            } else {
                (claude, codex)
            };
            let prepared = batch
                .prepare_target(ExactCpaAttemptRequest {
                    credential_ref: &matching.credential_ref,
                    upstream_model_id: model,
                    protocol: *protocol,
                })
                .unwrap();
            assert_eq!(prepared.credential_ref(), &matching.credential_ref);
            assert_eq!(prepared.protocol(), *protocol);
            assert_eq!(prepared.upstream_model_id(), model);
            assert_eq!(
                batch.prepare_target(ExactCpaAttemptRequest {
                    credential_ref: &wrong.credential_ref,
                    upstream_model_id: model,
                    protocol: *protocol,
                }),
                Err(CpaAttemptError::UnregisteredTarget)
            );
            if !is_codex {
                assert_eq!(
                    batch.prepare_target(ExactCpaAttemptRequest {
                        credential_ref: &matching.credential_ref,
                        upstream_model_id: model,
                        protocol: UpstreamProtocol::ChatCompletions,
                    }),
                    Err(CpaAttemptError::UnregisteredTarget)
                );
            }
            workspace.push(prepared);
        }
        assert_eq!(workspace.len(), workspace_candidates);
        let plan = workspace.iter().take(plan_candidates).collect::<Vec<_>>();
        assert_eq!(plan.len(), plan_candidates);
        // Repeated references to this plan must retain exact identity without more probes.
        for _ in 0..5 {
            for selected in &plan {
                let repeated = batch
                    .prepare_target(ExactCpaAttemptRequest {
                        credential_ref: selected.credential_ref(),
                        upstream_model_id: selected.upstream_model_id(),
                        protocol: selected.protocol(),
                    })
                    .unwrap();
                assert_eq!(&repeated, *selected);
            }
        }
        assert_eq!(
            control.probes.load(Ordering::SeqCst),
            after_begin,
            "per-candidate probes: plan={plan_candidates}, workspace={workspace_candidates}"
        );
        assert!(batch.finish().unwrap());
        // Fake discovery includes its own probe: two fresh discovery boundaries, four probes.
        assert_eq!(control.probes.load(Ordering::SeqCst) - probes, 4);
        assert_eq!(control.discoveries.load(Ordering::SeqCst) - discoveries, 2);
        let credentials_used = requests
            .iter()
            .map(|(codex, _, _)| codex)
            .collect::<BTreeSet<_>>()
            .len();
        let protocols_used = requests
            .iter()
            .map(|(_, _, protocol)| format!("{protocol:?}"))
            .collect::<BTreeSet<_>>()
            .len();
        eprintln!(
            "CPA batch count: plan_candidates={plan_candidates} workspace_candidates={workspace_candidates} credentials_used={credentials_used} protocols_used={protocols_used} repeated_plan_references=5 discoveries=2 fake_full_probes=4"
        );
        runtime.shutdown().unwrap();
    }
}

#[test]
fn routing_batch_rejects_restart_even_when_target_epochs_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeBackend::default());
    let control = Arc::new(FakeControl::default());
    control.set_accounts(vec![snapshot(CpaAccountKind::Codex, 'a', "gpt-5.5")]);
    let runtime = fixture_runtime(&root, Arc::clone(&backend), control, 3);
    runtime.start().unwrap();
    let batch = runtime.begin_routing_batch().unwrap();
    let epochs = runtime.epochs.current();
    backend.crash_latest(1);
    assert!(!batch.finish().unwrap());
    assert_eq!(
        runtime.epochs.current(),
        epochs,
        "restart semantics must remain unchanged"
    );
    assert!(runtime.begin_routing_batch().unwrap().finish().unwrap());
    runtime.shutdown().unwrap();
}

#[test]
fn routing_batch_refreshes_external_account_generation_management_and_authentication() {
    for change in [
        "account",
        "generation",
        "disabled",
        "authentication",
        "shutdown",
    ] {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(FakeBackend::default());
        let control = Arc::new(FakeControl::default());
        control.set_accounts(vec![snapshot(CpaAccountKind::Codex, 'a', "gpt-5.5")]);
        let runtime = fixture_runtime(&root, backend, Arc::clone(&control), 3);
        runtime.start().unwrap();
        let batch = runtime.begin_routing_batch().unwrap();
        let source = batch.sources()[0].clone();
        match change {
            "account" | "generation" => write_fixture_codex_source(
                &root.path().join("codex-auth.json"),
                if change == "account" {
                    "fixture-account-two"
                } else {
                    "fixture-account-one"
                },
                "rotated-access",
                "rotated-id-token",
                "rotated-refresh-time",
            ),
            "disabled" => runtime
                .apply_account_management(
                    &source.source.identity.account_subject_ref,
                    2,
                    CpaSourceManagementState::Disabled,
                )
                .unwrap(),
            "authentication" => control.set_fail_probes(true),
            "shutdown" => {
                runtime.shutdown().unwrap();
            }
            _ => unreachable!(),
        }
        assert!(!batch.finish().unwrap_or(false), "accepted stale {change}");
        if matches!(change, "account" | "authentication" | "shutdown") {
            assert!(runtime.begin_routing_batch().is_err());
        } else {
            let next = runtime.begin_routing_batch().unwrap();
            let prepared = next.prepare_target(ExactCpaAttemptRequest {
                credential_ref: &source.credential_ref,
                upstream_model_id: "gpt-5.5",
                protocol: UpstreamProtocol::Responses,
            });
            if change == "generation" {
                assert!(
                    prepared.unwrap().credential_ref().generation()
                        > source.credential_ref.generation(),
                    "same-account rotation did not advance the private generation"
                );
            } else {
                assert_eq!(
                    prepared,
                    Err(CpaAttemptError::RevokedCredential),
                    "disabled account was reused"
                );
            }
            assert!(
                next.finish().unwrap(),
                "stable second batch rejected after {change}"
            );
            if change == "disabled" {
                runtime
                    .apply_account_management(
                        &source.source.identity.account_subject_ref,
                        3,
                        CpaSourceManagementState::Enabled,
                    )
                    .unwrap();
                let enabled = runtime.begin_routing_batch().unwrap();
                assert!(
                    enabled.sources()[0].credential_ref.generation()
                        > source.credential_ref.generation()
                );
                assert!(enabled.finish().unwrap());
            }
        }
        if change != "shutdown" {
            runtime.shutdown().unwrap();
        }
    }
}
