use std::collections::HashMap;
use std::error::Error;
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hiroute_gateway_core::core::execution_plan::{
    AtomicityGroupId, ConfigBindingPolicy, ConfigBundle, ConfigCellDescriptor, ConfigCellHandle,
    ConfigCellId, ConfigGeneration, ImmutableConfig,
};
use hiroute_gateway_core::core::publication::{PrepareOutcome, PublicationInstaller};
use hiroute_gateway_core::runtime::body::{
    BodyPlan, BudgetTree, ChargedBytes, MemoryRole, RawRequestBodyStore,
};
use hiroute_gateway_core::runtime::executor::{BoundedExecutor, ChildScope, ExecutorKind};
use hiroute_gateway_core::runtime::sse::{
    BoundedEventEmitter, BoundedOutputSink, EofPolicy, SseError, SseEventView, SseFramer,
    SseLimits, SseVisitor,
};
use hiroute_gateway_core::test_support::{BootstrapPublicationBuilder, plain_target};
use tokio_util::sync::CancellationToken;

fn main() -> Result<(), Box<dyn Error>> {
    let iterations = std::env::var("HIROUTE_RESOURCE_BENCH_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1_000)
        .max(10);
    println!("resource\tvariant\titerations\telapsed_ns\tns_per_operation\tdetail");
    publication_and_config(iterations)?;
    body_paths()?;
    sse_paths()?;
    executor_paths(iterations.min(100))?;
    Ok(())
}

fn publication_and_config(iterations: usize) -> Result<(), Box<dyn Error>> {
    let installer = PublicationInstaller::new();
    publish(&installer, envelope(1, 1)?)?;
    measure("publication", "active-bind", iterations, || {
        black_box(installer.bind_request()?);
        Ok(())
    })?;

    let descriptor = ConfigCellDescriptor {
        id: ConfigCellId(1),
        compatibility_hash: [7; 32],
        atomicity_group: AtomicityGroupId(1),
        binding_policy: ConfigBindingPolicy::EventLive,
    };
    let initial = bundle(1);
    let cell = ConfigCellHandle::new(descriptor, Arc::clone(&initial))?;
    measure("config", "event-live-acquire", iterations, || {
        black_box(cell.acquire_event()?.value().generation);
        Ok(())
    })?;
    let weak_initial = Arc::downgrade(&initial);
    drop(initial);
    cell.publish(bundle(2))?;
    if weak_initial.upgrade().is_some() {
        return Err("retired EventLive generation remained pinned".into());
    }

    let storm_start = Instant::now();
    for revision in 2..=101 {
        publish(&installer, envelope(revision, revision)?)?;
    }
    let storm_elapsed = storm_start.elapsed();
    print_row(
        "publication",
        "100-revision-storm",
        100,
        storm_elapsed,
        "retired roots released by segmented request bindings",
    );
    Ok(())
}

fn body_paths() -> Result<(), Box<dyn Error>> {
    for size in [1024_usize, 4 * 1024 * 1024] {
        let tree = BudgetTree::new(size * 2, size * 2)?;
        let budget = tree.stream(size * 2)?;
        let start = Instant::now();
        let mut raw = RawRequestBodyStore::new(BodyPlan::BufferedTransform {
            max_body_bytes: size,
        })?;
        raw.push(ChargedBytes::from_exact_vec(
            &budget,
            MemoryRole::RawRequest,
            vec![0_u8; size],
        )?)?;
        let model = raw.into_model_backing()?;
        let ttfb = start.elapsed();
        let peak = budget.snapshot()?.peak;
        drop(model);
        if budget.snapshot()?.live != 0 {
            return Err("buffered body accounting did not return to zero".into());
        }
        print_row(
            "body",
            if size == 1024 {
                "buffered-1KiB-readiness"
            } else {
                "buffered-4MiB-readiness"
            },
            1,
            ttfb,
            &format!("retained_peak_bytes={peak}"),
        );
    }
    Ok(())
}

fn sse_paths() -> Result<(), Box<dyn Error>> {
    for size in [64_usize, 4 * 1024, 64 * 1024] {
        for fragmentation in [
            Fragmentation::Whole,
            Fragmentation::Byte,
            Fragmentation::PseudoRandom,
        ] {
            let tree = BudgetTree::new(2 * 1024 * 1024, 2 * 1024 * 1024)?;
            let budget = tree.stream(1024 * 1024)?;
            let mut event = b"data: ".to_vec();
            event.resize(size.saturating_sub(2), b'x');
            event.extend_from_slice(b"\n\n");
            let mut framer = SseFramer::new(sse_limits(), budget.clone())?;
            let mut visitor = TransformVisitor::new(Transform::Pass);
            let mut sink = CountingSink::default();
            let start = Instant::now();
            feed_fragmented(
                &mut framer,
                &budget,
                &mut visitor,
                &mut sink,
                &event,
                fragmentation,
            )?;
            let elapsed = start.elapsed();
            let complexity = framer.complexity();
            if complexity.scanned_bytes > event.len()
                || complexity.copied_bytes > event.len().saturating_mul(3)
            {
                return Err("SSE complexity exceeded the linear bound".into());
            }
            print_row(
                "sse",
                fragmentation.name(),
                event.len(),
                elapsed,
                &format!(
                    "event_bytes={};scanned={};copied={};allocations={}",
                    event.len(),
                    complexity.scanned_bytes,
                    complexity.copied_bytes,
                    complexity.allocations
                ),
            );
        }
    }

    for transform in [
        Transform::Pass,
        Transform::OneToMany,
        Transform::Drop,
        Transform::Replace,
    ] {
        let tree = BudgetTree::new(1024 * 1024, 1024 * 1024)?;
        let budget = tree.stream(1024 * 1024)?;
        let mut framer = SseFramer::new(sse_limits(), budget.clone())?;
        let mut visitor = TransformVisitor::new(transform);
        let mut sink = CountingSink::default();
        let start = Instant::now();
        feed_fragmented(
            &mut framer,
            &budget,
            &mut visitor,
            &mut sink,
            b"data: benchmark\n\n",
            Fragmentation::Whole,
        )?;
        print_row(
            "sse-transform",
            transform.name(),
            1,
            start.elapsed(),
            &format!("output_units={};output_bytes={}", sink.units, sink.bytes),
        );
    }
    Ok(())
}

fn executor_paths(iterations: usize) -> Result<(), Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let fast = measure("executor", "gateway-fast-zero-hop", iterations, || {
            black_box(1_u64.wrapping_add(1));
            Ok(())
        });
        fast?;
        for kind in [
            ExecutorKind::SidecallIo,
            ExecutorKind::Compute,
            ExecutorKind::BlockingControl,
        ] {
            for depth in [1_usize, 8, 64] {
                let executor = BoundedExecutor::new(kind, 1, depth)?;
                let mut queue = Duration::ZERO;
                let mut run = Duration::ZERO;
                let mut resume = Duration::ZERO;
                for _ in 0..iterations {
                    let scope = ChildScope::new(Instant::now() + Duration::from_secs(1));
                    let admission = executor.try_admit(0)?;
                    let completion = match kind {
                        ExecutorKind::SidecallIo => {
                            scope.run(&executor, admission, async { 1_u8 }).await?
                        }
                        ExecutorKind::Compute | ExecutorKind::BlockingControl => {
                            scope
                                .run_offloaded(&executor, admission, || 1_u8)
                                .await?
                        }
                        ExecutorKind::GatewayIo => unreachable!("gateway fast path is measured above"),
                    };
                    queue += completion.started_at.saturating_duration_since(completion.queued_at);
                    run += completion
                        .io_completed_at
                        .saturating_duration_since(completion.started_at);
                    let (_, timing) = scope.resume(&executor, completion)?;
                    resume += timing.scheduler_resume_lag;
                    scope.cancel_and_finalize(Duration::from_secs(1)).await?;
                }
                println!(
                    "executor\t{:?}-depth-{depth}\t{iterations}\t{}\t{}\tqueue_mean_ns={};run_mean_ns={};resume_mean_ns={}",
                    kind,
                    (queue + run + resume).as_nanos(),
                    (queue + run + resume).as_nanos() / iterations as u128,
                    queue.as_nanos() / iterations as u128,
                    run.as_nanos() / iterations as u128,
                    resume.as_nanos() / iterations as u128,
                );
            }
        }
        Ok::<(), Box<dyn Error>>(())
    })
}

