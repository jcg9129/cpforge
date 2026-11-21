//! Pure result derivation for the v1 [report contract](../../docs/contracts/report.md).
//!
//! Inputs are finalized evidence, not live processes. No function performs I/O,
//! catches panics, reads cancellation flags, or advances job phases. Callers
//! snapshot cancellation at check admission and record caught failures explicitly.
//! Wire tokens are independent of Rust names; serialization belongs to a later
//! layer. Observed coverage and classes survive invariant failures unchanged.

pub mod derive;
mod display;
mod types;

pub use derive::*;
pub use display::*;
pub use types::*;

#[cfg(test)]
mod tests;
