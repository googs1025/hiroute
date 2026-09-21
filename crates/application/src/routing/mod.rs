//! Stateless routing Preview contract.
//!
//! Preview materializes the complete route and binds exact facts into a digest, but deliberately
//! allocates no AgentPlan ID or model alias and writes no Operation/publication state.

mod drafts;
pub use drafts::*;
mod lifecycle;
pub use lifecycle::*;
mod authoring;
pub use authoring::*;
mod suggestions;
pub use suggestions::*;
