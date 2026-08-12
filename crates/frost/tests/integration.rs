//! Integration tests for the frost shell binary.
//!
//! Tests are organized into modules:
//! - `cli`: Tests for command-line argument parsing (work now).
//! - `execution`: Tests for `-c` command execution (ignored until parser lands).
//! - `script`: Tests for script file execution (ignored until parser lands).

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

// ---------------------------------------------------------------------------
// CLI argument handling — these tests exercise clap parsing and work today.
// ---------------------------------------------------------------------------
mod cli {
    use super::*;

    #[test]
    fn help_prints_usage_and_exits_zero() {
        let output = Command::new(frost_bin())
            .arg("--help")
            .output()
            .expect("failed to run frost");

        assert!(output.status.success(), "exit code was not 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("zsh-compatible shell"),
            "help text should mention 'zsh-compatible shell', got:\n{stdout}"
        );
    }

    #[test]
    fn version_prints_version_and_exits_zero() {
        let output = Command::new(frost_bin())
            .arg("--version")
            .output()
            .expect("failed to run frost");

        assert!(output.status.success(), "exit code was not 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("frost"),
            "version output should contain 'frost', got:\n{stdout}"
        );
        // Derived, never hardcoded: this asserted the literal "0.1.0" and went
        // red the moment `release: workspace v0.1.1` landed — a stale test
        // pinning a number the release is supposed to move. `CARGO_PKG_VERSION`
        // is the same value the binary prints, so the assertion tracks the
        // release instead of rotting behind it.
        let expected = env!("CARGO_PKG_VERSION");
        assert!(
            stdout.contains(expected),
            "version output should contain '{expected}', got:\n{stdout}"
        );
    }

    #[test]
    fn c_flag_without_argument_shows_error() {
        let output = Command::new(frost_bin())
            .arg("-c")
            .output()
            .expect("failed to run frost");

        assert!(
            !output.status.success(),
            "should fail when -c has no argument"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        // clap emits an error about missing a value for -c
        assert!(
            stderr.contains("error"),
            "stderr should contain an error message, got:\n{stderr}"
        );
    }

    #[test]
    fn nonexistent_file_shows_error_and_exits_nonzero() {
        let output = Command::new(frost_bin())
            .arg("/tmp/frost-test-nonexistent-file-that-does-not-exist.sh")
            .output()
            .expect("failed to run frost");

        assert!(
            !output.status.success(),
            "should exit non-zero for a missing file"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("No such file")
                || stderr.contains("not found")
                || stderr.contains("frost:"),
            "stderr should mention the missing file, got:\n{stderr}"
        );
    }
}

// ---------------------------------------------------------------------------
// Command execution via `frost -c "..."`.
// Parser is now implemented — tests are live.
// ---------------------------------------------------------------------------
mod execution {
    use super::*;

    #[test]
    fn true_exits_zero() {
        let output = Command::new(frost_bin())
            .args(["-c", "true"])
            .output()
            .expect("failed to run frost");

        assert!(
            output.status.success(),
            "`true` should exit 0, got {:?}",
            output.status.code()
        );
    }

    #[test]
    fn false_exits_one() {
        let output = Command::new(frost_bin())
            .args(["-c", "false"])
            .output()
            .expect("failed to run frost");

        assert_eq!(
            output.status.code(),
            Some(1),
            "`false` should exit 1, got {:?}",
            output.status.code()
        );
    }

    #[test]
    fn echo_hello() {
        let output = Command::new(frost_bin())
            .args(["-c", "echo hello"])
            .output()
            .expect("failed to run frost");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "hello\n",
            "echo should print 'hello' followed by newline"
        );
    }

