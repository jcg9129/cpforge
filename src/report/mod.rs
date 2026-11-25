//! Pure result derivation for the v1 [report contract](../../docs/contracts/report.md).
//!
//! Inputs are finalized evidence, not live processes. No function performs I/O,
//! catches panics, reads cancellation flags, or advances job phases. Callers
//! snapshot cancellation at check admission and record caught failures explicitly.
//! Wire tokens are independent of Rust names; canonical serialization is in
//! [`wire`]. Observed coverage and classes survive invariant failures unchanged.

pub mod derive;
mod display;
mod documents;
mod file_io;
mod judging;
mod roles;
mod types;
pub mod wire;

pub use derive::*;
pub use display::*;
pub use documents::*;
pub use file_io::*;
pub use judging::*;
pub use roles::*;
pub use types::*;

#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod tests;
