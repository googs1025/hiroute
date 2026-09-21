use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use hiroute_domain::{
    CompletenessDeltaV1, ExecutionFactV1, ExecutionLossReasonV1, ExecutionLossWatermarkV1,
    FactsCompleteness, LossNoticeV1, OBSERVATION_ACK_SCHEMA_V2, ObservationAckV2,
    ObservationChannel, ObservationFeedbackIdentityV1, ObservationGapHeartbeatV1,
    ObservationNackDetailV1, ObservationQueryPort, ObservationStreamV1, ReceiptId,
};
use tempfile::tempdir;

use crate::writer::{IngestOutcome, ObservationCommitPort, ObservationStoreError};
use crate::{FactChannel, LocalObservationWriter, OfferOutcome, WriterCycleOutcome};

use super::support::*;

#[test]
fn byte_bounded_fact_queue_reports_loss_without_blocking_response_semantics() {
    let temporary = tempdir().unwrap();
    let fixture = Fixture::new("loss");
    let store = open_store(temporary.path());
    let writer = writer(&store);
    let first = fixture.fact(1, route_decision("plan/codex-daily"), 10);
    let capacity = serde_json::to_vec(&first).unwrap().len() + 64;
    let channel = fact_channel(&fixture, capacity);
    let response = b"unchanged-model-response".to_vec();

    assert_eq!(channel.offer(first), OfferOutcome::Accepted);
    assert_eq!(
        channel.offer(fixture.fact(2, candidate("model-a"), 11)),
        OfferOutcome::DroppedCapacity
    );
    assert!(matches!(
        writer.consume_fact(&channel),
        WriterCycleOutcome::Ack(_)
    ));
    assert_eq!(
        channel.offer(fixture.fact(3, runtime_state(), 12)),
        OfferOutcome::Accepted
    );
    let WriterCycleOutcome::Ack(ack) = writer.consume_fact(&channel) else {
        panic!("loss-accounting heartbeat must be accepted")
    };
    assert_eq!(ack.highest_contiguous_sequence, 1);
    assert_eq!(ack.highest_accounted_sequence, 3);
    assert_eq!(response, b"unchanged-model-response");
    let status = store.get_status(&fixture.workspace).unwrap();
    assert_eq!(status.gaps.len(), 1);
    assert_eq!(status.gaps[0].first_sequence, 2);
    assert_eq!(status.gaps[0].last_sequence, 2);
    assert!(status.gaps[0].known_loss);
    assert_eq!(
        status.facts_completeness,
        hiroute_domain::FactsCompleteness::Partial
    );
}

