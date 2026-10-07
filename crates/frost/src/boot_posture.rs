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
    let via_ssh =
        std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_CLIENT").is_some();
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

// ── Nix system PATH integration ─────────────────────────────────────
//
// frostmourne (and bare frost) source none of nix-darwin's /etc/zshrc /
// path_helper chain, so a GUI-launched spawn (mado → frostmourne)
// inherits a PATH with no nix profile dirs at all — on a nix host that
// hides every home-manager tool. A login shell on a nix system is itself
// responsible for putting the nix profile dirs on PATH; this is frost
// doing that job directly, the same way nix-darwin's generated
// /etc/zshrc does for zsh.
//
// Reachability, not precedence: the canonical dirs are APPENDED (only the
// ones genuinely absent), so nothing that already resolves changes — the
// fix only ever ADDS a previously-unreachable tool. Nix-first precedence,
// where it matters, is owned by the frostmourne wrapper's --prefix and by
// the nix-darwin paths.d / launchd user-path layers; duplicating it here
// would reorder a caller's PATH behind their back.
//
// Behaviour, not preference: default ON, configured off with
// `FROST_NIX_PATH=0` (the fleet's configure-off-never-delete law), and a
// no-op off a nix system so non-nix hosts are untouched.

/// The canonical nix profile bin dirs, in nix-darwin login-shell order
/// (per-user first, then the system profile, then nix's own default
/// profile). `user` is `$USER` / `$LOGNAME` resolved by the caller, or
/// `None` when neither is set — in which case the per-user dir, which is
/// the only one that needs a name, is simply omitted.
fn nix_profile_dirs(user: Option<&str>) -> Vec<String> {
    let mut dirs = Vec::with_capacity(3);
    if let Some(u) = user.filter(|u| !u.is_empty()) {
        dirs.push(format!("/etc/profiles/per-user/{u}/bin"));
    }
    dirs.push("/run/current-system/sw/bin".to_string());
    dirs.push("/nix/var/nix/profiles/default/bin".to_string());
    dirs
}

/// Append each wanted dir that is not already present to `current` (a
/// `:`-joined PATH). Existing order is preserved exactly — nothing that
/// already resolves moves — and empty segments are dropped. Pure (no FS,
/// no env) so the append-and-dedup contract is unit-testable.
fn append_missing(want: &[String], current: &str) -> String {
    let present: Vec<&str> = current.split(':').filter(|s| !s.is_empty()).collect();
    let mut out: Vec<String> = present.iter().map(|s| (*s).to_string()).collect();
    for w in want {
        if !present.iter().any(|p| p == w) {
            out.push(w.clone());
        }
    }
    out.join(":")
}

/// True when this looks like a nix-managed system — the store is the one
/// signal present on both NixOS and nix-darwin.
fn on_nix_system() -> bool {
    std::path::Path::new("/nix/store").is_dir()
}

/// Given the current PATH, compute an enriched PATH with any missing nix
/// profile dirs appended, or `None` when nothing should change. `None` on
/// a non-nix host, when disabled via `FROST_NIX_PATH`, or when every
/// canonical dir is already present or absent-on-disk. Call near the top
/// of `main`, before rc-load, and apply the result to the shell env so
/// the rc and every child inherit it.
pub fn nix_enriched_path(current: &str) -> Option<String> {
    match std::env::var("FROST_NIX_PATH").as_deref() {
        Ok("0") | Ok("false") => return None,
        _ => {}
    }
    if !on_nix_system() {
        return None;
    }
    let user = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok());
    let want: Vec<String> = nix_profile_dirs(user.as_deref())
        .into_iter()
        .filter(|d| std::path::Path::new(d).is_dir())
        .collect();
    if want.is_empty() {
        return None;
    }
    let merged = append_missing(&want, current);
    (merged != current).then_some(merged)
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

    #[test]
    fn profile_dirs_order_is_nix_darwin_canonical() {
        assert_eq!(
            nix_profile_dirs(Some("luis.d")),
            vec![
                "/etc/profiles/per-user/luis.d/bin".to_string(),
                "/run/current-system/sw/bin".to_string(),
                "/nix/var/nix/profiles/default/bin".to_string(),
            ]
        );
    }

    #[test]
    fn profile_dirs_omit_per_user_without_a_name() {
        // No $USER → the per-user dir (the only one needing a name) drops,
        // the two static dirs remain. An empty name is treated as absent.
        assert_eq!(
            nix_profile_dirs(None),
            vec![
                "/run/current-system/sw/bin".to_string(),
                "/nix/var/nix/profiles/default/bin".to_string(),
            ]
        );
        assert_eq!(nix_profile_dirs(Some("")), nix_profile_dirs(None));
    }

    #[test]
    fn append_missing_adds_only_absent_dirs_at_the_end() {
        let want = vec![
            "/etc/profiles/per-user/luis.d/bin".to_string(),
            "/run/current-system/sw/bin".to_string(),
        ];
        // sys dir already present → only the per-user dir is appended, and
        // the existing order is preserved byte-for-byte up to the addition.
        let got = append_missing(&want, "/run/current-system/sw/bin:/usr/bin");
        assert_eq!(
            got,
            "/run/current-system/sw/bin:/usr/bin:/etc/profiles/per-user/luis.d/bin"
        );
    }

    #[test]
    fn append_missing_is_a_noop_when_all_present() {
        let want = vec!["/run/current-system/sw/bin".to_string()];
        let current = "/run/current-system/sw/bin:/usr/bin";
        assert_eq!(append_missing(&want, current), current);
    }

    #[test]
    fn append_missing_never_duplicates_and_drops_empty_segments() {
        let want = vec!["/nix/var/nix/profiles/default/bin".to_string()];
        // A trailing empty segment (PATH ending in ':') must not survive,
        // and a dir already present must not be added a second time.
        let got = append_missing(&want, "/usr/bin::/nix/var/nix/profiles/default/bin:");
        assert_eq!(got, "/usr/bin:/nix/var/nix/profiles/default/bin");
    }
}
