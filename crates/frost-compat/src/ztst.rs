use std::path::Path;

/// A parsed `.ztst` test file.
#[derive(Debug, Clone)]
pub struct TestFile {
    /// Filename without extension.
    pub name: String,
    /// Code from the `%prep` section.
    pub prep: Option<String>,
    /// All test cases from the `%test` section.
    pub tests: Vec<TestCase>,
    /// Code from the `%clean` section.
    pub clean: Option<String>,
}

/// A single test block within a `%test` section.
#[derive(Debug, Clone)]
pub struct TestCase {
    /// Line number in the source file where this test starts.
    pub line_number: usize,
    /// Description from the status line.
    pub description: String,
    /// Shell code to execute (indented lines).
    pub code: String,
    /// Stdin input (`<` lines joined).
    pub stdin: Option<String>,
    /// Expected stdout (`>` or `*>` lines).
    pub expected_stdout: Option<ExpectedOutput>,
    /// Expected stderr (`?` or `*?` lines).
    pub expected_stderr: Option<ExpectedOutput>,
    /// Expected exit code.
    pub expected_exit: ExpectedExit,
    /// Test flags from the status line.
    pub flags: TestFlags,
    /// Failure explanation text (`F:` lines).
    pub failure_message: Option<String>,
}

/// Expected output matching mode.
#[derive(Debug, Clone)]
pub enum ExpectedOutput {
    /// Exact match (`>` or `?` lines).
    Exact(String),
    /// Pattern/glob match (`*>` or `*?` lines).
    Pattern(String),
}

/// Expected exit code.
#[derive(Debug, Clone)]
pub enum ExpectedExit {
    /// A specific exit code.
    Code(i32),
    /// `-` means don't care about exit code.
    Any,
}

/// Flags parsed from the status line.
#[derive(Debug, Clone, Default)]
pub struct TestFlags {
    /// `d` — skip stdout diff.
    pub no_stdout_diff: bool,
    /// `D` — skip stderr diff.
    pub no_stderr_diff: bool,
    /// `q` — quoted expansion.
    pub quoted_expansion: bool,
    /// `f` — expected failure.
    pub expected_fail: bool,
}

/// Which section of the `.ztst` file we are currently inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Before any `%` directive.
    Preamble,
    Prep,
    Test,
    Clean,
}

/// Parse a `.ztst` file at the given path into a [`TestFile`].
pub fn parse_ztst(path: &Path) -> Result<TestFile, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;

    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    let mut section = Section::Preamble;
    let mut prep_lines: Vec<String> = Vec::new();
    let mut clean_lines: Vec<String> = Vec::new();
    let mut test_blocks: Vec<(usize, Vec<String>)> = Vec::new();

    // Accumulator for the current test block's raw lines.
    let mut current_block: Vec<String> = Vec::new();
    let mut current_block_start: usize = 0;

    let lines: Vec<&str> = content.lines().collect();

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1; // 1-based

        // Lines starting with `#` in column 1 are comments everywhere.
        if line.starts_with('#') {
            continue;
        }

        // Section directives: `%prep`, `%test`, `%clean` in column 1.
        if line.starts_with('%') {
            let directive = line.trim();
            // Before switching sections, flush any pending test block.
            if section == Section::Test && !current_block.is_empty() {
                test_blocks.push((current_block_start, std::mem::take(&mut current_block)));
            }
            match directive {
                "%prep" => section = Section::Prep,
                "%test" => section = Section::Test,
                "%clean" => section = Section::Clean,
                _ => {
                    // Unknown directive — ignore.
                }
            }
            continue;
        }

        match section {
            Section::Preamble => {
                // Ignore lines before any section directive.
            }
            Section::Prep => {
                prep_lines.push(line.to_string());
            }
            Section::Clean => {
                clean_lines.push(line.to_string());
            }
            Section::Test => {
                // Blank lines separate test blocks.
                if line.is_empty() {
                    if !current_block.is_empty() {
                        test_blocks
                            .push((current_block_start, std::mem::take(&mut current_block)));
                    }
                } else {
                    if current_block.is_empty() {
                        current_block_start = line_num;
                    }
                    current_block.push(line.to_string());
                }
            }
        }
    }

    // Flush any remaining test block.
    if section == Section::Test && !current_block.is_empty() {
        test_blocks.push((current_block_start, current_block));
    }

    // Convert raw line groups into TestCase structs.
    let mut tests = Vec::new();
    for (start_line, block) in &test_blocks {
        match parse_test_block(*start_line, block) {
            Ok(tc) => tests.push(tc),
            Err(e) => {
                // Include the error as a degenerate test case so the runner can report it.
                tests.push(TestCase {
                    line_number: *start_line,
                    description: format!("PARSE ERROR at line {start_line}"),
                    code: block.join("\n"),
                    stdin: None,
                    expected_stdout: None,
                    expected_stderr: None,
                    expected_exit: ExpectedExit::Any,
                    flags: TestFlags::default(),
                    failure_message: Some(e),
                });
            }
        }
    }

    let prep = if prep_lines.is_empty() {
        None
    } else {
        Some(join_section_lines(&prep_lines))
    };

    let clean = if clean_lines.is_empty() {
        None
    } else {
        Some(join_section_lines(&clean_lines))
    };

    Ok(TestFile {
        name,
        prep,
        tests,
        clean,
    })
}

