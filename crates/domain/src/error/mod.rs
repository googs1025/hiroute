use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortErrorCode {
    Conflict,
    Corrupt,
    Crypto,
    InvalidData,
    NotFound,
    PermissionDenied,
    Unavailable,
}

/// Adapter errors intentionally carry only a bounded, non-sensitive context label.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{code:?} at {context}")]
pub struct PortError {
    pub code: PortErrorCode,
    pub context: &'static str,
}

impl PortError {
    pub const fn new(code: PortErrorCode, context: &'static str) -> Self {
        Self { code, context }
    }
}

pub type PortResult<T> = Result<T, PortError>;
