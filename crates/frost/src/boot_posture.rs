//! Boot posture — typed knowledge of how frost was launched.
//!
//! At the top of frost's shell-startup path (before rc-load), frost
//! detects its boot posture once and stashes it for tracing + future
//! conditional rc-load behavior. The detected posture lets future code
//! branch on questions like "are we in a Nix shell?", "do we have
//! direnv?", "are we inside an SSH session?", "is this an interactive
//! login shell?" — all without re-probing the environment.
//!
//! # Substrate gap (2026-05-30)
//!
//! The eventual canonical implementation lives in
//! [`pleme-io/kindling`](https://github.com/pleme-io/kindling) as
//! `kindling::posture::detect()` — kindling already has all the
//! ingredients (`nix::detect()`, `platform::detect()`, direnv-setup
//! introspection) but only exposes them through its `main.rs` binary.
//! It ships **no `lib.rs`**, so frost cannot reach those types as a
//! Rust library consumer today.
//!
//! Until kindling grows a library surface, this module provides the
//! typed border with a minimal local implementation. The eventual
//! swap is a one-line change in [`detect`]:
//!
//! ```ignore
//! // After kindling exposes pub mod posture:
//! pub fn detect() -> BootPosture {
//!     kindling::posture::detect().into()
//! }
//! ```
//!
//! The substrate-level work to close the gap:
//!
//! 1. Add `src/lib.rs` to `pleme-io/kindling` re-exporting the
//!    crate-internal `nix`, `platform`, and (new) `posture` modules.
//! 2. Add `[lib]` + `[[bin]]` sections to `kindling/Cargo.toml`
//!    splitting the existing binary from the library crate.
//! 3. Author `kindling::posture::detect()` returning a typed
//!    `Posture` value composed from the existing nix + platform
//!    helpers + new SSH / login / interactive heuristics.
//! 4. Frost swaps the local implementation here for a delegating
//!    call.

use std::sync::OnceLock;

/// Static cache for the detected posture so detection runs once per
/// shell process. Frost calls [`detect`] near the top of `main` and
/// downstream consumers can call it freely without re-probing.
static POSTURE: OnceLock<BootPosture> = OnceLock::new();

/// Typed boot posture — what frost knows about how it was launched.
///
/// Every field is optional / boolean — frost must still start even if
/// detection fails. Booleans default to `false` (not detected).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPosture {
    /// Whether the parent process appears to be a Nix shell (heuristic:
    /// `IN_NIX_SHELL` env var present).
    pub in_nix_shell: bool,
    /// Whether direnv is active in the current process (heuristic:
    /// `DIRENV_DIR` env var present).
    pub direnv_active: bool,
    /// Whether the shell appears to be running inside an SSH session
    /// (heuristic: `SSH_CONNECTION` or `SSH_CLIENT` env var present).
    pub via_ssh: bool,
    /// Whether the shell is interactive (heuristic: stdin is a tty).
    pub interactive: bool,
    /// Whether the shell is a login shell (heuristic: argv[0] starts
    /// with `-` per the Bourne-shell convention).
    pub login: bool,
}

impl BootPosture {
    /// All-false default — used as a fallback when detection cannot run.
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            in_nix_shell: false,
            direnv_active: false,
            via_ssh: false,
            interactive: false,
            login: false,
        }
    }
}

/// Detect the current boot posture. Memoized — the heavy lifting runs
/// exactly once per process. Subsequent calls return the cached value
/// even if the underlying environment changes mid-run.
pub fn detect() -> &'static BootPosture {
    POSTURE.get_or_init(detect_inner)
}

fn detect_inner() -> BootPosture {
    use std::io::IsTerminal;

    let in_nix_shell = std::env::var_os("IN_NIX_SHELL").is_some();
    let direnv_active = std::env::var_os("DIRENV_DIR").is_some();
    let via_ssh = std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_CLIENT").is_some();
    let interactive = std::io::stdin().is_terminal();
    let login = std::env::args()
        .next()
        .is_some_and(|argv0| argv0.starts_with('-'));

    BootPosture {
        in_nix_shell,
        direnv_active,
        via_ssh,
        interactive,
        login,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_all_false() {
        let p = BootPosture::unknown();
        assert!(!p.in_nix_shell);
        assert!(!p.direnv_active);
        assert!(!p.via_ssh);
        assert!(!p.interactive);
        assert!(!p.login);
    }

    #[test]
    fn detect_returns_stable_reference() {
        // Memoization: two consecutive calls return the same address.
        let a = detect() as *const _;
        let b = detect() as *const _;
        assert_eq!(a, b);
    }

    #[test]
    fn detect_produces_a_value() {
        let posture = detect();
        // Field types are checked by the compiler; this just confirms
        // detection actually runs without panicking.
        let _ = posture.in_nix_shell;
        let _ = posture.direnv_active;
        let _ = posture.via_ssh;
        let _ = posture.interactive;
        let _ = posture.login;
    }
}
