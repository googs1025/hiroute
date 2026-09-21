use std::collections::BTreeSet;
use std::path::Path;

#[path = "support/validation_metadata.rs"]
mod validation_metadata;

use hiroute_application_api::{
    CanonicalDigest, ErrorCode, MachineStatus, SchemaVersion, descriptor_digest, released_commands,
};
use hiroute_domain::{EffectChannel, SideEffectSnapshotV1};
use hiroute_product_e2e::{
    AgentProfile, AssertionKind, BoundaryEvidence, CliInvocationEvidenceV1, ControlProbeEvidenceV1,
    ControlProbeKind, DaemonEvidenceV1, DaemonExecutable, DaemonLaunchResult, DaemonRole,
    EvidenceOrigin, EvidencePolarity, FindingCode, GoldenEvidenceV1, NativePayloadEvidenceV1,
    OracleEvidenceV1, OracleMode, PrivateArtifact, PrivateOracleInputs, ProductOracle,
    ProductScenarioV1, Protocol, ProtocolPath, ScenarioProofV1, SideEffectEvidenceV1,
    SideEffectExpectation, generated_product_contract_files, verify_dependency_metadata,
};

fn digest(label: &str) -> CanonicalDigest {
    CanonicalDigest::of_bytes(label.as_bytes())
}

fn all_assertions() -> BTreeSet<AssertionKind> {
    BTreeSet::from([
        AssertionKind::FormalDaemonStarted,
        AssertionKind::RealCliUsed,
        AssertionKind::LocalControlBoundaryUsed,
        AssertionKind::DescriptorDigestExact,
        AssertionKind::VersionMismatchFailsClosed,
        AssertionKind::PreviewHasZeroSideEffects,
        AssertionKind::ApplyRejectionHasZeroSideEffects,
        AssertionKind::ReservedOperationFailsClosed,
        AssertionKind::NativePayloadExact,
        AssertionKind::GoldenExact,
    ])
}

fn invocation(
    command_id: &str,
    polarity: EvidencePolarity,
    status: MachineStatus,
) -> CliInvocationEvidenceV1 {
    CliInvocationEvidenceV1 {
        origin: EvidenceOrigin::OracleSelfTestFixture,
        command_id: command_id.to_owned(),
        polarity,
        status,
        exit_code: status.exit_code(),
        envelope_digest: digest(&format!("{command_id}-{polarity:?}")),
    }
}

fn native_payload(
    path_id: &str,
    agent_profile: AgentProfile,
    ingress: Protocol,
    upstream: Protocol,
) -> NativePayloadEvidenceV1 {
    NativePayloadEvidenceV1 {
        origin: EvidenceOrigin::OracleSelfTestFixture,
        path_id: path_id.to_owned(),
        agent_profile,
        protocol_path: ProtocolPath { ingress, upstream },
        expected_payload_digest: digest(&format!("{path_id}-native-payload")),
        observed_payload_digest: digest(&format!("{path_id}-native-payload")),
    }
}

