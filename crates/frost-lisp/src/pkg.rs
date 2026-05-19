//! `defshellpkg` / `defload` / `deflockedpkg` — shell-package primitives.
//!
//! These three forms turn frost into a Nix-native, git-as-registry
//! consumer of shareable shell-behavior libraries. The split mirrors
//! Cargo:
//!
//! * **`(defshellpkg …)`** is the author's manifest. It lives in a
//!   `shellpkg.lisp` at the root of the *package* repo and declares
//!   what the package exports. frost-lisp parses it for introspection
//!   (count into `ApplySummary::packages` + retain in
//!   `ApplySummary::declared_packages`) but never applies its
//!   declarations directly — that's the `estante` resolver's job.
//!
//! * **`(deflockedpkg …)`** is one lockfile entry. `estante install`
//!   emits these into `shellpkg.lock.lisp`. Each pin carries name,
//!   source URL, resolved rev, NAR hash, BLAKE3 digest, and the
//!   filesystem path where the package's tree currently lives
//!   (typically a Nix store path).
//!
//! * **`(defload …)`** is the consumer's request. References a
//!   package by name; resolved against the locked-pkg map built up
//!   across the apply tree. Matching lock's `materialized-path` joined
//!   with the optional `:entrypoint` (default `rc.lisp`) becomes a
//!   `defsource`-style inclusion in the same pass.
//!
//! ```lisp
//! ;; shellpkg.lock.lisp — `estante install` emits this.
//! (deflockedpkg
//!   :name              "you-should-use"
//!   :source            "github:MichaelAquilina/zsh-you-should-use"
//!   :rev               "aa489f1d0bef818c4ec7d09b87a44d5cabaa9b6f"
//!   :nar-hash          "sha256-…"
//!   :blake3            "blake3-…"
//!   :materialized-path "/nix/store/abc-you-should-use-1.7.4/")
//!
//! ;; .frostrc.lisp — consumer side.
//! (defsource :path "./shellpkg.lock.lisp")
//! (defload   :pkg "you-should-use")
//! ```
//!
//! ## Apply semantics
//!
//! * `deflockedpkg` entries are collected before any `defload` runs.
//!   The locked-pkg map is threaded through the recursive apply tree
//!   alongside `visited`, so locks in any sourced file become visible
//!   to loads in any other file.
//! * Outer-file locks pre-scan BEFORE recursion descends so an inner
//!   file's `defload` can still resolve against an outer lock that
//!   appears later in source order.
//! * `defload` joins the lock's `materialized_path` with the optional
//!   `:entrypoint` (default `rc.lisp`) and dispatches through the same
//!   recursion as `defsource`. Unknown package → [`LispError::UnknownPkg`].
//! * Cycle guard: the same `visited` set used by `defsource` covers
//!   `defload` too, so a package that loads itself terminates cleanly.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// A package author's manifest. Lives in `shellpkg.lisp` at the package
/// repo root. frost reads this for introspection; `estante` reads it
/// to walk the dep graph.
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defshellpkg")]
pub struct PkgSpec {
    /// Slug-shape identifier — used by `defload :pkg` and as the
    /// stable key in the lockfile.
    pub name: String,
    /// Version string. Conventionally semver; not enforced in v0.1 so
    /// any opaque value a git tag can carry works.
    pub version: String,
    /// Resolver source URL. Recognized schemes: `github:owner/repo`,
    /// `github:owner/repo@ref`, `git+https://…#ref`, `git+ssh://…#ref`,
    /// `gist:gist-id`, `local:./rel/path`. Parsed by
    /// [`split_source_scheme`].
    pub source: String,
    /// Behavior kinds this package exports. Informational — surfaced
    /// in `estante search` output. Conventional values:
    /// `"alias"`, `"hook"`, `"completion"`, `"prompt"`, `"bind"`,
    /// `"trap"`, `"path"`, `"function"`, `"abbr"`, `"theme"`.
    #[serde(default)]
    pub exports: Vec<String>,
    /// Dependencies on other shell packages. Each entry is a
    /// `"name@constraint"` string (e.g. `"frost-defer@^1.0"`). The
    /// constraint half is informational in v0.1 — `estante`'s resolver
    /// pins by tag/rev. Empty for leaf packages.
    #[serde(default)]
    pub deps: Vec<String>,
    /// Hint that this package should be loaded lazily — only at first
    /// invocation of one of its triggers. v0.1 records the hint into
    /// the apply summary; honoring it in dispatch lands in a follow-up.
    #[serde(default)]
    pub lazy: bool,
}

/// A consumer's request to load a package. Resolved against the
/// `deflockedpkg` map built up by the apply pass.
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defload")]
pub struct LoadSpec {
    /// Package name — must match a `deflockedpkg :name` entry visible
    /// somewhere in the apply tree.
    pub pkg: String,
    /// Optional override of the entrypoint relative to the package
    /// root. Default is `rc.lisp`. Use when a package ships multiple
    /// loadable bundles (e.g. `"prompt.lisp"` vs `"hooks.lisp"`).
    #[serde(default)]
    pub entrypoint: Option<String>,
}

