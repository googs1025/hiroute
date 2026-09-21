//! Verified prepared/activate publication lifecycle and atomic request pinning.

mod activation;
pub mod admission;
mod target;
pub mod versions;

pub use activation::*;
pub use target::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod version_tests;
