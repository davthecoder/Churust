//! Experimental extractors and middleware for Churust.
//!
//! # This crate will never reach 1.0
//!
//! That is the point, not an oversight. Ideas that are worth trying in public
//! but not worth freezing into `churust-core`'s API live here first. Expect
//! breaking changes on most releases, and do not build anything you cannot
//! afford to edit.
//!
//! # How something graduates
//!
//! When an item earns its place it moves into `churust-core` (or the relevant
//! plugin crate) and the copy here is **deprecated rather than deleted**, so a
//! user migrates by dropping the `churust_lab::` prefix rather than by
//! discovering at compile time that it vanished. The deprecated item stays for
//! at least one release after graduation.
//!
//! # Versioning
//!
//! This crate shares the workspace version with every other Churust crate, so
//! its number moves in lockstep even though its stability promise does not.
//! **That is a deliberate decision, and it has an expiry:** when the rest of the
//! workspace approaches 1.0, this crate must be exempted from `shared-version`
//! and pinned to a `0.x` line of its own. Doing it now would complicate the
//! release tooling for no benefit while everything is `0.x` anyway; leaving it
//! unrecorded is how a crate accidentally ships a 1.0 it never meant to promise.

#![deny(missing_docs)]

mod body_limit;

pub use body_limit::BodyLimit;