    #[test]
    fn echo_multiple_words() {
        let output = Command::new(frost_bin())
            .args(["-c", "echo hello world"])
            .output()
            .expect("failed to run frost");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "hello world\n",
            "echo should join words with spaces"
        );
    }

    #[test]
    fn exit_with_code() {
        let output = Command::new(frost_bin())
            .args(["-c", "exit 42"])
            .output()
            .expect("failed to run frost");

        assert_eq!(
            output.status.code(),
            Some(42),
            "`exit 42` should produce exit code 42, got {:?}",
            output.status.code()
        );
    }

    #[test]
    fn export_and_variable_expansion() {
        let output = Command::new(frost_bin())
            .args(["-c", "export FOO=bar; echo $FOO"])
            .output()
            .expect("failed to run frost");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "bar\n",
            "variable expansion after export should work"
        );
    }

    #[test]
    fn output_redirection() {
        let test_file = "/tmp/frost-test-redir.txt";
        // Clean up any previous run.
        let _ = std::fs::remove_file(test_file);

        let output = Command::new(frost_bin())
            .args(["-c", "echo hello > /tmp/frost-test-redir.txt"])
            .output()
            .expect("failed to run frost");

        assert!(output.status.success(), "redirection command should exit 0");
        let content =
            std::fs::read_to_string(test_file).expect("redirect target file should exist");
        assert_eq!(content, "hello\n", "file should contain 'hello\\n'");

        // Clean up.
        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn pipeline() {
        let output = Command::new(frost_bin())
            .args(["-c", "echo a | cat"])
            .output()
            .expect("failed to run frost");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "a\n",
            "pipeline should pass stdout through"
        );
    }

    #[test]
    fn and_list_success() {
        let output = Command::new(frost_bin())
            .args(["-c", "true && echo yes"])
            .output()
            .expect("failed to run frost");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "yes\n",
            "&& should execute right-hand side when left succeeds"
        );
    }

    #[test]
    fn or_list_fallback() {
        let output = Command::new(frost_bin())
            .args(["-c", "false || echo fallback"])
            .output()
            .expect("failed to run frost");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "fallback\n",
            "|| should execute right-hand side when left fails"
        );
    }

    #[test]
    fn and_list_short_circuit() {
        let output = Command::new(frost_bin())
            .args(["-c", "false && echo nope"])
            .output()
            .expect("failed to run frost");

        assert!(
            !output.status.success(),
            "&& after false should exit non-zero"
        );
        assert!(
            output.stdout.is_empty(),
            "&& should short-circuit: nothing should be printed"
        );
    }
}

// ---------------------------------------------------------------------------
// Script file execution.
// Ignored until the parser is implemented.
// ---------------------------------------------------------------------------
mod script {
    use super::*;
    use std::io::Write;

    #[test]
    fn run_script_file() {
        let dir = std::env::temp_dir();
        let script_path = dir.join("frost-test-script.sh");

        {
            let mut f = std::fs::File::create(&script_path).expect("failed to create temp script");
            writeln!(f, "echo from-script").expect("failed to write script");
        }

        let output = Command::new(frost_bin())
            .arg(script_path.to_str().unwrap())
            .output()
            .expect("failed to run frost");

        assert!(output.status.success(), "script should exit 0");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "from-script\n",
            "script output should be 'from-script\\n'"
        );

        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn run_multiline_script() {
        let dir = std::env::temp_dir();
        let script_path = dir.join("frost-test-multiline.sh");

        {
            let mut f = std::fs::File::create(&script_path).expect("failed to create temp script");
            writeln!(f, "echo line1").unwrap();
            writeln!(f, "echo line2").unwrap();
        }

        let output = Command::new(frost_bin())
            .arg(script_path.to_str().unwrap())
            .output()
            .expect("failed to run frost");

        assert!(output.status.success(), "multi-line script should exit 0");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "line1\nline2\n",
            "multi-line script should produce two lines of output"
        );

        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn script_exit_code_propagates() {
        let dir = std::env::temp_dir();
        let script_path = dir.join("frost-test-exit-code.sh");

        {
            let mut f = std::fs::File::create(&script_path).expect("failed to create temp script");
            writeln!(f, "exit 7").unwrap();
        }

        let output = Command::new(frost_bin())
            .arg(script_path.to_str().unwrap())
            .output()
            .expect("failed to run frost");

        assert_eq!(
            output.status.code(),
            Some(7),
            "script's exit code should propagate, got {:?}",
            output.status.code()
        );

        let _ = std::fs::remove_file(&script_path);
    }
}

