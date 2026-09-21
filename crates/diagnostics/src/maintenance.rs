//! The per-role maintenance thread: settings watch and bounded heartbeat events.
//!
//! Only a runtime that owns its role's writer lock spawns this thread, and it never writes
//! the role's files: the only writer is the writer thread that holds `writer.lock`. It
//! sleeps in short slices so shutdown is noticed within [`POLL_SLICE`] and stops after the
//! writer had its bounded chance to drain.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::context::Emitter;
use crate::error::SubsystemReason;
use crate::event::{DiagnosticEvent, DiagnosticsDegraded, LevelApplied};
use crate::runtime::{StageReporter, WorkerStageReporter};
use crate::settings::SettingsStore;
use crate::shared::Shared;
use crate::writer::{WriterCompletion, WriterHandle};

/// How often the settings file is re-read.
pub(crate) const WATCH_INTERVAL: Duration = Duration::from_millis(1000);
/// How often the bounded summary heartbeat runs.
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(5000);
/// Sleep slice so a shutdown flag is noticed quickly without busy waiting.
const POLL_SLICE: Duration = Duration::from_millis(50);
/// The maintenance thread waits at most this long for the writer's bounded drain.
const WRITER_FINAL_WAIT: Duration = Duration::from_millis(1000);

/// Everything the maintenance thread needs from the runtime that armed it.
pub(crate) struct MaintenanceContext {
    pub shared: Arc<Shared>,
    pub emitter: Option<Arc<Emitter>>,
    pub stages: StageReporter,
    pub worker_stages: WorkerStageReporter,
    pub settings: Arc<SettingsStore>,
    pub override_active: bool,
    pub writer: Option<WriterHandle>,
    pub completion: Arc<WriterCompletion>,
}

pub(crate) fn spawn(context: MaintenanceContext) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("hiroute-diagnostics-maintenance".to_string())
        .spawn(move || run(context))
        .ok()
}

fn run(context: MaintenanceContext) {
    let mut last_watch = Instant::now();
    let mut last_heartbeat = Instant::now() - HEARTBEAT_INTERVAL;
    let mut last_drop_summary = Instant::now() - HEARTBEAT_INTERVAL;
    let mut last_writer_stats = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut last_writer_health = None;
    loop {
        if context.shared.shutdown.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(POLL_SLICE);
        if context.shared.shutdown.load(Ordering::SeqCst) {
            break;
        }
        // The heartbeat is checked on the watch cadence, so the first summary lands one
        // interval after the role armed instead of inside the first poll slice.
        if last_watch.elapsed() < WATCH_INTERVAL {
            continue;
        }
        last_watch = Instant::now();
        watch_settings(&context);
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            last_heartbeat = Instant::now();
            emit_writer_health(&context, &mut last_writer_health);
            emit_heartbeat(&context, &mut last_drop_summary);
            emit_stage_progress(&context);
            let stats = (
                context.shared.writer_counters.rotated(),
                context.shared.writer_counters.flushes(),
                context.shared.writer_counters.bytes_written(),
                context.shared.writer_counters.write_failures(),
                context.shared.writer_counters.lost_at_shutdown(),
            );
            if stats != last_writer_stats {
                last_writer_stats = stats;
                if let Some(emitter) = context.emitter.as_deref() {
                    emitter.emit(
                        DiagnosticEvent::WriterStats(crate::event::WriterStats {
                            rotated: stats.0,
                            flushes: stats.1,
                            bytes_written: stats.2,
                            write_failures: stats.3,
                            lost_at_shutdown: stats.4,
                        }),
                        None,
                        None,
                    );
                }
            }
        }
    }
    // The final counters belong to the writer's own bounded drain; this thread only waits
    // for it so the runtime's exit budget covers both without blocking the caller on I/O.
    if let Some(writer) = &context.writer {
        writer.completion.wait(WRITER_FINAL_WAIT);
    }
    context.completion.mark_finished();
}

/// Re-read the settings file. A damaged file degrades the report and keeps the last known
/// values; a new revision is applied and recorded promptly.
fn watch_settings(context: &MaintenanceContext) {
    let snapshot = context.settings.load();
    let level_source = snapshot.level_source();
    let error = snapshot.error.map(SubsystemReason::from_settings_error);
    let previous = context.shared.persisted().and_then(|view| view.error);
    context
        .shared
        .set_persisted_read(snapshot.settings.revision, snapshot.settings.level, error);
    if error != previous
        && let Some(reason) = error
        && let Some(emitter) = context.emitter.as_deref()
    {
        emitter.emit(
            DiagnosticEvent::DiagnosticsDegraded(DiagnosticsDegraded {
                reason,
                os_errno: None,
            }),
            None,
            None,
        );
    }
    if error.is_some() || context.override_active {
        return;
    }
    let (_, _, current_revision) = context.shared.level_state();
    if snapshot.settings.revision == current_revision {
        return;
    }
    context.shared.set_level_state(
        snapshot.settings.level,
        level_source,
        snapshot.settings.revision,
    );
    if let Some(emitter) = context.emitter.as_deref() {
        emitter.set_level(snapshot.settings.level, snapshot.settings.revision);
        emitter.emit(
            DiagnosticEvent::LevelApplied(LevelApplied {
                level: snapshot.settings.level,
                revision: snapshot.settings.revision,
                source: level_source,
            }),
            None,
            None,
        );
    }
}

/// One bounded event when the writer's health changes to a fault, so a write-side failure
/// is visible in the log instead of only in memory. Recovery clears the state silently;
/// the counters in `WriterStats` show the accumulated damage.
fn emit_writer_health(context: &MaintenanceContext, last: &mut Option<SubsystemReason>) {
    let current = context.shared.writer_health.snapshot();
    if current == *last {
        return;
    }
    *last = current;
    if let (Some(reason), Some(emitter)) = (current, context.emitter.as_deref()) {
        emitter.emit(
            DiagnosticEvent::DiagnosticsDegraded(DiagnosticsDegraded {
                reason,
                os_errno: None,
            }),
            None,
            None,
        );
    }
}

fn emit_heartbeat(context: &MaintenanceContext, last_drop_summary: &mut Instant) {
    let dropped = context.shared.counters.dropped();
    let rejected = context.shared.counters.rejected();
    if dropped == 0 && rejected == 0 {
        return;
    }
    if last_drop_summary.elapsed() < HEARTBEAT_INTERVAL {
        return;
    }
    *last_drop_summary = Instant::now();
    if let Some(emitter) = context.emitter.as_deref() {
        emitter.emit(
            DiagnosticEvent::DroppedSummary(crate::event::DroppedSummary {
                dropped_events: dropped,
                rejected_events: rejected,
                window_ms: HEARTBEAT_INTERVAL.as_millis() as u64,
            }),
            None,
            None,
        );
    }
}

fn emit_stage_progress(context: &MaintenanceContext) {
    if let Some((stage, started, budget_ms)) = context.stages.current()
        && let Some(emitter) = context.emitter.as_deref()
    {
        emitter.emit(
            DiagnosticEvent::StageProgress(crate::event::StageProgress {
                stage,
                elapsed_ms: started.elapsed().as_millis() as u64,
                budget_ms,
            }),
            None,
            None,
        );
    }
    // A Worker installation step that is still blocked republishes itself here, so its step
    // stays attributable without the blocked thread having to run.
    context.worker_stages.progress();
}