/// One resolved lockfile entry. `estante install` writes these into
/// `shellpkg.lock.lisp`. frost reads them at rc-apply time so it can
/// resolve `defload` requests without itself fetching.
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "deflockedpkg")]
pub struct LockedPkgSpec {
    /// Package name — must match a `defload :pkg` from the consumer.
    pub name: String,
    /// Source URL preserved verbatim from the originating `PkgSpec`.
    /// Carrying the URL makes the lockfile portable across forks
    /// (lazy.nvim's lockfile loses this; we deliberately keep it).
    pub source: String,
    /// Resolved git rev — 40-character SHA-1. The single source of
    /// reproducibility.
    pub rev: String,
    /// NAR hash of the materialized tree, Nix-compatible (sha256:…).
    /// Lets `builtins.fetchTarball` short-circuit on cache hits.
    pub nar_hash: String,
    /// BLAKE3 digest of the same tree. Independent attestation surface
    /// for the tameshi chain — independent of any Nix presence.
    pub blake3: String,
    /// Filesystem path where the package's contents currently live.
    /// Typically `$XDG_CACHE_HOME/estante/store/<name>-<rev>/` (cache
    /// placement) or `/nix/store/<hash>-<name>-<rev>/` (nix
    /// placement). Read at `defload`-apply time.
    pub materialized_path: String,
    /// Placement substrate — `"cache"`, `"nix"`, or `"both"`. Empty
    /// string defaults to `"cache"` for backward compatibility with
    /// pre-placement lockfiles.
    ///
    /// * `cache` — the package lives in estante's user-local cache
    ///   under `$XDG_CACHE_HOME/estante/store/…`. Fast, ad-hoc,
    ///   mutable. Default for `estante install` and `estante run`.
    /// * `nix`   — the package lives in `/nix/store/…`. Immutable,
    ///   content-addressed, fleet-reproducible. Required for
    ///   home-manager / NixOS / substrate fleet deploys. Created
    ///   via `nix store add-path` at `estante install` time.
    /// * `both`  — the package is present in BOTH stores. Useful
    ///   during a `place` migration so consumers don't lose the
    ///   in-flight reference.
    ///
    /// frost-lisp's `defload` itself is placement-agnostic — it just
    /// reads `materialized_path`. The field exists for the resolver,
    /// substrate's mk-shell-env, and tooling to reason about where
    /// the bytes physically live.
    #[serde(default)]
    pub placement: String,
}

/// Recognized `source:` scheme prefixes. Order is significant —
/// longest-first so `git+https` matches before `git`. Returned slice
/// is also the registry of schemes `estante` understands.
const RECOGNIZED_SCHEMES: &[&str] = &["git+https", "git+ssh", "github", "gist", "local"];

