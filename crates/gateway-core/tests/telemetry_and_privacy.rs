use std::io::{self, Write};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hiroute_gateway_core::core::execution_plan::{
    AtomicityGroupId, AuthorityId, ConfigBindingPolicy, ConfigBundle, ConfigCellDescriptor,
    ConfigCellGroup, ConfigCellId, ConfigGeneration, ConfigRevision, ImmutableConfig, PlanRevision,
    StableTargetKey,
};
use hiroute_gateway_core::core::publication::{
    InstallerPhase, PrepareOutcome, PublicationInstaller,
};
use hiroute_gateway_core::runtime::attempt::{
    AttemptGeneration, AttemptId, AttemptSnapshot, CommitFence, Disposition, RequestId, WriterState,
};
use hiroute_gateway_core::runtime::body::{BudgetTree, MemoryRole};
use hiroute_gateway_core::runtime::executor::{BoundedExecutor, ExecutorKind, ResumeTiming};
use hiroute_gateway_core::runtime::telemetry::{
    BudgetLevel, Correlation, DispositionFact, DispositionStage, ErrorClass, ErrorFact,
    ExecutorFact, ExecutorOutcome, LifecycleEvent, LifecycleKind, MemoryFact, MetricFamily,
    MetricKey, MetricsCollector, ObservationError, ObservationSink, ProductionObservationSink,
    PublicationFact, PublicationResult, PublicationStage, RequestTelemetry, ResponseFact,
    SidecallFact, SidecallOutcome, Telemetry, TelemetryProjector,
};
use hiroute_gateway_core::test_support::{BootstrapPublicationBuilder, plain_target};
use tokio_util::sync::CancellationToken;

fn correlation(request: u64) -> Correlation {
    Correlation {
        authority_id: AuthorityId::new("control-plane-A").unwrap(),
        authority_epoch: 7,
        config_revision: ConfigRevision(99),
        plan_revision: PlanRevision(41),
        config_generations: Arc::new([]),
        stable_target_key: Some(StableTargetKey::new("stable-target").unwrap()),
        binding_local_id: Some(3),
        request_id: Some(RequestId(request)),
        decision_id: Some(11),
        attempt_id: Some(AttemptId(12)),
        attempt_generation: Some(AttemptGeneration(13)),
    }
}

#[test]
fn all_memory_roles_are_present_and_native_retry_is_explicit_zero() {
    let tree = BudgetTree::new(1024, 1024).unwrap();
    let stream = tree.stream(1024).unwrap();
    let _raw = stream.reserve(MemoryRole::RawRequest, 100).unwrap();
    let events = TelemetryProjector::stream_memory(correlation(1), stream.snapshot().unwrap(), 1);
    assert_eq!(events.len(), MemoryRole::ALL.len());
    let retry = events
        .iter()
        .find_map(|event| match &event.kind {
            LifecycleKind::Memory(fact) if fact.role == MemoryRole::Retry => Some(fact),
            _ => None,
        })
        .unwrap();
    assert_eq!(retry.live_bytes, 0);
    assert_eq!(retry.peak_bytes, 0);
}

#[test]
fn metric_keys_cannot_contain_high_cardinality_correlation() {
    let collector = MetricsCollector::default();
    for request in 1..=100 {
        let event = LifecycleEvent {
            correlation: correlation(request),
            monotonic_nanos: request,
            kind: LifecycleKind::Disposition(DispositionFact {
                stage: DispositionStage::Candidate,
                disposition: Disposition::Accept,
                close_mode: None,
                response_preserving_half_close_capable: false,
                response_preserved: false,
                reset_stream: false,
                connection_teardown: false,
                gate_wait_micros: 0,
            }),
        };
        collector.observe(&event);
    }
    assert_eq!(collector.keys().len(), 1);
    assert_eq!(
        collector.get(MetricKey {
            family: MetricFamily::DispositionStage,
            budget_level: None,
            role: None,
            executor: None,
            executor_outcome: None,
            publication_result: None,
            disposition_stage: Some(DispositionStage::Candidate),
            disposition: Some(Disposition::Accept),
        }),
        100
    );
}