// ---------------------------------------------------------------------------
// Redirect seam — an in-process command's output must land on the redirect
// target, never on the shell's own stdio.
//
// Regression cover for two failure modes found together on 2026-08-12, both
// of which let output escape a redirect (see `RedirectScope` in
// frost-exec/src/execute.rs):
//
//   1. A resolution query (`command -v`, `type`, `whence`, `which`) returned
//      from `execute_simple` ABOVE the block that applies redirects, so the
//      resolved path was printed to the terminal regardless. Observed in the
//      wild as a stray `/usr/bin/osascript` after any command over 30s:
//      frostmourne's own `defnotify` precmd body probes with
//      `command -v osascript >/dev/null 2>&1`.
//   2. A builtin whose output has no trailing newline stayed buffered in
//      Rust's `LineWriter` until AFTER the fds were restored — `printf abc
//      >file` wrote an empty file and printed `abc` to the terminal. Data
//      loss, not a cosmetic leak.
// ---------------------------------------------------------------------------
mod redirect_seam {
    use super::*;

    /// Run `script` under `frost -c` and return `(stdout, stderr)`.
    fn run(script: &str) -> (String, String) {
        let output = Command::new(frost_bin())
            .arg("-c")
            .arg(script)
            .output()
            .expect("failed to run frost");
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// A temp path unique to `tag`, removed first so a stale file from an
    /// earlier run can never be mistaken for this run's output.
    fn temp_path(tag: &str) -> std::path::PathBuf {
        let p =
            std::env::temp_dir().join(format!("frost-redirect-seam-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// ANTI-VACUITY: without a redirect these commands really do print. If
    /// this fails, every assertion below is passing for the wrong reason —
    /// a resolver that emits nothing would satisfy them all.
    #[test]
    fn resolution_queries_print_when_not_redirected() {
        for script in ["command -v ls", "type ls", "which ls", "whence -v ls"] {
            let (stdout, _) = run(script);
            assert!(
                stdout.contains("ls"),
                "`{script}` should print its resolution to stdout, got: {stdout:?}"
            );
        }
    }

    #[test]
    fn command_v_honors_stdout_redirect() {
        let path = temp_path("command-v");
        let (stdout, _) = run(&format!("command -v ls >{}", path.display()));

        assert_eq!(
            stdout, "",
            "`command -v ls >FILE` must print NOTHING to the terminal, got: {stdout:?}"
        );
        let written = std::fs::read_to_string(&path).expect("redirect target should exist");
        assert!(
            written.contains("ls"),
            "the resolved path must land in the redirect target, got: {written:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn type_whence_which_honor_stdout_redirect() {
        for (tag, script) in [
            ("type", "type ls"),
            ("which", "which ls"),
            ("whence", "whence -v ls"),
        ] {
            let path = temp_path(tag);
            let (stdout, _) = run(&format!("{script} >{}", path.display()));

            assert_eq!(
                stdout, "",
                "`{script} >FILE` must print NOTHING to the terminal, got: {stdout:?}"
            );
            let written = std::fs::read_to_string(&path).expect("redirect target should exist");
            assert!(
                written.contains("ls"),
                "`{script}` output must land in the redirect target, got: {written:?}"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    /// The exact shape frostmourne's `defnotify` precmd emits. This is the
    /// bug as the operator met it: a stray notifier path painted into the
    /// terminal after every long-running command.
    #[test]
    fn defnotify_probe_shape_emits_nothing() {
        let (stdout, stderr) = run("if command -v ls >/dev/null 2>&1; then :; fi");
        assert_eq!(
            stdout, "",
            "the defnotify `command -v` probe must be silent, got: {stdout:?}"
        );
        assert_eq!(
            stderr, "",
            "the defnotify `command -v` probe must be silent, got: {stderr:?}"
        );
    }

    /// Failure mode (2): buffered output with no trailing newline.
    #[test]
    fn printf_without_trailing_newline_reaches_redirect_target() {
        let path = temp_path("printf");
        let (stdout, _) = run(&format!("printf abc >{}", path.display()));

        assert_eq!(
            stdout, "",
            "`printf abc >FILE` must print NOTHING to the terminal, got: {stdout:?}"
        );
        let written = std::fs::read_to_string(&path).expect("redirect target should exist");
        assert_eq!(
            written, "abc",
            "unterminated builtin output must be flushed INTO the redirect target"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A redirect must not outlive the command it was written on — the guard
    /// restores the shell's own stdio on every return path.
    #[test]
    fn redirect_does_not_leak_into_the_next_command() {
        let path = temp_path("no-leak");
        let (stdout, _) = run(&format!("command -v ls >{}; echo AFTER", path.display()));

        assert_eq!(
            stdout, "AFTER\n",
            "the following command must write to the restored stdout, got: {stdout:?}"
        );
        let _ = std::fs::remove_file(&path);
    }
}
