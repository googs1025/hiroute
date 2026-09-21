use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use hiroute_domain::{
    CanonicalDigest, LifecycleFactEnvelopeV2, LifecycleTelemetryReceiverV2,
    OBSERVATION_ACK_SCHEMA_V2, OBSERVATION_NACK_SCHEMA_V1, ObservationAckV2, ObservationFeedback,
    ObservationFeedbackIdentityV1, ObservationGapHeartbeatV1, ObservationNackDetailV1,
    ObservationNackV1, ObservationSequenceRangeV1, ObservationStreamV1, PortError, PortErrorCode,
    PortResult,
};

/// Bounded operational structured-log receiver. It is intentionally independent from
/// `LocalObservationStore`: lifecycle acceptance is observable, but never becomes conversation
/// retention or a routing dependency.
pub struct BoundedLifecycleReceiverV2 {
    capacity: usize,
    inner: Mutex<LifecycleReceiverState>,
}

#[derive(Default)]
struct LifecycleReceiverState {
    streams: BTreeMap<ObservationStreamV1, StreamCheckpoint>,
    stream_order: VecDeque<ObservationStreamV1>,
    records: VecDeque<LifecycleFactEnvelopeV2>,
    accepted_by_kind: BTreeMap<&'static str, u64>,
}

#[derive(Default)]
struct StreamCheckpoint {
    contiguous: u64,
    accounted: u64,
    last_event: Option<(u64, String, CanonicalDigest)>,
}

impl BoundedLifecycleReceiverV2 {
    pub fn new(capacity: usize) -> PortResult<Self> {
        if capacity == 0 {
            return Err(port(
                PortErrorCode::InvalidData,
                "observation.lifecycle.capacity",
            ));
        }
        Ok(Self {
            capacity,
            inner: Mutex::new(LifecycleReceiverState::default()),
        })
    }

    pub fn accepted_len(&self) -> PortResult<usize> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| port(PortErrorCode::Unavailable, "observation.lifecycle.lock"))?
            .records
            .len())
    }

    pub fn records(&self) -> PortResult<Vec<LifecycleFactEnvelopeV2>> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| port(PortErrorCode::Unavailable, "observation.lifecycle.lock"))?
            .records
            .iter()
            .cloned()
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn bounded_state_sizes(&self) -> PortResult<(usize, usize, usize)> {
        let state = self
            .inner
            .lock()
            .map_err(|_| port(PortErrorCode::Unavailable, "observation.lifecycle.lock"))?;
        Ok((
            state.records.len(),
            state.streams.len(),
            state
                .streams
                .values()
                .filter(|checkpoint| checkpoint.last_event.is_some())
                .count(),
        ))
    }
}

