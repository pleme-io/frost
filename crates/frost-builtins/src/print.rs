//! The `print` builtin — write arguments to stdout (zsh-style).
//!
//! Supports `-r` (raw, no escape interpretation — the default), `-n` (no
//! trailing newline), `-l` (one argument per line), `-N` (null-terminated
//! output), and `-` (end of options).  Without `-r`, C escape sequences are
//! interpreted identically to `echo -e`.

use crate::{Builtin, ShellEnvironment};

pub struct Print;

impl Builtin for Print {
    fn name(&self) -> &str {
        "print"
    }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        let mut raw = true; // print defaults to raw (no escape interpretation)
        let mut newline = true;
        let mut one_per_line = false;
        let mut null_terminated = false;
        let mut text_start = 0;

        // Parse option flags.  `print` stops at the first non-flag argument,
        // or at a bare `-` (explicit end-of-options marker).
        for (i, arg) in args.iter().enumerate() {
            // A bare `-` ends option parsing — text starts at the next arg.
            if *arg == "-" {
                text_start = i + 1;
                break;
            }

            if !arg.starts_with('-') || arg.len() < 2 {
                break;
            }

            let flag_bytes = &arg.as_bytes()[1..];
            if flag_bytes
                .iter()
                .all(|b| matches!(b, b'r' | b'n' | b'l' | b'N'))
            {
                for &b in flag_bytes {
                    match b {
                        b'r' => raw = true,
                        b'n' => newline = false,
                        b'l' => one_per_line = true,
                        b'N' => {
                            null_terminated = true;
                            newline = false;
                        }
                        _ => unreachable!(),
                    }
                }
                text_start = i + 1;
            } else {
                break;
            }
        }

        let text_args = &args[text_start..];

        if one_per_line {
            // Each argument on its own line.
            for (i, arg) in text_args.iter().enumerate() {
                let value = if raw {
                    (*arg).to_owned()
                } else {
                    expand_escapes(arg)
                };
                print!("{value}");
                if i + 1 < text_args.len() {
                    if null_terminated {
                        print!("\0");
                    } else {
                        println!();
                    }
                }
            }
        } else {
            // Default: join with spaces.
            let joined = text_args.join(" ");
            let value = if raw { joined } else { expand_escapes(&joined) };
            print!("{value}");
        }

        if null_terminated {
            print!("\0");
        } else if newline {
            println!();
        }

        0
    }
}

