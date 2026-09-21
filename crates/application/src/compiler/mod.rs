//! Deterministic AgentPlan compiler.
//!
//! The compiler is the only component that may interpret desired routing modes, ratings, ordering
//! prices, free-offer facts, and native reasoning capabilities. Its output is a closed domain
//! publication; Gateway runtime code receives no compiler inputs.

mod explicit;
mod facts;
mod materialize;

pub use explicit::*;
pub use facts::*;
pub use materialize::*;

#[cfg(test)]
pub(crate) mod test_fixtures;
#[cfg(test)]
mod tests;

// Only suggestions may offer the lowest discrete profile. Published arrays use RequireExplicit.
pub(crate) fn resolve_suggestion_attempt(
    fact: &CandidateCompilationFactV1,
    selection: Option<&hiroute_domain::ReasoningSelectionV1>,
    requirements: &hiroute_domain::CapabilityRequirementsV1,
) -> Result<hiroute_domain::AttemptOwnedCandidateV1, AgentPlanCompilerError> {
    materialize::resolve_attempt(
        fact,
        selection,
        requirements,
        hiroute_domain::DiscreteReasoningDefault::Lowest,
    )
}
