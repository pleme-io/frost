//! Frost shell — public library surface (currently boot-posture only).
//!
//! Most of frost is a binary crate (`src/main.rs`). This `lib.rs`
//! holds the typed shapes that benefit from being reachable by name
//! from tests and downstream consumers (frostmourne curated distro,
//! frost-mcp tools, kanshou introspection).

pub mod boot_posture;
