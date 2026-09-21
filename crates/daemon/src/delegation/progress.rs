//! One bounded best-effort progress buffer per owned Worker run.
//!
//! ACP notification delivery only takes the small in-memory mutex. SQLite work is serialized by
//! one outer task and one `spawn_blocking` call at a time; failures declare a segment gap instead
//! of replaying a batch whose commit result may be uncertain.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hiroute_observation::LocalObservationStore;
use hiroute_observation::managed_text::{
    ManagedTextError, ManagedTextProgressTarget, ManagedTextProgressWriteOutcome,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

const PENDING_BYTES: usize = 64 * 1024;
const SIZE_FLUSH_BYTES: usize = 32 * 1024;
const FLUSH_INTERVAL: Duration = Duration::from_secs(10);

pub(crate) trait ProgressBatchWriter: Send + Sync {
    fn write_batch(
        &self,
        text: &str,
        reset_segment: bool,
        now_ms: i64,
    ) -> Result<ManagedTextProgressWriteOutcome, ManagedTextError>;
}

pub(crate) struct ObservationProgressWriter {
    store: Arc<LocalObservationStore>,
    target: ManagedTextProgressTarget,
}

impl ObservationProgressWriter {
    pub(crate) fn new(
        store: Arc<LocalObservationStore>,
        target: ManagedTextProgressTarget,
    ) -> Self {
        Self { store, target }
    }
}

impl ProgressBatchWriter for ObservationProgressWriter {
    fn write_batch(
        &self,
        text: &str,
        reset_segment: bool,
        now_ms: i64,
    ) -> Result<ManagedTextProgressWriteOutcome, ManagedTextError> {
        self.store
            .managed_text_progress_write_batch(&self.target, text, reset_segment, now_ms)
    }
}

#[derive(Clone)]
pub(crate) struct ProgressSink {
    shared: Arc<Shared>,
}

pub(crate) struct ProgressCapture {
    sink: ProgressSink,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

struct Shared {
    state: Mutex<BufferState>,
    wake: Notify,
    size_bytes: usize,
}

struct BufferState {
    pending: String,
    gap_pending: bool,
    size_notified: bool,
    accepting: bool,
}

struct Batch {
    text: String,
    reset_segment: bool,
}

#[derive(Clone, Copy)]
struct Schedule {
    interval: Duration,
    size_bytes: usize,
}

impl ProgressCapture {
    pub(crate) fn start(writer: Arc<dyn ProgressBatchWriter>) -> Self {
        Self::start_with_schedule(
            writer,
            Schedule {
                interval: FLUSH_INTERVAL,
                size_bytes: SIZE_FLUSH_BYTES,
            },
        )
    }

    fn start_with_schedule(writer: Arc<dyn ProgressBatchWriter>, schedule: Schedule) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(BufferState {
                pending: String::new(),
                gap_pending: false,
                size_notified: false,
                accepting: true,
            }),
            wake: Notify::new(),
            size_bytes: schedule.size_bytes,
        });
        let sink = ProgressSink {
            shared: Arc::clone(&shared),
        };
        let stop = CancellationToken::new();
        let task = tokio::spawn(run(shared, Arc::clone(&writer), stop.clone(), schedule));
        Self { sink, stop, task }
    }

    pub(crate) fn sink(&self) -> ProgressSink {
        self.sink.clone()
    }

    /// Stop accepting fragments and perform exactly one final best-effort flush after any current
    /// blocking write returns. The lifecycle awaits this after owned-process cleanup.
    pub(crate) async fn stop_and_flush(self) {
        {
            let mut state = self
                .sink
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.accepting = false;
        }
        self.stop.cancel();
        let _ = self.task.await;
    }
}

impl ProgressSink {
    pub(crate) fn push(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !state.accepting {
            return;
        }
        if text.len() >= PENDING_BYTES {
            let start = suffix_boundary(text, PENDING_BYTES);
            let lost_text = !state.pending.is_empty() || start > 0;
            state.pending.clear();
            state.pending.push_str(&text[start..]);
            state.gap_pending |= lost_text;
        } else {
            let overflow = state
                .pending
                .len()
                .saturating_add(text.len())
                .saturating_sub(PENDING_BYTES);
            if overflow > 0 {
                let cut = next_boundary(&state.pending, overflow);
                state.pending.drain(..cut);
                state.gap_pending = true;
            }
            state.pending.push_str(text);
        }
        if state.pending.len() >= self.shared.size_bytes && !state.size_notified {
            state.size_notified = true;
            self.shared.wake.notify_one();
        }
    }
}

async fn run(
    shared: Arc<Shared>,
    writer: Arc<dyn ProgressBatchWriter>,
    stop: CancellationToken,
    schedule: Schedule,
) {
    let mut next_period = Instant::now() + schedule.interval;
    let mut retry_not_before = Instant::now();
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => {
                flush_once(&shared, &writer).await;
                return;
            }
            _ = sleep_until(next_period) => {
                if Instant::now() >= retry_not_before
                    && !flush_once(&shared, &writer).await
                {
                    retry_not_before = Instant::now() + schedule.interval;
                }
                next_period = Instant::now() + schedule.interval;
            }
            _ = shared.wake.notified() => {
                let should_flush = {
                    let state = shared.state.lock().unwrap_or_else(|error| error.into_inner());
                    state.pending.len() >= schedule.size_bytes
                };
                if should_flush && Instant::now() >= retry_not_before {
                    if !flush_once(&shared, &writer).await {
                        retry_not_before = Instant::now() + schedule.interval;
                    }
                    next_period = Instant::now() + schedule.interval;
                }
            }
        }
    }
}

/// Returns false only for a persistence/clock/join failure. Hidden is a successful privacy stop.
async fn flush_once(shared: &Arc<Shared>, writer: &Arc<dyn ProgressBatchWriter>) -> bool {
    let Some(batch) = take_batch(shared) else {
        return true;
    };
    let Some(now_ms) = now_ms() else {
        mark_failed(shared);
        return false;
    };
    let writer = Arc::clone(writer);
    let result = tokio::task::spawn_blocking(move || {
        writer.write_batch(&batch.text, batch.reset_segment, now_ms)
    })
    .await;
    match result {
        Ok(Ok(ManagedTextProgressWriteOutcome::Hidden)) => {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.pending.clear();
            state.gap_pending = false;
            state.size_notified = false;
            state.accepting = false;
            true
        }
        Ok(Ok(
            ManagedTextProgressWriteOutcome::Committed { .. }
            | ManagedTextProgressWriteOutcome::Noop,
        )) => true,
        Ok(Err(_)) | Err(_) => {
            mark_failed(shared);
            false
        }
    }
}

fn take_batch(shared: &Shared) -> Option<Batch> {
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if state.pending.is_empty() && !state.gap_pending {
        return None;
    }
    let batch = Batch {
        text: std::mem::take(&mut state.pending),
        reset_segment: std::mem::take(&mut state.gap_pending),
    };
    state.size_notified = false;
    Some(batch)
}

fn mark_failed(shared: &Shared) {
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // The in-flight text is never replayed: the commit result may be uncertain. A later successful
    // batch announces the unknown interval by changing segment.
    state.gap_pending = true;
}

fn suffix_boundary(text: &str, max_bytes: usize) -> usize {
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

fn next_boundary(text: &str, at_least: usize) -> usize {
    let mut boundary = at_least.min(text.len());
    while boundary < text.len() && !text.is_char_boundary(boundary) {
        boundary += 1;
    }
    boundary
}

fn now_ms() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

#[cfg(test)]
mod tests;