#[test]
fn declared_loss_does_not_block_later_independent_facts_or_partial_receipt() {
    let temporary = tempdir().unwrap();
    let fixture = Fixture::new("independent-loss");
    let store = open_store(temporary.path());
    let writer = writer(&store);
    let channel = fact_channel(&fixture, 128 * 1024);

    let mut candidate = fixture.fact(2, candidate("model-a"), 10);
    candidate.loss_watermark = Some(ExecutionLossWatermarkV1 {
        first_sequence: 1,
        last_sequence: 1,
        reason: ExecutionLossReasonV1::QueueEventsExceeded,
    });
    candidate.completeness_delta = Some(CompletenessDeltaV1::Partial);
    assert!(matches!(
        offer_fact(&writer, &channel, candidate),
        WriterCycleOutcome::Ack(_)
    ));
    assert!(matches!(
        offer_fact(
            &writer,
            &channel,
            fixture.fact(
                3,
                ExecutionFactV1::RequestFinished {
                    outcome: hiroute_domain::ExecutionRequestOutcomeV1::Failed,
                    attempts_started: 0,
                    attempts_finished: 0,
                    accepted_attempt_ordinal: None,
                    facts_completeness: FactsCompleteness::Partial,
                },
                11,
            ),
        ),
        WriterCycleOutcome::Ack(_)
    ));

    let receipt = store
        .get_receipt(
            &fixture.workspace,
            &ReceiptId::parse(fixture.request.as_str()).unwrap(),
        )
        .unwrap();
    assert_eq!(receipt.facts_completeness, FactsCompleteness::Partial);
    assert_eq!(receipt.ordered_facts.len(), 2);
    assert!(
        receipt
            .ordered_facts
            .iter()
            .all(|event| !matches!(event.fact, ExecutionFactV1::RouteDecision(_)))
    );
    let fact_rows: i64 = store
        .connection
        .lock()
        .query_row("SELECT COUNT(*) FROM execution_fact_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(fact_rows, 2);
}

#[test]
fn unexplained_sequence_hole_is_nacked_then_replay_backfills_the_unknown_gap() {
    let temporary = tempdir().unwrap();
    let fixture = Fixture::new("missing");
    let store = open_store(temporary.path());
    let writer = writer(&store);
    let channel = fact_channel(&fixture, 64 * 1024);
    let blocked = fixture.fact(2, candidate("model-a"), 20);
    assert_eq!(channel.offer(blocked.clone()), OfferOutcome::Accepted);
    let WriterCycleOutcome::Nack(nack) = writer.consume_fact(&channel) else {
        panic!("unexplained hole must NACK")
    };
    assert!(matches!(
        nack.detail,
        ObservationNackDetailV1::MissingSequenceRanges { ref ranges }
            if ranges[0].first_sequence == 1 && ranges[0].last_sequence == 1
    ));
    let status = store.get_status(&fixture.workspace).unwrap();
    assert_eq!(status.gaps.len(), 1);
    assert!(!status.gaps[0].known_loss);

    assert_eq!(
        channel.offer(fixture.fact(1, route_decision("plan/codex-daily"), 19)),
        OfferOutcome::Accepted
    );
    assert!(matches!(
        writer.consume_fact(&channel),
        WriterCycleOutcome::Ack(_)
    ));
    assert_eq!(channel.offer(blocked), OfferOutcome::Accepted);
    let WriterCycleOutcome::Ack(recovered) = writer.consume_fact(&channel) else {
        panic!("backfilled stream must accept the originally NACKed event")
    };
    assert_eq!(recovered.highest_contiguous_sequence, 2);
    let recovered_status = store.get_status(&fixture.workspace).unwrap();
    assert!(recovered_status.gaps.is_empty());
    assert_eq!(
        recovered_status.facts_completeness,
        hiroute_domain::FactsCompleteness::Complete
    );
}

#[derive(Clone, Copy)]
enum FaultMode {
    Slow,
    Fail,
    Panic,
}

struct FaultCommit {
    mode: FaultMode,
    entered: Arc<AtomicBool>,
}

impl ObservationCommitPort for FaultCommit {
    fn ingest_fact(
        &self,
        envelope: &hiroute_domain::ExecutionFactEnvelopeV1,
        _channel_losses: &[LossNoticeV1],
    ) -> Result<IngestOutcome, ObservationStoreError> {
        self.entered.store(true, Ordering::Release);
        match self.mode {
            FaultMode::Slow => std::thread::sleep(Duration::from_millis(120)),
            FaultMode::Fail => return Err(ObservationStoreError::ActivityUnavailable),
            FaultMode::Panic => panic!("injected observation panic"),
        }
        Ok(IngestOutcome::Ack(ack(
            "execution_fact",
            &envelope.producer.stream,
            envelope.sequence,
            envelope.sequence,
        )))
    }

    fn ingest_content(
        &self,
        envelope: &hiroute_domain::ConversationContentEnvelopeV1,
        _channel_losses: &[LossNoticeV1],
    ) -> Result<IngestOutcome, ObservationStoreError> {
        self.entered.store(true, Ordering::Release);
        match self.mode {
            FaultMode::Slow => std::thread::sleep(Duration::from_millis(120)),
            FaultMode::Fail => return Err(ObservationStoreError::ContentUnavailable),
            FaultMode::Panic => panic!("injected content observation panic"),
        }
        Ok(IngestOutcome::Ack(ack(
            "conversation_content",
            &envelope.producer.stream,
            envelope.sequence,
            envelope.sequence,
        )))
    }

    fn ingest_gap_heartbeat(
        &self,
        heartbeat: &ObservationGapHeartbeatV1,
    ) -> Result<IngestOutcome, ObservationStoreError> {
        Ok(IngestOutcome::Ack(ack(
            &heartbeat.channel,
            &heartbeat.producer.stream,
            heartbeat.sequence,
            heartbeat.sequence,
        )))
    }

    fn record_losses(
        &self,
        _channel: ObservationChannel,
        _stream: &ObservationStreamV1,
        _losses: &[LossNoticeV1],
    ) -> Result<(), ObservationStoreError> {
        Ok(())
    }
}

#[test]
fn slow_failing_and_panicking_writer_cycles_are_isolated_from_the_producer() {
    let fixture = Fixture::new("fault");
    let slow_entered = Arc::new(AtomicBool::new(false));
    let slow_writer = LocalObservationWriter::new(Arc::new(FaultCommit {
        mode: FaultMode::Slow,
        entered: slow_entered.clone(),
    }));
    let channel = FactChannel::new(fixture.fact_stream.clone(), 128 * 1024);
    assert_eq!(
        channel.offer(fixture.fact(1, route_decision("plan/codex-daily"), 30)),
        OfferOutcome::Accepted
    );
    let worker_channel = channel.clone();
    let worker = std::thread::spawn(move || slow_writer.consume_fact(&worker_channel));
    while !slow_entered.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    let started = Instant::now();
    assert_eq!(
        channel.offer(fixture.fact(2, candidate("model-a"), 31)),
        OfferOutcome::Accepted
    );
    assert!(started.elapsed() < Duration::from_millis(50));
    assert!(matches!(worker.join().unwrap(), WriterCycleOutcome::Ack(_)));

    for (mode, expected) in [(FaultMode::Fail, "failed"), (FaultMode::Panic, "panicked")] {
        let entered = Arc::new(AtomicBool::new(false));
        let writer = LocalObservationWriter::new(Arc::new(FaultCommit { mode, entered }));
        let channel = FactChannel::new(fixture.fact_stream.clone(), 64 * 1024);
        assert_eq!(
            channel.offer(fixture.fact(1, route_decision("plan/codex-daily"), 32)),
            OfferOutcome::Accepted
        );
        let result = writer.consume_fact(&channel);
        match expected {
            "failed" => assert_eq!(
                result,
                WriterCycleOutcome::StoreFailed(ObservationStoreError::ActivityUnavailable)
            ),
            "panicked" => assert_eq!(result, WriterCycleOutcome::StorePanicked),
            _ => unreachable!(),
        }
    }
    for (mode, expected) in [
        (FaultMode::Fail, ObservationStoreError::ContentUnavailable),
        (FaultMode::Panic, ObservationStoreError::Corrupt),
    ] {
        let entered = Arc::new(AtomicBool::new(false));
        let writer = LocalObservationWriter::new(Arc::new(FaultCommit { mode, entered }));
        let channel = content_channel(&fixture, 64 * 1024);
        assert_eq!(
            channel.offer(fixture.content_begin(1, 33)),
            OfferOutcome::Accepted
        );
        let result = writer.consume_content(&channel);
        if matches!(mode, FaultMode::Panic) {
            assert_eq!(result, WriterCycleOutcome::StorePanicked);
        } else {
            assert_eq!(result, WriterCycleOutcome::StoreFailed(expected));
        }
    }
    let response = "response-remains-successful";
    assert_eq!(response, "response-remains-successful");
}

fn ack(
    channel: &str,
    stream: &ObservationStreamV1,
    highest_contiguous_sequence: u64,
    highest_accounted_sequence: u64,
) -> ObservationAckV2 {
    ObservationAckV2 {
        schema_version: OBSERVATION_ACK_SCHEMA_V2.into(),
        identity: ObservationFeedbackIdentityV1 {
            channel: channel.into(),
            producer_id: stream.producer_id.to_string(),
            producer_epoch: stream.producer_epoch.to_string(),
            stream_id: stream.stream_id.to_string(),
        },
        highest_contiguous_sequence,
        highest_accounted_sequence,
        content_acknowledgement: None,
    }
}
