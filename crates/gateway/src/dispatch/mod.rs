//! Authentication, bounded selector extraction and alias/grant authorization.

mod authority;
mod run;
mod selector;

pub use authority::{AuthenticatedRequest, DispatchError, GatewayRequestAuthority};
pub use run::{
    RunObservationMetadata, RunRequestAuthorityError, RunRequestAuthorityPort, RunRequestLocator,
    RunRequestSafetyPort, VerifiedRunRequestAuthority,
};
pub use selector::{ModelSelector, SelectorError};

#[cfg(test)]
mod tests;
