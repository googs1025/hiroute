#![forbid(unsafe_code)]

//! Typed Product E2E contract and independent evidence Oracle.
//!
//! This crate is not an alternate product implementation. It cannot write product storage,
//! invoke an internal Application seam, accept arbitrary shell/URL/Secret payloads, or turn
//! fixture self-reporting into a product-completion verdict.

mod actions;
pub mod control;
pub mod cpa;
mod dependency_gate;
pub mod gateway_adapters;
mod generated;
mod ledger;
mod oracle;
mod runner;
#[cfg(unix)]
pub mod smoke;

pub use actions::{
    AssertionKind, CapabilityCase, ControlOperationClass, DaemonFailpoint, DaemonRole,
    EvidencePolarity, ProductActionV1, ProductScenarioV1, ScenarioProofV1,
};
pub use dependency_gate::{DependencyGateFinding, verify_dependency_metadata};
pub use generated::{GeneratedProductContractFile, generated_product_contract_files};
pub use ledger::{FaultLedgerV1, SideEffectLedgerV1};
pub use oracle::{
    AgentProfile, BoundaryEvidence, CliInvocationEvidenceV1, ControlProbeEvidenceV1,
    ControlProbeKind, DaemonEvidenceV1, DaemonExecutable, DaemonLaunchResult, EvidenceOrigin,
    FindingCode, GoldenEvidenceV1, NativePayloadEvidenceV1, OracleEvidenceV1, OracleFindingV1,
    OracleMode, OraclePassV1, PrivateArtifact, PrivateOracleInputs, ProductOracle, Protocol,
    ProtocolPath, SideEffectEvidenceV1, SideEffectExpectation,
};
pub use runner::ProductEvaluationWitness;