#[test]
fn candidate_gate_and_published_are_distinct_facts() {
    let collector = MetricsCollector::default();
    for stage in [
        DispositionStage::Candidate,
        DispositionStage::GateReady,
        DispositionStage::Published,
    ] {
        collector.observe(&LifecycleEvent {
            correlation: correlation(1),
            monotonic_nanos: 1,
            kind: LifecycleKind::Disposition(DispositionFact {
                stage,
                disposition: Disposition::Accept,
                close_mode: None,
                response_preserving_half_close_capable: false,
                response_preserved: stage == DispositionStage::Published,
                reset_stream: false,
                connection_teardown: false,
                gate_wait_micros: 9,
            }),
        });
    }
    for stage in [
        DispositionStage::Candidate,
        DispositionStage::GateReady,
        DispositionStage::Published,
    ] {
        assert_eq!(
            collector.get(MetricKey {
                family: MetricFamily::DispositionStage,
                budget_level: None,
                role: None,
                executor: None,
                executor_outcome: None,
                publication_result: None,
                disposition_stage: Some(stage),
                disposition: Some(Disposition::Accept),
            }),
            1
        );
    }
}

#[test]
fn low_cardinality_metrics_keep_budget_levels_and_executor_outcomes_distinct() {
    let collector = MetricsCollector::default();
    for level in [
        BudgetLevel::Process,
        BudgetLevel::Worker,
        BudgetLevel::Stream,
    ] {
        collector.observe(&LifecycleEvent {
            correlation: correlation(1),
            monotonic_nanos: 1,
            kind: LifecycleKind::Memory(MemoryFact {
                level,
                role: MemoryRole::RawRequest,
                live_bytes: 1,
                peak_bytes: 2,
                rejections: 0,
                queue_high_water_bytes: 2,
            }),
        });
    }
    for outcome in [ExecutorOutcome::Admitted, ExecutorOutcome::Overloaded] {
        collector.observe(&LifecycleEvent {
            correlation: correlation(1),
            monotonic_nanos: 1,
            kind: LifecycleKind::Executor(ExecutorFact {
                executor: ExecutorKind::SidecallIo,
                outcome,
                queue_depth: 0,
                queue_wait_micros: 0,
                concurrency: 1,
            }),
        });
    }
    let keys = collector.keys();
    assert!(keys.iter().any(|key| {
        key.budget_level == Some(BudgetLevel::Process) && key.family == MetricFamily::MemoryLive
    }));
    assert!(keys.iter().any(|key| {
        key.budget_level == Some(BudgetLevel::Stream) && key.family == MetricFamily::MemoryPeak
    }));
    assert!(keys.iter().any(|key| {
        key.executor_outcome == Some(ExecutorOutcome::Admitted)
            && key.family == MetricFamily::ExecutorOutcome
    }));
    assert!(keys.iter().any(|key| {
        key.executor_outcome == Some(ExecutorOutcome::Overloaded)
            && key.family == MetricFamily::ExecutorOutcome
    }));
}

#[test]
fn io_completion_before_deadline_with_late_resume_is_scheduler_lag() {
    let epoch = Instant::now();
    let io_completed_at = epoch + Duration::from_millis(5);
    let call_deadline = epoch + Duration::from_millis(10);
    let request_resumed_at = epoch + Duration::from_millis(50);
    let fact = SidecallFact::from_resume_timing(
        ResumeTiming {
            io_completed_at,
            request_resumed_at,
            scheduler_resume_lag: request_resumed_at - io_completed_at,
        },
        call_deadline,
        epoch,
    );
    assert_eq!(fact.outcome, SidecallOutcome::SchedulerResumeLag);
    assert_eq!(fact.scheduler_resume_lag_micros, 45_000);
}

struct FailingSink;

impl ObservationSink for FailingSink {
    fn try_emit(&self, _event: LifecycleEvent) -> Result<(), ObservationError> {
        Err(ObservationError::Unavailable)
    }
}

#[test]
fn observation_failure_never_changes_data_plane_result() {
    let telemetry = Telemetry::new(Arc::new(FailingSink));
    let event = LifecycleEvent {
        correlation: correlation(1),
        monotonic_nanos: 1,
        kind: LifecycleKind::Error(ErrorFact {
            class: ErrorClass::DownstreamWrite,
        }),
    };
    telemetry.emit(event);
    telemetry.flush(Duration::from_secs(1)).unwrap();
    assert_eq!(telemetry.sink_failures(), 1);
    let disposition = Disposition::Continue;
    assert_eq!(disposition, Disposition::Continue);
}

struct BlockingSink {
    entered: SyncSender<()>,
    release: Mutex<Receiver<()>>,
}

impl ObservationSink for BlockingSink {
    fn try_emit(&self, _event: LifecycleEvent) -> Result<(), ObservationError> {
        let _ = self.entered.try_send(());
        let _ = self.release.lock().unwrap().recv();
        Ok(())
    }
}

