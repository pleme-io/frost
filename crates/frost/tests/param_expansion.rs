//! Parameter-expansion parity tests against zsh 5.9.
//!
//! Each `parity` row was captured directly from `/usr/bin/zsh` 5.9 on
//! 2026-06-24 (`zsh --no-rcs -c '<snippet>'`) and the expected stdout baked
//! in as the oracle. The test drives the `frost` binary with `-c` and asserts
//! byte-identical stdout — the same methodology the errexit truth-table tests
//! use, lifted to stdout capture for the expansion forms.
//!
//! Covered hardening backlog (all five HIGH-value gaps from the prior audit):
//!   1. Recursive default-word expansion — `${u:-pre_${o}_post}`, command sub.
//!   2. `${v:=word}` assign-writeback (value AND environment).
//!   3. `${v:?word}` / `${v?word}` abort with non-zero status.
//!   4. Glob pattern-ops — `${v#pat}` … `${v//pat/rep}` via the glob engine.
//!   5. `${=x}` / shwordsplit.

use std::path::PathBuf;
use std::process::Command;

/// Locate the `frost` binary built by cargo.
fn frost_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("frost");
    path
}

/// Run `code` through `frost -c` and return `(stdout, exit_code)`.
fn run(code: &str) -> (String, i32) {
    let out = Command::new(frost_bin())
        .arg("-c")
        .arg(code)
        .output()
        .expect("failed to run frost");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Assert frost's stdout exactly equals `expected` (the zsh 5.9 oracle).
fn parity(code: &str, expected: &str) {
    let (stdout, _) = run(code);
    assert_eq!(
        stdout, expected,
        "stdout mismatch for `{code}`\n  frost=[{stdout}]\n  zsh  =[{expected}]"
    );
}

// ── Gap 1: recursive default-word expansion ──────────────────────────

#[test]
fn default_word_expands_nested_brace() {
    // `${o}` inside the default must be expanded, not used literally.
    parity(r#"o=OTHER; echo "${u:-pre_${o}_post}""#, "pre_OTHER_post\n");
}

#[test]
fn default_word_expands_plain_var() {
    parity(r#"o=OTHER; echo "${u:-$o}""#, "OTHER\n");
}

#[test]
fn default_word_expands_nested_default() {
    parity(r#"o=v; echo "${u:-${o:-fallback}}""#, "v\n");
}

#[test]
fn default_word_expands_command_sub() {
    parity(r#"echo "${u:-$(echo cmdsub)}""#, "cmdsub\n");
}

#[test]
fn default_word_expands_nested_command_sub() {
    parity(r#"echo "${u:-$(echo a $(echo b))}""#, "a b\n");
}

#[test]
fn default_word_expands_arithmetic() {
    parity(r#"echo "${u:-$((2+3))}""#, "5\n");
}

#[test]
fn default_word_preserves_internal_whitespace() {
    // Runs of literal whitespace in the default are kept verbatim.
    parity(r#"echo "[${u:-a  b}]""#, "[a  b]\n");
}

#[test]
fn default_word_mixes_literal_and_var() {
    parity(r#"o=X; echo "${u:-a $o c}""#, "a X c\n");
}

#[test]
fn alternative_word_expands_recursively() {
    // `${v:+word}` runs the recursive expander on the alternative word too.
    parity(r#"v=set; o=Y; echo "${v:+got_$o}""#, "got_Y\n");
}

// ── Gap 2: ${v:=word} assign-writeback ───────────────────────────────

#[test]
fn assign_writeback_returns_and_assigns() {
    parity(r#"echo "${v:=def}"; echo "after=$v""#, "def\nafter=def\n");
}

#[test]
fn assign_writeback_skips_when_set() {
    parity(r#"v=set; echo "${v:=def}"; echo "after=$v""#, "set\nafter=set\n");
}

#[test]
fn assign_writeback_expands_default() {
    parity(r#"o=World; echo "${v:=Hello_$o}"; echo "$v""#, "Hello_World\nHello_World\n");
}

#[test]
fn assign_unset_only_form() {
    // `${v=word}` assigns only when unset (an empty-but-set var is left).
    parity(r#"v=; echo "[${v=def}]"; echo "after=[$v]""#, "[]\nafter=[]\n");
}

#[test]
fn assign_visible_to_later_command() {
    // The writeback from one command is seen by the next.
    parity(
        r#"echo "${a:=1}"; echo "${a:=2}"; echo "a=$a""#,
        "1\n1\na=1\n",
    );
}

#[test]
fn assign_in_null_command_persists() {
    parity(r#": ${x:=hello}; echo "$x""#, "hello\n");
}

// ── Gap 3: ${v:?word} / ${v?word} abort ──────────────────────────────

#[test]
fn error_unset_aborts_nonzero() {
    let (stdout, code) = run(r#"echo "${v:?must be set}"; echo AFTER"#);
    // The shell aborts before `echo AFTER` and exits non-zero.
    assert_eq!(stdout, "", "no stdout should be produced, got [{stdout}]");
    assert_eq!(code, 1, "expected exit 1 (zsh aborts), got {code}");
}

#[test]
fn error_set_value_does_not_abort() {
    parity(r#"v=x; echo "${v:?msg}"; echo AFTER"#, "x\nAFTER\n");
}

#[test]
fn error_in_assignment_aborts() {
    // `x=${y:?msg}` aborts before any command runs.
    let (stdout, code) = run(r#"x=${y:?nope}; echo AFTER"#);
    assert_eq!(stdout, "");
    assert_eq!(code, 1);
}

#[test]
fn error_question_unset_only() {
    // `${v?word}` aborts on unset but not on empty-but-set.
    parity(r#"v=; echo "[${v?msg}]"; echo AFTER"#, "[]\nAFTER\n");
}

// ── Gap 4: glob pattern-ops via the frost_glob engine ────────────────

#[test]
fn strip_prefix_shortest_and_longest() {
    parity(
        r#"f=a.b.c.d; echo "${f#*.}"; echo "${f##*.}""#,
        "b.c.d\nd\n",
    );
}

#[test]
fn strip_suffix_shortest_and_longest() {
    parity(
        r#"f=a.b.c.d; echo "${f%.*}"; echo "${f%%.*}""#,
        "a.b.c\na\n",
    );
}

#[test]
fn strip_prefix_char_class() {
    parity(r#"s=abc123; echo "${s#[a-c]}""#, "bc123\n");
}

#[test]
fn strip_suffix_char_class() {
    parity(r#"s=abc123; echo "${s%[0-9]}""#, "abc12\n");
}

#[test]
fn strip_star_spans_slashes() {
    // `*` in a pattern-op spans `/` (unlike filename globbing).
    parity(
        r#"p=/usr/local/bin; echo "${p##*/}"; echo "${p%/*}""#,
        "bin\n/usr/local\n",
    );
}

#[test]
fn strip_prefix_with_question() {
    parity(r#"v=hello; echo "${v#h?l}""#, "lo\n");
}

#[test]
fn replace_first_literal() {
    parity(r#"v=aXbXc; echo "${v/X/-}""#, "a-bXc\n");
}

#[test]
fn replace_all_literal() {
    parity(r#"v=aXbXc; echo "${v//X/-}""#, "a-b-c\n");
}

#[test]
fn replace_all_char_class() {
    parity(r#"v=a1b2c3; echo "${v//[0-9]/_}""#, "a_b_c_\n");
}

#[test]
fn replace_all_delete() {
    parity(r#"v=foobar; echo "${v//o/}""#, "fbar\n");
}

#[test]
fn replace_anchored_start() {
    parity(r#"v=abcabc; echo "${v/#abc/X}""#, "Xabc\n");
}

#[test]
fn replace_anchored_end() {
    parity(r#"v=abcabc; echo "${v/%abc/Y}""#, "abcY\n");
}

#[test]
fn replace_greedy_star() {
    parity(r#"v=foobar; echo "${v/o*/X}""#, "fX\n");
}

#[test]
fn strip_no_match_returns_value() {
    parity(r#"v=xyz; echo "${v#a}""#, "xyz\n");
}

// ── Gap 5: ${=name} shwordsplit ──────────────────────────────────────

#[test]
fn shwordsplit_basic() {
    // `${=x}` splits the value on IFS into separate words.
    parity(r#"x="a b c"; for w in ${=x}; do echo "[$w]"; done"#, "[a]\n[b]\n[c]\n");
}

#[test]
fn shwordsplit_collapses_runs() {
    parity(r#"x="a  b"; for w in ${=x}; do echo "[$w]"; done"#, "[a]\n[b]\n");
}

#[test]
fn shwordsplit_inside_double_quotes() {
    // The flag splits even inside double quotes — its whole purpose.
    parity(r#"x="a b c"; for w in "${=x}"; do echo "[$w]"; done"#, "[a]\n[b]\n[c]\n");
}

#[test]
fn shwordsplit_empty_yields_nothing() {
    parity(r#"x=""; for w in ${=x}; do echo "[$w]"; done; echo done"#, "done\n");
}
