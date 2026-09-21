//! Environment-independent, black-box E2E harness for HiRoute.
//!
//! The harness intentionally has no dependency on `hiroute-poc` or
//! `hiroute-gateway-core`. It verifies only executable, TCP, file-receipt, and
//! native-provider contracts.

#![forbid(unsafe_code)]

pub mod contract;
mod fixtures;
pub mod gateway_fixture;
mod http;
mod mock;
pub mod p0;
mod process;
pub mod runner;

pub use contract::{Profile, Scenario, ValidationSummary};
pub use runner::{RunFailure, RunOptions, RunReport, run_suite, write_report};
