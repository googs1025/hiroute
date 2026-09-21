use std::process::{Child, Output};

use hiroute_application_api::{CanonicalDigest, MachineEnvelopeV2};
use hiroute_domain::SideEffectSnapshotV1;

use crate::actions::{DaemonRole, EvidencePolarity};
use crate::oracle::{DaemonLaunchResult, OracleEvidenceV1, SideEffectExpectation};

type CliProcessObservation = (String, EvidencePolarity, Output);
type SideEffectObservation = (
    String,
    SideEffectExpectation,
    SideEffectSnapshotV1,
    SideEffectSnapshotV1,
);

/// Non-serializable proof owned by the formal Product runner.
///
/// The witness owns the daemon process handle, real CLI process outputs, and external
/// side-effect snapshots that authorize a product verdict. Its fields intentionally have no
/// public or test-fixture constructor. The formal composition added under this module is the
/// only place that may construct it after launching and observing those resources.
pub struct ProductEvaluationWitness {
    evidence: OracleEvidenceV1,
    daemon_process: Child,
    daemon_role: DaemonRole,
    daemon_executable_digest: CanonicalDigest,
    cli_processes: Vec<CliProcessObservation>,
    side_effect_observations: Vec<SideEffectObservation>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct RunnerWitnessFinding {
    pub(crate) anchor: String,
    pub(crate) details: &'static str,
}

impl ProductEvaluationWitness {
    pub(crate) fn evidence(&self) -> &OracleEvidenceV1 {
        &self.evidence
    }

    pub(crate) fn validate(&mut self) -> Vec<RunnerWitnessFinding> {
        let mut findings = Vec::new();
        let evidence = &self.evidence;

        if let Some(daemon) = &evidence.daemon {
            let process_is_running = self
                .daemon_process
                .try_wait()
                .is_ok_and(|status| status.is_none());
            if daemon.launch_result != DaemonLaunchResult::Started || !process_is_running {
                push(
                    &mut findings,
                    "product_witness.daemon.process",
                    "formal daemon process was not running when the Oracle evaluated it",
                );
            }
            if daemon.role != self.daemon_role
                || daemon.executable_digest != self.daemon_executable_digest
            {
                push(
                    &mut findings,
                    "product_witness.daemon.identity",
                    "daemon evidence is not bound to the runner-owned launch observation",
                );
            }
        }

        if evidence.cli_invocations.len() != self.cli_processes.len() {
            push(
                &mut findings,
                "product_witness.cli_invocations",
                "CLI evidence count differs from runner-owned process observations",
            );
        }
        for (index, (invocation, (command_id, polarity, output))) in evidence
            .cli_invocations
            .iter()
            .zip(&self.cli_processes)
            .enumerate()
        {
            if invocation.command_id != *command_id || invocation.polarity != *polarity {
                push(
                    &mut findings,
                    format!("product_witness.cli_invocations[{index}].identity"),
                    "CLI evidence identity differs from the runner-owned invocation",
                );
            }
            if output.status.code() != Some(i32::from(invocation.exit_code)) {
                push(
                    &mut findings,
                    format!("product_witness.cli_invocations[{index}].exit_status"),
                    "CLI evidence exit code differs from the observed child exit status",
                );
            }
            let observed_envelope =
                serde_json::from_slice::<MachineEnvelopeV2<serde_json::Value>>(&output.stdout);
            if !observed_envelope.is_ok_and(|envelope| envelope.status == invocation.status)
                || CanonicalDigest::of_bytes(&output.stdout) != invocation.envelope_digest
            {
                push(
                    &mut findings,
                    format!("product_witness.cli_invocations[{index}].envelope"),
                    "CLI evidence is not bound to the observed machine-envelope bytes",
                );
            }
        }

        if evidence.side_effects.len() != self.side_effect_observations.len() {
            push(
                &mut findings,
                "product_witness.side_effects",
                "side-effect evidence count differs from external runner observations",
            );
        }
        for (index, (side_effect, (command_id, expectation, before, after))) in evidence
            .side_effects
            .iter()
            .zip(&self.side_effect_observations)
            .enumerate()
        {
            if side_effect.command_id != *command_id || side_effect.expectation != *expectation {
                push(
                    &mut findings,
                    format!("product_witness.side_effects[{index}].identity"),
                    "side-effect evidence identity differs from the runner observation",
                );
            }
            if side_effect.before != *before || side_effect.after != *after {
                push(
                    &mut findings,
                    format!("product_witness.side_effects[{index}].snapshots"),
                    "side-effect evidence differs from externally captured snapshots",
                );
            }
        }

        findings
    }
}

fn push(
    findings: &mut Vec<RunnerWitnessFinding>,
    anchor: impl Into<String>,
    details: &'static str,
) {
    findings.push(RunnerWitnessFinding {
        anchor: anchor.into(),
        details,
    });
}