fn self_test_fixture() -> OracleEvidenceV1 {
    let unchanged = SideEffectSnapshotV1::default();
    OracleEvidenceV1 {
        schema_version: SchemaVersion::new(1, 0),
        mode: OracleMode::AdversarialSelfTest,
        scenario_id: "oracle-adversarial-self-test".to_owned(),
        descriptor_digest: descriptor_digest(),
        assertions: all_assertions(),
        daemon: Some(DaemonEvidenceV1 {
            origin: EvidenceOrigin::OracleSelfTestFixture,
            executable: DaemonExecutable::Hirouted,
            role: DaemonRole::All,
            launch_result: DaemonLaunchResult::Started,
            executable_digest: digest("formal-hirouted-fixture"),
        }),
        boundary: Some(BoundaryEvidence::LocalControlV1),
        control_probes: vec![
            ControlProbeEvidenceV1 {
                origin: EvidenceOrigin::OracleSelfTestFixture,
                kind: ControlProbeKind::UnknownMajor,
                observed_error: ErrorCode::SchemaIncompatible,
                before: unchanged.clone(),
                after: unchanged.clone(),
            },
            ControlProbeEvidenceV1 {
                origin: EvidenceOrigin::OracleSelfTestFixture,
                kind: ControlProbeKind::ReservedOperation,
                observed_error: ErrorCode::FeatureNotEnabled,
                before: unchanged.clone(),
                after: unchanged.clone(),
            },
        ],
        cli_invocations: released_commands()
            .into_iter()
            .flat_map(|descriptor| {
                let command_id = descriptor.command_id;
                [
                    invocation(
                        &command_id,
                        EvidencePolarity::Positive,
                        MachineStatus::Succeeded,
                    ),
                    invocation(
                        &command_id,
                        EvidencePolarity::Negative,
                        MachineStatus::UsageError,
                    ),
                ]
            })
            .collect(),
        side_effects: vec![
            SideEffectEvidenceV1 {
                origin: EvidenceOrigin::OracleSelfTestFixture,
                command_id: "setup.preview".to_owned(),
                expectation: SideEffectExpectation::PreviewZero,
                before: unchanged.clone(),
                after: unchanged.clone(),
            },
            SideEffectEvidenceV1 {
                origin: EvidenceOrigin::OracleSelfTestFixture,
                command_id: "setup.apply".to_owned(),
                expectation: SideEffectExpectation::RejectedApplyZero,
                before: unchanged.clone(),
                after: unchanged,
            },
        ],
        goldens: vec![GoldenEvidenceV1 {
            origin: EvidenceOrigin::OracleSelfTestFixture,
            golden_id: "machine-envelope.schema.list.positive".to_owned(),
            expected_digest: digest("schema-list-golden"),
            observed_digest: digest("schema-list-golden"),
        }],
        native_payloads: vec![
            native_payload(
                "r_to_r",
                AgentProfile::Codex,
                Protocol::Responses,
                Protocol::Responses,
            ),
            native_payload(
                "r_to_c",
                AgentProfile::Codex,
                Protocol::Responses,
                Protocol::ChatCompletions,
            ),
            native_payload(
                "r_to_m",
                AgentProfile::Codex,
                Protocol::Responses,
                Protocol::Messages,
            ),
            native_payload(
                "m_to_r",
                AgentProfile::ClaudeCode,
                Protocol::Messages,
                Protocol::Responses,
            ),
            native_payload(
                "m_to_c",
                AgentProfile::ClaudeCode,
                Protocol::Messages,
                Protocol::ChatCompletions,
            ),
            native_payload(
                "m_to_m",
                AgentProfile::ClaudeCode,
                Protocol::Messages,
                Protocol::Messages,
            ),
        ],
    }
}

fn relabel_all_origins_as_external_observer(evidence: &mut OracleEvidenceV1) {
    if let Some(daemon) = &mut evidence.daemon {
        daemon.origin = EvidenceOrigin::ExternalObserver;
    }
    for probe in &mut evidence.control_probes {
        probe.origin = EvidenceOrigin::ExternalObserver;
    }
    for invocation in &mut evidence.cli_invocations {
        invocation.origin = EvidenceOrigin::ExternalObserver;
    }
    for side_effect in &mut evidence.side_effects {
        side_effect.origin = EvidenceOrigin::ExternalObserver;
    }
    for golden in &mut evidence.goldens {
        golden.origin = EvidenceOrigin::ExternalObserver;
    }
    for payload in &mut evidence.native_payloads {
        payload.origin = EvidenceOrigin::ExternalObserver;
    }
}

fn finding_codes(
    evidence: &OracleEvidenceV1,
    private: &PrivateOracleInputs<'_>,
) -> BTreeSet<FindingCode> {
    ProductOracle
        .verify(evidence, private)
        .unwrap_err()
        .into_iter()
        .map(|finding| finding.code)
        .collect()
}

#[test]
fn oracle_self_test_pass_is_explicitly_not_product_completion() {
    let pass = ProductOracle
        .verify(&self_test_fixture(), &PrivateOracleInputs::default())
        .unwrap();
    assert_eq!(pass.scope, "oracle_self_test_only");
    assert_eq!(pass.verified_assertions, all_assertions());
}

#[test]
fn missing_formal_daemon_is_expected_red() {
    let mut evidence = self_test_fixture();
    evidence.daemon = None;
    assert!(
        finding_codes(&evidence, &PrivateOracleInputs::default())
            .contains(&FindingCode::FormalDaemonMissing)
    );
}

#[test]
fn deleting_an_exact_typed_assertion_fails_the_oracle() {
    let mut evidence = self_test_fixture();
    evidence.assertions.remove(&AssertionKind::GoldenExact);
    assert!(
        finding_codes(&evidence, &PrivateOracleInputs::default())
            .contains(&FindingCode::AssertionMissing)
    );
}

#[test]
fn corrupt_golden_fails_the_oracle() {
    let mut evidence = self_test_fixture();
    evidence.goldens[0].observed_digest = digest("corrupt");
    assert!(
        finding_codes(&evidence, &PrivateOracleInputs::default())
            .contains(&FindingCode::GoldenDigestMismatch)
    );
}

#[test]
fn wrong_native_payload_fails_the_oracle() {
    let mut evidence = self_test_fixture();
    evidence.native_payloads[0].observed_payload_digest = digest("wrong-native-payload");
    assert!(
        finding_codes(&evidence, &PrivateOracleInputs::default())
            .contains(&FindingCode::NativePayloadMismatch)
    );
}

