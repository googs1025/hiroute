mod attestation;
mod candidate;
pub use candidate::{
    CandidateRunReport, prepare_current_candidate, run_current_candidate, write_current_report,
};
mod collector;
mod contract;
mod listener;
mod record;
mod report;
mod runtime;
mod types;

pub use collector::verify_collector_evidence;
pub use contract::{ProductionBundle, ProductionValidationSummary};
pub use report::{
    read_report, verify_launcher_evidence, verify_native_evidence, verify_readiness_evidence,
    verify_report, write_report,
};
pub use runtime::run as run_production_oracle;
pub use types::{
    ChannelEvidence, CollectorEvidence, ContentTerminal, ExpectedObservation, LauncherRecord,
    ProductReady, ProductionError, ProductionRunOptions, ProductionRunReport, ReadinessEvidence,
    ScenarioState, SutBuildAttestation, VerifiedProductionRun,
};

mod current_inputs;

mod legacy_inputs;
