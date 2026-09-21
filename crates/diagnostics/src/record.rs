//! The versioned record envelope written to JSONL files.
//!
//! A record is the only thing that can reach a diagnostic file. Its schema, component,
//! level, ids and event payload are all validated types, and `deny_unknown_fields` makes
//! an unknown field an error rather than silently imported data.

use serde::de::Error as _;
use serde::{Deserialize, Serialize};

use crate::event::DiagnosticEvent;
use crate::identity::{BootId, SessionId, SpanId};
use crate::level::DiagnosticLevel;

pub const RECORD_SCHEMA_V1: &str = "hiroute.diagnostic-event/v1";
/// Maximum serialized record size including the trailing newline.
pub const MAX_RECORD_BYTES: usize = 4096;

/// Which product component produced a record. Files are owned per process role
/// (`desktop`/`daemon`); the component names the subsystem inside that process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    Desktop,
    Daemon,
    ClientCore,
    Gateway,
    Worker,
    Cpa,
    Diagnostics,
}

impl Component {
    pub fn as_str(self) -> &'static str {
        match self {
            Component::Desktop => "desktop",
            Component::Daemon => "daemon",
            Component::ClientCore => "client_core",
            Component::Gateway => "gateway",
            Component::Worker => "worker",
            Component::Cpa => "cpa",
            Component::Diagnostics => "diagnostics",
        }
    }
}

/// The record's `schema` field, validated on construction and on parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordSchema;

impl Serialize for RecordSchema {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(RECORD_SCHEMA_V1)
    }
}

impl<'de> Deserialize<'de> for RecordSchema {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == RECORD_SCHEMA_V1 {
            Ok(RecordSchema)
        } else {
            Err(D::Error::custom("unsupported diagnostic record schema"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticRecordV1 {
    pub schema: RecordSchema,
    pub timestamp_ms: u64,
    pub monotonic_ms: u64,
    pub component: Component,
    pub boot_id: BootId,
    pub sequence: u64,
    pub level: DiagnosticLevel,
    pub level_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<SpanId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<SpanId>,
    pub event: DiagnosticEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    #[error("diagnostic record exceeds the single-record limit")]
    TooLarge,
    #[error("diagnostic record is not valid JSONL")]
    Invalid,
    #[error("diagnostic record serialization failed")]
    Serialize,
}

impl DiagnosticRecordV1 {
    /// Serialize one record as a complete JSONL line, enforcing the size limit before the
    /// bytes can be reserved in the queue.
    pub fn encode_jsonl(&self) -> Result<Vec<u8>, RecordError> {
        let mut bytes = serde_json::to_vec(self).map_err(|_| RecordError::Serialize)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(RecordError::TooLarge);
        }
        Ok(bytes)
    }

    /// Parse and validate one record line read back from a file.
    pub fn parse_line(line: &[u8]) -> Result<Self, RecordError> {
        if line.len() > MAX_RECORD_BYTES {
            return Err(RecordError::TooLarge);
        }
        serde_json::from_slice::<DiagnosticRecordV1>(line).map_err(|_| RecordError::Invalid)
    }
}
