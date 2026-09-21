//! Diagnostic levels and their origin.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The four local diagnostic levels. There is no `trace`/`off`; a level is a threshold
/// that includes every more severe level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum DiagnosticLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl DiagnosticLevel {
    /// Default for an unconfigured runtime: development/test builds keep diagnostic
    /// detail; release builds remain at Info. Persisted levels and explicit overrides win.
    pub const fn runtime_default() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Info
        }
    }

    pub const ALL: [DiagnosticLevel; 4] = [
        DiagnosticLevel::Error,
        DiagnosticLevel::Warn,
        DiagnosticLevel::Info,
        DiagnosticLevel::Debug,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticLevel::Error => "error",
            DiagnosticLevel::Warn => "warn",
            DiagnosticLevel::Info => "info",
            DiagnosticLevel::Debug => "debug",
        }
    }

    /// Whether an event at `event` severity passes this threshold.
    pub fn admits(self, event: DiagnosticLevel) -> bool {
        event <= self
    }
}

impl fmt::Display for DiagnosticLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DiagnosticLevel {
    type Err = InvalidLevel;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "error" => Ok(DiagnosticLevel::Error),
            "warn" => Ok(DiagnosticLevel::Warn),
            "info" => Ok(DiagnosticLevel::Info),
            "debug" => Ok(DiagnosticLevel::Debug),
            _ => Err(InvalidLevel),
        }
    }
}

impl Serialize for DiagnosticLevel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DiagnosticLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid diagnostic level")]
pub struct InvalidLevel;

/// Where the effective level of one process came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelSource {
    /// No usable settings: the safe built-in default.
    Default,
    /// The persisted `settings.json` level.
    Persisted,
    /// A smoke-test process override; never persisted.
    SmokeOverride,
}

impl LevelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LevelSource::Default => "default",
            LevelSource::Persisted => "persisted",
            LevelSource::SmokeOverride => "smoke_override",
        }
    }
}

impl Serialize for LevelSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LevelSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "default" => Ok(LevelSource::Default),
            "persisted" => Ok(LevelSource::Persisted),
            "smoke_override" => Ok(LevelSource::SmokeOverride),
            _ => Err(serde::de::Error::custom("invalid level source")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_includes_more_severe_levels() {
        assert!(DiagnosticLevel::Info.admits(DiagnosticLevel::Error));
        assert!(DiagnosticLevel::Info.admits(DiagnosticLevel::Warn));
        assert!(DiagnosticLevel::Info.admits(DiagnosticLevel::Info));
        assert!(!DiagnosticLevel::Info.admits(DiagnosticLevel::Debug));
        assert!(DiagnosticLevel::Debug.admits(DiagnosticLevel::Debug));
        assert!(!DiagnosticLevel::Error.admits(DiagnosticLevel::Warn));
    }

    #[test]
    fn level_parsing_is_exact() {
        assert_eq!("error".parse(), Ok(DiagnosticLevel::Error));
        assert_eq!("debug".parse(), Ok(DiagnosticLevel::Debug));
        assert!("Debug".parse::<DiagnosticLevel>().is_err());
        assert!("trace".parse::<DiagnosticLevel>().is_err());
        assert!("off".parse::<DiagnosticLevel>().is_err());
        assert_eq!(DiagnosticLevel::default(), DiagnosticLevel::Info);
    }
}
