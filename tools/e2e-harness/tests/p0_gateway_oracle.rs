use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use base64::Engine as _;
use hiroute_e2e::p0::production::{
    ChannelEvidence, CollectorEvidence, ContentTerminal, ExpectedObservation, LauncherRecord,
    ProductReady, ProductionBundle, ProductionRunOptions, ReadinessEvidence, ScenarioState,
    SutBuildAttestation, run_production_oracle, verify_collector_evidence,
    verify_launcher_evidence, verify_native_evidence, verify_readiness_evidence,
};
use serde_json::{Value, json};

const DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const SUT_DIGEST: &str = "sha256:8888888888888888888888888888888888888888888888888888888888888888";
const LIFECYCLE_DIGEST: &str =
    "sha256:762143e3ab3121bf40cb7039a73ea0b47a5724c38837ba4152f75fc54e09e316";
const EXECUTION_DIGEST: &str =
    "sha256:499cc87ff8aabf124c6613da86b076c1e34a7890fafe034dd897d50e10cae036";
const EXECUTION_PRICED_DIGEST: &str =
    "sha256:a22fc62f35454021f1df21b2b9f0ce54cde325b3fc7b3aa2e990ab9ecba71292";
const CONTENT_DIGEST: &str =
    "sha256:6f17cd772a0fb322d25dd31dc95e34325cbabbd68912b32f5bb9bed526adfe44";
const OTEL_DIGEST: &str = "sha256:d730badfd13c19d1c464c5a0c871a35a3e72a0f9c662d13d1997f066bf762e33";
const PUBLICATION_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const FRESHNESS: &str =
    "challenge-1111111111111111111111111111111111111111111111111111111111111111";

#[test]
fn checked_in_contract_is_one_green_only_production_case() {
    let bundle = bundle();
    let summary = bundle.summary();
    assert_eq!(summary.case_count, 1);
    assert_eq!(summary.completion_policy, "green_only");
    assert_eq!(summary.artifact_digests.len(), 11);
    assert_eq!(summary.schema_digests.len(), 8);
}

#[test]
fn deterministic_contract_generator_is_byte_stable() {
    let root = repository_root().join("e2e");
    let first = ProductionBundle::seal_manifest(&root).unwrap();
    let second = ProductionBundle::seal_manifest(&root).unwrap();
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(second).unwrap()
    );
}

