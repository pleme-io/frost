//! The `[` (test) builtin — evaluate conditional expressions.

use crate::{Builtin, ShellEnvironment};

pub struct Test;

impl Builtin for Test {
    fn name(&self) -> &str {
        "["
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        // Strip trailing `]` if present.
        let args = if args.last() == Some(&"]") {
            &args[..args.len() - 1]
        } else {
            args
        };

        if eval_test(args, env) { 0 } else { 1 }
    }
}

pub struct TestKeyword;

impl Builtin for TestKeyword {
    fn name(&self) -> &str {
        "test"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if eval_test(args, env) { 0 } else { 1 }
    }
}

fn eval_test(args: &[&str], _env: &dyn ShellEnvironment) -> bool {
    match args.len() {
        0 => false,
        1 => !args[0].is_empty(),
        2 => eval_unary(args[0], args[1]),
        3 => eval_binary(args[0], args[1], args[2]),
        _ => {
            // Handle ! prefix
            if args[0] == "!" {
                return !eval_test(&args[1..], _env);
            }
            // Handle compound expressions with -a and -o
            // Find top-level -o first (lower precedence)
            for i in 0..args.len() {
                if args[i] == "-o" {
                    return eval_test(&args[..i], _env) || eval_test(&args[i + 1..], _env);
                }
            }
            for i in 0..args.len() {
                if args[i] == "-a" {
                    return eval_test(&args[..i], _env) && eval_test(&args[i + 1..], _env);
                }
            }
            // Parentheses
            if args[0] == "(" && args[args.len() - 1] == ")" {
                return eval_test(&args[1..args.len() - 1], _env);
            }
            false
        }
    }
}

fn eval_unary(op: &str, arg: &str) -> bool {
    match op {
        "-n" => !arg.is_empty(),
        "-z" => arg.is_empty(),
        "-e" | "-a" => std::path::Path::new(arg).exists(),
        "-f" => std::path::Path::new(arg).is_file(),
        "-d" => std::path::Path::new(arg).is_dir(),
        "-r" | "-w" | "-x" => std::path::Path::new(arg).exists(), // simplified
        "-s" => std::fs::metadata(arg).map(|m| m.len() > 0).unwrap_or(false),
        "-L" | "-h" => std::fs::symlink_metadata(arg)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false),
        "-t" => {
            // -t fd: check if fd is a terminal. Simplified: assume not a tty
            // in non-interactive contexts.
            false
        }
        "!" => arg.is_empty(),
        _ => false,
    }
}

fn eval_binary(left: &str, op: &str, right: &str) -> bool {
    match op {
        "=" | "==" => left == right,
        "!=" => left != right,
        "-eq" => parse_int(left) == parse_int(right),
        "-ne" => parse_int(left) != parse_int(right),
        "-lt" => parse_int(left) < parse_int(right),
        "-le" => parse_int(left) <= parse_int(right),
        "-gt" => parse_int(left) > parse_int(right),
        "-ge" => parse_int(left) >= parse_int(right),
        "-nt" => newer_than(left, right),
        "-ot" => newer_than(right, left),
        _ => false,
    }
}

fn parse_int(s: &str) -> i64 {
    s.parse().unwrap_or(0)
}

fn newer_than(a: &str, b: &str) -> bool {
    let a_time = std::fs::metadata(a).and_then(|m| m.modified()).ok();
    let b_time = std::fs::metadata(b).and_then(|m| m.modified()).ok();
    match (a_time, b_time) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}