fn envelope(
    plan: u64,
    revision: u64,
) -> Result<
    hiroute_gateway_core::core::publication::CompiledGatewayPublicationEnvelope,
    Box<dyn Error>,
> {
    Ok(BootstrapPublicationBuilder::new(plan, revision)
        .route(
            "benchmark.test",
            "/",
            1,
            plain_target("127.0.0.1:18080".parse()?, revision),
        )?
        .build()?)
}

fn publish(
    installer: &PublicationInstaller,
    envelope: hiroute_gateway_core::core::publication::CompiledGatewayPublicationEnvelope,
) -> Result<(), Box<dyn Error>> {
    let cancel = CancellationToken::new();
    let prepared =
        match installer.prepare(envelope, &cancel, Instant::now() + Duration::from_secs(1))? {
            PrepareOutcome::Prepared(prepared) => prepared,
            PrepareOutcome::Duplicate(_) => {
                return Err("unexpected duplicate benchmark input".into());
            }
        };
    installer.publish(prepared, &cancel, Instant::now() + Duration::from_secs(1))?;
    Ok(())
}

fn bundle(generation: u64) -> Arc<ConfigBundle> {
    Arc::new(ConfigBundle::new(
        AtomicityGroupId(1),
        HashMap::from([(
            ConfigCellId(1),
            ImmutableConfig {
                generation: ConfigGeneration(generation),
                compatibility_hash: [7; 32],
                bytes: Arc::from(&b"compact"[..]),
            },
        )]),
    ))
}

