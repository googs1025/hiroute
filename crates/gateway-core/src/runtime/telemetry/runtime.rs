use super::*;

pub trait ObservationSink: Send + Sync {
    fn try_emit(&self, event: LifecycleEvent) -> Result<(), ObservationError>;
}

pub struct Telemetry {
    sender: SyncSender<TelemetryMessage>,
    sink_failures: Arc<AtomicUsize>,
    dropped_events: Arc<AtomicUsize>,
}

enum TelemetryMessage {
    Event(LifecycleEvent),
    Flush(SyncSender<()>),
}

const DEFAULT_TELEMETRY_CAPACITY: usize = 1024;
/// Design #18 explicitly excludes request-stream half-close. Early Accept
/// therefore waits for normal request EOS instead of advertising a transport
/// capability that the gateway does not own.
const REQUEST_HALF_CLOSE_SUPPORTED: bool = false;

impl fmt::Debug for Telemetry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Telemetry")
            .field("sink_failures", &self.sink_failures())
            .field("dropped_events", &self.dropped_events())
            .finish_non_exhaustive()
    }
}

impl Telemetry {
    pub fn new(sink: Arc<dyn ObservationSink>) -> Self {
        Self::with_capacity(sink, DEFAULT_TELEMETRY_CAPACITY)
            .unwrap_or_else(|_| Self::disconnected())
    }