impl LifecycleTelemetryReceiverV2 for BoundedLifecycleReceiverV2 {
    fn receive_lifecycle(
        &self,
        envelope: &LifecycleFactEnvelopeV2,
    ) -> PortResult<ObservationFeedback> {
        if envelope.validate().is_err() {
            return Ok(ObservationFeedback::Nack(nack(
                "lifecycle",
                &envelope.producer.stream,
                envelope.sequence.max(1),
                1,
                false,
                ObservationNackDetailV1::InvalidEnvelope {
                    violation: hiroute_domain::ObservationEnvelopeViolationV1::InvalidFieldValue,
                    field: Some("envelope".into()),
                },
            )));
        }
        let digest = CanonicalDigest::of(envelope)
            .map_err(|_| port(PortErrorCode::Corrupt, "observation.lifecycle.digest"))?;
        let mut state = self
            .inner
            .lock()
            .map_err(|_| port(PortErrorCode::Unavailable, "observation.lifecycle.lock"))?;
        ensure_stream(&mut state, &envelope.producer.stream, self.capacity);
        let (expected, gap_is_exact) = {
            let checkpoint = state
                .streams
                .get(&envelope.producer.stream)
                .expect("stream inserted before sequence validation");
            if envelope.sequence <= checkpoint.accounted {
                return Ok(replay_feedback(
                    "lifecycle",
                    &envelope.producer.stream,
                    checkpoint,
                    envelope.sequence,
                    envelope.event_id.as_str(),
                    &digest,
                ));
            }
            let expected = checkpoint.accounted.saturating_add(1);
            let gap_is_exact = envelope.loss_watermark.as_ref().is_some_and(|loss| {
                envelope.sequence > expected
                    && loss.first_sequence == expected
                    && loss.last_sequence == envelope.sequence - 1
            });
            (expected, gap_is_exact)
        };
        if envelope.sequence != expected && !gap_is_exact {
            return Ok(ObservationFeedback::Nack(nack(
                "lifecycle",
                &envelope.producer.stream,
                envelope.sequence,
                expected,
                true,
                ObservationNackDetailV1::MissingSequenceRanges {
                    ranges: vec![ObservationSequenceRangeV1 {
                        first_sequence: expected,
                        last_sequence: envelope.sequence.saturating_sub(1).max(expected),
                    }],
                },
            )));
        }
        if state.records.len() == self.capacity {
            state.records.pop_front();
        }
        let checkpoint = state
            .streams
            .get_mut(&envelope.producer.stream)
            .expect("checkpoint inserted before capacity check");
        if !gap_is_exact && checkpoint.contiguous.saturating_add(1) == envelope.sequence {
            checkpoint.contiguous = envelope.sequence;
        }
        checkpoint.accounted = envelope.sequence;
        checkpoint.last_event = Some((envelope.sequence, envelope.event_id.to_string(), digest));
        let contiguous = checkpoint.contiguous;
        let accounted = checkpoint.accounted;
        let kind = lifecycle_kind(envelope);
        state.records.push_back(envelope.clone());
        let accepted = state.accepted_by_kind.entry(kind).or_default();
        *accepted = accepted.saturating_add(1);
        Ok(ObservationFeedback::Ack(ack(
            "lifecycle",
            &envelope.producer.stream,
            contiguous,
            accounted,
        )))
    }

    fn receive_lifecycle_gap(
        &self,
        heartbeat: &ObservationGapHeartbeatV1,
    ) -> PortResult<ObservationFeedback> {
        if heartbeat.validate().is_err() || heartbeat.channel != "lifecycle" {
            return Ok(ObservationFeedback::Nack(nack(
                &heartbeat.channel,
                &heartbeat.producer.stream,
                heartbeat.sequence.max(1),
                1,
                false,
                ObservationNackDetailV1::InvalidEnvelope {
                    violation: hiroute_domain::ObservationEnvelopeViolationV1::InvalidFieldValue,
                    field: Some("channel".into()),
                },
            )));
        }
        let digest = CanonicalDigest::of(heartbeat)
            .map_err(|_| port(PortErrorCode::Corrupt, "observation.lifecycle.gap_digest"))?;
        let mut state = self
            .inner
            .lock()
            .map_err(|_| port(PortErrorCode::Unavailable, "observation.lifecycle.lock"))?;
        ensure_stream(&mut state, &heartbeat.producer.stream, self.capacity);
        let checkpoint = state
            .streams
            .get_mut(&heartbeat.producer.stream)
            .expect("stream inserted before gap validation");
        if heartbeat.sequence <= checkpoint.accounted {
            return Ok(replay_feedback(
                "lifecycle",
                &heartbeat.producer.stream,
                checkpoint,
                heartbeat.sequence,
                heartbeat.event_id.as_str(),
                &digest,
            ));
        }
        let expected = checkpoint.accounted.saturating_add(1);
        if heartbeat.loss_watermark.first_sequence != expected {
            return Ok(ObservationFeedback::Nack(nack(
                "lifecycle",
                &heartbeat.producer.stream,
                heartbeat.sequence,
                expected,
                true,
                ObservationNackDetailV1::MissingSequenceRanges {
                    ranges: vec![ObservationSequenceRangeV1 {
                        first_sequence: expected,
                        last_sequence: heartbeat.sequence,
                    }],
                },
            )));
        }
        checkpoint.accounted = heartbeat.sequence;
        checkpoint.last_event = Some((heartbeat.sequence, heartbeat.event_id.to_string(), digest));
        Ok(ObservationFeedback::Ack(ack(
            "lifecycle",
            &heartbeat.producer.stream,
            checkpoint.contiguous,
            checkpoint.accounted,
        )))
    }
}

