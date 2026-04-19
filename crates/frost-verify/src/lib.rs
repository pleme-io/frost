//! Verifiable shell configuration loading.
//!
//! Compares a Nix-generated manifest (expected load order + content hashes)
//! against a runtime trace (what actually loaded) to prove that shell
//! configuration is correct, complete, and in the right order.

pub mod manifest;
pub mod trace;
pub mod verify;
