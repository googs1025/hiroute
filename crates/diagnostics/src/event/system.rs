//! Events about the diagnostic subsystem itself and process-level anomalies.

use serde::{Deserialize, Serialize};

use crate::error::SubsystemReason;
use crate::identity::{CorrelationToken, SourceFileRef};
use crate::level::{DiagnosticLevel, LevelSource};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanicObserved {
    pub source_file: SourceFileRef,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelApplied {
    pub level: DiagnosticLevel,
    pub revision: u64,
    pub source: LevelSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsDegraded {
    pub reason: SubsystemReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_errno: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DroppedSummary {
    pub dropped_events: u64,
    pub rejected_events: u64,
    pub window_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterStats {
    pub rotated: u64,
    pub flushes: u64,
    pub bytes_written: u64,
    /// Records the writer could not write; the count only grows.
    #[serde(default)]
    pub write_failures: u64,
    /// Records the bounded exit drain could not write.
    #[serde(default)]
    pub lost_at_shutdown: u64,
}

/// The authoritative link between two token spaces (for example a control request token
/// and the model request token it caused). Only the layer that holds the real relationship
/// may emit this event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    ControlToModelRequest,
    ProductToKernelRequest,
    ProductToKernelAttempt,
    ModelRequestToAttempt,
    OperationToRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrelationLink {
    pub kind: LinkKind,
    pub left: CorrelationToken,
    pub right: CorrelationToken,
}
