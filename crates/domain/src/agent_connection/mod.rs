//! AgentConnection grant, catalog, guidance, and field-owned configuration contracts.
//!
//! These values reference independently published AgentPlans. They never make an Agent the
//! owner of a Plan and never alter AgentPlan identity or lifecycle.

mod catalog;
mod configuration;
mod connection;
mod guidance;

pub use catalog::*;
pub use configuration::*;
pub use connection::*;
pub use guidance::*;

#[cfg(test)]
mod tests;
