//! Exact Product ↔ Gateway adapters for the final composition owner.
//!
//! This module exposes independently constructible ports. It does not start a listener, read
//! release data, launch a managed connector, or claim the `role=all` composition.

mod credential;
mod observation;
mod pricing;
mod publication;
mod runtime_state;

pub use credential::GatewayCredentialResolver;
pub use observation::{
    GatewayConversationContentSink, GatewayExecutionFactSink, GatewayLifecycleTelemetrySink,
    GatewayRunRelationSink,
};
pub use pricing::GatewayRequestPriceSource;
pub use publication::GatewayPublicationAdapter;
pub use runtime_state::GatewayRuntimeStateStore;

#[cfg(test)]
mod tests;
