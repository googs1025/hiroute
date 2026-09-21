//! Frozen, process-only P0 Gateway Oracle.
//!
//! The runner has no dependency on the gateway or gateway-core crates. Green
//! coverage can only be derived from traffic crossing an external `hirouted`
//! listener and protocol-native provider listeners.

#![forbid(unsafe_code)]

mod bindings;
mod canonical;
mod client;
mod collector;
mod contract;
pub mod coverage;
mod fixture;
mod oracle;
pub(crate) mod privacy;
pub mod production;
mod provider;
mod runner;
pub mod schema;
mod types;

pub use contract::{BundleError, P0Bundle, ValidationSummary};
pub use oracle::{EvidenceSet, exact_json_matches};
pub use runner::{
    P0ProcessFailureKind, P0RunError, P0RunOptions, read_report, run_oracle, write_report,
};
pub use types::{
    AssertionOutcome, CorpusDocument, GoldenDocument, OracleStatus, P0Profile, P0RunReport,
    ProductExpectedRed, ReviewedSut, ScenarioDocument, VerifiedP0Run,
};
