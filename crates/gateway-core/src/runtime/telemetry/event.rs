use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Correlation {
    pub authority_id: AuthorityId,
    pub authority_epoch: u64,
    pub config_revision: ConfigRevision,
    pub plan_revision: PlanRevision,
    pub config_generations: Arc<[(u64, ConfigGeneration)]>,
    pub stable_target_key: Option<StableTargetKey>,
    pub binding_local_id: Option<u32>,
    pub request_id: Option<RequestId>,
    pub decision_id: Option<u64>,
    pub attempt_id: Option<AttemptId>,
    pub attempt_generation: Option<AttemptGeneration>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleEvent {
    pub correlation: Correlation,
    pub monotonic_nanos: u64,
    pub kind: LifecycleKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleKind {
    Publication(PublicationFact),
    ConfigLease(ConfigLeaseFact),
    Memory(MemoryFact),
    Release(ReleaseFact),
    Disposition(DispositionFact),
    Commit(CommitFact),
    Executor(ExecutorFact),
    Sidecall(SidecallFact),
    Scope(ScopeFact),
    Error(ErrorFact),
    Body(BodyFact),
    Response(ResponseFact),
    Cleanup(CleanupFact),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PublicationStage {
    Discovered,
    Fetched,
    Prepared,
    Published,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PublicationResult {
    Applied,
    Duplicate,
    Stale,
    Conflict,
    Gap,
    ResyncRequired,
    Cancelled,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationFact {
    pub stage: PublicationStage,
    pub result: PublicationResult,
    pub installer_phase: InstallerPhase,
    pub lag_micros: u64,
    pub last_good_preserved: bool,
    pub retired_config_bytes: usize,
    pub retired_generation_count: usize,
    pub oldest_lease_age_micros: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigLeaseFact {
    pub cell_id: u64,
    pub generation: ConfigGeneration,
    pub acquire_scope: ConfigAcquireScope,
    pub stage: ConfigLeaseStage,
    pub release_latency_micros: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConfigLeaseStage {
    Acquired,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConfigAcquireScope {
    Connection,
    Request,
    Attempt,
    Phase,
    Event,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BudgetLevel {
    Process,
    Worker,
    Stream,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryFact {
    pub level: BudgetLevel,
    pub role: MemoryRole,
    pub live_bytes: usize,
    pub peak_bytes: usize,
    pub rejections: usize,
    pub queue_high_water_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BodyMode {
    PassThrough,
    StreamingReplay,
    BufferedTransform,
    SseFramedStreaming,
}

impl From<&BodyPlan> for BodyMode {
    fn from(plan: &BodyPlan) -> Self {
        match plan {
            BodyPlan::PassThrough { .. } => Self::PassThrough,
            BodyPlan::StreamingReplay { .. } => Self::StreamingReplay,
            BodyPlan::BufferedTransform { .. } => Self::BufferedTransform,
            BodyPlan::SseFramedStreaming { .. } => Self::SseFramedStreaming,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BodyFact {
    pub direction: BodyDirection,
    pub mode: BodyMode,
    pub admitted_bytes: usize,
    pub queue_high_water_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseFact {
    pub ttfb_micros: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CleanupKind {
    WriterJoin,
    TransportReset,
    AcceptedRelease,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupFact {
    pub kind: CleanupKind,
    pub latency_micros: u64,
    pub timed_out: bool,
    pub response_preserving_half_close_capable: bool,
    pub connection_teardown: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReleasePoint {
    LastRawConsumer,
    WireSourceConsumedOrCancelled,
    TransportSendOrReset,
    RequestWriterQuiesced,
    LastRequestBodyLeaseDrop,
    ModelIrReleased,
    ScopeFinalized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseFact {
    pub point: ReleasePoint,
    pub latency_from_terminal_micros: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DispositionStage {
    Candidate,
    GateReady,
    AcceptBlocked,
    Published,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispositionFact {
    pub stage: DispositionStage,
    pub disposition: Disposition,
    pub close_mode: Option<RequestCloseMode>,
    pub response_preserving_half_close_capable: bool,
    pub response_preserved: bool,
    pub reset_stream: bool,
    pub connection_teardown: bool,
    pub gate_wait_micros: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FenceKind {
    UpstreamAttemptRequest,
    DownstreamFinalHeaders,
    DownstreamSemanticOutput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitFact {
    pub fence: FenceKind,
    pub state: CommitFence,
    pub connection_sub_attempt: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExecutorOutcome {
    Admitted,
    Completed,
    Overloaded,
    Cancelled,
    Panicked,
    LateResultDropped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutorFact {
    pub executor: ExecutorKind,
    pub outcome: ExecutorOutcome,
    pub queue_depth: usize,
    pub queue_wait_micros: u64,
    pub concurrency: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SidecallOutcome {
    CompletedInTime,
    BackendTimeout,
    SchedulerResumeLag,
    Cancelled,
    Overloaded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SidecallFact {
    pub outcome: SidecallOutcome,
    pub queue_micros: u64,
    pub connect_micros: u64,
    pub io_completed_nanos: Option<u64>,
    pub request_resumed_nanos: Option<u64>,
    pub scheduler_resume_lag_micros: u64,
}

impl SidecallFact {
    pub fn from_resume_timing(
        timing: ResumeTiming,
        call_deadline: Instant,
        epoch: Instant,
    ) -> Self {
        let completed_in_time = timing.io_completed_at <= call_deadline;
        Self {
            outcome: if !completed_in_time {
                SidecallOutcome::BackendTimeout
            } else if !timing.scheduler_resume_lag.is_zero() {
                SidecallOutcome::SchedulerResumeLag
            } else {
                SidecallOutcome::CompletedInTime
            },
            queue_micros: 0,
            connect_micros: 0,
            io_completed_nanos: Some(nanos_since(epoch, timing.io_completed_at)),
            request_resumed_nanos: Some(nanos_since(epoch, timing.request_resumed_at)),
            scheduler_resume_lag_micros: saturating_micros(timing.scheduler_resume_lag),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeFact {
    pub scope_kind: ScopeKind,
    pub scope_id: ScopeId,
    pub phase: ScopePhase,
    pub finalize_count: usize,
    pub latency_micros: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScopePhase {
    Headers,
    Body,
    Paused,
    Finalized,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ErrorClass {
    PublicationFence,
    InvalidBinding,
    BodyLimit,
    SseFraming,
    UpstreamConnect,
    UpstreamProtocol,
    DownstreamWrite,
    ExecutorOverload,
    Cancelled,
    Deadline,
    CallbackPanic,
}

/// Carries only a closed low-cardinality class. Raw provider errors, paths,
/// query strings, prompts, responses, credentials, and authorization values
/// have no field in the schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorFact {
    pub class: ErrorClass,
}