/// Join section lines, stripping a single leading space of indentation when
/// all lines are indented (prep/clean sections contain code blocks).
fn join_section_lines(lines: &[String]) -> String {
    let mut result = Vec::new();
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Strip one level of indentation (single space or tab).
            if line.starts_with("  ") {
                result.push(&line[2..]);
            } else if line.starts_with(' ') {
                result.push(&line[1..]);
            } else if line.starts_with('\t') {
                result.push(&line[1..]);
            } else {
                result.push(line.as_str());
            }
        } else {
            result.push(line.as_str());
        }
    }
    result.join("\n")
}

/// Parse a single test block (a group of non-empty lines between blank lines
/// within the `%test` section) into a [`TestCase`].
fn parse_test_block(start_line: usize, lines: &[String]) -> Result<TestCase, String> {
    let mut code_lines: Vec<String> = Vec::new();
    let mut stdin_lines: Vec<String> = Vec::new();
    let mut stdout_lines: Vec<String> = Vec::new();
    let mut stderr_lines: Vec<String> = Vec::new();
    let mut stdout_is_pattern = false;
    let mut stderr_is_pattern = false;
    let mut failure_lines: Vec<String> = Vec::new();
    let mut status_line: Option<String> = None;

    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Indented line — shell code.
            code_lines.push(strip_one_indent(line));
        } else if let Some(rest) = line.strip_prefix("*>") {
            // Pattern stdout. Must check `*>` before `>`.
            stdout_is_pattern = true;
            stdout_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        } else if let Some(rest) = line.strip_prefix('>') {
            // Exact stdout.
            stdout_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        } else if let Some(rest) = line.strip_prefix("*?") {
            // Pattern stderr.
            stderr_is_pattern = true;
            stderr_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        } else if let Some(rest) = line.strip_prefix('?') {
            // Exact stderr.
            stderr_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        } else if let Some(rest) = line.strip_prefix('<') {
            // Stdin.
            stdin_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        } else if let Some(rest) = line.strip_prefix("F:") {
            // Failure explanation.
            failure_lines.push(rest.trim().to_string());
        } else {
            // Anything else is treated as the status line.
            // The last such line wins (there should only be one).
            status_line = Some(line.clone());
        }
    }

    // Parse the status line.
    let (expected_exit, flags, description) = if let Some(ref sl) = status_line {
        parse_status_line(sl)?
    } else {
        // Default: expect exit 0, no flags, no description.
        (ExpectedExit::Code(0), TestFlags::default(), String::new())
    };

    let code = code_lines.join("\n");
    if code.is_empty() {
        return Err(format!(
            "test block at line {start_line} has no shell code"
        ));
    }

    let stdin = if stdin_lines.is_empty() {
        None
    } else {
        Some(stdin_lines.join("\n"))
    };

    let expected_stdout = if stdout_lines.is_empty() {
        None
    } else {
        let text = stdout_lines.join("\n");
        if stdout_is_pattern {
            Some(ExpectedOutput::Pattern(text))
        } else {
            Some(ExpectedOutput::Exact(text))
        }
    };

    let expected_stderr = if stderr_lines.is_empty() {
        None
    } else {
        let text = stderr_lines.join("\n");
        if stderr_is_pattern {
            Some(ExpectedOutput::Pattern(text))
        } else {
            Some(ExpectedOutput::Exact(text))
        }
    };

    let failure_message = if failure_lines.is_empty() {
        None
    } else {
        Some(failure_lines.join("\n"))
    };

    Ok(TestCase {
        line_number: start_line,
        description,
        code,
        stdin,
        expected_stdout,
        expected_stderr,
        expected_exit,
        flags,
        failure_message,
    })
}

