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
        // The workspace version is 0.1.0 — verify it appears.
        assert!(
            stdout.contains("0.1.0"),
            "version output should contain '0.1.0', got:\n{stdout}"
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

        assert!(
            output.status.success(),
            "redirection command should exit 0"
        );
        let content = std::fs::read_to_string(test_file)
            .expect("redirect target file should exist");
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
            let mut f = std::fs::File::create(&script_path)
                .expect("failed to create temp script");
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
            let mut f = std::fs::File::create(&script_path)
                .expect("failed to create temp script");
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
            let mut f = std::fs::File::create(&script_path)
                .expect("failed to create temp script");
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
