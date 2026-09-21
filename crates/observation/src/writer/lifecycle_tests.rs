use hiroute_domain::{
    ContentCompletenessDeltaV2, ContentLossWatermarkV2, CorrelationProvenance, EventId,
    LIFECYCLE_FACT_PORT_DIGEST_V2, LIFECYCLE_FACT_SCHEMA_V2, LifecycleCorrelationV2,
    LifecycleFactEnvelopeV2, LifecycleFactV2, LifecycleProducerComponentV2, LifecycleProducerV2,
    LifecycleTelemetryChannelV2, LifecycleTelemetryReceiverV2, LogicalRequestId,
    OBSERVATION_GAP_HEARTBEAT_SCHEMA_V1, ObservationFeedback, ObservationGapHeartbeatV1,
    ObservationNackDetailV1, ObservationProducerV2, ObservationStreamV1, ProducerEpoch, ProducerId,
    SessionId, SessionScopeV1, StreamId, TurnId, WorkspaceId,
};

use super::BoundedLifecycleReceiverV2;

fn stream() -> ObservationStreamV1 {
    ObservationStreamV1 {
        producer_id: ProducerId::parse("lifecycle-producer").unwrap(),
        producer_epoch: ProducerEpoch::parse("lifecycle-epoch").unwrap(),
        stream_id: StreamId::parse("lifecycle-stream").unwrap(),
    }
}

fn envelope(sequence: u64, fact: LifecycleFactV2) -> LifecycleFactEnvelopeV2 {
    LifecycleFactEnvelopeV2 {
        schema_version: LIFECYCLE_FACT_SCHEMA_V2.into(),
        schema_digest: LIFECYCLE_FACT_PORT_DIGEST_V2.into(),
        channel: LifecycleTelemetryChannelV2::Lifecycle,
        producer: LifecycleProducerV2 {
            component: LifecycleProducerComponentV2::GatewayLifecycle,
            revision: "gateway-observation/1".into(),
            stream: stream(),
        },
        sequence,
        event_id: EventId::parse(format!("lifecycle-event-{sequence}")).unwrap(),
        correlation: LifecycleCorrelationV2 {
            workspace_id: WorkspaceId::default(),
            conversation_id: SessionId::parse("conversation-lifecycle").unwrap(),
            session_scope: SessionScopeV1::Conversation,
            correlation_provenance: CorrelationProvenance::AgentSupplied,
            turn_id: TurnId::parse("turn-lifecycle").unwrap(),
            request_id: LogicalRequestId::parse("request-lifecycle").unwrap(),
        },
        occurred_at_unix_nanos: 1_000 + sequence,
        fact,
        loss_watermark: None,
        completeness_delta: None,
    }
}

#[test]
fn lifecycle_receiver_accepts_every_exact_event_kind_and_replay() {
    let receiver = BoundedLifecycleReceiverV2::new(6).unwrap();
    let events = [
        LifecycleFactV2::RequestAccepted {
            ingress_protocol: "messages".into(),
        },
        LifecycleFactV2::CanonicalRequestAccepted {
            canonicalization_version: "canonical/1".into(),
        },
        LifecycleFactV2::AttemptStarted { ordinal: 1 },
        LifecycleFactV2::AttemptFinished {
            ordinal: 1,
            outcome: "accepted".into(),
        },
        LifecycleFactV2::ResponseFrameAccepted {
            frame_id: "frame-1".into(),
            byte_count: 7,
            downstream_delivery: "full_frame_transport_accepted".into(),
        },
        LifecycleFactV2::RequestFinished {
            outcome: "succeeded".into(),
        },
    ];
    for (index, fact) in events.into_iter().enumerate() {
        let sequence = u64::try_from(index + 1).unwrap();
        let event = envelope(sequence, fact);
        assert!(matches!(
            receiver.receive_lifecycle(&event).unwrap(),
            ObservationFeedback::Ack(_)
        ));
        if sequence == 1 {
            assert!(matches!(
                receiver.receive_lifecycle(&event).unwrap(),
                ObservationFeedback::Ack(_)
            ));
        }
    }
    assert_eq!(receiver.accepted_len().unwrap(), 6);
}

