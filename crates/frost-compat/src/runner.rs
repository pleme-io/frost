use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Serialize;

use crate::ztst::{ExpectedExit, ExpectedOutput, TestCase, TestFile};

/// Outcome of a single test execution.
#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    /// Source `.ztst` filename.
    pub file: String,
    /// Zero-based index within the file's test list.
    pub test_index: usize,
    /// Line number in the `.ztst` source.
    pub line_number: usize,
    /// Description from the status line.
    pub description: String,
    /// Pass/fail/skip/crash status.
    pub status: TestStatus,
    /// Optional diagnostic details.
    pub details: Option<String>,
}

/// Status of a single test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum TestStatus {
    Pass,
    Fail,
    Skip,
    /// frost panicked or was killed.
    Crash,
    /// Could not parse the `.ztst` test block.
    ParseError,
}

/// Aggregate summary across all tests.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub crashed: usize,
    pub parse_errors: usize,
    pub compatibility_pct: f64,
}

/// Per-test timeout.
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Run all tests in a parsed [`TestFile`] against the frost binary at `frost_path`.
pub fn run_test_file(test_file: &TestFile, frost_path: &Path, verbose: bool) -> Vec<TestResult> {
    // Run %prep if present.
    if let Some(prep) = &test_file.prep {
        if verbose {
            eprintln!("[prep] running prep for {}", test_file.name);
        }
        let prep_result = run_code(frost_path, prep, None);
        match prep_result {
            RunOutcome::Completed {
                exit_code, stderr, ..
            } => {
                if exit_code != 0 {
                    eprintln!(
                        "[prep] WARNING: prep for {} exited with code {exit_code}",
                        test_file.name
                    );
                    if !stderr.is_empty() {
                        eprintln!("[prep] stderr: {stderr}");
                    }
                }
            }
            RunOutcome::Timeout => {
                eprintln!("[prep] WARNING: prep for {} timed out", test_file.name);
            }
            RunOutcome::Error(e) => {
                eprintln!("[prep] WARNING: prep for {} failed: {e}", test_file.name);
            }
        }
    }

    let mut results = Vec::with_capacity(test_file.tests.len());

    for (idx, test) in test_file.tests.iter().enumerate() {
        let result = run_single_test(&test_file.name, idx, test, frost_path, verbose);
        results.push(result);
    }

    // Run %clean if present.
    if let Some(clean) = &test_file.clean {
        if verbose {
            eprintln!("[clean] running cleanup for {}", test_file.name);
        }
        let _ = run_code(frost_path, clean, None);
    }

    results
}

/// Execute a single test case and return the result.
fn run_single_test(
    file_name: &str,
    index: usize,
    test: &TestCase,
    frost_path: &Path,
    verbose: bool,
) -> TestResult {
    // If the description contains PARSE ERROR, it was a parse failure.
    if test.description.starts_with("PARSE ERROR") {
        return TestResult {
            file: file_name.to_string(),
            test_index: index,
            line_number: test.line_number,
            description: test.description.clone(),
            status: TestStatus::ParseError,
            details: test.failure_message.clone(),
        };
    }

    let outcome = run_code(frost_path, &test.code, test.stdin.as_deref());

    let (status, details) = match outcome {
        RunOutcome::Completed {
            exit_code,
            stdout,
            stderr,
        } => evaluate_test(test, exit_code, &stdout, &stderr),
        RunOutcome::Timeout => (
            TestStatus::Crash,
            Some(format!("test timed out after {}s", TEST_TIMEOUT.as_secs())),
        ),
        RunOutcome::Error(e) => (TestStatus::Crash, Some(format!("execution error: {e}"))),
    };

    // Apply the `f` (expected failure) flag: if the test was expected to fail,
    // a Fail becomes Pass and a Pass becomes Fail.
    let final_status = if test.flags.expected_fail {
        match status {
            TestStatus::Fail => TestStatus::Pass,
            TestStatus::Pass => TestStatus::Fail,
            other => other,
        }
    } else {
        status
    };

    if verbose {
        let symbol = match final_status {
            TestStatus::Pass => "PASS",
            TestStatus::Fail => "FAIL",
            TestStatus::Skip => "SKIP",
            TestStatus::Crash => "CRASH",
            TestStatus::ParseError => "PARSE_ERROR",
        };
        eprintln!(
            "  [{symbol}] {}:{} - {}",
            file_name, test.line_number, test.description
        );
        if let Some(ref d) = details {
            if final_status != TestStatus::Pass {
                for line in d.lines() {
                    eprintln!("         {line}");
                }
            }
        }
    }

    TestResult {
        file: file_name.to_string(),
        test_index: index,
        line_number: test.line_number,
        description: test.description.clone(),
        status: final_status,
        details,
    }
}

