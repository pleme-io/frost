//! The `whence` and `which` builtins — locate commands.

use crate::{Builtin, ShellEnvironment};

pub struct Whence;

impl Builtin for Whence {
    fn name(&self) -> &str {
        "whence"
    }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        let mut verbose = false;
        let mut names: &[&str] = args;

        if let Some(&first) = args.first() {
            if first == "-v" {
                verbose = true;
                names = &args[1..];
            } else if first == "-p" || first == "-w" {
                names = &args[1..];
            }
        }

        let mut status = 0;
        for name in names {
            if let Some(path) = find_in_path(name) {
                if verbose {
                    println!("{name} is {path}");
                } else {
                    println!("{path}");
                }
            } else {
                status = 1;
            }
        }
        status
    }
}

pub struct Which;

impl Builtin for Which {
    fn name(&self) -> &str {
        "which"
    }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        let mut status = 0;
        for name in args {
            if let Some(path) = find_in_path(name) {
                println!("{path}");
            } else {
                eprintln!("{name} not found");
                status = 1;
            }
        }
        status
    }
}

fn find_in_path(name: &str) -> Option<String> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let candidate = format!("{dir}/{name}");
            if std::path::Path::new(&candidate).is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