#[test]
fn wrong_reserved_operation_error_fails_the_oracle() {
    let mut evidence = self_test_fixture();
    evidence.control_probes[1].observed_error = ErrorCode::NotImplemented;
    assert!(
        finding_codes(&evidence, &PrivateOracleInputs::default())
            .contains(&FindingCode::ControlProbeMismatch)
    );
}

#[test]
fn wrong_agent_profile_protocol_binding_fails_the_oracle() {
    let mut evidence = self_test_fixture();
    evidence.native_payloads[0].agent_profile = AgentProfile::ClaudeCode;
    assert!(
        finding_codes(&evidence, &PrivateOracleInputs::default())
            .contains(&FindingCode::AgentProtocolMismatch)
    );
}

#[test]
fn side_effect_delta_fails_a_zero_write_assertion() {
    let mut evidence = self_test_fixture();
    evidence.side_effects[0]
        .after
        .generations
        .insert(EffectChannel::SecretStore, 1);
    assert!(
        finding_codes(&evidence, &PrivateOracleInputs::default())
            .contains(&FindingCode::SideEffectDetected)
    );
}

#[test]
fn raw_secret_scan_uses_private_bytes_and_never_persists_them_in_evidence() {
    let evidence = self_test_fixture();
    let private = PrivateOracleInputs {
        raw_secrets: vec![b"fixture-secret"],
        artifacts: vec![PrivateArtifact {
            artifact_id: "cli-stdout",
            bytes: b"redacted=false; value=fixture-secret",
        }],
    };
    assert!(finding_codes(&evidence, &private).contains(&FindingCode::SecretLeak));
    let serialized = serde_json::to_vec(&evidence).unwrap();
    assert!(
        !serialized
            .windows(b"fixture-secret".len())
            .any(|window| window == b"fixture-secret")
    );
}

#[test]
fn internal_service_or_storage_seam_cannot_prove_product_behavior() {
    let mut evidence = self_test_fixture();
    evidence.boundary = Some(BoundaryEvidence::InternalApplicationSeam);
    assert!(
        finding_codes(&evidence, &PrivateOracleInputs::default())
            .contains(&FindingCode::InternalSeamBypass)
    );
}

#[test]
fn public_dto_cannot_mint_product_provenance_by_relabelling_mode_and_every_origin() {
    let mut evidence = self_test_fixture();
    evidence.mode = OracleMode::ProductEvaluation;
    relabel_all_origins_as_external_observer(&mut evidence);

    let findings = ProductOracle
        .verify(&evidence, &PrivateOracleInputs::default())
        .unwrap_err();
    assert_eq!(findings.len(), 1, "all semantic evidence remains valid");
    assert_eq!(findings[0].code, FindingCode::FixtureEvidenceForbidden);
    assert_eq!(findings[0].anchor, "product_witness");
}

#[test]
fn status_and_exit_code_are_checked_as_one_typed_semantic() {
    let mut evidence = self_test_fixture();
    evidence.cli_invocations[0].exit_code = 3;
    assert!(
        finding_codes(&evidence, &PrivateOracleInputs::default())
            .contains(&FindingCode::ExitStatusMismatch)
    );
}

#[test]
fn typed_action_contract_rejects_unknown_command_and_arbitrary_path_fixture() {
    let proof = ScenarioProofV1 {
        proves: vec![AssertionKind::RealCliUsed],
        expected_evidence_digest: digest("proof"),
    };
    let scenario = ProductScenarioV1 {
        schema_version: SchemaVersion::new(1, 0),
        scenario_id: "typed-action-negative".to_owned(),
        descriptor_digest: descriptor_digest(),
        covers: BTreeSet::from([AssertionKind::RealCliUsed]),
        actions: vec![hiroute_product_e2e::ProductActionV1::RunCli {
            command_id: "unknown.command".to_owned(),
            polarity: EvidencePolarity::Negative,
            fixture_ref: "../not-allowed".to_owned(),
            proof,
        }],
    };
    assert!(scenario.validate().is_err());
}

#[test]
fn generated_product_contracts_are_exact_and_collect_no_runtime_session_id() {
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../e2e/product/schema");
    for file in generated_product_contract_files() {
        let actual = std::fs::read_to_string(output.join(file.relative_path))
            .unwrap_or_else(|error| panic!("missing generated {}: {error}", file.relative_path));
        assert_eq!(actual, file.contents, "{} is stale", file.relative_path);
        assert!(!file.contents.contains("\"session_id\""));
    }
}

#[test]
fn workspace_dependency_direction_gate_matches_the_frozen_graph() {
    let metadata = validation_metadata::current();
    if let Err(findings) = verify_dependency_metadata(&metadata) {
        panic!("dependency gate failed: {findings:#?}");
    }
}