/// Evaluate actual output against expected values.
fn evaluate_test(
    test: &TestCase,
    exit_code: i32,
    actual_stdout: &str,
    actual_stderr: &str,
) -> (TestStatus, Option<String>) {
    let mut failures: Vec<String> = Vec::new();

    // Check exit code.
    match &test.expected_exit {
        ExpectedExit::Code(expected) => {
            if exit_code != *expected {
                failures.push(format!("exit code: expected {expected}, got {exit_code}"));
            }
        }
        ExpectedExit::Any => {}
    }

    // Check stdout (unless `d` flag is set).
    if !test.flags.no_stdout_diff {
        if let Some(ref expected) = test.expected_stdout {
            let actual = actual_stdout.strip_suffix('\n').unwrap_or(actual_stdout);
            match expected {
                ExpectedOutput::Exact(exp) => {
                    if actual != exp {
                        failures.push(format!(
                            "stdout mismatch:\n  expected: {exp:?}\n  actual:   {actual:?}"
                        ));
                    }
                }
                ExpectedOutput::Pattern(pat) => {
                    if !glob_match(pat, actual) {
                        failures.push(format!(
                            "stdout pattern mismatch:\n  pattern:  {pat:?}\n  actual:   {actual:?}"
                        ));
                    }
                }
            }
        }
    }

    // Check stderr (unless `D` flag is set).
    if !test.flags.no_stderr_diff {
        if let Some(ref expected) = test.expected_stderr {
            let actual = actual_stderr.strip_suffix('\n').unwrap_or(actual_stderr);
            match expected {
                ExpectedOutput::Exact(exp) => {
                    if actual != exp {
                        failures.push(format!(
                            "stderr mismatch:\n  expected: {exp:?}\n  actual:   {actual:?}"
                        ));
                    }
                }
                ExpectedOutput::Pattern(pat) => {
                    if !glob_match(pat, actual) {
                        failures.push(format!(
                            "stderr pattern mismatch:\n  pattern:  {pat:?}\n  actual:   {actual:?}"
                        ));
                    }
                }
            }
        }
    }

    if failures.is_empty() {
        (TestStatus::Pass, None)
    } else {
        (TestStatus::Fail, Some(failures.join("\n")))
    }
}

/// Simple line-by-line glob matching. Each line of the pattern is matched
/// against the corresponding line of the actual text using shell-style globs
/// (`*`, `?`, `[...]`).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat_lines: Vec<&str> = pattern.lines().collect();
    let text_lines: Vec<&str> = text.lines().collect();

    if pat_lines.len() != text_lines.len() {
        return false;
    }

    for (p, t) in pat_lines.iter().zip(text_lines.iter()) {
        if !glob_match_line(p, t) {
            return false;
        }
    }

    true
}

/// Match a single line against a glob pattern with `*` and `?` wildcards.
fn glob_match_line(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    glob_match_chars(&pat, &txt)
}

/// Recursive glob matching on character slices.
fn glob_match_chars(pat: &[char], txt: &[char]) -> bool {
    if pat.is_empty() {
        return txt.is_empty();
    }

    match pat[0] {
        '*' => {
            // `*` matches zero or more characters.
            // Try matching the rest of the pattern at every position.
            for i in 0..=txt.len() {
                if glob_match_chars(&pat[1..], &txt[i..]) {
                    return true;
                }
            }
            false
        }
        '?' => {
            // `?` matches exactly one character.
            if txt.is_empty() {
                false
            } else {
                glob_match_chars(&pat[1..], &txt[1..])
            }
        }
        c => {
            if txt.is_empty() || txt[0] != c {
                false
            } else {
                glob_match_chars(&pat[1..], &txt[1..])
            }
        }
    }
}

