//! test / [ builtin — POSIX conditional expression evaluation.

use crate::{Builtin, ShellEnvironment};
use std::path::Path;

pub struct Test;
pub struct Bracket;

impl Builtin for Test {
    fn name(&self) -> &str { "test" }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        evaluate_test(args)
    }
}

impl Builtin for Bracket {
    fn name(&self) -> &str { "[" }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        // Strip trailing ]
        let args = if args.last() == Some(&"]") {
            &args[..args.len() - 1]
        } else {
            eprintln!("frost: [: missing ]");
            return 2;
        };
        evaluate_test(args)
    }
}

fn evaluate_test(args: &[&str]) -> i32 {
    if args.is_empty() {
        return 1; // empty = false
    }

    // Single arg: true if non-empty string
    if args.len() == 1 {
        return if args[0].is_empty() { 1 } else { 0 };
    }

    // Unary operators
    if args.len() == 2 {
        let op = args[0];
        let val = args[1];
        return match op {
            "-n" => if val.is_empty() { 1 } else { 0 },
            "-z" => if val.is_empty() { 0 } else { 1 },
            "-e" | "-a" => if Path::new(val).exists() { 0 } else { 1 },
            "-f" => if Path::new(val).is_file() { 0 } else { 1 },
            "-d" => if Path::new(val).is_dir() { 0 } else { 1 },
            "-r" => if Path::new(val).exists() { 0 } else { 1 }, // simplified
            "-w" => if Path::new(val).exists() { 0 } else { 1 }, // simplified
            "-x" => if Path::new(val).exists() { 0 } else { 1 }, // simplified
            "-s" => {
                Path::new(val).metadata()
                    .map(|m| if m.len() > 0 { 0 } else { 1 })
                    .unwrap_or(1)
            }
            "-L" | "-h" => if Path::new(val).is_symlink() { 0 } else { 1 },
            "-b" | "-c" | "-p" | "-S" | "-t" | "-u" | "-g" | "-k" | "-G" | "-O" => 1, // stub
            "!" => if evaluate_test(&args[1..]) == 0 { 1 } else { 0 },
            _ => if val.is_empty() { 1 } else { 0 }, // treat as -n
        };
    }

    // Binary operators
    if args.len() == 3 {
        let left = args[0];
        let op = args[1];
        let right = args[2];
        return match op {
            "=" | "==" => if left == right { 0 } else { 1 },
            "!=" => if left != right { 0 } else { 1 },
            "-eq" => cmp_int(left, right, |a, b| a == b),
            "-ne" => cmp_int(left, right, |a, b| a != b),
            "-lt" => cmp_int(left, right, |a, b| a < b),
            "-le" => cmp_int(left, right, |a, b| a <= b),
            "-gt" => cmp_int(left, right, |a, b| a > b),
            "-ge" => cmp_int(left, right, |a, b| a >= b),
            "-nt" => newer_than(left, right),
            "-ot" => newer_than(right, left),
            "-ef" => same_file(left, right),
            _ => 2,
        };
    }

    // Negation: ! expr
    if args[0] == "!" {
        return if evaluate_test(&args[1..]) == 0 { 1 } else { 0 };
    }

    // -a and -o connectives
    if let Some(pos) = args.iter().position(|&a| a == "-a") {
        let left = evaluate_test(&args[..pos]);
        let right = evaluate_test(&args[pos + 1..]);
        return if left == 0 && right == 0 { 0 } else { 1 };
    }
    if let Some(pos) = args.iter().position(|&a| a == "-o") {
        let left = evaluate_test(&args[..pos]);
        let right = evaluate_test(&args[pos + 1..]);
        return if left == 0 || right == 0 { 0 } else { 1 };
    }

    2 // unrecognized
}

fn cmp_int(a: &str, b: &str, f: fn(i64, i64) -> bool) -> i32 {
    let a = a.parse::<i64>().unwrap_or(0);
    let b = b.parse::<i64>().unwrap_or(0);
    if f(a, b) { 0 } else { 1 }
}

fn newer_than(a: &str, b: &str) -> i32 {
    let ma = std::fs::metadata(a).and_then(|m| m.modified()).ok();
    let mb = std::fs::metadata(b).and_then(|m| m.modified()).ok();
    match (ma, mb) {
        (Some(a), Some(b)) => if a > b { 0 } else { 1 },
        _ => 1,
    }
}

fn same_file(a: &str, b: &str) -> i32 {
    let ma = std::fs::metadata(a).ok();
    let mb = std::fs::metadata(b).ok();
    match (ma, mb) {
        (Some(a), Some(b)) => {
            use std::os::unix::fs::MetadataExt;
            if a.dev() == b.dev() && a.ino() == b.ino() { 0 } else { 1 }
        }
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn empty_is_false() { assert_eq!(evaluate_test(&[]), 1); }
    #[test] fn nonempty_string_is_true() { assert_eq!(evaluate_test(&["hello"]), 0); }
    #[test] fn empty_string_is_false() { assert_eq!(evaluate_test(&[""]), 1); }
    #[test] fn string_eq() { assert_eq!(evaluate_test(&["a", "=", "a"]), 0); }
    #[test] fn string_ne() { assert_eq!(evaluate_test(&["a", "!=", "b"]), 0); }
    #[test] fn int_eq() { assert_eq!(evaluate_test(&["42", "-eq", "42"]), 0); }
    #[test] fn int_lt() { assert_eq!(evaluate_test(&["1", "-lt", "2"]), 0); }
    #[test] fn negation() { assert_eq!(evaluate_test(&["!", "hello"]), 1); }
    #[test] fn file_exists() { assert_eq!(evaluate_test(&["-f", "/dev/null"]), 1); } // /dev/null is not a regular file
    #[test] fn dir_exists() { assert_eq!(evaluate_test(&["-d", "/tmp"]), 0); }
    #[test] fn n_flag_nonempty() { assert_eq!(evaluate_test(&["-n", "x"]), 0); }
    #[test] fn z_flag_empty() { assert_eq!(evaluate_test(&["-z", ""]), 0); }
}