fn error_event(request: u64) -> LifecycleEvent {
    LifecycleEvent {
        correlation: correlation(request),
        monotonic_nanos: request,
        kind: LifecycleKind::Error(ErrorFact {
            class: ErrorClass::DownstreamWrite,
        }),
    }
}

#[test]
fn blocking_sink_and_full_channel_never_block_request_emission() {
    let (entered_tx, entered_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    let telemetry = Telemetry::with_capacity(
        Arc::new(BlockingSink {
            entered: entered_tx,
            release: Mutex::new(release_rx),
        }),
        1,
    )
    .unwrap();
    telemetry.emit(error_event(1));
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    telemetry.emit(error_event(2));
    let started = Instant::now();
    telemetry.emit(error_event(3));
    assert!(started.elapsed() < Duration::from_millis(20));
    assert_eq!(telemetry.dropped_events(), 1);
    release_tx.send(()).unwrap();
    // Release the second queued call as well.
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    release_tx.send(()).unwrap();
    telemetry.flush(Duration::from_secs(1)).unwrap();
}

struct PanickingSink;

impl ObservationSink for PanickingSink {
    fn try_emit(&self, _event: LifecycleEvent) -> Result<(), ObservationError> {
        panic!("sink panic must remain on telemetry worker")
    }
}

#[test]
fn sink_panic_is_isolated_and_worker_continues() {
    let telemetry = Telemetry::with_capacity(Arc::new(PanickingSink), 4).unwrap();
    telemetry.emit(error_event(1));
    telemetry.emit(error_event(2));
    telemetry.flush(Duration::from_secs(1)).unwrap();
    assert_eq!(telemetry.sink_failures(), 2);
}

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn production_sink_emits_structured_log_and_prometheus_metrics() {
    let writer = SharedWriter::default();
    let metrics = Arc::new(MetricsCollector::default());
    let sink = Arc::new(ProductionObservationSink::new(
        writer.clone(),
        Arc::clone(&metrics),
    ));
    let telemetry = Telemetry::new(sink);
    let mut correlated = correlation(88);
    correlated.authority_id = AuthorityId::new("control plane/A").unwrap();
    correlated.stable_target_key = Some(StableTargetKey::new("target A/blue").unwrap());
    correlated.config_generations = Arc::from([(5, ConfigGeneration(6))]);
    telemetry.emit(LifecycleEvent {
        correlation: correlated,
        monotonic_nanos: 123,
        kind: LifecycleKind::Response(ResponseFact { ttfb_micros: 77 }),
    });
    telemetry.flush(Duration::from_secs(1)).unwrap();
    let log = String::from_utf8(writer.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("monotonic_nanos=123"));
    assert!(log.contains("authority_id=control_plane_A"));
    assert!(log.contains("config_generations=5:6"));
    assert!(log.contains("stable_target_key=target_A_blue"));
    assert!(log.contains("binding_local_id=3"));
    assert!(log.contains("request_id=88"));
    assert!(log.contains("decision_id=11"));
    assert!(log.contains("attempt_id=12"));
    assert!(log.contains("attempt_generation=13"));
    assert!(!log.contains("control plane/A"));
    assert!(!log.contains("target A/blue"));
    assert!(log.contains("ResponseFact"));
    assert!(
        metrics
            .render_prometheus()
            .contains("family=\"responsettfbmicros\"")
    );
}

#[test]
fn authoritative_budget_levels_preserve_concurrent_stream_memory() {
    fn memory_key(level: BudgetLevel) -> MetricKey {
        MetricKey {
            family: MetricFamily::MemoryLive,
            budget_level: Some(level),
            role: Some(MemoryRole::RawRequest),
            executor: None,
            executor_outcome: None,
            publication_result: None,
            disposition_stage: None,
            disposition: None,
        }
    }

    let sink = Arc::new(RecordingSink::default());
    let telemetry = Arc::new(Telemetry::new(sink.clone()));
    let tree = BudgetTree::new(4 * 1024 * 1024, 4 * 1024 * 1024).unwrap();
    let stream_a = tree
        .stream_with_telemetry(
            2 * 1024 * 1024,
            RequestTelemetry::new(Arc::clone(&telemetry), correlation(101)),
        )
        .unwrap();
    let stream_b = tree
        .stream_with_telemetry(
            2 * 1024 * 1024,
            RequestTelemetry::new(Arc::clone(&telemetry), correlation(102)),
        )
        .unwrap();
    let collector = MetricsCollector::default();
    let mut consumed = 0;
    let mut project = || {
        telemetry.flush(Duration::from_secs(1)).unwrap();
        let events = sink.events.lock().unwrap();
        for event in &events[consumed..] {
            collector.observe(event);
        }
        consumed = events.len();
    };

    let reservation_a = stream_a
        .reserve(MemoryRole::RawRequest, 1024 * 1024)
        .unwrap();
    project();
    assert_eq!(collector.get(memory_key(BudgetLevel::Process)), 1024 * 1024);
    let reservation_b = stream_b
        .reserve(MemoryRole::RawRequest, 1024 * 1024)
        .unwrap();
    project();
    assert_eq!(
        collector.get(memory_key(BudgetLevel::Process)),
        2 * 1024 * 1024
    );
    assert_eq!(
        collector.get(memory_key(BudgetLevel::Worker)),
        2 * 1024 * 1024
    );
    assert!(
        !collector.keys().contains(&memory_key(BudgetLevel::Stream)),
        "a low-cardinality global gauge must not masquerade as one arbitrary stream",
    );

    drop(reservation_a);
    project();
    assert_eq!(
        collector.get(memory_key(BudgetLevel::Process)),
        1024 * 1024,
        "stream A release must leave stream B in the authoritative aggregate",
    );
    drop(reservation_b);
    project();
    assert_eq!(collector.get(memory_key(BudgetLevel::Process)), 0);
    assert_eq!(collector.get(memory_key(BudgetLevel::Worker)), 0);
}

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<LifecycleEvent>>,
}