/// Outcome of running a code snippet.
enum RunOutcome {
    Completed {
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    Timeout,
    Error(String),
}

/// Run a code string via `frost -c "<code>"`, capturing output.
fn run_code(frost_path: &Path, code: &str, stdin_input: Option<&str>) -> RunOutcome {
    let mut cmd = Command::new(frost_path);
    cmd.arg("-c").arg(code);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    if stdin_input.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RunOutcome::Error(format!("failed to spawn frost: {e}")),
    };

    // Write stdin if needed.
    if let Some(input) = stdin_input {
        if let Some(mut stdin_handle) = child.stdin.take() {
            // Ignore write errors — the child may have already exited.
            let _ = stdin_handle.write_all(input.as_bytes());
            let _ = stdin_handle.write_all(b"\n");
            drop(stdin_handle);
        }
    }

    // Wait with timeout.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = child
                    .stdout
                    .take()
                    .map(|s| std::io::read_to_string(s).unwrap_or_default())
                    .unwrap_or_default();
                let stderr = child
                    .stderr
                    .take()
                    .map(|s| std::io::read_to_string(s).unwrap_or_default())
                    .unwrap_or_default();
                return RunOutcome::Completed {
                    exit_code: status.code().unwrap_or(-1),
                    stdout,
                    stderr,
                };
            }
            Ok(None) => {
                if start.elapsed() > TEST_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return RunOutcome::Timeout;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                return RunOutcome::Error(format!("wait failed: {e}"));
            }
        }
    }
}

/// Compute a summary from a slice of test results.
pub fn summarize(results: &[TestResult]) -> Summary {
    let total = results.len();
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut crashed = 0usize;
    let mut parse_errors = 0usize;

    for r in results {
        match r.status {
            TestStatus::Pass => passed += 1,
            TestStatus::Fail => failed += 1,
            TestStatus::Skip => skipped += 1,
            TestStatus::Crash => crashed += 1,
            TestStatus::ParseError => parse_errors += 1,
        }
    }

    let denominator = total.saturating_sub(skipped).saturating_sub(parse_errors);
    let compatibility_pct = if denominator > 0 {
        (passed as f64 / denominator as f64) * 100.0
    } else {
        0.0
    };

    Summary {
        total,
        passed,
        failed,
        skipped,
        crashed,
        parse_errors,
        compatibility_pct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_exact() {
        assert!(glob_match_line("hello", "hello"));
        assert!(!glob_match_line("hello", "world"));
    }

    #[test]
    fn glob_match_star() {
        assert!(glob_match_line("hel*", "hello"));
        assert!(glob_match_line("*llo", "hello"));
        assert!(glob_match_line("h*o", "hello"));
        assert!(glob_match_line("*", "anything"));
        assert!(glob_match_line("*", ""));
    }

    #[test]
    fn glob_match_question() {
        assert!(glob_match_line("hell?", "hello"));
        assert!(!glob_match_line("hell?", "hell"));
        assert!(glob_match_line("?ello", "hello"));
    }

    #[test]
    fn glob_match_multiline() {
        assert!(glob_match("foo\nbar", "foo\nbar"));
        assert!(!glob_match("foo\nbar", "foo\nbaz"));
        assert!(glob_match("f*\nb*", "foo\nbar"));
    }

    #[test]
    fn summarize_basic() {
        let results = vec![
            TestResult {
                file: "test".into(),
                test_index: 0,
                line_number: 1,
                description: "a".into(),
                status: TestStatus::Pass,
                details: None,
            },
            TestResult {
                file: "test".into(),
                test_index: 1,
                line_number: 5,
                description: "b".into(),
                status: TestStatus::Fail,
                details: None,
            },
            TestResult {
                file: "test".into(),
                test_index: 2,
                line_number: 10,
                description: "c".into(),
                status: TestStatus::Pass,
                details: None,
            },
        ];
        let s = summarize(&results);
        assert_eq!(s.total, 3);
        assert_eq!(s.passed, 2);
        assert_eq!(s.failed, 1);
        assert!((s.compatibility_pct - 66.66).abs() < 1.0);
    }
}
