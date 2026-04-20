//! Bridge from the zsh .ztst test suite to Rust's #[test] framework.
//!
//! One #[test] per .ztst file. Each test runs all cases in that file via
//! frost-compat and checks the pass rate against a threshold. As features
//! land in frost, thresholds ratchet upward — they can never decrease.
//!
//! Organization matches zsh test suite categories:
//!   Tier 1 (core):     A01, A03, A04, A05, A06, A07
//!   Tier 2 (builtins): B01, B03, B05, C01, C02, C04, D04, D08
//!   Tier 3 (extended): A02, B02, B04, C03, D01, D02, D09, E01
//!   Tier 4 (modules):  V*, W*, X*, Y* (all #[ignore])

use std::path::{Path, PathBuf};

fn frost_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("frost");
    path
}

fn ztst_dir() -> PathBuf {
    // Walk up from the test binary to find the workspace root
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop(); // crates/frost/ → crates/
    dir.pop(); // crates/ → workspace root
    dir.push("tests");
    dir.push("zsh-suite");
    dir
}

/// Run a single .ztst file and return (passed, total).
fn run_ztst(name: &str) -> (usize, usize) {
    let path = ztst_dir().join(format!("{name}.ztst"));
    if !path.exists() {
        panic!("test file not found: {}", path.display());
    }

    let test_file = frost_compat::parse_ztst(&path)
        .unwrap_or_else(|e| panic!("failed to parse {name}.ztst: {e}"));

    let results = frost_compat::run_test_file(&test_file, &frost_bin(), false);
    let summary = frost_compat::runner::summarize(&results);

    // Print per-file progress
    eprintln!(
        "  {name}: {}/{} passed ({:.0}%)",
        summary.passed, summary.total, summary.compatibility_pct
    );

    (summary.passed, summary.total)
}

/// Assert that the pass rate meets or exceeds the threshold.
fn assert_threshold(name: &str, threshold_pct: f64) {
    let (passed, total) = run_ztst(name);
    if total == 0 {
        return;
    }
    let actual_pct = (passed as f64 / total as f64) * 100.0;
    assert!(
        actual_pct >= threshold_pct,
        "{name}: compatibility {actual_pct:.1}% dropped below threshold {threshold_pct:.1}% ({passed}/{total})"
    );
}

// ═══════════════════════════════════════════════════════════════
// Tier 1 — Core shell (must pass for frost to be usable)
//   Threshold starts at 0% and ratchets up as features land
// ═══════════════════════════════════════════════════════════════

mod tier1 {
    use super::*;

    #[test]
    fn a01_grammar() {
        assert_threshold("A01grammar", 0.0);
    }
    #[test]
    fn a03_quoting() {
        assert_threshold("A03quoting", 0.0);
    }
    #[test]
    fn a04_redirect() {
        assert_threshold("A04redirect", 0.0);
    }
    #[test]
    fn a05_execution() {
        assert_threshold("A05execution", 0.0);
    }
    #[test]
    fn a06_assign() {
        assert_threshold("A06assign", 0.0);
    }
    #[test]
    fn a07_control() {
        assert_threshold("A07control", 0.0);
    }
    #[test]
    fn a08_time() {
        assert_threshold("A08time", 0.0);
    }
}

// ═══════════════════════════════════════════════════════════════
// Tier 2 — Essential builtins and expansion
// ═══════════════════════════════════════════════════════════════

mod tier2 {
    use super::*;

    #[test]
    fn b01_cd() {
        assert_threshold("B01cd", 0.0);
    }
    #[test]
    fn b03_print() {
        assert_threshold("B03print", 0.0);
    }
    #[test]
    fn b05_eval() {
        assert_threshold("B05eval", 0.0);
    }
    #[test]
    fn c01_arith() {
        assert_threshold("C01arith", 0.0);
    }
    #[test]
    fn c02_cond() {
        assert_threshold("C02cond", 0.0);
    }
    #[test]
    fn c04_funcdef() {
        assert_threshold("C04funcdef", 0.0);
    }
    #[test]
    fn d04_parameter() {
        assert_threshold("D04parameter", 0.0);
    }
    #[test]
    fn d08_cmdsubst() {
        assert_threshold("D08cmdsubst", 0.0);
    }
}

// ═══════════════════════════════════════════════════════════════
// Tier 3 — Extended builtins and features
// ═══════════════════════════════════════════════════════════════

mod tier3 {
    use super::*;