fn measure(
    resource: &str,
    variant: &str,
    iterations: usize,
    mut operation: impl FnMut() -> Result<(), Box<dyn Error>>,
) -> Result<Duration, Box<dyn Error>> {
    let start = Instant::now();
    for _ in 0..iterations {
        operation()?;
    }
    let elapsed = start.elapsed();
    print_row(resource, variant, iterations, elapsed, "");
    Ok(elapsed)
}

fn print_row(resource: &str, variant: &str, iterations: usize, elapsed: Duration, detail: &str) {
    println!(
        "{resource}\t{variant}\t{iterations}\t{}\t{}\t{detail}",
        elapsed.as_nanos(),
        elapsed.as_nanos() / iterations.max(1) as u128
    );
}

fn sse_limits() -> SseLimits {
    SseLimits {
        max_event_bytes: 64 * 1024,
        max_pending_bytes: 64 * 1024,
        max_output_event_bytes: 128 * 1024,
        expansion_ratio_numerator: 2,
        expansion_ratio_denominator: 1,
        expansion_slack_bytes: 64,
        retained_capacity_threshold: 1024,
        eof_policy: EofPolicy::Strict,
    }
}

#[derive(Clone, Copy)]
enum Fragmentation {
    Whole,
    Byte,
    PseudoRandom,
}

impl Fragmentation {
    fn name(self) -> &'static str {
        match self {
            Self::Whole => "whole",
            Self::Byte => "byte-fragmented",
            Self::PseudoRandom => "pseudo-random-fragmented",
        }
    }
}

fn feed_fragmented(
    framer: &mut SseFramer,
    budget: &hiroute_gateway_core::runtime::body::StreamBudget,
    visitor: &mut TransformVisitor,
    sink: &mut CountingSink,
    event: &[u8],
    fragmentation: Fragmentation,
) -> Result<(), Box<dyn Error>> {
    let mut cursor = 0;
    let mut state = 0x9e37_79b9_u64;
    while cursor < event.len() {
        let width = match fragmentation {
            Fragmentation::Whole => event.len(),
            Fragmentation::Byte => 1,
            Fragmentation::PseudoRandom => {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                (state as usize % 257) + 1
            }
        };
        let end = cursor.saturating_add(width).min(event.len());
        framer.feed(
            ChargedBytes::copy_from_opaque(
                budget,
                MemoryRole::TransportInflight,
                &event[cursor..end],
            )?,
            end == event.len(),
            visitor,
            sink,
        )?;
        cursor = end;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Transform {
    Pass,
    OneToMany,
    Drop,
    Replace,
}

impl Transform {
    fn name(self) -> &'static str {
        match self {
            Self::Pass => "1:1-pass",
            Self::OneToMany => "1:N",
            Self::Drop => "drop",
            Self::Replace => "replace",
        }
    }
}

struct TransformVisitor {
    transform: Transform,
}

impl TransformVisitor {
    fn new(transform: Transform) -> Self {
        Self { transform }
    }
}

impl SseVisitor for TransformVisitor {
    fn on_event(
        &mut self,
        event: SseEventView<'_>,
        emitter: &mut BoundedEventEmitter<'_, '_>,
    ) -> Result<(), SseError> {
        match self.transform {
            Transform::Pass => emitter.pass_raw(),
            Transform::OneToMany => {
                emit_budgeted(emitter, event.raw())?;
                emit_budgeted(emitter, b"data: derived\n\n")
            }
            Transform::Drop => emitter.drop_event(),
            Transform::Replace => emit_budgeted(emitter, b"data: replacement\n\n"),
        }
    }
}

fn emit_budgeted(emitter: &mut BoundedEventEmitter<'_, '_>, bytes: &[u8]) -> Result<(), SseError> {
    let mut builder = emitter.output_builder(bytes.len())?;
    builder
        .extend_from_slice(bytes)
        .map_err(|_| SseError::BudgetExceeded)?;
    emitter.emit_owned(builder.finish())
}

#[derive(Default)]
struct CountingSink {
    units: usize,
    bytes: usize,
}

impl BoundedOutputSink for CountingSink {
    fn emit_borrowed(&mut self, bytes: &[u8]) -> Result<(), SseError> {
        self.units += 1;
        self.bytes += bytes.len();
        Ok(())
    }

    fn emit_owned(&mut self, bytes: ChargedBytes) -> Result<(), SseError> {
        self.units += 1;
        self.bytes += bytes.bytes().len();
        Ok(())
    }
}
