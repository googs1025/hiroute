use std::collections::{BTreeMap, BTreeSet};

use hiroute_application_api::{
    CanonicalDigest, CommandLifecycle, ErrorCode, MachineStatus, SchemaVersion, command_by_id,
    descriptor_digest, released_commands,
};
use hiroute_domain::SideEffectSnapshotV1;
use serde::{Deserialize, Serialize};

use crate::actions::{AssertionKind, DaemonRole, EvidencePolarity};
use crate::runner::ProductEvaluationWitness;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleMode {
    ProductEvaluation,
    AdversarialSelfTest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    ExternalObserver,
    OracleSelfTestFixture,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonExecutable {
    Hirouted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonLaunchResult {
    Started,
    Missing,
    ExitedBeforeReady,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DaemonEvidenceV1 {
    pub origin: EvidenceOrigin,
    pub executable: DaemonExecutable,
    pub role: DaemonRole,
    pub launch_result: DaemonLaunchResult,
    pub executable_digest: CanonicalDigest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryEvidence {
    LocalControlV1,
    InternalApplicationSeam,
    DirectStorage,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlProbeKind {
    UnknownMajor,
    ReservedOperation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlProbeEvidenceV1 {
    pub origin: EvidenceOrigin,
    pub kind: ControlProbeKind,
    pub observed_error: ErrorCode,
    pub before: SideEffectSnapshotV1,
    pub after: SideEffectSnapshotV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CliInvocationEvidenceV1 {
    pub origin: EvidenceOrigin,
    pub command_id: String,
    pub polarity: EvidencePolarity,
    pub status: MachineStatus,
    pub exit_code: u8,
    pub envelope_digest: CanonicalDigest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectExpectation {
    PreviewZero,
    RejectedApplyZero,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SideEffectEvidenceV1 {
    pub origin: EvidenceOrigin,
    pub command_id: String,
    pub expectation: SideEffectExpectation,
    pub before: SideEffectSnapshotV1,
    pub after: SideEffectSnapshotV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GoldenEvidenceV1 {
    pub origin: EvidenceOrigin,
    pub golden_id: String,
    pub expected_digest: CanonicalDigest,
    pub observed_digest: CanonicalDigest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProfile {
    Codex,
    ClaudeCode,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Responses,
    ChatCompletions,
    Messages,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtocolPath {
    pub ingress: Protocol,
    pub upstream: Protocol,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativePayloadEvidenceV1 {
    pub origin: EvidenceOrigin,
    pub path_id: String,
    pub agent_profile: AgentProfile,
    pub protocol_path: ProtocolPath,
    pub expected_payload_digest: CanonicalDigest,
    pub observed_payload_digest: CanonicalDigest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OracleEvidenceV1 {
    pub schema_version: SchemaVersion,
    pub mode: OracleMode,
    pub scenario_id: String,
    pub descriptor_digest: CanonicalDigest,
    pub assertions: BTreeSet<AssertionKind>,
    pub daemon: Option<DaemonEvidenceV1>,
    pub boundary: Option<BoundaryEvidence>,
    pub control_probes: Vec<ControlProbeEvidenceV1>,
    pub cli_invocations: Vec<CliInvocationEvidenceV1>,
    pub side_effects: Vec<SideEffectEvidenceV1>,
    pub goldens: Vec<GoldenEvidenceV1>,
    pub native_payloads: Vec<NativePayloadEvidenceV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivateArtifact<'a> {
    pub artifact_id: &'a str,
    pub bytes: &'a [u8],
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PrivateOracleInputs<'a> {
    pub raw_secrets: Vec<&'a [u8]>,
    pub artifacts: Vec<PrivateArtifact<'a>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FindingCode {
    OracleSchemaIncompatible,
    DescriptorDigestMismatch,
    AssertionMissing,
    FormalDaemonMissing,
    RealCliMissing,
    LocalControlBoundaryMissing,
    InternalSeamBypass,
    FixtureEvidenceForbidden,
    CommandCoverageMissing,
    ExitStatusMismatch,
    SideEffectDetected,
    GoldenEvidenceMissing,
    GoldenDigestMismatch,
    NativePayloadEvidenceMissing,
    NativePayloadMismatch,
    AgentProtocolMismatch,
    ControlProbeMissing,
    ControlProbeMismatch,
    SecretLeak,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OracleFindingV1 {
    pub code: FindingCode,
    pub anchor: String,
    pub details: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OraclePassV1 {
    pub schema_version: SchemaVersion,
    pub scope: String,
    pub scenario_id: String,
    pub descriptor_digest: CanonicalDigest,
    pub verified_assertions: BTreeSet<AssertionKind>,
}

#[derive(Clone, Debug, Default)]
pub struct ProductOracle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VerifiedScope {
    ProductEvaluation,
    OracleSelfTestOnly,
}

impl ProductOracle {
    /// Verify caller-supplied evidence as an Oracle self-test.
    ///
    /// `OracleEvidenceV1` is a serializable DTO, so neither its `mode` nor any nested
    /// `origin` can authorize product completion. The formal Product runner must use a
    /// separate process-bound path once it owns the real daemon/CLI processes and external
    /// side-effect observations. Until that composition exists, a caller requesting
    /// `ProductEvaluation` through this DTO-only entry point is expected-red.
    pub fn verify(
        &self,
        evidence: &OracleEvidenceV1,
        private: &PrivateOracleInputs<'_>,
    ) -> Result<OraclePassV1, Vec<OracleFindingV1>> {
        let mut findings = Vec::new();
        if evidence.mode == OracleMode::ProductEvaluation {
            push(
                &mut findings,
                FindingCode::FixtureEvidenceForbidden,
                "product_witness",
                "serialized mode/origin labels are not a formal-runner process witness",
            );
        }
        self.verify_evidence(
            evidence,
            private,
            VerifiedScope::OracleSelfTestOnly,
            findings,
        )
    }

    /// Verify product evidence that is bound to formal-runner process observations.
    ///
    /// `ProductEvaluationWitness` is non-serializable and has no public constructor. Only
    /// the formal runner module can create one while it owns the real daemon/CLI child
    /// processes and the external side-effect snapshots.
    pub fn verify_product(
        &self,
        witness: &mut ProductEvaluationWitness,
        private: &PrivateOracleInputs<'_>,
    ) -> Result<OraclePassV1, Vec<OracleFindingV1>> {
        let mut findings = witness
            .validate()
            .into_iter()
            .map(|finding| OracleFindingV1 {
                code: FindingCode::FixtureEvidenceForbidden,
                anchor: finding.anchor,
                details: finding.details.to_owned(),
            })
            .collect::<Vec<_>>();
        let evidence = witness.evidence();
        if evidence.mode != OracleMode::ProductEvaluation {
            push(
                &mut findings,
                FindingCode::FixtureEvidenceForbidden,
                "product_witness",
                "formal-runner witness cannot authorize self-test evidence",
            );
        }
        self.verify_evidence(
            evidence,
            private,
            VerifiedScope::ProductEvaluation,
            findings,
        )
    }

    fn verify_evidence(
        &self,
        evidence: &OracleEvidenceV1,
        private: &PrivateOracleInputs<'_>,
        verified_scope: VerifiedScope,
        mut findings: Vec<OracleFindingV1>,
    ) -> Result<OraclePassV1, Vec<OracleFindingV1>> {
        if evidence.schema_version.major != 1 {
            push(
                &mut findings,
                FindingCode::OracleSchemaIncompatible,
                "evidence.schema_version",
                "unknown Product Oracle major",
            );
        }
        if evidence.descriptor_digest != descriptor_digest() {
            push(
                &mut findings,
                FindingCode::DescriptorDigestMismatch,
                "evidence.descriptor_digest",
                "evidence is not bound to the exact command registry",
            );
        }

        for assertion in required_assertions() {
            if !evidence.assertions.contains(assertion) {
                push(
                    &mut findings,
                    FindingCode::AssertionMissing,
                    format!("assertions.{assertion:?}"),
                    "required exact typed assertion is absent",
                );
            }
        }

        match &evidence.daemon {
            Some(daemon)
                if daemon.launch_result == DaemonLaunchResult::Started
                    && daemon.role == DaemonRole::All =>
            {
                check_origin(evidence.mode, daemon.origin, "daemon", &mut findings);
            }
            _ => push(
                &mut findings,
                FindingCode::FormalDaemonMissing,
                "daemon",
                "formal hirouted --role=all did not reach started state",
            ),
        }

        match evidence.boundary {
            Some(BoundaryEvidence::LocalControlV1) => {}
            Some(BoundaryEvidence::InternalApplicationSeam | BoundaryEvidence::DirectStorage) => {
                push(
                    &mut findings,
                    FindingCode::InternalSeamBypass,
                    "boundary",
                    "product evidence bypassed versioned Local Control",
                );
            }
            None => push(
                &mut findings,
                FindingCode::LocalControlBoundaryMissing,
                "boundary",
                "Local Control boundary evidence is absent",
            ),
        }

        for (kind, expected_error) in [
            (
                ControlProbeKind::UnknownMajor,
                ErrorCode::SchemaIncompatible,
            ),
            (
                ControlProbeKind::ReservedOperation,
                ErrorCode::FeatureNotEnabled,
            ),
        ] {
            let matching = evidence
                .control_probes
                .iter()
                .enumerate()
                .filter(|(_, probe)| probe.kind == kind)
                .collect::<Vec<_>>();
            if matching.is_empty() {
                push(
                    &mut findings,
                    FindingCode::ControlProbeMissing,
                    format!("control_probes.{kind:?}"),
                    "required fail-closed Local Control probe is absent",
                );
            }
            for (index, probe) in matching {
                check_origin(
                    evidence.mode,
                    probe.origin,
                    &format!("control_probes[{index}]"),
                    &mut findings,
                );
                if probe.observed_error != expected_error {
                    push(
                        &mut findings,
                        FindingCode::ControlProbeMismatch,
                        format!("control_probes[{index}]"),
                        "Local Control probe returned the wrong stable error code",
                    );
                }
                if !probe.before.changed_channels(&probe.after).is_empty() {
                    push(
                        &mut findings,
                        FindingCode::SideEffectDetected,
                        format!("control_probes[{index}]"),
                        "fail-closed Local Control probe changed an external ledger",
                    );
                }
            }
        }

        if evidence.cli_invocations.is_empty() {
            push(
                &mut findings,
                FindingCode::RealCliMissing,
                "cli_invocations",
                "no real hiroute CLI invocation evidence exists",
            );
        }
        let mut coverage = BTreeMap::<String, BTreeSet<EvidencePolarity>>::new();
        for (index, invocation) in evidence.cli_invocations.iter().enumerate() {
            let anchor = format!("cli_invocations[{index}]");
            check_origin(evidence.mode, invocation.origin, &anchor, &mut findings);
            let released = command_by_id(&invocation.command_id)
                .is_some_and(|command| command.lifecycle == CommandLifecycle::Released);
            if released {
                coverage
                    .entry(invocation.command_id.clone())
                    .or_default()
                    .insert(invocation.polarity);
            }
            if invocation.exit_code != invocation.status.exit_code() {
                push(
                    &mut findings,
                    FindingCode::ExitStatusMismatch,
                    anchor,
                    "machine status and process exit code disagree",
                );
            }
        }
        for descriptor in released_commands() {
            let polarities = coverage.get(&descriptor.command_id);
            for required in [EvidencePolarity::Positive, EvidencePolarity::Negative] {
                if !polarities.is_some_and(|values| values.contains(&required)) {
                    push(
                        &mut findings,
                        FindingCode::CommandCoverageMissing,
                        format!("command_coverage.{}.{required:?}", descriptor.command_id),
                        "released command lacks independently observed positive or negative execution",
                    );
                }
            }
        }

        let mut saw_preview = false;
        let mut saw_rejected_apply = false;
        for (index, side_effect) in evidence.side_effects.iter().enumerate() {
            check_origin(
                evidence.mode,
                side_effect.origin,
                &format!("side_effects[{index}]"),
                &mut findings,
            );
            saw_preview |= side_effect.expectation == SideEffectExpectation::PreviewZero;
            saw_rejected_apply |=
                side_effect.expectation == SideEffectExpectation::RejectedApplyZero;
            let changed = side_effect.before.changed_channels(&side_effect.after);
            if !changed.is_empty() {
                push(
                    &mut findings,
                    FindingCode::SideEffectDetected,
                    format!("side_effects[{index}].{:?}", changed),
                    "a zero-side-effect path changed an independent external ledger",
                );
            }
        }
        if !saw_preview {
            push(
                &mut findings,
                FindingCode::AssertionMissing,
                "side_effects.preview",
                "Preview zero-side-effect evidence is absent",
            );
        }
        if !saw_rejected_apply {
            push(
                &mut findings,
                FindingCode::AssertionMissing,
                "side_effects.rejected_apply",
                "rejected Apply zero-side-effect evidence is absent",
            );
        }

        if evidence.goldens.is_empty() {
            push(
                &mut findings,
                FindingCode::GoldenEvidenceMissing,
                "goldens",
                "no independently observed golden exists",
            );
        }
        for (index, golden) in evidence.goldens.iter().enumerate() {
            check_origin(
                evidence.mode,
                golden.origin,
                &format!("goldens[{index}]"),
                &mut findings,
            );
            if golden.expected_digest != golden.observed_digest {
                push(
                    &mut findings,
                    FindingCode::GoldenDigestMismatch,
                    format!("goldens[{index}]"),
                    "observed golden digest differs from the frozen expected digest",
                );
            }
        }

        for (path_id, profile, protocol_path) in expected_native_paths() {
            let matching = evidence
                .native_payloads
                .iter()
                .filter(|payload| payload.path_id == path_id)
                .collect::<Vec<_>>();
            if matching.len() != 1 {
                push(
                    &mut findings,
                    FindingCode::NativePayloadEvidenceMissing,
                    format!("native_payloads.{path_id}"),
                    "each frozen walking-skeleton protocol path requires exactly one native payload observation",
                );
            } else if matching[0].agent_profile != profile
                || matching[0].protocol_path != protocol_path
            {
                push(
                    &mut findings,
                    FindingCode::AgentProtocolMismatch,
                    format!("native_payloads.{path_id}"),
                    "path identifier, Agent profile, and protocol selection disagree",
                );
            }
        }
        for (index, payload) in evidence.native_payloads.iter().enumerate() {
            let anchor = format!("native_payloads[{index}]");
            check_origin(evidence.mode, payload.origin, &anchor, &mut findings);
            let ingress_matches_profile = matches!(
                (payload.agent_profile, payload.protocol_path.ingress),
                (AgentProfile::Codex, Protocol::Responses)
                    | (AgentProfile::ClaudeCode, Protocol::Messages)
            );
            if !ingress_matches_profile {
                push(
                    &mut findings,
                    FindingCode::AgentProtocolMismatch,
                    &anchor,
                    "Agent profile did not select its frozen ingress protocol",
                );
            }
            if payload.expected_payload_digest != payload.observed_payload_digest {
                push(
                    &mut findings,
                    FindingCode::NativePayloadMismatch,
                    anchor,
                    "renderer/native payload digest differs from the expected typed golden",
                );
            }
        }

        scan_secrets(private, &mut findings);

        findings.sort_by(|left, right| (left.code, &left.anchor).cmp(&(right.code, &right.anchor)));
        findings.dedup_by(|left, right| left.code == right.code && left.anchor == right.anchor);
        if findings.is_empty() {
            Ok(OraclePassV1 {
                schema_version: SchemaVersion::new(1, 0),
                scope: match verified_scope {
                    VerifiedScope::ProductEvaluation => "product_evaluation",
                    VerifiedScope::OracleSelfTestOnly => "oracle_self_test_only",
                }
                .to_owned(),
                scenario_id: evidence.scenario_id.clone(),
                descriptor_digest: evidence.descriptor_digest.clone(),
                verified_assertions: evidence.assertions.clone(),
            })
        } else {
            Err(findings)
        }
    }
}

fn expected_native_paths() -> [(&'static str, AgentProfile, ProtocolPath); 6] {
    [
        (
            "r_to_r",
            AgentProfile::Codex,
            ProtocolPath {
                ingress: Protocol::Responses,
                upstream: Protocol::Responses,
            },
        ),
        (
            "r_to_c",
            AgentProfile::Codex,
            ProtocolPath {
                ingress: Protocol::Responses,
                upstream: Protocol::ChatCompletions,
            },
        ),
        (
            "r_to_m",
            AgentProfile::Codex,
            ProtocolPath {
                ingress: Protocol::Responses,
                upstream: Protocol::Messages,
            },
        ),
        (
            "m_to_r",
            AgentProfile::ClaudeCode,
            ProtocolPath {
                ingress: Protocol::Messages,
                upstream: Protocol::Responses,
            },
        ),
        (
            "m_to_c",
            AgentProfile::ClaudeCode,
            ProtocolPath {
                ingress: Protocol::Messages,
                upstream: Protocol::ChatCompletions,
            },
        ),
        (
            "m_to_m",
            AgentProfile::ClaudeCode,
            ProtocolPath {
                ingress: Protocol::Messages,
                upstream: Protocol::Messages,
            },
        ),
    ]
}

fn required_assertions() -> &'static [AssertionKind] {
    &[
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
    ]
}

fn check_origin(
    mode: OracleMode,
    origin: EvidenceOrigin,
    anchor: &str,
    findings: &mut Vec<OracleFindingV1>,
) {
    if mode == OracleMode::ProductEvaluation && origin != EvidenceOrigin::ExternalObserver {
        push(
            findings,
            FindingCode::FixtureEvidenceForbidden,
            anchor,
            "fixture self-report cannot prove product completion",
        );
    }
}

fn scan_secrets(private: &PrivateOracleInputs<'_>, findings: &mut Vec<OracleFindingV1>) {
    for secret in private
        .raw_secrets
        .iter()
        .copied()
        .filter(|secret| !secret.is_empty())
    {
        for artifact in &private.artifacts {
            if artifact
                .bytes
                .windows(secret.len())
                .any(|window| window == secret)
            {
                push(
                    findings,
                    FindingCode::SecretLeak,
                    format!("private_artifacts.{}", artifact.artifact_id),
                    "raw Secret bytes appeared in a scanned product artifact",
                );
            }
        }
    }
}

fn push(
    findings: &mut Vec<OracleFindingV1>,
    code: FindingCode,
    anchor: impl Into<String>,
    details: impl Into<String>,
) {
    findings.push(OracleFindingV1 {
        code,
        anchor: anchor.into(),
        details: details.into(),
    });
}