/// Split a `source` string into `(scheme, rest)`. Returns `None` for
/// unrecognized prefixes. Recognized: `github:`, `git+https:`,
/// `git+ssh:`, `gist:`, `local:`. The rest half is returned verbatim
/// (caller parses `org/repo`, `https://…#rev`, etc.).
#[must_use]
pub fn split_source_scheme(source: &str) -> Option<(&'static str, &str)> {
    for scheme in RECOGNIZED_SCHEMES {
        if let Some(rest) = source.strip_prefix(scheme).and_then(|s| s.strip_prefix(':')) {
            return Some((scheme, rest));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tatara_lisp::compile_typed;

    #[test]
    fn pkg_spec_minimum_roundtrip() {
        let src = r#"
            (defshellpkg
              :name "foo"
              :version "1.0.0"
              :source "github:org/foo")
        "#;
        let pkgs: Vec<PkgSpec> = compile_typed(src).unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "foo");
        assert_eq!(pkgs[0].version, "1.0.0");
        assert_eq!(pkgs[0].source, "github:org/foo");
        assert!(pkgs[0].exports.is_empty());
        assert!(pkgs[0].deps.is_empty());
        assert!(!pkgs[0].lazy);
    }

    #[test]
    fn pkg_spec_with_deps_and_exports() {
        let src = r#"
            (defshellpkg
              :name "bar"
              :version "0.1.0"
              :source "github:org/bar@v0.1.0"
              :exports ("alias" "hook")
              :deps ("frost-defer@^1.0")
              :lazy #t)
        "#;
        let pkgs: Vec<PkgSpec> = compile_typed(src).unwrap();
        assert_eq!(pkgs[0].exports, vec!["alias", "hook"]);
        assert_eq!(pkgs[0].deps, vec!["frost-defer@^1.0"]);
        assert!(pkgs[0].lazy);
    }

    #[test]
    fn load_spec_roundtrip() {
        let src = r#"
            (defload :pkg "foo")
            (defload :pkg "bar" :entrypoint "init.lisp")
        "#;
        let loads: Vec<LoadSpec> = compile_typed(src).unwrap();
        assert_eq!(loads.len(), 2);
        assert_eq!(loads[0].pkg, "foo");
        assert_eq!(loads[0].entrypoint, None);
        assert_eq!(loads[1].pkg, "bar");
        assert_eq!(loads[1].entrypoint.as_deref(), Some("init.lisp"));
    }

    #[test]
    fn locked_pkg_spec_roundtrip() {
        let src = r#"
            (deflockedpkg
              :name "foo"
              :source "github:org/foo"
              :rev "abc123"
              :nar-hash "sha256-zzz"
              :blake3 "blake3-yyy"
              :materialized-path "/nix/store/abc-foo/")
        "#;
        let locks: Vec<LockedPkgSpec> = compile_typed(src).unwrap();
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].name, "foo");
        assert_eq!(locks[0].rev, "abc123");
        assert_eq!(locks[0].materialized_path, "/nix/store/abc-foo/");
    }

    #[test]
    fn split_source_scheme_known() {
        assert_eq!(
            split_source_scheme("github:org/repo"),
            Some(("github", "org/repo"))
        );
        assert_eq!(
            split_source_scheme("github:org/repo@v1.0.0"),
            Some(("github", "org/repo@v1.0.0"))
        );
        assert_eq!(
            split_source_scheme("git+https://example.org/foo.git#abc"),
            Some(("git+https", "//example.org/foo.git#abc"))
        );
        assert_eq!(
            split_source_scheme("local:./pkgs/foo"),
            Some(("local", "./pkgs/foo"))
        );
    }

    #[test]
    fn split_source_scheme_unknown() {
        assert_eq!(split_source_scheme("ftp:foo"), None);
        assert_eq!(split_source_scheme("no-scheme"), None);
        assert_eq!(split_source_scheme(""), None);
    }

    // ─── Extensive coverage. ─────────────────────────────────────────

    #[test]
    fn pkg_spec_with_multiple_exports_in_alphabetical_order() {
        // The :exports list must round-trip in the authored order
        // (not sorted by tatara-lisp), so consumers can rely on order
        // for things like "first export is the principal kind."
        let src = r#"
            (defshellpkg :name "f" :version "0.1" :source "github:o/r"
                         :exports ("hook" "alias" "completion"))
        "#;
        let pkgs: Vec<PkgSpec> = compile_typed(src).unwrap();
        assert_eq!(pkgs[0].exports, vec!["hook", "alias", "completion"]);
    }

    #[test]
    fn pkg_spec_multiple_in_one_source_all_parsed() {
        let src = r#"
            (defshellpkg :name "a" :version "1" :source "github:o/a")
            (defshellpkg :name "b" :version "2" :source "github:o/b")
            (defshellpkg :name "c" :version "3" :source "github:o/c")
        "#;
        let pkgs: Vec<PkgSpec> = compile_typed(src).unwrap();
        assert_eq!(pkgs.len(), 3);
        assert_eq!(pkgs[0].name, "a");
        assert_eq!(pkgs[1].name, "b");
        assert_eq!(pkgs[2].name, "c");
    }

    #[test]
    fn locked_pkg_spec_multiple_entries_all_parsed() {
        let src = r#"
            (deflockedpkg :name "a" :source "github:o/a" :rev "r1"
                          :nar-hash "" :blake3 "h1" :materialized-path "/p/a")
            (deflockedpkg :name "b" :source "github:o/b" :rev "r2"
                          :nar-hash "" :blake3 "h2" :materialized-path "/p/b")
        "#;
        let locks: Vec<LockedPkgSpec> = compile_typed(src).unwrap();
        assert_eq!(locks.len(), 2);
        assert_eq!(locks[0].name, "a");
        assert_eq!(locks[1].rev, "r2");
    }

    #[test]
    fn load_spec_no_entrypoint_field_serializes_to_none() {
        let src = r#"(defload :pkg "foo")"#;
        let loads: Vec<LoadSpec> = compile_typed(src).unwrap();
        assert_eq!(loads[0].pkg, "foo");
        assert_eq!(loads[0].entrypoint, None);
    }

    #[test]
    fn pkg_spec_empty_exports_round_trips() {
        // Distinguish "no :exports field given" from ":exports ()". Both
        // should land as an empty Vec.
        let s1 = r#"(defshellpkg :name "x" :version "1" :source "github:o/r")"#;
        let s2 = r#"(defshellpkg :name "x" :version "1" :source "github:o/r" :exports ())"#;
        let p1: Vec<PkgSpec> = compile_typed(s1).unwrap();
        let p2: Vec<PkgSpec> = compile_typed(s2).unwrap();
        assert!(p1[0].exports.is_empty());
        assert!(p2[0].exports.is_empty());
    }

    #[test]
    fn split_source_scheme_distinguishes_git_https_from_git_ssh() {
        // The longest-first ordering matters here — `git+https:` and
        // `git+ssh:` share the `git` prefix but must not collide.
        let (s1, _) = split_source_scheme("git+https:example.org/x").unwrap();
        assert_eq!(s1, "git+https");
        let (s2, _) = split_source_scheme("git+ssh:git@example.org:x").unwrap();
        assert_eq!(s2, "git+ssh");
    }

    #[test]
    fn split_source_scheme_local_with_relative_path() {
        let (scheme, rest) = split_source_scheme("local:./pkgs/foo").unwrap();
        assert_eq!(scheme, "local");
        assert_eq!(rest, "./pkgs/foo");
    }
}