fn ensure_stream(
    state: &mut LifecycleReceiverState,
    stream: &ObservationStreamV1,
    capacity: usize,
) {
    if state.streams.contains_key(stream) {
        if let Some(position) = state.stream_order.iter().position(|item| item == stream) {
            state.stream_order.remove(position);
        }
    } else {
        if state.streams.len() == capacity
            && let Some(evicted) = state.stream_order.pop_front()
        {
            state.streams.remove(&evicted);
        }
        state
            .streams
            .insert(stream.clone(), StreamCheckpoint::default());
    }
    state.stream_order.push_back(stream.clone());
}

fn replay_feedback(
    channel: &str,
    stream: &ObservationStreamV1,
    checkpoint: &StreamCheckpoint,
    sequence: u64,
    event_id: &str,
    digest: &CanonicalDigest,
) -> ObservationFeedback {
    if let Some((known_sequence, known_event_id, known_digest)) = &checkpoint.last_event
        && *known_sequence == sequence
        && (known_event_id != event_id || known_digest != digest)
    {
        return ObservationFeedback::Nack(nack(
            channel,
            stream,
            sequence,
            checkpoint.accounted.saturating_add(1),
            false,
            ObservationNackDetailV1::SequenceEventConflict {
                sequence,
                expected_event_id: known_event_id.clone(),
                rejected_event_id: event_id.to_owned(),
            },
        ));
    }
    ObservationFeedback::Ack(ack(
        channel,
        stream,
        checkpoint.contiguous,
        checkpoint.accounted,
    ))
}

fn lifecycle_kind(envelope: &LifecycleFactEnvelopeV2) -> &'static str {
    use hiroute_domain::LifecycleFactV2;
    match &envelope.fact {
        LifecycleFactV2::RequestAccepted { .. } => "request_accepted",
        LifecycleFactV2::CanonicalRequestAccepted { .. } => "canonical_request_accepted",
        LifecycleFactV2::AttemptStarted { .. } => "attempt_started",
        LifecycleFactV2::AttemptFinished { .. } => "attempt_finished",
        LifecycleFactV2::ResponseFrameAccepted { .. } => "response_frame_accepted",
        LifecycleFactV2::RequestFinished { .. } => "request_finished",
    }
}

fn identity(channel: &str, stream: &ObservationStreamV1) -> ObservationFeedbackIdentityV1 {
    ObservationFeedbackIdentityV1 {
        channel: channel.into(),
        producer_id: stream.producer_id.to_string(),
        producer_epoch: stream.producer_epoch.to_string(),
        stream_id: stream.stream_id.to_string(),
    }
}

fn ack(
    channel: &str,
    stream: &ObservationStreamV1,
    contiguous: u64,
    accounted: u64,
) -> ObservationAckV2 {
    ObservationAckV2 {
        schema_version: OBSERVATION_ACK_SCHEMA_V2.into(),
        identity: identity(channel, stream),
        highest_contiguous_sequence: contiguous,
        highest_accounted_sequence: accounted,
        content_acknowledgement: None,
    }
}

fn nack(
    channel: &str,
    stream: &ObservationStreamV1,
    rejected_sequence: u64,
    expected_sequence: u64,
    retryable: bool,
    detail: ObservationNackDetailV1,
) -> ObservationNackV1 {
    ObservationNackV1 {
        schema_version: OBSERVATION_NACK_SCHEMA_V1.into(),
        identity: identity(channel, stream),
        rejected_sequence,
        expected_sequence,
        retryable,
        detail,
    }
}

fn port(code: PortErrorCode, context: &'static str) -> PortError {
    PortError::new(code, context)
}
