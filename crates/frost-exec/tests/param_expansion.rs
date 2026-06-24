//! In-process parameter-expansion mechanics tests.
//!
//! These complement the byte-identical stdout-parity suite in
//! `frost/tests/param_expansion.rs` by asserting the *side effects* that are
//! invisible from stdout alone — the `${v:=word}` environment writeback and
//! the `${v:?word}` abort control-flow — directly against `ShellEnv` /
//! `ControlFlow`, the same harness-free style as `errexit.rs`. Oracle
//! behaviour verified against zsh 5.9 on 2026-06-24.

use frost_exec::{ControlFlow, ExecError, Executor, ShellEnv};

/// Parse + execute `code` in a fresh env; return `(env, exit_code)` where
/// `exit_code` is `Some(n)` iff the run aborted via `ControlFlow::Exit(n)`.
fn run(code: &str) -> (ShellEnv, Option<i32>) {
    let mut env = ShellEnv::new();
    let exit_code = {
        let mut exec = Executor::new(&mut env);
        let tokens = frost_exec::tokenize(code);
        let program = frost_parser::Parser::new(&tokens).parse();
        match exec.execute_program(&program) {
            Ok(_) => None,
            Err(ExecError::ControlFlow(ControlFlow::Exit(c))) => Some(c),
            Err(other) => panic!("unexpected non-Exit error: {other:?}"),
        }
    };
    (env, exit_code)
}

// ── ${v:=word} writeback lands in the environment ────────────────────

#[test]
fn assign_writeback_sets_var() {
    let (env, exit) = run(r#": ${v:=def}"#);
    assert_eq!(exit, None);
    assert_eq!(env.get_var("v"), Some("def"));
}

#[test]
fn assign_writeback_skips_when_set() {
    let (env, _) = run(r#"v=keep; : ${v:=def}"#);
    assert_eq!(env.get_var("v"), Some("keep"));
}

#[test]
fn assign_writeback_expands_default_before_storing() {
    let (env, _) = run(r#"o=World; : ${v:=Hi_$o}"#);
    assert_eq!(env.get_var("v"), Some("Hi_World"));
}

#[test]
fn assign_eq_form_only_when_unset() {
    // `${v=word}` assigns on unset only — an empty-but-set var is untouched.
    let (env, _) = run(r#"v=; : ${v=def}"#);
    assert_eq!(env.get_var("v"), Some(""));
}

// ── ${v:?word} aborts the run with a non-zero status ─────────────────

#[test]
fn error_unset_aborts_with_exit_one() {
    let (env, exit) = run(r#"echo "${v:?nope}"; SENTINEL=1"#);
    assert_eq!(exit, Some(1), "zsh aborts a non-interactive shell on :?");
    // The sentinel after the aborting command never runs.
    assert_eq!(env.get_var("SENTINEL"), None);
}

#[test]
fn error_in_assignment_aborts_before_any_command() {
    let (env, exit) = run(r#"x=${y:?nope}; SENTINEL=1"#);
    assert_eq!(exit, Some(1));
    assert_eq!(env.get_var("SENTINEL"), None);
    assert_eq!(env.get_var("x"), None);
}

#[test]
fn error_set_value_does_not_abort() {
    let (env, exit) = run(r#"v=ok; : ${v:?nope}; SENTINEL=1"#);
    assert_eq!(exit, None);
    assert_eq!(env.get_var("SENTINEL"), Some("1"));
}

#[test]
fn error_question_form_unset_only() {
    // `${v?word}` aborts on unset but tolerates empty-but-set.
    let (env, exit) = run(r#"v=; : ${v?nope}; SENTINEL=1"#);
    assert_eq!(exit, None);
    assert_eq!(env.get_var("SENTINEL"), Some("1"));
}