impl ObservationSink for RecordingSink {
    fn try_emit(&self, event: LifecycleEvent) -> Result<(), ObservationError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}

#[test]
fn publication_reports_actual_retired_bundle_lease_bytes_generation_and_age() {
    fn group(generation: u64, fill: u8) -> ConfigCellGroup {
        ConfigCellGroup::new(
            [ConfigCellDescriptor {
                id: ConfigCellId(501),
                compatibility_hash: [5; 32],
                atomicity_group: AtomicityGroupId(50),
                binding_policy: ConfigBindingPolicy::RequestPinned,
            }],
            Arc::new(ConfigBundle::new(
                AtomicityGroupId(50),
                std::collections::HashMap::from([(
                    ConfigCellId(501),
                    ImmutableConfig {
                        generation: ConfigGeneration(generation),
                        compatibility_hash: [5; 32],
                        bytes: Arc::from(vec![fill; 64]),
                    },
                )]),
            )),
        )
        .unwrap()
    }

    let sink = Arc::new(RecordingSink::default());
    let telemetry = Arc::new(Telemetry::new(sink.clone()));
    let installer = PublicationInstaller::new().with_telemetry(Arc::clone(&telemetry));
    let first_group = group(1, 1);
    let first_handle = first_group.handle(ConfigCellId(501)).unwrap();
    let lease = first_handle.acquire_request().unwrap();
    let address = "127.0.0.1:8080".parse().unwrap();
    let first = BootstrapPublicationBuilder::new(1, 1)
        .route("gateway.test", "/", 1, plain_target(address, 51))
        .unwrap()
        .config_cells(Arc::new(first_group.handles()))
        .build()
        .unwrap();
    let cancel = CancellationToken::new();
    let prepared = match installer
        .prepare(first, &cancel, Instant::now() + Duration::from_secs(1))
        .unwrap()
    {
        PrepareOutcome::Prepared(prepared) => prepared,
        PrepareOutcome::Duplicate(_) => panic!("first publication cannot be duplicate"),
    };
    installer
        .publish(prepared, &cancel, Instant::now() + Duration::from_secs(1))
        .unwrap();

    std::thread::sleep(Duration::from_millis(2));
    let second_group = group(2, 2);
    let second = BootstrapPublicationBuilder::new(2, 2)
        .route("gateway.test", "/", 1, plain_target(address, 51))
        .unwrap()
        .config_cells(Arc::new(second_group.handles()))
        .build()
        .unwrap();
    let prepared = match installer
        .prepare(second, &cancel, Instant::now() + Duration::from_secs(1))
        .unwrap()
    {
        PrepareOutcome::Prepared(prepared) => prepared,
        PrepareOutcome::Duplicate(_) => panic!("second publication cannot be duplicate"),
    };
    std::thread::sleep(Duration::from_millis(2));
    installer
        .publish(prepared, &cancel, Instant::now() + Duration::from_secs(1))
        .unwrap();
    telemetry.flush(Duration::from_secs(1)).unwrap();

    let events = sink.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Publication(PublicationFact {
            stage: PublicationStage::Published,
            result: PublicationResult::Applied,
            retired_config_bytes: 64,
            retired_generation_count: 1,
            oldest_lease_age_micros,
            lag_micros,
            ..
        }) if oldest_lease_age_micros > 0 && lag_micros > 0
    )));
    drop(events);
    assert_eq!(lease.value().generation, ConfigGeneration(1));
}

