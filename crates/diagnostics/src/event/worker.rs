//! Worker installation, admission and task lifecycle events. Harness text, working
//! directories, commands and task content never appear here. Task and run relationships are
//! carried as domain-separated tokens of the authoritative identifiers, never as the ids.

use serde::{Deserialize, Serialize};

use super::OutcomeKind;
use crate::identity::CorrelationToken;

/// Which local worker harness an event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    Claude,
    Codex,
    Unknown,
}

/// The stable phases of an installation check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStageKind {
    ArtifactMeasure,
    ProbeLoad,
    ProbeSession,
    ProbePrompt,
    Loopback,
    Verify,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallCheckEnd {
    pub harness: HarnessKind,
    pub outcome: OutcomeKind,
    pub permission: PermissionOutcome,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOutcome {
    Granted,
    Denied,
    NotRequired,
    Unknown,
}

/// One installation-check step report. `elapsed_ms` is measured from the step's entry, so a
/// paused step is attributable from the log alone without timing other events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerStage {
    pub harness: HarnessKind,
    pub stage: WorkerStageKind,
    /// Entry is recorded before the blocking call; only a real return or a real observation
    /// inside the running step produces any other outcome.
    pub outcome: WorkerStageOutcome,
    pub elapsed_ms: u64,
}

/// The lifecycle of one step. `Entered` may repeat (bounded by the heartbeat) while the step
/// is still running; the other outcomes are recorded once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStageOutcome {
    /// The step was entered and its blocking call has not returned yet.
    Entered,
    /// The blocking call returned and the step's contract held.
    Completed,
    /// A fact inside the running step was observed; the step itself is still running.
    Observed,
    /// The blocking call returned but the step's contract did not hold.
    Failed { code: WorkerStageFailureCode },
}

/// Stable step-failure vocabulary. It mirrors the domain error variants that can end a probe
/// step; the underlying error text is never recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStageFailureCode {
    /// A selected artifact was missing, unreadable or not executable.
    ArtifactUnavailable,
    /// The step's own bounded contract did not hold (native answer, loopback request, load).
    ContractFailed,
    /// The bounded probe deadline (or an explicit cancellation) ended the step.
    DeadlineExceeded,
    /// A required local capability (process start or stop proof, private probe root) was
    /// unavailable.
    CapabilityUnavailable,
    /// The measured behavior could not produce a verified installation.
    EvidenceRejected,
    /// The failure is outside this stable vocabulary and stays explicitly unknown.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunAdmission {
    pub outcome: AdmissionOutcome,
    /// Token of the authoritative task id; `None` means correlation was unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<CorrelationToken>,
    /// Token of the authoritative run id this admission decided about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<CorrelationToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionOutcome {
    Admitted,
    Rejected,
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerLifecycle {
    pub phase: WorkerLifecyclePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<CorrelationToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<CorrelationToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerLifecyclePhase {
    Spawn,
    Exit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLifecycle {
    pub phase: TaskLifecyclePhase,
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<CorrelationToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<CorrelationToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLifecyclePhase {
    Cancel,
    End,
}

/// Mirrors the product task-state vocabulary; diagnostics never invent a new state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}
