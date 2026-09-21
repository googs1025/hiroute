use serde::{Deserialize, Serialize};

use super::{ConversationContentDirectionV2, ConversationContentPhaseV2};

pub const OBSERVATION_ACK_SCHEMA_V2: &str = "hiroute.observation.ack/v2";
pub const OBSERVATION_NACK_SCHEMA_V1: &str = "hiroute.observation.nack/v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationFeedbackIdentityV1 {
    pub channel: String,
    pub producer_id: String,
    pub producer_epoch: String,
    pub stream_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationBlobAcknowledgementV1 {
    pub content_id: String,
    pub digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationContentAcknowledgementV1 {
    pub request_id: String,
    pub direction: ConversationContentDirectionV2,
    pub fork_id: String,
    pub next_chunk_ordinal: u32,
    pub transcript_root: Option<String>,
    pub delta_parent_transcript_root: Option<String>,
    pub acknowledged_blobs: Vec<ObservationBlobAcknowledgementV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationAckV2 {
    pub schema_version: String,
    pub identity: ObservationFeedbackIdentityV1,
    pub highest_contiguous_sequence: u64,
    pub highest_accounted_sequence: u64,
    pub content_acknowledgement: Option<ObservationContentAcknowledgementV1>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationEnvelopeViolationV1 {
    Malformed,
    MissingField,
    UnknownField,
    InvalidFieldValue,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationDigestSubjectV1 {
    EnvelopeSchema,
    Event,
    Transcript,
    ContentBlob,
    Projection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationSequenceRangeV1 {
    pub first_sequence: u64,
    pub last_sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationNackDetailV1 {
    ReceiverUnavailable {
        retry_after_millis: Option<u64>,
    },
    UnsupportedSchema {
        rejected_schema_version: String,
        supported_schema_versions: Vec<String>,
    },
    InvalidEnvelope {
        violation: ObservationEnvelopeViolationV1,
        field: Option<String>,
    },
    MissingSequenceRanges {
        ranges: Vec<ObservationSequenceRangeV1>,
    },
    SequenceEventConflict {
        sequence: u64,
        expected_event_id: String,
        rejected_event_id: String,
    },
    MissingPrerequisite {
        prerequisite_sequence: u64,
        prerequisite_event_id: Option<String>,
    },
    UnknownTranscriptRoot {
        request_id: String,
        direction: ConversationContentDirectionV2,
        fork_id: String,
        transcript_root: String,
    },
    MissingBlob {
        request_id: String,
        direction: ConversationContentDirectionV2,
        fork_id: String,
        blobs: Vec<ObservationBlobAcknowledgementV1>,
    },
    ChunkOrdinalConflict {
        request_id: String,
        direction: ConversationContentDirectionV2,
        fork_id: String,
        expected_chunk_ordinal: u32,
        rejected_chunk_ordinal: u32,
    },
    ContentStateConflict {
        request_id: String,
        direction: ConversationContentDirectionV2,
        fork_id: String,
        expected_phase: ConversationContentPhaseV2,
        rejected_phase: ConversationContentPhaseV2,
    },
    DigestMismatch {
        subject: ObservationDigestSubjectV1,
        subject_id: String,
        expected_digest: String,
        rejected_digest: String,
    },
    ImmutableProjectionConflict {
        projection_key: String,
        existing_digest: String,
        rejected_digest: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationNackV1 {
    pub schema_version: String,
    pub identity: ObservationFeedbackIdentityV1,
    pub rejected_sequence: u64,
    pub expected_sequence: u64,
    pub retryable: bool,
    pub detail: ObservationNackDetailV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservationFeedback {
    Ack(ObservationAckV2),
    Nack(ObservationNackV1),
}