    pub fn with_capacity(
        sink: Arc<dyn ObservationSink>,
        capacity: usize,
    ) -> Result<Self, ObservationError> {
        if capacity == 0 {
            return Err(ObservationError::InvalidCapacity);
        }
        let (sender, receiver) = sync_channel(capacity);
        let sink_failures = Arc::new(AtomicUsize::new(0));
        let worker_failures = Arc::clone(&sink_failures);
        thread::Builder::new()
            .name("hiroute-telemetry".into())
            .spawn(move || telemetry_worker(receiver, sink, worker_failures))
            .map_err(|_| ObservationError::Unavailable)?;
        Ok(Self {
            sender,
            sink_failures,
            dropped_events: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn disconnected() -> Self {
        let (sender, receiver) = sync_channel(1);
        drop(receiver);
        Self {
            sender,
            sink_failures: Arc::new(AtomicUsize::new(1)),
            dropped_events: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Observation is write-only and best-effort. Sink failure cannot change
    /// a data-plane result or disposition.
    pub fn emit(&self, event: LifecycleEvent) {
        if let Err(error) = self.sender.try_send(TelemetryMessage::Event(event)) {
            match error {
                TrySendError::Full(_) => {
                    self.dropped_events.fetch_add(1, Ordering::Relaxed);
                }
                TrySendError::Disconnected(_) => {
                    self.sink_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Control-plane/test drain only. Runtime owners never call this blocking
    /// method on the request path.
    pub fn flush(&self, timeout: Duration) -> Result<(), ObservationError> {
        let (acknowledge, acknowledged) = sync_channel(1);
        self.sender
            .try_send(TelemetryMessage::Flush(acknowledge))
            .map_err(|_| ObservationError::Unavailable)?;
        acknowledged
            .recv_timeout(timeout)
            .map_err(|_| ObservationError::Unavailable)
    }

    pub fn sink_failures(&self) -> usize {
        self.sink_failures.load(Ordering::Acquire)
    }

    pub fn dropped_events(&self) -> usize {
        self.dropped_events.load(Ordering::Acquire)
    }
}

fn telemetry_worker(
    receiver: Receiver<TelemetryMessage>,
    sink: Arc<dyn ObservationSink>,
    sink_failures: Arc<AtomicUsize>,
) {
    while let Ok(message) = receiver.recv() {
        match message {
            TelemetryMessage::Event(event) => {
                let result = catch_unwind(AssertUnwindSafe(|| sink.try_emit(event)));
                if !matches!(result, Ok(Ok(()))) {
                    sink_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
            TelemetryMessage::Flush(acknowledge) => {
                let _ = acknowledge.try_send(());
            }
        }
    }
}

/// Request-scoped, non-blocking observation handle. It carries only bounded,
/// pre-sanitized correlation fields; runtime owners can therefore emit at the
/// transition point without accepting paths, headers, prompts, bodies, or raw
/// provider errors.
#[derive(Clone)]
pub struct RequestTelemetry {
    telemetry: Arc<Telemetry>,
    correlation: Correlation,
    epoch: Instant,
    first_byte_emitted: Arc<AtomicBool>,
}

impl fmt::Debug for RequestTelemetry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestTelemetry")
            .field("plan_revision", &self.correlation.plan_revision)
            .field("request_id", &self.correlation.request_id)
            .finish_non_exhaustive()
    }
}

impl RequestTelemetry {
    pub fn new(telemetry: Arc<Telemetry>, correlation: Correlation) -> Self {
        Self {
            telemetry,
            correlation,
            epoch: Instant::now(),
            first_byte_emitted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_decision(&self, decision_id: u64) -> Self {
        let mut correlation = self.correlation.clone();
        correlation.decision_id = Some(decision_id);
        Self {
            telemetry: Arc::clone(&self.telemetry),
            correlation,
            epoch: self.epoch,
            first_byte_emitted: Arc::clone(&self.first_byte_emitted),
        }
    }

    pub fn with_attempt(
        &self,
        decision_id: u64,
        stable_target_key: StableTargetKey,
        binding_local_id: u32,
        attempt_id: AttemptId,
        attempt_generation: AttemptGeneration,
        config_generations: Arc<[(u64, ConfigGeneration)]>,
    ) -> Self {
        let mut correlation = self.correlation.clone();
        correlation.decision_id = Some(decision_id);
        correlation.stable_target_key = Some(stable_target_key);
        correlation.binding_local_id = Some(binding_local_id);
        correlation.attempt_id = Some(attempt_id);
        correlation.attempt_generation = Some(attempt_generation);
        correlation.config_generations = config_generations;
        Self {
            telemetry: Arc::clone(&self.telemetry),
            correlation,
            epoch: self.epoch,
            // TTFB is an Attempt fact, so every attempt gets its own emission
            // fence even though the request correlation owner is shared.
            first_byte_emitted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn correlation(&self) -> &Correlation {
        &self.correlation
    }

    pub fn emit_kind(&self, kind: LifecycleKind) {
        self.telemetry.emit(LifecycleEvent {
            correlation: self.correlation.clone(),
            monotonic_nanos: nanos_since(self.epoch, Instant::now()),
            kind,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn disposition(
        &self,
        stage: DispositionStage,
        disposition: Disposition,
        close_mode: Option<RequestCloseMode>,
        response_preserved: bool,
        reset_stream: bool,
        connection_teardown: bool,
        gate_wait: Duration,
    ) {
        self.emit_kind(LifecycleKind::Disposition(DispositionFact {
            stage,
            disposition,
            close_mode,
            response_preserving_half_close_capable: REQUEST_HALF_CLOSE_SUPPORTED,
            response_preserved,
            reset_stream,
            connection_teardown,
            gate_wait_micros: saturating_micros(gate_wait),
        }));
    }

    pub fn commit(&self, fence: FenceKind, state: CommitFence, connection_sub_attempt: usize) {
        self.emit_kind(LifecycleKind::Commit(CommitFact {
            fence,
            state,
            connection_sub_attempt,
        }));
    }

    pub fn release(&self, point: ReleasePoint, latency: Duration) {
        self.emit_kind(LifecycleKind::Release(ReleaseFact {
            point,
            latency_from_terminal_micros: saturating_micros(latency),
        }));
    }

    pub fn memory_role(&self, role: MemoryRole, snapshot: BudgetMemorySnapshot) {
        let facts = [
            (
                BudgetLevel::Process,
                snapshot.process_role_live[role as usize],
                snapshot.process_role_peak[role as usize],
                snapshot.process_rejected,
            ),
            (
                BudgetLevel::Worker,
                snapshot.worker_role_live[role as usize],
                snapshot.worker_role_peak[role as usize],
                snapshot.worker_rejected,
            ),
            (
                BudgetLevel::Stream,
                snapshot.stream.role_live[role as usize],
                snapshot.stream.role_peak[role as usize],
                snapshot.stream.rejected,
            ),
        ];
        for (level, live_bytes, peak_bytes, rejections) in facts {
            self.emit_kind(LifecycleKind::Memory(MemoryFact {
                level,
                role,
                live_bytes,
                peak_bytes,
                rejections,
                queue_high_water_bytes: peak_bytes,
            }));
        }
    }

    pub fn body(
        &self,
        direction: BodyDirection,
        plan: &BodyPlan,
        admitted_bytes: usize,
        queue_high_water_bytes: usize,
    ) {
        self.emit_kind(LifecycleKind::Body(BodyFact {
            direction,
            mode: plan.into(),
            admitted_bytes,
            queue_high_water_bytes,
        }));
    }

    pub fn upstream_first_byte(&self, ttfb: Duration) {
        if self
            .first_byte_emitted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.emit_kind(LifecycleKind::Response(ResponseFact {
                ttfb_micros: saturating_micros(ttfb),
            }));
        }
    }

    pub fn config_lease(
        &self,
        cell_id: u64,
        generation: ConfigGeneration,
        acquire_scope: ConfigAcquireScope,
    ) {
        self.config_lease_event(
            cell_id,
            generation,
            acquire_scope,
            ConfigLeaseStage::Acquired,
            Duration::ZERO,
        );
    }

    pub fn config_lease_released(
        &self,
        cell_id: u64,
        generation: ConfigGeneration,
        acquire_scope: ConfigAcquireScope,
        release_latency: Duration,
    ) {
        self.config_lease_event(
            cell_id,
            generation,
            acquire_scope,
            ConfigLeaseStage::Released,
            release_latency,
        );
    }

    fn config_lease_event(
        &self,
        cell_id: u64,
        generation: ConfigGeneration,
        acquire_scope: ConfigAcquireScope,
        stage: ConfigLeaseStage,
        release_latency: Duration,
    ) {
        self.emit_kind(LifecycleKind::ConfigLease(ConfigLeaseFact {
            cell_id,
            generation,
            acquire_scope,
            stage,
            release_latency_micros: saturating_micros(release_latency),
        }));
    }

    pub fn cleanup(
        &self,
        kind: CleanupKind,
        latency: Duration,
        timed_out: bool,
        connection_teardown: bool,
    ) {
        self.emit_kind(LifecycleKind::Cleanup(CleanupFact {
            kind,
            latency_micros: saturating_micros(latency),
            timed_out,
            response_preserving_half_close_capable: REQUEST_HALF_CLOSE_SUPPORTED,
            connection_teardown,
        }));
    }

    pub fn scope(
        &self,
        scope_kind: ScopeKind,
        scope_id: ScopeId,
        phase: ScopePhase,
        finalize_count: usize,
        latency: Duration,
    ) {
        self.emit_kind(LifecycleKind::Scope(ScopeFact {
            scope_kind,
            scope_id,
            phase,
            finalize_count,
            latency_micros: saturating_micros(latency),
        }));
    }

    pub fn executor(
        &self,
        kind: ExecutorKind,
        outcome: ExecutorOutcome,
        snapshot: ExecutorSnapshot,
        queue_wait: Duration,
    ) {
        self.emit_kind(LifecycleKind::Executor(ExecutorFact {
            executor: kind,
            outcome,
            queue_depth: snapshot.queued,
            queue_wait_micros: saturating_micros(queue_wait),
            concurrency: snapshot.running,
        }));
    }

    pub fn error(&self, class: ErrorClass) {
        self.emit_kind(LifecycleKind::Error(ErrorFact { class }));
    }
}
