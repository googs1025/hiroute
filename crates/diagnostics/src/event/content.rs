//! Replay and captured-content events. These describe sizes, counts and stable reasons;
//! they never carry content bytes, schemas, content references or file names.

use serde::{Deserialize, Serialize};

/// Summary of one content capture cycle: aggregate counters only, so a burst of short
/// reads produces one event instead of one event per read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentCaptureEnd {
    pub role: ContentRole,
    pub outcome: CaptureOutcome,
    pub bytes: u64,
    pub chunks: u64,
    pub read_calls: u64,
    pub short_reads: u64,
    pub max_chunk_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort: Option<CaptureAbortReason>,
}

/// Stable reason an aborted capture cycle ended, never a free-form message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureAbortReason {
    CanonicalContentUnavailable,
    CanonicalCaptureNotBound,
    CanonicalCaptureStateUnavailable,
    ResponseSerializationFailed,
    IntegrityFailure,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentRole {
    Request,
    Response,
    Stream,
    Backing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureOutcome {
    Success,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpillBegin {
    pub mode: StorageMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    Memory,
    Disk,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrityFailure {
    pub reason: IntegrityReason,
    pub expected_bytes: u64,
    pub actual_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityReason {
    BackingMissing,
    BackingShorterThanDeclared,
    DigestMismatch,
    Io,
}
