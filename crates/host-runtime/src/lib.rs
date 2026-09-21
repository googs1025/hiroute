#![forbid(unsafe_code)]

//! Concrete host-owned contracts shared by the current Desktop and standalone CLI hosts.
//!
//! This crate intentionally contains no service-manager abstraction and no business state. It
//! owns only the one listener record and one standalone installation/layout contract that are
//! consumed by all current native entrypoints.

mod gateway_listener;
mod standalone;

pub use gateway_listener::*;
pub use standalone::*;
