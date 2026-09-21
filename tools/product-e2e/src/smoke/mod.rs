//! Finite development smoke cases. Production effects stay behind real executables.
mod build;
mod control;
pub mod fixtures;
mod gateway;
pub mod isolation;
mod preparation;
mod process;
mod registry;
mod report;
mod run;

pub use preparation::prepare;
pub use registry::{Case, catalog, select};
pub use report::{CaseReport, Execution, Report, State};
pub use run::run;

use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SmokeError(pub &'static str);
type Result<T> = std::result::Result<T, SmokeError>;
impl From<std::io::Error> for SmokeError {
    fn from(_: std::io::Error) -> Self {
        Self("io_error")
    }
}
impl From<serde_json::Error> for SmokeError {
    fn from(_: serde_json::Error) -> Self {
        Self("invalid_json")
    }
}
fn require(condition: bool, code: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(SmokeError(code))
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn nonce() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| SmokeError("random_unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