/// Strip one level of leading indentation (spaces or tab).
fn strip_one_indent(line: &str) -> String {
    if line.starts_with("  ") {
        line[2..].to_string()
    } else if line.starts_with('\t') {
        line[1..].to_string()
    } else if line.starts_with(' ') {
        line[1..].to_string()
    } else {
        line.to_string()
    }
}

/// Parse a status line of the form `exit_code[flags]:description`.
///
/// Examples:
///   `0:simple echo test`
///   `-:don't care about exit`
///   `1dD:expected fail with no diff`
///   `0f:expected failure`
fn parse_status_line(line: &str) -> Result<(ExpectedExit, TestFlags, String), String> {
    // Split on the first `:`.
    let (prefix, description) = match line.find(':') {
        Some(pos) => (&line[..pos], line[pos + 1..].trim().to_string()),
        None => {
            // No colon — entire line is prefix, no description.
            (line.as_ref(), String::new())
        }
    };

    if prefix.is_empty() {
        return Ok((ExpectedExit::Code(0), TestFlags::default(), description));
    }

    // The prefix is: exit_code optionally followed by flag characters.
    // exit_code is either `-` or a sequence of digits.
    let mut chars = prefix.chars().peekable();
    let exit_code = if chars.peek() == Some(&'-') {
        chars.next();
        ExpectedExit::Any
    } else {
        let mut digits = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                digits.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            // No digits and no `-` — default to 0.
            ExpectedExit::Code(0)
        } else {
            let code: i32 = digits
                .parse()
                .map_err(|e| format!("invalid exit code '{digits}': {e}"))?;
            ExpectedExit::Code(code)
        }
    };

    let mut flags = TestFlags::default();
    for c in chars {
        match c {
            'd' => flags.no_stdout_diff = true,
            'D' => flags.no_stderr_diff = true,
            'q' => flags.quoted_expansion = true,
            'f' => flags.expected_fail = true,
            _ => {
                // Unknown flag — ignore for forward compatibility.
            }
        }
    }

    Ok((exit_code, flags, description))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_ztst(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn parse_simple_test_file() {
        let content = "\
# A test file
%prep
  mkdir -p /tmp/test_ztst

%test

  echo hello
>hello
0:simple echo

  echo world >&2
?world
0:stderr echo

%clean
  rm -rf /tmp/test_ztst
";
        let f = write_temp_ztst(content);
        let tf = parse_ztst(f.path()).unwrap();
        assert!(tf.prep.is_some());
        assert!(tf.clean.is_some());
        assert_eq!(tf.tests.len(), 2);
        assert_eq!(tf.tests[0].description, "simple echo");
        assert_eq!(tf.tests[0].code, "echo hello");
        assert_eq!(tf.tests[1].description, "stderr echo");
    }

    #[test]
    fn parse_status_line_basic() {
        let (exit, flags, desc) = parse_status_line("0:hello world").unwrap();
        assert!(matches!(exit, ExpectedExit::Code(0)));
        assert!(!flags.expected_fail);
        assert_eq!(desc, "hello world");
    }

    #[test]
    fn parse_status_line_any_exit() {
        let (exit, _flags, desc) = parse_status_line("-:any exit").unwrap();
        assert!(matches!(exit, ExpectedExit::Any));
        assert_eq!(desc, "any exit");
    }

    #[test]
    fn parse_status_line_flags() {
        let (exit, flags, desc) = parse_status_line("1dDf:flagged test").unwrap();
        assert!(matches!(exit, ExpectedExit::Code(1)));
        assert!(flags.no_stdout_diff);
        assert!(flags.no_stderr_diff);
        assert!(flags.expected_fail);
        assert_eq!(desc, "flagged test");
    }

    #[test]
    fn parse_pattern_output() {
        let content = "\
%test

  ls /nonexistent
*?ls:*nonexistent*
1:pattern stderr match
";
        let f = write_temp_ztst(content);
        let tf = parse_ztst(f.path()).unwrap();
        assert_eq!(tf.tests.len(), 1);
        assert!(matches!(
            tf.tests[0].expected_stderr,
            Some(ExpectedOutput::Pattern(_))
        ));
    }

    #[test]
    fn parse_stdin_input() {
        let content = "\
%test

  cat
<hello from stdin
>hello from stdin
0:stdin passthrough
";
        let f = write_temp_ztst(content);
        let tf = parse_ztst(f.path()).unwrap();
        assert_eq!(tf.tests.len(), 1);
        assert_eq!(
            tf.tests[0].stdin.as_deref(),
            Some("hello from stdin")
        );
    }
}
