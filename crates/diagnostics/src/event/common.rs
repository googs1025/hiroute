//! Small vocabularies shared by more than one event family.

use serde::{Deserialize, Serialize};

/// Which process produced a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRole {
    Desktop,
    Daemon,
    Control,
    Cli,
}

impl ProcessRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ProcessRole::Desktop => "desktop",
            ProcessRole::Daemon => "daemon",
            ProcessRole::Control => "control",
            ProcessRole::Cli => "cli",
        }
    }
}

/// A generic, stable completion state for bounded operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Completed,
    Failed,
    Rejected,
    Cancelled,
    Skipped,
}
