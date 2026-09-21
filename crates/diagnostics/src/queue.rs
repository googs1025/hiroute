//! A bounded, non-blocking event queue.
//!
//! Callers serialize a record and try to reserve its encoded length in the queue. The
//! reservation fails immediately when the event count or byte budget is exhausted; a
//! request thread never waits for the writer or for disk I/O. Bytes are released when the
//! writer takes an event and when a push is rejected.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::level::DiagnosticLevel;

/// Maximum queued events per process.
pub const MAX_QUEUE_EVENTS: usize = 1024;
/// Maximum queued bytes per process (encoded records, including newline).
pub const MAX_QUEUE_BYTES: usize = 1024 * 1024;

/// One queued record. Its severity is carried beside the encoded bytes so the queue can
/// apply the level that becomes known while the record is still waiting for the writer.
#[derive(Debug)]
struct QueuedRecord {
    severity: DiagnosticLevel,
    bytes: Vec<u8>,
}

#[derive(Debug, Default)]
struct QueueState {
    events: VecDeque<QueuedRecord>,
    bytes: usize,
    closed: bool,
    high_water_events: usize,
    high_water_bytes: usize,
    pushed: u64,
    popped: u64,
}

#[derive(Debug)]
pub struct BoundedQueue {
    state: Mutex<QueueState>,
    available: Condvar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushError {
    /// The queue count or byte budget is exhausted.
    Full,
    /// The writer is gone; records would accumulate without a consumer.
    Closed,
}

#[derive(Debug, Clone)]
pub struct QueueSender {
    queue: Arc<BoundedQueue>,
}

#[derive(Debug)]
pub struct QueueReceiver {
    queue: Arc<BoundedQueue>,
}

/// Create a queue plus its single consumer side.
pub fn bounded_queue() -> (QueueSender, QueueReceiver) {
    let queue = Arc::new(BoundedQueue {
        state: Mutex::new(QueueState::default()),
        available: Condvar::new(),
    });
    (
        QueueSender {
            queue: queue.clone(),
        },
        QueueReceiver { queue },
    )
}

impl QueueSender {
    /// Reserve and push one encoded record. Fails immediately when the queue is full; the
    /// caller counts the drop. `bytes.len()` is the encoded record length, so the byte
    /// budget is exact.
    pub fn try_push(&self, severity: DiagnosticLevel, bytes: Vec<u8>) -> Result<(), PushError> {
        let mut state = self.queue.state.lock().expect("diagnostic queue lock");
        if state.closed {
            return Err(PushError::Closed);
        }
        if state.events.len() >= MAX_QUEUE_EVENTS || state.bytes + bytes.len() > MAX_QUEUE_BYTES {
            return Err(PushError::Full);
        }
        state.bytes += bytes.len();
        state.events.push_back(QueuedRecord { severity, bytes });
        state.high_water_events = state.high_water_events.max(state.events.len());
        state.high_water_bytes = state.high_water_bytes.max(state.bytes);
        state.pushed += 1;
        drop(state);
        self.queue.available.notify_one();
        Ok(())
    }

    /// Drop the queued records the given level does not admit and return how many were
    /// dropped. Used exactly once, when the effective level becomes known before the
    /// writer starts: records that were admitted while no level was known are then judged
    /// by the same threshold the process will use.
    pub fn discard_below(&self, level: DiagnosticLevel) -> usize {
        let mut state = self.queue.state.lock().expect("diagnostic queue lock");
        let before = state.events.len();
        let mut bytes = 0usize;
        state.events.retain(|record| {
            let keep = level.admits(record.severity);
            if !keep {
                bytes += record.bytes.len();
            }
            keep
        });
        state.bytes -= bytes;
        before - state.events.len()
    }

    /// Mark the queue closed so future pushes fail fast during shutdown.
    pub fn close(&self) {
        let mut state = self.queue.state.lock().expect("diagnostic queue lock");
        state.closed = true;
        drop(state);
        self.queue.available.notify_all();
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        let state = self.queue.state.lock().expect("diagnostic queue lock");
        QueueSnapshot {
            events: state.events.len(),
            bytes: state.bytes,
            high_water_events: state.high_water_events,
            high_water_bytes: state.high_water_bytes,
            pushed: state.pushed,
            popped: state.popped,
        }
    }
}

impl QueueReceiver {
    /// Wait up to `timeout` for the next record. Returns `None` on timeout or when the
    /// queue is closed and drained.
    pub fn pop_timeout(&self, timeout: Duration) -> Option<Vec<u8>> {
        let mut state = self.queue.state.lock().expect("diagnostic queue lock");
        if state.events.is_empty() && !state.closed {
            let (next, _) = self
                .queue
                .available
                .wait_timeout(state, timeout)
                .expect("diagnostic queue lock");
            state = next;
        }
        match state.events.pop_front() {
            Some(record) => {
                state.bytes -= record.bytes.len();
                state.popped += 1;
                Some(record.bytes)
            }
            None => None,
        }
    }

