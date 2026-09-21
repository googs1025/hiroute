//! Finite Worker execution adapters; the Application owns task admission and state.
pub mod acp;
pub mod content;
pub mod credentials;
pub(crate) mod finalization;
pub mod lifecycle;
pub mod local_worker;
pub(crate) mod native_cleanup;
pub mod persistent_journal;
pub mod platform;
pub mod profile;
pub(crate) mod progress;
pub mod run_authority;

pub mod bootstrap;

pub mod dispatcher;

pub mod executor;

pub mod installation;

#[cfg(test)]
mod authorization_tests;

pub mod authorization_recovery;