#[test]
fn final_profile_and_result_schema_have_no_skip_or_expected_red_state() {
    let root = repository_root();
    let profile: Value = read_json(&root.join("e2e/profiles/gateway-isolated.json"));
    let result_schema: Value = read_json(&root.join("e2e/schema/p0-gateway-result.schema.json"));
    assert_eq!(profile["completion_policy"], "green_only");
    assert_eq!(profile["launch_mode"], "production_publication_credentials");
    assert!(profile.get("expected_sut_executable_sha256").is_none());
    assert_eq!(
        result_schema["properties"]["scenario_state"]["enum"],
        json!(["green", "red"])
    );
    assert!(result_schema["properties"].get("expected_red").is_none());
    assert!(result_schema["properties"].get("skip").is_none());
    assert_eq!(result_schema["additionalProperties"], false);

    let help = Command::new(env!("CARGO_BIN_EXE_hiroute-e2e"))
        .args(["run", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(!help.contains("expect-status"));
    assert!(!help.contains("expected_red"));
}

#[test]
fn current_collector_contract_has_one_native_terminal_frame() {
    let schema: Value =
        read_json(&repository_root().join("e2e/schema/current-production-collector.schema.json"));
    let lifecycle = schema
        .pointer("/properties/lifecycle/properties")
        .expect("current collector lifecycle schema");
    assert_eq!(lifecycle["last_sequence"]["const"], json!(6));
    assert_eq!(lifecycle["record_count"]["const"], json!(6));
    assert_eq!(lifecycle["records"]["minItems"], json!(6));
    assert_eq!(lifecycle["records"]["maxItems"], json!(6));
    let content = schema
        .pointer("/properties/conversation_content/properties")
        .expect("current collector content schema");
    assert_eq!(content["last_sequence"]["const"], json!(8));
    assert_eq!(content["record_count"]["const"], json!(8));
    assert_eq!(content["records"]["minItems"], json!(8));
    assert_eq!(content["records"]["maxItems"], json!(8));
}

#[test]
fn wrong_sut_revision_is_rejected_after_an_independent_reseal() {
    let copied = copied_e2e();
    let root = copied.path().join("e2e");
    let path = root.join("profiles/gateway-isolated.json");
    let mut profile: Value = read_json(&path);
    profile["sut_source_revision"] = json!("0000000000000000000000000000000000000000");
    write_json(&path, &profile);
    ProductionBundle::write_sealed_manifest(&root).unwrap();
    let error = ProductionBundle::load(
        &root.join("scenarios/p0-gateway.json"),
        Some(&path),
        &root.join("schema"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("sut_source_revision"));
}

#[test]
fn hard_coded_freshness_evidence_is_rejected_after_an_independent_reseal() {
    let copied = copied_e2e();
    let root = copied.path().join("e2e");
    let path = root.join("fixtures/p0-oracle/production-smoke.json");
    let fixture = replace_exact(read_json(&path), "${RUN_CHALLENGE}", "hard-coded");
    write_json(&path, &fixture);
    ProductionBundle::write_sealed_manifest(&root).unwrap();
    let error = ProductionBundle::load(
        &root.join("scenarios/p0-gateway.json"),
        None,
        &root.join("schema"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("fresh challenge"));
}

#[test]
fn missing_or_hard_coded_coverage_cannot_certify_after_an_independent_reseal() {
    let copied = copied_e2e();
    let root = copied.path().join("e2e");
    let path = root.join("scenarios/p0-gateway.json");
    let mut scenario: Value = read_json(&path);
    scenario["coverage"][0]["evidence"]
        .as_array_mut()
        .unwrap()
        .pop();
    write_json(&path, &scenario);
    ProductionBundle::write_sealed_manifest(&root).unwrap();
    let error = ProductionBundle::load(
        &root.join("scenarios/p0-gateway.json"),
        None,
        &root.join("schema"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("coverage/checkpoints"));

    let copied = copied_e2e();
    let root = copied.path().join("e2e");
    let path = root.join("scenarios/p0-gateway.json");
    let mut scenario: Value = read_json(&path);
    scenario["coverage"][0]["passed"] = json!(true);
    write_json(&path, &scenario);
    ProductionBundle::write_sealed_manifest(&root).unwrap();
    let error = ProductionBundle::load(
        &root.join("scenarios/p0-gateway.json"),
        None,
        &root.join("schema"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("schema"));
}

#[test]
fn corrupt_manifested_fixture_fails_before_execution() {
    let copied = copied_e2e();
    let root = copied.path().join("e2e");
    let path = root.join("fixtures/p0-oracle/production-smoke.json");
    let mut fixture: Value = read_json(&path);
    fixture["case"]["providers"][0]["expected_request"]["path"] = json!("/wrong");
    write_json(&path, &fixture);
    let error = ProductionBundle::load(
        &root.join("scenarios/p0-gateway.json"),
        None,
        &root.join("schema"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("artifact digest mismatch"));
}

#[test]
fn wrong_readiness_binary_or_publication_digest_fails_exactly() {
    let (launcher, mut readiness) = readiness_pair();
    verify_readiness_evidence(&launcher, &readiness).unwrap();
    readiness.product.executable_sha256 = DIGEST.replace('0', "2");
    assert!(
        verify_readiness_evidence(&launcher, &readiness)
            .unwrap_err()
            .to_string()
            .contains("exact live launcher child")
    );
    let (launcher, mut readiness) = readiness_pair();
    readiness.product.publication_digest = DIGEST.into();
    assert!(verify_readiness_evidence(&launcher, &readiness).is_err());
}

#[test]
fn self_discovered_binary_and_unowned_pid_cannot_certify_readiness() {
    let (mut launcher, mut readiness) = readiness_pair();
    launcher.executable_sha256 = DIGEST.replace('0', "3");
    readiness.product.executable_sha256 = launcher.executable_sha256.clone();
    assert!(verify_launcher_evidence(&launcher).is_err());

    let (mut launcher, mut readiness) = readiness_pair();
    launcher.child_pid = 8;
    readiness.child_pid = 8;
    assert!(verify_readiness_evidence(&launcher, &readiness).is_err());
}

#[test]
fn forged_build_attestation_cannot_certify_launcher() {
    let (launcher, _) = readiness_pair();
    verify_launcher_evidence(&launcher).unwrap();

    let mut wrong_tree = launcher.clone();
    wrong_tree.build_attestation.sealed_source_tree = "0".repeat(40);
    assert!(verify_launcher_evidence(&wrong_tree).is_err());
    let mut wrong_build_inputs = launcher.clone();
    wrong_build_inputs.build_attestation.build_input_digest = DIGEST.into();
    assert!(verify_launcher_evidence(&wrong_build_inputs).is_err());

    let mut wrong_command = launcher.clone();
    wrong_command.build_attestation.build_command[4] = "forged-gateway".into();
    assert!(verify_launcher_evidence(&wrong_command).is_err());

    let mut unbound_digest = launcher;
    unbound_digest.build_attestation.executable_sha256 = DIGEST.into();
    assert!(verify_launcher_evidence(&unbound_digest).is_err());
}

#[cfg(unix)]
#[test]
fn wrapper_binary_is_rejected_before_launch() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let wrapper = temp.path().join("hirouted-wrapper");
    fs::write(&wrapper, b"#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&wrapper, permissions).unwrap();
    let result = temp.path().join("result.json");
    let root = repository_root();
    let output = Command::new(env!("CARGO_BIN_EXE_hiroute-e2e"))
        .args([
            "run",
            "--profile",
            root.join("e2e/profiles/gateway-isolated.json")
                .to_str()
                .unwrap(),
            "--scenario",
            root.join("e2e/scenarios/p0-gateway.json").to_str().unwrap(),
            "--result",
            result.to_str().unwrap(),
        ])
        .env("HIROUTE_E2E_SUT_BIN", &wrapper)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("SUT source checkout discovery"), "{stderr}");
    assert!(!result.exists());
}

#[test]
fn fixture_or_non_exact_runtime_inputs_cannot_pass_the_launcher_contract() {
    let (mut launcher, _) = readiness_pair();
    verify_launcher_evidence(&launcher).unwrap();
    launcher.arguments.push("--fixture".into());
    assert!(
        verify_launcher_evidence(&launcher)
            .unwrap_err()
            .to_string()
            .contains("normal production")
    );

    let (mut launcher, _) = readiness_pair();
    launcher
        .environment
        .insert("HIROUTE_OBSERVATION_CONTENT_SINK".into(), "panic".into());
    assert!(verify_launcher_evidence(&launcher).is_err());
}

#[test]
fn wrong_native_provider_payload_fails_exactly() {
    let expected = json!({"entries": [{"body": {"model": "native-a"}}], "accepted": 1});
    let actual = json!({"entries": [{"body": {"model": "native-b"}}], "accepted": 1});
    assert!(verify_native_evidence(&expected, &expected).is_ok());
    assert!(
        verify_native_evidence(&expected, &actual)
            .unwrap_err()
            .to_string()
            .contains("Provider payload")
    );
}

#[test]
fn observation_terminal_order_gap_missing_and_corruption_all_fail() {
    let expected = expected_observation();
    let valid = synthetic_observations();
    verify_collector_evidence(&valid, &expected, PUBLICATION_DIGEST, 22_012, FRESHNESS).unwrap();

    let mut hard_coded = valid.clone();
    hard_coded.conversation_content.records[1]["canonical_bytes_base64"] =
        json!(base64::engine::general_purpose::STANDARD.encode("hard-coded-observation"));
    refresh_channel(&mut hard_coded.conversation_content);
    assert!(
        verify_collector_evidence(
            &hard_coded,
            &expected,
            PUBLICATION_DIGEST,
            22_012,
            FRESHNESS,
        )
        .unwrap_err()
        .to_string()
        .contains("fresh challenge")
    );

    let mut wrong_terminal = valid.clone();
    wrong_terminal.execution_fact.records[12]["fact"]["kind"] = json!("attempt_finished");
    refresh_channel(&mut wrong_terminal.execution_fact);
    let error = verify_collector_evidence(
        &wrong_terminal,
        &expected,
        PUBLICATION_DIGEST,
        22_012,
        FRESHNESS,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("terminal") || error.contains("exact product schema"));

    let mut wrong_order = valid.clone();
    wrong_order.conversation_content.records[1]["sequence"] = json!(9);
    assert!(
        verify_collector_evidence(
            &wrong_order,
            &expected,
            PUBLICATION_DIGEST,
            22_012,
            FRESHNESS,
        )
        .is_err()
    );

    let mut gap = valid.clone();
    gap.lifecycle.records[0]["loss_watermark"] =
        json!({"first_sequence": 1, "last_sequence": 1, "reason": "sink_failed"});
    assert!(
        verify_collector_evidence(&gap, &expected, PUBLICATION_DIGEST, 22_012, FRESHNESS).is_err()
    );

    let mut missing = valid.clone();
    missing.otel.records.clear();
    assert!(
        verify_collector_evidence(&missing, &expected, PUBLICATION_DIGEST, 22_012, FRESHNESS)
            .is_err()
    );

    let mut orphan = valid.clone();
    orphan.otel.records[0]["correlation"]["request_id"] = json!("orphan-request");
    refresh_channel(&mut orphan.otel);
    assert!(
        verify_collector_evidence(&orphan, &expected, PUBLICATION_DIGEST, 22_012, FRESHNESS,)
            .unwrap_err()
            .to_string()
            .contains("identity")
    );

    let mut corrupt = valid;
    corrupt.lifecycle.records[0]["schema_version"] = json!("corrupt");
    assert!(
        verify_collector_evidence(&corrupt, &expected, PUBLICATION_DIGEST, 22_012, FRESHNESS,)
            .is_err()
    );
}

#[test]
fn schema_sequence_origin_unknown_fields_and_orphan_content_all_fail() {
    let expected = expected_observation();
    let valid = synthetic_observations();
    verify_collector_evidence(&valid, &expected, PUBLICATION_DIGEST, 22_012, FRESHNESS).unwrap();

    let mut wrong_digest = valid.clone();
    wrong_digest.lifecycle.records[0]["schema_digest"] = json!(DIGEST.replace('0', "4"));
    refresh_channel(&mut wrong_digest.lifecycle);
    assert!(
        verify_collector_evidence(
            &wrong_digest,
            &expected,
            PUBLICATION_DIGEST,
            22_012,
            FRESHNESS,
        )
        .is_err()
    );

    let mut shifted_origin = valid.clone();
    for record in &mut shifted_origin.execution_fact.records {
        let sequence = record["sequence"].as_u64().unwrap();
        record["sequence"] = json!(sequence + 1);
    }
    shifted_origin.execution_fact.first_sequence += 1;
    shifted_origin.execution_fact.last_sequence += 1;
    refresh_channel(&mut shifted_origin.execution_fact);
    assert!(
        verify_collector_evidence(
            &shifted_origin,
            &expected,
            PUBLICATION_DIGEST,
            22_012,
            FRESHNESS,
        )
        .is_err()
    );

    let mut unknown_field = valid.clone();
    unknown_field.lifecycle.records[0]["unsealed"] = json!(true);
    refresh_channel(&mut unknown_field.lifecycle);
    assert!(
        verify_collector_evidence(
            &unknown_field,
            &expected,
            PUBLICATION_DIGEST,
            22_012,
            FRESHNESS,
        )
        .is_err()
    );

    let mut orphan_content = valid;
    let mut extra = orphan_content.conversation_content.records[0].clone();
    extra["direction"] = json!("privacy_unsafe_unknown_direction");
    extra["sequence"] = json!(8);
    extra["event_id"] = json!("content-event-orphan");
    orphan_content.conversation_content.records.push(extra);
    orphan_content.conversation_content.last_sequence = 8;
    orphan_content.conversation_content.record_count = 8;
    refresh_channel(&mut orphan_content.conversation_content);
    assert!(
        verify_collector_evidence(
            &orphan_content,
            &expected,
            PUBLICATION_DIGEST,
            22_012,
            FRESHNESS,
        )
        .is_err()
    );
}

#[test]
fn request_finished_cannot_substitute_for_content_finish() {
    let expected = expected_observation();
    let mut evidence = synthetic_observations();
    evidence.conversation_content.records[3]["phase"] = json!("append");
    refresh_channel(&mut evidence.conversation_content);
    assert!(
        verify_collector_evidence(&evidence, &expected, PUBLICATION_DIGEST, 22_012, FRESHNESS,)
            .unwrap_err()
            .to_string()
            .contains("content terminal")
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an explicitly supplied production SUT; use run-current for current-checkout smoke"]
async fn real_production_listener_smoke_when_a_sut_is_supplied() {
    assert!(
        std::env::var_os("HIROUTE_E2E_SUT_BIN").is_some(),
        "required HIROUTE_E2E_SUT_BIN is missing"
    );
    let bundle = bundle();
    let report = run_production_oracle(
        &bundle,
        ProductionRunOptions {
            sut: bundle.resolve_sut().unwrap(),
            timeout: Duration::from_secs(15),
        },
    )
    .await
    .unwrap();
    assert_eq!(report.scenario_state, ScenarioState::Green);
    assert_eq!(report.process_exit.test_process_code, 0);
    assert_eq!(
        report.readiness.listener_owner_pid_before_probe,
        report.launcher.child_pid
    );
    assert_eq!(
        report.readiness.listener_owner_pid_after_probe,
        report.launcher.child_pid
    );
    assert!(
        !report
            .launcher
            .arguments
            .iter()
            .any(|arg| arg == "--fixture")
    );
    assert!(
        report
            .launcher
            .arguments
            .iter()
            .any(|arg| arg == "--publication")
    );
    assert!(
        report
            .launcher
            .arguments
            .iter()
            .any(|arg| arg == "--credentials")
    );
}

fn expected_observation() -> ExpectedObservation {
    ExpectedObservation {
        lifecycle_terminal: "request_finished".into(),
        execution_terminal: "request_finished".into(),
        content_terminals: vec![
            ContentTerminal {
                direction: "request_input".into(),
                phase: "finish".into(),
            },
            ContentTerminal {
                direction: "response_delivered".into(),
                phase: "finish".into(),
            },
        ],
        required_execution_facts: [
            "route_decision",
            "attempt_started",
            "semantic_commit",
            "attempt_finished",
            "request_finished",
            "usage_and_cache",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        required_otel_signal: "span".into(),
        freshness_binding: "request_and_response_content".into(),
    }
}

fn synthetic_observations() -> CollectorEvidence {
    let request_id = "request-test";
    let lifecycle_records = [
        json!({"kind": "request_accepted", "ingress_protocol": "responses"}),
        json!({"kind": "canonical_request_accepted", "canonicalization_version": "hiroute.model-request-ir/v1"}),
        json!({"kind": "attempt_started", "ordinal": 1}),
        json!({"kind": "response_frame_accepted", "frame_id": "frame-one", "byte_count": 100, "downstream_delivery": "full_frame_transport_accepted"}),
        json!({"kind": "response_frame_accepted", "frame_id": "frame-two", "byte_count": 0, "downstream_delivery": "full_frame_transport_accepted"}),
        json!({"kind": "attempt_finished", "ordinal": 1, "outcome": "accepted"}),
        json!({"kind": "request_finished", "outcome": "accepted"}),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, fact)| {
        fact_record(
            "hiroute.gateway.lifecycle-fact-envelope/v2",
            "lifecycle",
            "lifecycle-producer",
            "lifecycle-epoch",
            "lifecycle-stream",
            request_id,
            index as u64 + 1,
            fact,
        )
    })
    .collect();
    let lifecycle = channel(
        "lifecycle",
        "lifecycle-producer",
        "lifecycle-epoch",
        "lifecycle-stream",
        lifecycle_records,
    );
    let execution = channel(
        "execution_fact",
        "execution-producer",
        "execution-epoch",
        "execution-stream",
        execution_records(request_id),
    );
    let response_start = json!({
        "schema_version": "hiroute.model-stream-event/v1",
        "sequence": 1,
        "event": {"kind": "content_block_started", "index": 0, "block_kind": "text"}
    });
    let response_text = json!({
        "schema_version": "hiroute.model-stream-event/v1",
        "sequence": 2,
        "event": {"kind": "text_delta", "index": 0, "text": FRESHNESS}
    });
    let content_steps = vec![
        json!({"direction": "request_input", "phase": "begin"}),
        json!({
            "direction": "request_input",
            "phase": "append",
            "content_kind": "text",
            "canonical_bytes_base64": base64::engine::general_purpose::STANDARD.encode(FRESHNESS),
        }),
        json!({"direction": "request_input", "phase": "finish", "completeness_delta": "complete"}),
        json!({"direction": "response_delivered", "phase": "begin"}),
        json!({
            "direction": "response_delivered",
            "phase": "append",
            "content_kind": "content_block_started",
            "downstream_delivery": "full_frame_transport_accepted",
            "canonical_bytes_base64": base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&response_start).unwrap()),
        }),
        json!({
            "direction": "response_delivered",
            "phase": "append",
            "content_kind": "text_delta",
            "downstream_delivery": "full_frame_transport_accepted",
            "canonical_bytes_base64": base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&response_text).unwrap()),
        }),
        json!({"direction": "response_delivered", "phase": "finish", "completeness_delta": "complete", "downstream_delivery": "full_frame_transport_accepted"}),
    ];
    let content_records = content_steps
        .into_iter()
        .enumerate()
        .map(|(index, mut extra)| {
            extra["fork_id"] = json!("fork-test");
            base_record(
                "hiroute.observation.conversation-content-envelope/v2",
                "conversation_content",
                "content-producer",
                "content-epoch",
                "content-stream",
                request_id,
                index as u64 + 1,
                extra,
            )
        })
        .collect();
    let content = channel(
        "conversation_content",
        "content-producer",
        "content-epoch",
        "content-stream",
        content_records,
    );
    let otel = channel(
        "otel",
        "otel-producer",
        "otel-epoch",
        "otel-stream",
        ["chat oracle-native-provider", "hiroute.gateway.request"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| otel_record(request_id, index as u64 + 1, name))
            .collect(),
    );
    CollectorEvidence {
        schema_version: "hiroute.e2e.production-collector/v1".into(),
        request_id: request_id.into(),
        lifecycle,
        execution_fact: execution,
        conversation_content: content,
        otel,
        independent_streams: true,
        no_gap_or_loss: true,
        content_terminal_independent: true,
    }
}

fn execution_records(request_id: &str) -> Vec<Value> {
    let facts = vec![
        json!({"kind": "route_decision", "outcome": "ready", "max_attempts": 1}),
        json!({"kind": "candidate_decision", "stable_binding_id": "oracle-native-provider", "ingress_protocol": "responses", "upstream_protocol": "responses"}),
        json!({"kind": "runtime_state"}),
        json!({"kind": "credential_lease"}),
        json!({"kind": "runtime_state"}),
        json!({"kind": "runtime_state"}),
        json!({"kind": "runtime_state"}),
        json!({"kind": "runtime_state"}),
        json!({"kind": "runtime_state"}),
        json!({"kind": "attempt_started", "ordinal": 1, "stable_binding_id": "oracle-native-provider", "request_model": "oracle-native-provider"}),
        json!({"kind": "semantic_commit", "ordinal": 1, "boundary": "full_frame_transport_accepted"}),
        json!({"kind": "attempt_finished", "ordinal": 1, "outcome": "accepted", "provider_http_status": 200}),
        json!({"kind": "request_finished", "outcome": "accepted", "attempts_started": 1, "attempts_finished": 1, "accepted_attempt_ordinal": 1, "facts_completeness": "unknown"}),
        json!({"kind": "usage_and_cache", "ordinal": 1, "input_tokens": 3, "output_tokens": 2}),
    ];
    facts
        .into_iter()
        .enumerate()
        .map(|(index, fact)| {
            let fact = complete_execution_fact(fact);
            let priced = fact["kind"] == "attempt_started";
            let mut record = base_record(
                if priced {
                    "hiroute.observation.execution-fact-envelope/v3"
                } else {
                    "hiroute.observation.execution-fact-envelope/v2"
                },
                "execution_fact",
                "execution-producer",
                "execution-epoch",
                "execution-stream",
                request_id,
                index as u64 + 1,
                json!({
                    "attempt_id": null,
                    "authority_id": "oracle-authority",
                    "authority_epoch": 1,
                    "served_model_id": "oracle-smoke",
                    "selector_source": "trusted_model_alias",
                    "agent_plan_id": "agent-plan-22012",
                    "route": {
                        "kind": "plan",
                        "revision": 22012,
                        "semantic_digest": DIGEST
                    },
                    "plan_display_name": "Oracle smoke",
                    "gateway_publication_revision": "22012",
                    "gateway_publication_digest": PUBLICATION_DIGEST,
                    "grant_id": "oracle-grant",
                    "grant_generation": 1,
                    "ingress_protocol": "responses",
                    "fact": fact,
                }),
            );
            if priced {
                record["schema_digest"] = json!(EXECUTION_PRICED_DIGEST);
                record["pricing"] = json!({
                    "usage_semantics": {
                        "frame_kind": "unknown",
                        "input": "unknown",
                        "output": "unknown",
                        "cache_buckets_exclusive": false
                    },
                    "schema_version": "hiroute.observation.execution-pricing/v1",
                    "request_generation": null,
                    "captured_at_ms": 1,
                    "attempt_execution_at_ms": 2,
                    "quote": null,
                    "reference_quote": null,
                    "unknown_reason": "snapshot_unavailable"
                });
            }
            record
        })
        .collect()
}

fn complete_execution_fact(overrides: Value) -> Value {
    let kind = overrides["kind"].as_str().unwrap();
    let mut fact = match kind {
        "route_decision" => json!({
            "kind": "route_decision", "planner_version": "hiroute-deterministic-planner/v1",
            "plan_id": "agent-plan-22012",
            "route": {"kind": "plan", "revision": 22012, "semantic_digest": DIGEST},
            "input_digest": DIGEST,
            "policy_digest": DIGEST, "output_digest": DIGEST, "branch": "custom_exact_order",
            "complexity": null, "groups": [], "reason_ledger": [],
            "requirements": {
                "text": true, "image_url": false, "image_base64": false, "image_media_types": [],
                "function_tools": false, "strict_tools": false, "parallel_tools": false,
                "tool_choice": {"kind": "auto"}, "tool_result_text": false,
                "tool_result_json": false, "tool_roundtrip": false, "logical_tool_id_mapping": false,
                "provider_state": false, "initial_instructions": false,
                "mid_conversation_instructions": false, "streaming": false, "stream_text": false,
                "stream_reasoning": false, "stream_tool_arguments": false, "stream_usage": false,
                "ingress_protocol": "responses"
            },
            "requested_reasoning_disposition": "absent", "requested_reasoning_value": null,
            "requested_max_output_tokens": null, "stream": false, "outcome": "ready",
            "outcome_code": null, "max_attempts": 1
        }),
        "candidate_decision" => json!({
            "kind": "candidate_decision", "candidate_id": "oracle-native-provider",
            "stable_binding_id": "oracle-native-provider", "group_id": "authorized",
            "declared_order": 0, "profile_digest": DIGEST, "ingress_protocol": "responses",
            "upstream_protocol": "responses", "path_id": "responses-to-responses",
            "provider_id": "provider-oracle-native-provider", "endpoint_id": "endpoint-oracle",
            "entitlement_id": "entitlement-oracle", "connector_id": "builtin-openai",
            "connector_revision": "1", "capability_id": "exact-responses-to-responses",
            "capability_revision": "1", "model_configuration_id": "fixture-model-config-responses",
            "native_model": "oracle-native-provider", "adapter_revision": "builtin-protocol-adapter/v1",
            "serializer_revision": "hiroute-target-json/v1", "decoder_revision": "hiroute-native-response/v1",
            "target_serialized_bytes": 1, "eligible": true, "exclusion_reason": null,
            "reasoning_profile_id": "fixed", "overall_score_tenths": null,
            "effective_cost_micros": null, "api_equivalent_cost_micros": null,
            "cost_class": "free", "cache_cost": {"kind": "none"}, "cache_affinity": false,
            "compute_scope_order": 0, "ranking_reasons": ["PUBLISHED_MANUAL_ORDER"]
        }),
        "credential_lease" => json!({
            "kind": "credential_lease", "stable_binding_id": "oracle-native-provider",
            "credential_ref": "oracle-credential", "key_id": "oracle-key",
            "credential_generation": 1, "excluded_key_count": 0, "outcome": "leased"
        }),
        "runtime_state" => json!({
            "kind": "runtime_state", "operation": "read_exact", "key_scope": "binding",
            "stable_binding_id": "oracle-native-provider", "credential_ref": null, "key_id": null,
            "expected_generation": null, "observed_generation": 0, "health": "active",
            "cooldown_remaining_millis": null, "probe_lease_remaining_millis": null, "outcome": "ok"
        }),
        "attempt_started" => json!({
            "kind": "attempt_started", "ordinal": 1, "candidate_id": "oracle-native-provider",
            "stable_binding_id": "oracle-native-provider", "profile_digest": DIGEST,
            "credential_ref": "oracle-credential", "key_id": "oracle-key",
            "provider_name": "provider-oracle-native-provider", "request_model": "oracle-native-provider",
            "upstream_protocol": "responses", "model_configuration_id": "fixture-model-config-responses",
            "adapter_revision": "builtin-protocol-adapter/v1", "start_reason": "initial_candidate",
            "previous_attempt_id": null
        }),
        "semantic_commit" => json!({
            "kind": "semantic_commit", "ordinal": 1,
            "boundary": "full_frame_transport_accepted", "frame_id": "frame-one"
        }),
        "attempt_finished" => json!({
            "kind": "attempt_finished", "ordinal": 1, "stable_binding_id": "oracle-native-provider",
            "outcome": "accepted", "error_class": null, "retryable": null, "duration_micros": 1,
            "disposition": "accept", "provider_http_status": 200, "provider_code": null,
            "provider_request_id": null, "retry_after_millis": null, "reset_after_millis": null,
            "provider_readiness": "semantic_response", "provider_model_event": "response_complete",
            "time_to_first_model_event_micros": null, "provider_ended_micros_from_start": 1,
            "transport": {"connect_micros": 1, "request_write_micros": 1, "upstream_ttfb_micros": 1,
                "last_upstream_progress_micros_from_start": 1, "local_read_suppressed_micros": 0,
                "upstream_body_bytes": 1, "timeout_kind": null},
            "commits": {"upstream_request": "write_confirmed", "downstream_headers": "write_confirmed",
                "downstream_semantic": "write_confirmed"},
            "stream_outcome": "completed_eos", "downstream_outcome": "completed",
            "cleanup_outcome": "completed", "termination_reason": "accepted_eos"
        }),
        "request_finished" => json!({
            "kind": "request_finished", "outcome": "accepted", "attempts_started": 1,
            "attempts_finished": 1, "accepted_attempt_ordinal": 1, "facts_completeness": "unknown"
        }),
        "usage_and_cache" => json!({
            "kind": "usage_and_cache", "ordinal": 1, "source": "accepted_canonical_model_event",
            "input_tokens": 3, "output_tokens": 2, "billable_tokens": null,
            "cache_read_tokens": null, "cache_write_tokens": null, "reasoning_tokens": null,
            "input_provenance": "reported", "output_provenance": "reported",
            "billable_provenance": "unknown", "cache_read_provenance": "unknown",
            "cache_write_provenance": "unknown", "reasoning_provenance": "unknown",
            "effective_cost_micros": null, "cost_class": "free", "cache_status": "unknown"
        }),
        _ => unreachable!("synthetic execution fact is frozen"),
    };
    fact.as_object_mut()
        .unwrap()
        .extend(overrides.as_object().unwrap().clone());
    fact
}

fn otel_record(request_id: &str, sequence: u64, name: &str) -> Value {
    json!({
        "schema_version": "hiroute.otel.gen-ai-mapping/v1",
        "schema_digest": OTEL_DIGEST,
        "mapper_version": "hiroute.otel-gen-ai-mapper/1",
        "semantic_conventions_version": "1.37.0",
        "production_exporter": "not_installed",
        "producer": {
            "component": "otel", "revision": "test/v1", "producer_id": "otel-producer",
            "producer_epoch": "otel-epoch", "stream_id": "otel-stream"
        },
        "sequence": sequence,
        "event_id": format!("otel-event-{sequence}"),
        "correlation": {
            "workspace_id": "workspace-test", "conversation_id": "conversation-test",
            "session_scope": "request_scoped", "correlation_provenance": "unproven",
            "turn_id": "turn-test", "request_id": request_id
        },
        "signal": {
            "kind": "span", "name": name, "span_kind": "client", "status": "ok",
            "attributes": [], "events": []
        },
        "loss_watermark": null
    })
}

#[allow(clippy::too_many_arguments)]
fn fact_record(
    schema: &str,
    channel: &str,
    producer: &str,
    epoch: &str,
    stream: &str,
    request_id: &str,
    sequence: u64,
    fact: Value,
) -> Value {
    base_record(
        schema,
        channel,
        producer,
        epoch,
        stream,
        request_id,
        sequence,
        json!({"fact": fact}),
    )
}

#[allow(clippy::too_many_arguments)]
fn base_record(
    schema: &str,
    channel: &str,
    producer: &str,
    epoch: &str,
    stream: &str,
    request_id: &str,
    sequence: u64,
    extra: Value,
) -> Value {
    let schema_digest = match channel {
        "lifecycle" => LIFECYCLE_DIGEST,
        "execution_fact" => EXECUTION_DIGEST,
        "conversation_content" => CONTENT_DIGEST,
        "otel" => OTEL_DIGEST,
        _ => unreachable!("synthetic channel is frozen"),
    };
    let mut record = json!({
        "schema_version": schema,
        "schema_digest": schema_digest,
        "channel": channel,
        "producer": {
            "component": channel,
            "revision": "test/v1",
            "producer_id": producer,
            "producer_epoch": epoch,
            "stream_id": stream
        },
        "sequence": sequence,
        "event_id": format!("{channel}-event-{sequence}"),
        "correlation": {
            "workspace_id": "workspace-test",
            "conversation_id": "conversation-test",
            "session_scope": "request_scoped",
            "correlation_provenance": "unproven",
            "turn_id": "turn-test",
            "request_id": request_id,
        },
        "occurred_at_unix_nanos": sequence,
        "loss_watermark": null,
        "completeness_delta": null
    });
    record
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    record
}

fn channel(
    name: &str,
    producer: &str,
    epoch: &str,
    stream: &str,
    records: Vec<Value>,
) -> ChannelEvidence {
    ChannelEvidence {
        name: name.into(),
        file_sha256: DIGEST.into(),
        records_digest: ChannelEvidence::digest_records(&records),
        producer_id: producer.into(),
        producer_epoch: epoch.into(),
        stream_id: stream.into(),
        first_sequence: 1,
        last_sequence: records.len() as u64,
        record_count: records.len(),
        terminal: "test".into(),
        records,
    }
}

fn refresh_channel(channel: &mut ChannelEvidence) {
    channel.records_digest = ChannelEvidence::digest_records(&channel.records);
}

fn readiness_pair() -> (LauncherRecord, ReadinessEvidence) {
    let runtime = "/tmp/hiroute-production-oracle-test";
    let executable_path = "/tmp/hiroute-source/target/debug/hirouted";
    let launcher = LauncherRecord {
        schema_version: "hiroute.e2e.production-launcher/v1".into(),
        mode: "production_publication_credentials".into(),
        executable_path: executable_path.into(),
        executable_sha256: SUT_DIGEST.into(),
        sut_source_revision: "8af977300795c467894371055f7fbfc9716252fa".into(),
        build_attestation: build_attestation(executable_path),
        child_pid: 7,
        listen_address: "127.0.0.1:12345".into(),
        arguments: vec![
            "--role".into(),
            "gateway".into(),
            "--listen".into(),
            "127.0.0.1:12345".into(),
            "--lkg".into(),
            format!("{runtime}/inputs/publication-lkg.json"),
            "--publication".into(),
            format!("{runtime}/inputs/publication.json"),
            "--credentials".into(),
            format!("{runtime}/inputs/credentials.json"),
        ],
        environment: BTreeMap::from([
            ("HIROUTE_E2E_OBSERVATION_CAPTURE".into(), "1".into()),
            (
                "HIROUTE_OBSERVATION_DIRECTORY".into(),
                format!("{runtime}/observation"),
            ),
            ("HIROUTE_OBSERVATION_QUEUE_BYTES".into(), "4194304".into()),
            (
                "HIROUTE_OBSERVATION_LIFECYCLE_SINK".into(),
                "healthy".into(),
            ),
            (
                "HIROUTE_OBSERVATION_EXECUTION_SINK".into(),
                "healthy".into(),
            ),
            ("HIROUTE_OBSERVATION_CONTENT_SINK".into(), "healthy".into()),
            ("HIROUTE_OBSERVATION_OTEL_SINK".into(), "healthy".into()),
        ]),
        publication_digest: PUBLICATION_DIGEST.into(),
        credential_manifest_digest: DIGEST.into(),
        credential_lease_digest: DIGEST.into(),
    };
    let readiness = ReadinessEvidence {
        schema_version: "hiroute.e2e.production-readiness/v1".into(),
        child_pid: 7,
        listen_address: "127.0.0.1:12345".into(),
        probe_path: "/_hiroute/ready".into(),
        product: ProductReady {
            schema_version: "hiroute.gateway.ready/v2".into(),
            status: "ready".into(),
            publication_revision: 22_012,
            publication_digest: PUBLICATION_DIGEST.into(),
            executable_sha256: SUT_DIGEST.into(),
        },
        listener_owner_verifier: "darwin_lsof_tcp_listener/v1".into(),
        listener_owner_pid_before_probe: 7,
        listener_owner_pid_after_probe: 7,
        child_alive_before_probe: true,
        child_alive_after_probe: true,
    };
    (launcher, readiness)
}

fn build_attestation(executable_path: &str) -> SutBuildAttestation {
    let build_nonce = "a".repeat(64);
    SutBuildAttestation {
        schema_version: "hiroute.e2e.sut-build-attestation/v1".into(),
        source_revision: "8af977300795c467894371055f7fbfc9716252fa".into(),
        sealed_source_tree: "5ed527f53fa4e8ba9395b5a89b155a1b67228e6e".into(),
        build_input_digest:
            "sha256:203d9b4b4024b458673c8a1f5573e202476d6b69f03902ed160c0737b4f689b4".into(),
        source_checkout: "/tmp/hiroute-source".into(),
        cargo_package: "hiroute-gateway".into(),
        cargo_binary: "hirouted".into(),
        cargo_profile: "dev".into(),
        enabled_features: vec!["all".into()],
        target_triple: "test-target".into(),
        cargo_version: "cargo test-version".into(),
        rustc_version: "rustc test-version".into(),
        rustc_wrapper: None,
        rustc_wrapper_version: None,
        toolchain_digest: "sha256:9cd60db9faa25b76a6fcf75aedd6ec406051bbbcf662b101cf9eaa0c5ccca661"
            .into(),
        build_nonce: build_nonce.clone(),
        build_command: vec![
            "cargo".into(),
            "rustc".into(),
            "--locked".into(),
            "-p".into(),
            "hiroute-gateway".into(),
            "--bin".into(),
            "hirouted".into(),
            "--profile".into(),
            "dev".into(),
            "--all-features".into(),
            "--message-format=json-render-diagnostics".into(),
            "--".into(),
            "-C".into(),
            format!("metadata=hiroute_e2e_{build_nonce}"),
        ],
        executable_path: executable_path.into(),
        executable_sha256: SUT_DIGEST.into(),
    }
}

fn bundle() -> ProductionBundle {
    let root = repository_root();
    ProductionBundle::load(
        &root.join("e2e/scenarios/p0-gateway.json"),
        Some(&root.join("e2e/profiles/gateway-isolated.json")),
        &root.join("e2e/schema"),
    )
    .unwrap()
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn copied_e2e() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    copy_directory(&repository_root().join("e2e"), &temp.path().join("e2e"));
    temp
}

fn copy_directory(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}

fn replace_exact(value: Value, expected: &str, replacement: &str) -> Value {
    match value {
        Value::String(value) if value == expected => Value::String(replacement.into()),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| replace_exact(value, expected, replacement))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, replace_exact(value, expected, replacement)))
                .collect(),
        ),
        value => value,
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_json(path: &Path, value: &Value) {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
}