#[tokio::test]
async fn production_budget_and_executor_transitions_emit_without_manual_projection() {
    let sink = Arc::new(RecordingSink::default());
    let telemetry = Arc::new(Telemetry::new(sink.clone()));
    let request = RequestTelemetry::new(Arc::clone(&telemetry), correlation(77));

    let tree = BudgetTree::new(1024, 1024).unwrap();
    let stream = tree.stream_with_telemetry(1024, request.clone()).unwrap();
    let reservation = stream.reserve(MemoryRole::RawRequest, 32).unwrap();
    drop(reservation);

    let executor = BoundedExecutor::new(ExecutorKind::Compute, 1, 1)
        .unwrap()
        .with_telemetry(request);
    let completion = executor
        .try_admit(8)
        .unwrap()
        .run_offloaded(
            Instant::now() + Duration::from_secs(1),
            CancellationToken::new(),
            || 7_u8,
        )
        .await
        .unwrap();
    assert_eq!(*completion.value(), 7);

    telemetry.flush(Duration::from_secs(1)).unwrap();

    let events = sink.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Memory(MemoryFact {
            role: MemoryRole::RawRequest,
            live_bytes: 32,
            ..
        })
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Memory(MemoryFact {
            role: MemoryRole::RawRequest,
            live_bytes: 0,
            peak_bytes: 32,
            ..
        })
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Executor(ExecutorFact {
            outcome: ExecutorOutcome::Admitted,
            ..
        })
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Executor(ExecutorFact {
            outcome: ExecutorOutcome::Completed,
            ..
        })
    )));
}

#[test]
fn schema_has_no_slot_for_secret_prompt_response_query_or_private_error() {
    let canary = "Authorization: Bearer SUPER_SECRET?prompt=PRIVATE";
    let event = LifecycleEvent {
        correlation: correlation(1),
        monotonic_nanos: 1,
        kind: LifecycleKind::Error(ErrorFact {
            class: ErrorClass::UpstreamProtocol,
        }),
    };
    let rendered = format!("{event:?}");
    assert!(!rendered.contains(canary));
    assert!(!rendered.contains("SUPER_SECRET"));
    assert!(!rendered.contains("PRIVATE"));

    let sink = Arc::new(RecordingSink::default());
    let telemetry = Telemetry::new(sink.clone());
    telemetry.emit(event);
    telemetry.flush(Duration::from_secs(1)).unwrap();
    assert_eq!(sink.events.lock().unwrap().len(), 1);
}

#[test]
fn attempt_projection_emits_only_authoritative_snapshot_fences() {
    let snapshot = AttemptSnapshot {
        semantic_upstream_calls: 1,
        connection_sub_attempts: 2,
        writer_state: WriterState::QuiescedCancelReset,
        upstream_request_fence: CommitFence::WriteStartedMayHaveCommitted,
        downstream_header_fence: CommitFence::Clear,
        downstream_semantic_fence: CommitFence::Clear,
        accepted_response_scope_created: false,
        published: Some(Disposition::Continue),
        reset_count: 1,
        finalized: true,
    };
    let events = TelemetryProjector::attempt(correlation(1), snapshot, 5);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, LifecycleKind::Commit(_)))
            .count(),
        3
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, LifecycleKind::Disposition(_)))
    );
}

#[test]
fn publication_and_executor_projection_keep_ids_out_of_metric_labels() {
    let publication = TelemetryProjector::publication(
        correlation(0xdead_beef),
        PublicationFact {
            stage: PublicationStage::Prepared,
            result: PublicationResult::Gap,
            installer_phase: InstallerPhase::Rejected,
            lag_micros: 17,
            last_good_preserved: true,
            retired_config_bytes: 23,
            retired_generation_count: 2,
            oldest_lease_age_micros: 29,
        },
        7,
    );
    let collector = MetricsCollector::default();
    collector.observe(&publication);
    assert_eq!(collector.keys().len(), 1);

    let executor = BoundedExecutor::new(ExecutorKind::Compute, 1, 1).unwrap();
    let _admitted = executor.try_admit(1).unwrap();
    assert!(executor.try_admit(1).is_err());
    let facts = TelemetryProjector::executor(
        correlation(0xcafe_babe),
        ExecutorKind::Compute,
        executor.snapshot(),
        8,
    );
    assert!(facts.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Executor(ref fact)
            if fact.outcome == hiroute_gateway_core::runtime::telemetry::ExecutorOutcome::Overloaded
    )));
}
