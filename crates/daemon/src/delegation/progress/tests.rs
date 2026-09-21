use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use super::*;

#[derive(Default)]
struct RecordingWriter {
    attempts: Mutex<Vec<(String, bool)>>,
    fail_remaining: AtomicUsize,
    hide: AtomicBool,
    block_first: AtomicBool,
    first_started: AtomicBool,
    release: (Mutex<bool>, Condvar),
}

impl ProgressBatchWriter for RecordingWriter {
    fn write_batch(
        &self,
        text: &str,
        reset_segment: bool,
        _: i64,
    ) -> Result<ManagedTextProgressWriteOutcome, ManagedTextError> {
        let attempt = {
            let mut attempts = self
                .attempts
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            attempts.push((text.to_owned(), reset_segment));
            attempts.len()
        };
        if attempt == 1 && self.block_first.load(Ordering::Acquire) {
            self.first_started.store(true, Ordering::Release);
            let (lock, ready) = &self.release;
            let mut released = lock.lock().unwrap_or_else(|error| error.into_inner());
            while !*released {
                released = ready
                    .wait(released)
                    .unwrap_or_else(|error| error.into_inner());
            }
        }
        if self
            .fail_remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_sub(1)
            })
            .is_ok()
        {
            return Err(ManagedTextError::Storage);
        }
        if self.hide.load(Ordering::Acquire) {
            return Ok(ManagedTextProgressWriteOutcome::Hidden);
        }
        Ok(ManagedTextProgressWriteOutcome::Committed {
            visibility_generation: 0,
            segment: u64::from(reset_segment),
            head: 0,
            end: text.len() as u64,
        })
    }
}

impl RecordingWriter {
    fn attempts(&self) -> Vec<(String, bool)> {
        self.attempts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn release_first(&self) {
        let (lock, ready) = &self.release;
        *lock.lock().unwrap_or_else(|error| error.into_inner()) = true;
        ready.notify_all();
    }
}

fn start_capture(writer: Arc<RecordingWriter>) -> ProgressCapture {
    let writer: Arc<dyn ProgressBatchWriter> = writer;
    ProgressCapture::start_with_schedule(
        writer,
        Schedule {
            interval: Duration::from_millis(25),
            size_bytes: 8,
        },
    )
}

async fn wait_for(predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn size_period_and_final_flush_are_single_owned_and_idle_is_zero_write() {
    let idle = Arc::new(RecordingWriter::default());
    let capture = start_capture(Arc::clone(&idle));
    tokio::time::sleep(Duration::from_millis(40)).await;
    capture.stop_and_flush().await;
    assert!(idle.attempts().is_empty());

    let writer = Arc::new(RecordingWriter::default());
    let capture = start_capture(Arc::clone(&writer));
    let sink = capture.sink();
    sink.push("12345678");
    wait_for(|| writer.attempts().len() == 1).await;
    sink.push("period");
    wait_for(|| writer.attempts().len() == 2).await;
    sink.push("final");
    capture.stop_and_flush().await;
    assert_eq!(
        writer.attempts(),
        vec![
            ("12345678".into(), false),
            ("period".into(), false),
            ("final".into(), false),
        ]
    );
}

#[tokio::test]
async fn overflow_keeps_a_utf8_suffix_and_marks_the_next_commit_as_a_gap() {
    let writer = Arc::new(RecordingWriter::default());
    let capture = start_capture(Arc::clone(&writer));
    let text = "🙂".repeat(20_000);
    capture.sink().push(&text);
    capture.stop_and_flush().await;
    let attempts = writer.attempts();
    assert_eq!(attempts.len(), 1);
    assert!(attempts[0].1);
    assert!(attempts[0].0.len() <= PENDING_BYTES);
    assert!(text.ends_with(&attempts[0].0));
    assert!(std::str::from_utf8(attempts[0].0.as_bytes()).is_ok());
}

#[tokio::test]
async fn an_exact_full_buffer_is_not_reported_as_a_gap_when_nothing_was_lost() {
    let writer = Arc::new(RecordingWriter::default());
    let capture = start_capture(Arc::clone(&writer));
    capture.sink().push(&"x".repeat(PENDING_BYTES));
    capture.stop_and_flush().await;
    assert_eq!(writer.attempts(), vec![("x".repeat(PENDING_BYTES), false)]);
}

#[tokio::test]
async fn failed_uncertain_batch_is_not_replayed_and_later_text_resets_the_segment() {
    let writer = Arc::new(RecordingWriter::default());
    writer.fail_remaining.store(1, Ordering::Release);
    let capture = start_capture(Arc::clone(&writer));
    let sink = capture.sink();
    sink.push("first");
    wait_for(|| writer.attempts().len() == 1).await;
    sink.push("later");
    wait_for(|| writer.attempts().len() == 2).await;
    capture.stop_and_flush().await;
    assert_eq!(
        writer.attempts(),
        vec![("first".into(), false), ("later".into(), true)]
    );
}

#[tokio::test]
async fn a_new_overflow_during_an_inflight_success_keeps_its_own_gap_marker() {
    let writer = Arc::new(RecordingWriter::default());
    writer.block_first.store(true, Ordering::Release);
    let capture = start_capture(Arc::clone(&writer));
    let sink = capture.sink();
    sink.push("12345678");
    wait_for(|| writer.first_started.load(Ordering::Acquire)).await;
    sink.push(&"界".repeat(30_000));
    writer.release_first();
    wait_for(|| writer.attempts().len() == 2).await;
    capture.stop_and_flush().await;
    let attempts = writer.attempts();
    assert_eq!(attempts[0], ("12345678".into(), false));
    assert!(attempts[1].1);
    assert!(attempts[1].0.len() <= PENDING_BYTES);
}

#[tokio::test]
async fn hidden_scope_closes_the_producer_without_repeated_empty_work() {
    let writer = Arc::new(RecordingWriter::default());
    writer.hide.store(true, Ordering::Release);
    let capture = start_capture(Arc::clone(&writer));
    let sink = capture.sink();
    sink.push("12345678");
    wait_for(|| writer.attempts().len() == 1).await;
    sink.push("ignored");
    tokio::time::sleep(Duration::from_millis(40)).await;
    capture.stop_and_flush().await;
    assert_eq!(writer.attempts().len(), 1);
}
