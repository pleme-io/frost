//! The `echo` builtin — write arguments to stdout.
//!
//! Supports `-n` (no trailing newline), `-e` (interpret escapes),
//! and `-E` (do not interpret escapes, the default).

use crate::{Builtin, ShellEnvironment};

pub struct Echo;

impl Builtin for Echo {
    fn name(&self) -> &str {
        "echo"
    }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        let mut newline = true;
        let mut interpret_escapes = false;
        let mut text_start = 0;

        // Parse option flags. `echo` stops parsing flags at the first
        // argument that is not a recognized flag.
        for (i, arg) in args.iter().enumerate() {
            if !arg.starts_with('-') || arg.len() < 2 {
                break;
            }
            let flag_bytes = &arg.as_bytes()[1..];
            if flag_bytes.iter().all(|b| matches!(b, b'n' | b'e' | b'E')) {
                for &b in flag_bytes {
                    match b {
                        b'n' => newline = false,
                        b'e' => interpret_escapes = true,
                        b'E' => interpret_escapes = false,
                        _ => unreachable!(),
                    }
                }
                text_start = i + 1;
            } else {
                break;
            }
        }

        let text_args = &args[text_start..];
        let joined = text_args.join(" ");

        if interpret_escapes {
            print!("{}", expand_escapes(&joined));
        } else {
            print!("{joined}");
        }

        if newline {
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
                Some('c') => {
                    // \c suppresses further output (including trailing newline).
                    break;
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

    #[test]
    fn expand_newline() {
        assert_eq!(expand_escapes("hello\\nworld"), "hello\nworld");
    }

    #[test]
    fn expand_tab() {
        assert_eq!(expand_escapes("a\\tb"), "a\tb");
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
    fn expand_c_stops_output() {
        assert_eq!(expand_escapes("hello\\cworld"), "hello");
    }

    #[test]
    fn unknown_escape_preserved() {
        assert_eq!(expand_escapes("\\q"), "\\q");
    }
}
