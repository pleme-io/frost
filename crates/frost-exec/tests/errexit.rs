//! ERR_EXIT (`set -e`) execution-semantics regression tests.
//!
//! Each case mirrors a row of the zsh 5.9 reference truth table captured
//! while implementing the feature (verified directly against `/usr/bin/zsh`
//! 5.9 on 2026-06-23). The shell aborts on a failing command UNLESS the
//! failure is "tested" rather than "fatal": a non-final `&&`/`||` operand,
//! the argument of `!`, or an if/while/until condition.
//!
//! Methodology: every script ends with `REACHED=1`. If ERR_EXIT aborts the
//! run, `execute_program` returns `Err(ControlFlow::Exit(..))` before the
//! marker runs, so the sentinel stays unset — a faithful, in-process analogue
//! of zsh's "did `echo AFTER` print?" observation, with zero shell-harness
//! flakiness.

use frost_exec::{ControlFlow, ExecError, Executor, ShellEnv};

/// Outcome of running a script to either completion or an ERR_EXIT abort.
struct Outcome {
    /// The trailing `REACHED=1` ran — i.e. execution continued past any
    /// failing command (zsh: CONTINUED / "AFTER printed").
    reached: bool,
    /// `execute_program` returned `Err(ControlFlow::Exit(code))`.
    exit_code: Option<i32>,
}

/// Parse + execute `code`, appending a `REACHED=1` sentinel as the final
/// command, and report whether the sentinel ran and how the run terminated.
fn run(code: &str) -> Outcome {
    let mut env = ShellEnv::new();
    let mut script = String::with_capacity(code.len() + 12);
    script.push_str(code);
    script.push_str("\nREACHED=1\n");

    let exit_code = {
        let mut exec = Executor::new(&mut env);
        let tokens = frost_exec::tokenize(&script);
        let program = frost_parser::Parser::new(&tokens).parse();
        match exec.execute_program(&program) {
            Ok(_) => None,
            Err(ExecError::ControlFlow(ControlFlow::Exit(code))) => Some(code),
            Err(other) => panic!("unexpected non-Exit error: {other:?}"),
        }
    };

    Outcome {
        reached: env.get_var("REACHED").is_some(),
        exit_code,
    }
}

/// Assert the script ran to completion (zsh: CONTINUED).
fn continues(code: &str) {
    let o = run(code);
    assert!(
        o.reached && o.exit_code.is_none(),
        "expected CONTINUED for `{code}`, got reached={} exit={:?}",
        o.reached,
        o.exit_code
    );
}

/// Assert ERR_EXIT aborted the run before the sentinel (zsh: EXITED).
fn aborts(code: &str) {
    let o = run(code);
    assert!(
        !o.reached && o.exit_code.is_some(),
        "expected EXITED for `{code}`, got reached={} exit={:?}",
        o.reached,
        o.exit_code
    );
}

// ── The core rule ────────────────────────────────────────────────────

#[test]
fn bare_failure_aborts() {
    aborts("set -e; false");
}

#[test]
fn success_continues() {
    continues("set -e; true");
}

#[test]
fn no_errexit_failure_continues() {
    // Without `set -e`, a failing command never aborts.
    continues("false; false");
}

#[test]
fn exit_code_is_preserved() {
    // The failing command's status becomes the shell's exit code.
    let o = run("set -e; false");
    assert_eq!(o.exit_code, Some(1));
}

// ── AND-OR lists: only the final operand is fatal ────────────────────

#[test]
fn nonfinal_and_operand_is_exempt() {
    // `false` is a non-final `&&` operand → exempt; `true` never runs.
    continues("set -e; false && true");
}

#[test]
fn final_and_operand_is_fatal() {
    continues("set -e; true && true"); // sanity: both succeed
    aborts("set -e; true && false");
}

#[test]
fn or_list_final_operand_governs() {
    continues("set -e; false || true"); // recovered → final op succeeds
    aborts("set -e; false || false"); // final op fails
}

// ── Negation (`!`) ───────────────────────────────────────────────────

#[test]
fn negated_pipeline_is_exempt() {
    continues("set -e; ! false"); // ! false → 0 anyway
    continues("set -e; ! true"); // ! true → 1, but negated → exempt
}

// ── Conditions are tested, not fatal ─────────────────────────────────

#[test]
fn if_condition_is_exempt() {
    continues("set -e; if false; then :; fi");
}

#[test]
fn while_condition_is_exempt() {
    continues("set -e; while false; do :; done");
}

#[test]
fn until_condition_is_exempt() {
    continues("set -e; until true; do :; done");
}

// ── Pipelines ────────────────────────────────────────────────────────

#[test]
fn pipeline_final_status_governs() {
    aborts("set -e; true | false"); // last element fails (no pipefail needed)
    continues("set -e; false | true"); // last element succeeds
}

// ── Compound-statement bodies are fatal at depth 0 ───────────────────

#[test]
fn failing_command_in_then_body_aborts() {
    aborts("set -e; if true; then false; true; fi");
}

#[test]
fn failing_command_in_brace_group_aborts() {
    aborts("set -e; { true; false; }");
}

#[test]
fn failing_command_in_for_body_aborts() {
    aborts("set -e; for x in 1 2; do false; done");
}

// ── Suppression unwinds: still fatal afterward ───────────────────────

#[test]
fn errexit_re_arms_after_condition() {
    aborts("set -e; if false; then :; fi; false");
}

#[test]
fn errexit_re_arms_after_loop() {
    aborts("set -e; while false; do :; done; false");
}

// ── Functions: errexit applies inside, and dynamically through calls ──

#[test]
fn failing_command_in_function_aborts() {
    aborts("set -e; myfn(){ false; }; myfn");
}

#[test]
fn function_as_nonfinal_operand_is_exempt() {
    // A failing function used as a non-final `||` operand stays exempt —
    // suppression propagates dynamically through the call.
    continues("set -e; myfn(){ false; }; myfn || true");
}

#[test]
fn function_as_final_operand_is_fatal() {
    aborts("set -e; myfn(){ false; }; true && myfn");
}

#[test]
fn function_called_in_condition_is_exempt() {
    continues("set -e; myfn(){ false; true; }; if myfn; then :; fi");
}

// ── Toggling the option on and off ───────────────────────────────────

#[test]
fn set_minus_o_errexit_enables() {
    aborts("set -o errexit; false");
}

#[test]
fn set_plus_e_disables() {
    continues("set -e; set +e; false");
}

#[test]
fn set_plus_o_errexit_disables() {
    continues("set +o errexit; false");
}
