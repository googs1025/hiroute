//! Provider-neutral execution primitives for the HiRoute gateway.
//!
//! The crate deliberately contains no configuration source client, provider
//! protocol, candidate-selection policy, or session persistence.

#![forbid(unsafe_code)]

pub mod core;
pub mod runtime;
pub mod test_support;
pub mod transport;