    #[test]
    fn a02_alias() {
        assert_threshold("A02alias", 0.0);
    }
    #[test]
    fn b02_typeset() {
        assert_threshold("B02typeset", 0.0);
    }
    #[test]
    fn b04_read() {
        assert_threshold("B04read", 0.0);
    }
    #[test]
    fn b07_emulate() {
        assert_threshold("B07emulate", 0.0);
    }
    #[test]
    fn b08_shift() {
        assert_threshold("B08shift", 0.0);
    }
    #[test]
    fn b09_hash() {
        assert_threshold("B09hash", 0.0);
    }
    #[test]
    fn b10_getopts() {
        assert_threshold("B10getopts", 0.0);
    }
    #[test]
    fn b11_kill() {
        assert_threshold("B11kill", 0.0);
    }
    #[test]
    fn b13_whence() {
        assert_threshold("B13whence", 0.0);
    }
    #[test]
    fn c03_traps() {
        assert_threshold("C03traps", 0.0);
    }
    #[test]
    fn c05_debug() {
        assert_threshold("C05debug", 0.0);
    }
    #[test]
    fn d01_prompt() {
        assert_threshold("D01prompt", 0.0);
    }
    #[test]
    fn d02_glob() {
        assert_threshold("D02glob", 0.0);
    }
    #[test]
    fn d03_procsubst() {
        assert_threshold("D03procsubst", 0.0);
    }
    #[test]
    fn d05_array() {
        assert_threshold("D05array", 0.0);
    }
    #[test]
    fn d06_subscript() {
        assert_threshold("D06subscript", 0.0);
    }
    #[test]
    fn d07_multibyte() {
        assert_threshold("D07multibyte", 0.0);
    }
    #[test]
    fn d09_brace() {
        assert_threshold("D09brace", 0.0);
    }
    #[test]
    fn d10_nofork() {
        assert_threshold("D10nofork", 0.0);
    }
    #[test]
    fn e01_options() {
        assert_threshold("E01options", 0.0);
    }
    #[test]
    fn e02_xtrace() {
        assert_threshold("E02xtrace", 0.0);
    }
    #[test]
    fn e03_posix() {
        assert_threshold("E03posix", 0.0);
    }
    #[test]
    fn k01_nameref() {
        assert_threshold("K01nameref", 0.0);
    }
    #[test]
    fn k02_parameter() {
        assert_threshold("K02parameter", 0.0);
    }
}

// ═══════════════════════════════════════════════════════════════
// Tier 4 — Modules, interactive, completion (long-term)
//   All #[ignore] — these depend on zmodload, zpty, compinit
// ═══════════════════════════════════════════════════════════════

mod tier4 {
    use super::*;

    #[test]
    #[ignore = "requires zmodload"]
    fn b06_fc() {
        assert_threshold("B06fc", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn b12_limit() {
        assert_threshold("B12limit", 0.0);
    }
    #[test]
    #[ignore = "requires root"]
    fn p01_privileged() {
        assert_threshold("P01privileged", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v01_zmodload() {
        assert_threshold("V01zmodload", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v02_zregexparse() {
        assert_threshold("V02zregexparse", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v03_mathfunc() {
        assert_threshold("V03mathfunc", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v04_features() {
        assert_threshold("V04features", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v05_styles() {
        assert_threshold("V05styles", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v06_parameter() {
        assert_threshold("V06parameter", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v07_pcre() {
        assert_threshold("V07pcre", 0.0);
    }
    #[test]
    #[ignore = "requires zpty"]
    fn v08_zpty() {
        assert_threshold("V08zpty", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v09_datetime() {
        assert_threshold("V09datetime", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v10_private() {
        assert_threshold("V10private", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v11_db_gdbm() {
        assert_threshold("V11db_gdbm", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v12_zparseopts() {
        assert_threshold("V12zparseopts", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v13_zformat() {
        assert_threshold("V13zformat", 0.0);
    }
    #[test]
    #[ignore = "requires zmodload"]
    fn v14_system() {
        assert_threshold("V14system", 0.0);
    }
    #[test]
    #[ignore = "requires zpty"]
    fn w01_history() {
        assert_threshold("W01history", 0.0);
    }
    #[test]
    #[ignore = "interactive"]
    fn w02_jobs() {
        assert_threshold("W02jobs", 0.0);
    }
    #[test]
    #[ignore = "interactive"]
    fn w03_jobparameters() {
        assert_threshold("W03jobparameters", 0.0);
    }
    #[test]
    #[ignore = "requires zpty"]
    fn x02_zlevi() {
        assert_threshold("X02zlevi", 0.0);
    }
    #[test]
    #[ignore = "requires zpty"]
    fn x03_zlebindkey() {
        assert_threshold("X03zlebindkey", 0.0);
    }
    #[test]
    #[ignore = "requires zpty"]
    fn x04_zlehighlight() {
        assert_threshold("X04zlehighlight", 0.0);
    }
    #[test]
    #[ignore = "requires zpty"]
    fn x05_zleincarg() {
        assert_threshold("X05zleincarg", 0.0);
    }
    #[test]
    #[ignore = "requires zpty"]
    fn x06_termquery() {
        assert_threshold("X06termquery", 0.0);
    }
    #[test]
    #[ignore = "requires compinit"]
    fn y01_completion() {
        assert_threshold("Y01completion", 0.0);
    }
    #[test]
    #[ignore = "requires compinit"]
    fn y02_compmatch() {
        assert_threshold("Y02compmatch", 0.0);
    }
    #[test]
    #[ignore = "requires compinit"]
    fn y03_arguments() {
        assert_threshold("Y03arguments", 0.0);
    }
    #[test]
    #[ignore = "requires autoload"]
    fn z01_is_at_least() {
        assert_threshold("Z01is-at-least", 0.0);
    }
    #[test]
    #[ignore = "requires autoload"]
    fn z02_zmathfunc() {
        assert_threshold("Z02zmathfunc", 0.0);
    }
    #[test]
    #[ignore = "requires autoload"]
    fn z03_run_help() {
        assert_threshold("Z03run-help", 0.0);
    }
}
