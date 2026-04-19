//! Verifiable shell configuration loading.
//!
//! Compares a Nix-generated manifest (expected load order + BLAKE3 content
//! hashes) against a runtime trace (what actually loaded) to prove that
//! shell configuration is correct, complete, and in the right order.
//!
//! Hash conventions match pleme-io's `tameshi` attestation library:
//! RFC 9162 domain-separated BLAKE3 Merkle composition over canonical
//! [`manifest::ConfigLayer`] ordering. The manifest root produced here
//! is directly gatable by `inshou` (Nix pre-rebuild) and `sekiban` (K8s
//! admission webhook) without translation.

pub mod manifest;
pub mod merkle;
pub mod trace;
pub mod verify;
