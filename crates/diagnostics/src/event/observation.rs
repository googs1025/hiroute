//! Observation producer/store feedback: gaps, NACKs, rejected projections and writer
//! health. Counts, ordinals and stable reasons only; no envelope, receipt or body.

use serde::{Deserialize, Serialize};

/// The bounded observation channels a gap or NACK can be attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationChannel {
    Facts,
    Content,
    Control,
    Store,
    Projection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationGap {
    pub channel: ObservationChannel,
    pub first_seq: u64,
    pub last_seq: u64,
    pub missing: u64,
    pub reason: GapReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    QueueOverflow,
    SinkUnavailable,
    Backpressure,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SinkNack {
    pub channel: ObservationChannel,
    pub reason: NackReason,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NackReason {
    StoreRejected,
    IntegrityFailed,
    SerializationFailed,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionRejected {
    pub channel: ObservationChannel,
    pub reason: ProjectionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionReason {
    Conflict,
    Invalid,
    Stale,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterHealth {
    pub channel: ObservationChannel,
    pub health: ObservationHealth,
    pub queue_events: u64,
    pub queue_bytes: u64,
    pub high_water: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_frontier: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationHealth {
    Ok,
    Degraded,
    Unavailable,
}
