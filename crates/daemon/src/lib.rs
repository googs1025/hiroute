#![forbid(unsafe_code)]

//! Production daemon composition and authenticated, versioned Local Control transport.

pub mod control;
pub mod delegation;
mod release_catalog;
#[cfg(unix)]
pub mod role_all;

pub mod gateway_ports;
pub use control::{ControlEndpoint, LocalControlDaemon, serve_production_control};
#[cfg(unix)]
pub use role_all::{RoleAllConfig, RoleAllCpaConfig, RoleAllError, RoleAllHandle, start_role_all};
mod publication_failpoint;

#[cfg(all(test, unix))]
mod test_support;