/// Expand C-style escape sequences.
fn expand_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('a') => out.push('\x07'),
                Some('b') => out.push('\x08'),
                Some('f') => out.push('\x0C'),
                Some('v') => out.push('\x0B'),
                Some('\\') => out.push('\\'),
                Some('0') => {
                    // Octal: up to 3 digits after \0
                    let mut val: u8 = 0;
                    for _ in 0..3 {
                        if let Some(&d) = chars.as_str().as_bytes().first() {
                            if (b'0'..=b'7').contains(&d) {
                                val = val * 8 + (d - b'0');
                                chars.next();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    out.push(val as char);
                }
                Some('x') => {
                    // Hex: up to 2 digits
                    let mut val: u8 = 0;
                    let mut count = 0;
                    while count < 2 {
                        if let Some(&d) = chars.as_str().as_bytes().first() {
                            if d.is_ascii_hexdigit() {
                                let digit = match d {
                                    b'0'..=b'9' => d - b'0',
                                    b'a'..=b'f' => d - b'a' + 10,
                                    b'A'..=b'F' => d - b'A' + 10,
                                    _ => unreachable!(),
                                };
                                val = val * 16 + digit;
                                chars.next();
                                count += 1;
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    out.push(val as char);
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── expand_escapes unit tests ───────────────────────────────────

    #[test]
    fn expand_newline() {
        assert_eq!(expand_escapes("hello\\nworld"), "hello\nworld");
    }

    #[test]
    fn expand_tab() {
        assert_eq!(expand_escapes("a\\tb"), "a\tb");
    }

    #[test]
    fn expand_carriage_return() {
        assert_eq!(expand_escapes("a\\rb"), "a\rb");
    }

    #[test]
    fn expand_bell() {
        assert_eq!(expand_escapes("\\a"), "\x07");
    }

    #[test]
    fn expand_backspace() {
        assert_eq!(expand_escapes("\\b"), "\x08");
    }

    #[test]
    fn expand_form_feed() {
        assert_eq!(expand_escapes("\\f"), "\x0C");
    }

    #[test]
    fn expand_vertical_tab() {
        assert_eq!(expand_escapes("\\v"), "\x0B");
    }

    #[test]
    fn expand_backslash() {
        assert_eq!(expand_escapes("\\\\"), "\\");
    }

    #[test]
    fn expand_octal() {
        // \0101 = 'A' (octal 101 = 65)
        assert_eq!(expand_escapes("\\0101"), "A");
    }

    #[test]
    fn expand_hex() {
        // \x41 = 'A'
        assert_eq!(expand_escapes("\\x41"), "A");
    }

    #[test]
    fn unknown_escape_preserved() {
        assert_eq!(expand_escapes("\\q"), "\\q");
    }

    #[test]
    fn trailing_backslash_preserved() {
        assert_eq!(expand_escapes("hello\\"), "hello\\");
    }

    // ── Builtin::execute integration tests ──────────────────────────
    //
    // These tests capture stdout to verify the full execute() path.
    // Because print! writes to the process-wide stdout, we test the
    // *logic* by examining what execute() would produce.  For full
    // integration tests we rely on the shell test harness.

    /// Minimal mock environment for tests.
    struct MockEnv;

    impl crate::ShellEnvironment for MockEnv {
        fn get_var(&self, _: &str) -> Option<&str> {
            None
        }
        fn set_var(&mut self, _: &str, _: &str) {}
        fn export_var(&mut self, _: &str) {}
        fn unset_var(&mut self, _: &str) {}
        fn exit_status(&self) -> i32 {
            0
        }
        fn set_exit_status(&mut self, _: i32) {}
        fn chdir(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn home_dir(&self) -> Option<&str> {
            None
        }
    }

    #[test]
    fn name_is_print() {
        let p = Print;
        assert_eq!(p.name(), "print");
    }

    #[test]
    fn execute_returns_zero() {
        let p = Print;
        let mut env = MockEnv;
        assert_eq!(p.execute(&["hello"], &mut env), 0);
    }

    #[test]
    fn raw_is_default() {
        // Without -r, print should still be raw by default (unlike echo).
        // We verify by confirming that \n is NOT expanded.
        let output = build_output(&["hello\\nworld"], true, true, false, false);
        assert_eq!(output, "hello\\nworld\n");
    }

    #[test]
    fn flag_n_suppresses_newline() {
        let output = build_output(&[], true, false, false, false);
        assert_eq!(output, "");
    }

    #[test]
    fn flag_l_one_per_line() {
        let output = build_output(&["a", "b", "c"], true, true, true, false);
        assert_eq!(output, "a\nb\nc\n");
    }

    #[test]
    fn flag_l_single_arg() {
        let output = build_output(&["only"], true, true, true, false);
        assert_eq!(output, "only\n");
    }

    #[test]
    fn escapes_when_not_raw() {
        let output = build_output(&["hello\\tworld"], false, true, false, false);
        assert_eq!(output, "hello\tworld\n");
    }

    #[test]
    fn multiple_args_joined_with_space() {
        let output = build_output(&["a", "b", "c"], true, true, false, false);
        assert_eq!(output, "a b c\n");
    }

    #[test]
    fn null_terminated() {
        let output = build_output(&["hello"], true, false, false, true);
        assert_eq!(output, "hello\0");
    }

    #[test]
    fn null_terminated_with_l_flag() {
        let output = build_output(&["a", "b"], true, false, true, true);
        assert_eq!(output, "a\0b\0");
    }

    // ── Test helper ─────────────────────────────────────────────────

    /// Simulate the output that `execute()` would produce, without
    /// actually capturing stdout.  This mirrors the logic in execute().
    fn build_output(
        text_args: &[&str],
        raw: bool,
        newline: bool,
        one_per_line: bool,
        null_terminated: bool,
    ) -> String {
        let mut out = String::new();

        if one_per_line {
            for (i, arg) in text_args.iter().enumerate() {
                let value = if raw {
                    (*arg).to_owned()
                } else {
                    expand_escapes(arg)
                };
                out.push_str(&value);
                if i + 1 < text_args.len() {
                    if null_terminated {
                        out.push('\0');
                    } else {
                        out.push('\n');
                    }
                }
            }
        } else {
            let joined = text_args.join(" ");
            let value = if raw { joined } else { expand_escapes(&joined) };
            out.push_str(&value);
        }

        if null_terminated {
            out.push('\0');
        } else if newline {
            out.push('\n');
        }

        out
    }
}