#[test]
fn bounded_lifecycle_receiver_evicts_after_capacity_and_nacks_sequence_gap() {
    let receiver = BoundedLifecycleReceiverV2::new(1).unwrap();
    receiver
        .receive_lifecycle(&envelope(
            1,
            LifecycleFactV2::RequestAccepted {
                ingress_protocol: "responses".into(),
            },
        ))
        .unwrap();
    let accepted_after_rotation = receiver
        .receive_lifecycle(&envelope(
            2,
            LifecycleFactV2::RequestFinished {
                outcome: "succeeded".into(),
            },
        ))
        .unwrap();
    assert!(matches!(
        accepted_after_rotation,
        ObservationFeedback::Ack(ref ack)
            if ack.highest_contiguous_sequence == 2
                && ack.highest_accounted_sequence == 2
    ));
    assert_eq!(receiver.accepted_len().unwrap(), 1);
    assert_eq!(receiver.records().unwrap()[0].sequence, 2);

    let gap_receiver = BoundedLifecycleReceiverV2::new(2).unwrap();
    let gap = gap_receiver
        .receive_lifecycle(&envelope(
            2,
            LifecycleFactV2::RequestFinished {
                outcome: "succeeded".into(),
            },
        ))
        .unwrap();
    assert!(matches!(
        gap,
        ObservationFeedback::Nack(ref nack)
            if matches!(nack.detail, ObservationNackDetailV1::MissingSequenceRanges { .. })
    ));
}

#[test]
fn lifecycle_gap_heartbeat_advances_accounted_not_contiguous() {
    let receiver = BoundedLifecycleReceiverV2::new(1).unwrap();
    let heartbeat = ObservationGapHeartbeatV1 {
        schema_version: OBSERVATION_GAP_HEARTBEAT_SCHEMA_V1.into(),
        channel: "lifecycle".into(),
        producer: ObservationProducerV2 {
            component: "gateway-lifecycle".into(),
            revision: "gateway-observation/1".into(),
            stream: stream(),
        },
        sequence: 2,
        event_id: EventId::parse("lifecycle-gap-1-2").unwrap(),
        loss_watermark: ContentLossWatermarkV2 {
            first_sequence: 1,
            last_sequence: 2,
            reason: "queue_bytes_exceeded".into(),
        },
        completeness_delta: ContentCompletenessDeltaV2::Partial,
    };
    let feedback = receiver.receive_lifecycle_gap(&heartbeat).unwrap();
    assert!(matches!(
        feedback,
        ObservationFeedback::Ack(ref ack)
            if ack.highest_contiguous_sequence == 0
                && ack.highest_accounted_sequence == 2
    ));
}

#[test]
fn lifecycle_records_streams_and_gap_replay_metadata_remain_bounded() {
    let receiver = BoundedLifecycleReceiverV2::new(2).unwrap();
    for index in 0..64_u64 {
        let mut heartbeat = ObservationGapHeartbeatV1 {
            schema_version: OBSERVATION_GAP_HEARTBEAT_SCHEMA_V1.into(),
            channel: "lifecycle".into(),
            producer: ObservationProducerV2 {
                component: "gateway-lifecycle".into(),
                revision: "gateway-observation/1".into(),
                stream: stream(),
            },
            sequence: 1,
            event_id: EventId::parse(format!("lifecycle-gap-{index}")).unwrap(),
            loss_watermark: ContentLossWatermarkV2 {
                first_sequence: 1,
                last_sequence: 1,
                reason: "queue_bytes_exceeded".into(),
            },
            completeness_delta: ContentCompletenessDeltaV2::Partial,
        };
        heartbeat.producer.stream.stream_id =
            StreamId::parse(format!("lifecycle-stream-{index}")).unwrap();
        assert!(matches!(
            receiver.receive_lifecycle_gap(&heartbeat).unwrap(),
            ObservationFeedback::Ack(_)
        ));
        assert_eq!(receiver.bounded_state_sizes().unwrap().0, 0);
        assert!(receiver.bounded_state_sizes().unwrap().1 <= 2);
        assert!(receiver.bounded_state_sizes().unwrap().2 <= 2);
    }

    let latest = receiver.bounded_state_sizes().unwrap();
    assert_eq!(latest, (0, 2, 2));
}