    /// Drain everything currently queued without waiting.
    pub fn drain(&self) -> Vec<Vec<u8>> {
        let mut state = self.queue.state.lock().expect("diagnostic queue lock");
        let drained: Vec<Vec<u8>> = state.events.drain(..).map(|record| record.bytes).collect();
        state.bytes = 0;
        state.popped += drained.len() as u64;
        drained
    }

    pub fn is_closed(&self) -> bool {
        self.queue
            .state
            .lock()
            .expect("diagnostic queue lock")
            .closed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueSnapshot {
    pub events: usize,
    pub bytes: usize,
    pub high_water_events: usize,
    pub high_water_bytes: usize,
    pub pushed: u64,
    pub popped: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_pop_preserve_bytes_accounting() {
        let (sender, receiver) = bounded_queue();
        let payload = vec![b'x'; 100];
        sender
            .try_push(DiagnosticLevel::Info, payload.clone())
            .expect("push");
        sender
            .try_push(DiagnosticLevel::Info, payload.clone())
            .expect("push");
        assert_eq!(sender.snapshot().bytes, 200);
        assert_eq!(sender.snapshot().events, 2);
        assert_eq!(
            receiver.pop_timeout(Duration::from_millis(1)),
            Some(payload)
        );
        assert_eq!(sender.snapshot().bytes, 100);
    }

    #[test]
    fn queue_is_bounded_by_events_and_bytes() {
        let (sender, _receiver) = bounded_queue();
        let mut pushed = 0usize;
        loop {
            if sender
                .try_push(DiagnosticLevel::Info, vec![b'a'; 1024])
                .is_err()
            {
                break;
            }
            pushed += 1;
            assert!(pushed <= MAX_QUEUE_EVENTS);
        }
        assert_eq!(pushed, 1024.min(MAX_QUEUE_BYTES / 1024));
        // Byte budget is the binding limit for 1 KiB records.
        assert_eq!(pushed, MAX_QUEUE_BYTES / 1024);
    }

    #[test]
    fn closed_queue_rejects_new_records() {
        let (sender, _receiver) = bounded_queue();
        sender.close();
        assert_eq!(
            sender.try_push(DiagnosticLevel::Error, vec![1]),
            Err(PushError::Closed)
        );
    }

    /// Records admitted while no level was known are judged by the level that becomes
    /// known: below-threshold records are dropped, and their bytes are released.
    #[test]
    fn discard_below_keeps_only_admitted_records() {
        let (sender, receiver) = bounded_queue();
        sender
            .try_push(DiagnosticLevel::Debug, vec![b'd'; 10])
            .expect("push");
        sender
            .try_push(DiagnosticLevel::Info, vec![b'i'; 20])
            .expect("push");
        sender
            .try_push(DiagnosticLevel::Error, vec![b'e'; 30])
            .expect("push");
        assert_eq!(sender.snapshot().bytes, 60);
        assert_eq!(sender.discard_below(DiagnosticLevel::Error), 2);
        assert_eq!(sender.snapshot().events, 1);
        assert_eq!(sender.snapshot().bytes, 30);
        assert_eq!(
            receiver.pop_timeout(Duration::from_millis(1)),
            Some(vec![b'e'; 30])
        );
        // A level that admits everything drops nothing.
        sender
            .try_push(DiagnosticLevel::Debug, vec![b'd'; 5])
            .expect("push");
        assert_eq!(sender.discard_below(DiagnosticLevel::Debug), 0);
        assert_eq!(sender.snapshot().events, 1);
    }

    #[test]
    fn pop_timeout_returns_none_without_waiting_forever() {
        let (_sender, receiver) = bounded_queue();
        let started = std::time::Instant::now();
        assert_eq!(receiver.pop_timeout(Duration::from_millis(5)), None);
        assert!(started.elapsed() >= Duration::from_millis(4));
    }
}
