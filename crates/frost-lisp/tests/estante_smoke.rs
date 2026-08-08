//! Estante smoke test — proves the typed `(defshellpkg …)` /
//! `(defload …)` / `(deflockedpkg …)` surface exposed by
//! `frost-lisp::pkg` is alive end-to-end.
//!
//! `pleme-io/estante` ships the typed primitives for shell-package
//! authoring (`PkgSpec`), consumer requests (`LoadSpec`), and
//! lockfile entries (`LockedPkgSpec`) inside `frost-lisp::pkg`,
//! re-exported at the crate root. This integration test constructs
//! one of each shape via tatara-lisp source, parses+validates it
//! through `tatara_lisp::compile_typed`, and asserts the round-trip
//! produces the expected typed value. The test runs in the same
//! `cargo test -p frost-lisp` envelope as the existing unit tests
//! in `pkg.rs` itself, but lives at integration level so future
//! changes that break the public surface fail loudly.

use frost_lisp::{LoadSpec, LockedPkgSpec, PkgSpec};
use tatara_lisp::compile_typed;

#[test]
fn estante_typed_builders_alive_end_to_end() {
    // ── defshellpkg — author's manifest ──────────────────────────
    let pkg_src = r#"
        (defshellpkg
          :name "frost-defer"
          :version "1.0.0"
          :source "github:pleme-io/frost-defer"
          :exports ("hook" "function")
          :deps ()
          :lazy #t)
    "#;
    let pkgs: Vec<PkgSpec> = compile_typed(pkg_src).expect("PkgSpec compile_typed");
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].name, "frost-defer");
    assert_eq!(pkgs[0].version, "1.0.0");
    assert_eq!(pkgs[0].source, "github:pleme-io/frost-defer");
    assert_eq!(pkgs[0].exports, vec!["hook", "function"]);
    assert!(pkgs[0].lazy);

    // ── defload — consumer's request ─────────────────────────────
    let load_src = r#"
        (defload :pkg "frost-defer")
    "#;
    let loads: Vec<LoadSpec> = compile_typed(load_src).expect("LoadSpec compile_typed");
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0].pkg, "frost-defer");
    assert_eq!(loads[0].entrypoint, None);

    // ── deflockedpkg — resolver's lockfile entry ─────────────────
    let lock_src = r#"
        (deflockedpkg
          :name "frost-defer"
          :source "github:pleme-io/frost-defer"
          :rev "aa489f1d0bef818c4ec7d09b87a44d5cabaa9b6f"
          :nar-hash "sha256-deadbeef"
          :blake3 "blake3-cafebabe"
          :materialized-path "/nix/store/abc-frost-defer-1.0.0/"
          :placement "nix")
    "#;
    let locks: Vec<LockedPkgSpec> = compile_typed(lock_src).expect("LockedPkgSpec compile_typed");
    assert_eq!(locks.len(), 1);
    assert_eq!(locks[0].name, "frost-defer");
    assert_eq!(locks[0].rev, "aa489f1d0bef818c4ec7d09b87a44d5cabaa9b6f");
    assert_eq!(
        locks[0].materialized_path,
        "/nix/store/abc-frost-defer-1.0.0/"
    );
    assert_eq!(locks[0].placement, "nix");
}
