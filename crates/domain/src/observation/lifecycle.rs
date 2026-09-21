use serde::{Deserialize, Serialize};

use crate::{PortResult, WorkspaceId};

use super::{
    CorrelationProvenance, EventId, LogicalRequestId, ObservationFeedback,
    ObservationGapHeartbeatV1, ObservationStreamV1, SessionId, SessionScopeV1, TurnId,
};

pub const LIFECYCLE_FACT_SCHEMA_V2: &str = "hiroute.gateway.lifecycle-fact-envelope/v2";
pub const LIFECYCLE_FACT_PORT_DIGEST_V2: &str =
    "sha256:762143e3ab3121bf40cb7039a73ea0b47a5724c38837ba4152f75fc54e09e316";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleTelemetryChannelV2 {
    Lifecycle,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LifecycleProducerComponentV2 {
    #[serde(rename = "gateway-lifecycle")]
    GatewayLifecycle,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleProducerV2 {
    pub component: LifecycleProducerComponentV2,
    pub revision: String,
    #[serde(flatten)]
    pub stream: ObservationStreamV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleCorrelationV2 {
    pub workspace_id: WorkspaceId,
    pub conversation_id: SessionId,
    pub session_scope: SessionScopeV1,
    pub correlation_provenance: CorrelationProvenance,
    pub turn_id: TurnId,
    pub request_id: LogicalRequestId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleLossWatermarkV2 {
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LifecycleFactV2 {
    RequestAccepted {
        ingress_protocol: String,
    },
    CanonicalRequestAccepted {
        canonicalization_version: String,
    },
    AttemptStarted {
        ordinal: u32,
    },
    AttemptFinished {
        ordinal: u32,
        outcome: String,
    },
    ResponseFrameAccepted {
        frame_id: String,
        byte_count: u64,
        downstream_delivery: String,
    },
    RequestFinished {
        outcome: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleFactEnvelopeV2 {
    pub schema_version: String,
    pub schema_digest: String,
    pub channel: LifecycleTelemetryChannelV2,
    pub producer: LifecycleProducerV2,
    pub sequence: u64,
    pub event_id: EventId,
    pub correlation: LifecycleCorrelationV2,
    pub occurred_at_unix_nanos: u64,
    pub fact: LifecycleFactV2,
    pub loss_watermark: Option<LifecycleLossWatermarkV2>,
    pub completeness_delta: Option<String>,
}

impl LifecycleFactEnvelopeV2 {
    pub fn validate(&self) -> Result<(), LifecycleContractErrorV2> {
        if self.schema_version != LIFECYCLE_FACT_SCHEMA_V2
            || self.schema_digest != LIFECYCLE_FACT_PORT_DIGEST_V2
            || self.channel != LifecycleTelemetryChannelV2::Lifecycle
            || self.producer.revision.trim().is_empty()
            || self.sequence == 0
            || self.occurred_at_unix_nanos == 0
        {
            return Err(LifecycleContractErrorV2::InvalidEnvelope);
        }
        if let Some(loss) = &self.loss_watermark {
            if loss.first_sequence == 0
                || loss.first_sequence > loss.last_sequence
                || loss.last_sequence >= self.sequence
                || loss.reason.trim().is_empty()
                || self.completeness_delta.as_deref() != Some("partial")
            {
                return Err(LifecycleContractErrorV2::InvalidLossWatermark);
            }
        } else if self.completeness_delta.is_some() {
            return Err(LifecycleContractErrorV2::InvalidLossWatermark);
        }
        match &self.fact {
            LifecycleFactV2::RequestAccepted { ingress_protocol }
                if ingress_protocol.trim().is_empty() =>
            {
                Err(LifecycleContractErrorV2::InvalidFact)
            }
            LifecycleFactV2::CanonicalRequestAccepted {
                canonicalization_version,
            } if canonicalization_version.trim().is_empty() => {
                Err(LifecycleContractErrorV2::InvalidFact)
            }
            LifecycleFactV2::AttemptStarted { ordinal }
            | LifecycleFactV2::AttemptFinished { ordinal, .. }
                if *ordinal == 0 =>
            {
                Err(LifecycleContractErrorV2::InvalidFact)
            }
            LifecycleFactV2::AttemptFinished { outcome, .. }
            | LifecycleFactV2::RequestFinished { outcome }
                if outcome.trim().is_empty() =>
            {
                Err(LifecycleContractErrorV2::InvalidFact)
            }
            LifecycleFactV2::ResponseFrameAccepted {
                frame_id,
                byte_count,
                downstream_delivery,
            } if frame_id.trim().is_empty()
                || *byte_count == 0
                || downstream_delivery.trim().is_empty() =>
            {
                Err(LifecycleContractErrorV2::InvalidFact)
            }
            _ => Ok(()),
        }
    }
}

/// Operational-only receiver. Implementations may keep bounded structured logs/metrics, but this
/// port is not conversation retention and must not write the activity/content stores.
pub trait LifecycleTelemetryReceiverV2: Send + Sync {
    fn receive_lifecycle(
        &self,
        envelope: &LifecycleFactEnvelopeV2,
    ) -> PortResult<ObservationFeedback>;

    fn receive_lifecycle_gap(
        &self,
        heartbeat: &ObservationGapHeartbeatV1,
    ) -> PortResult<ObservationFeedback>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleContractErrorV2 {
    InvalidEnvelope,
    InvalidLossWatermark,
    InvalidFact,
}
