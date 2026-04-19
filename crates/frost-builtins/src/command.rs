//! The `command` builtin — run a command, bypassing shell functions.
//!
//! Also handles `command -v` (like which) and `command -V`.

use crate::{Builtin, ShellEnvironment};

pub struct CommandBuiltin;

impl Builtin for CommandBuiltin {
    fn name(&self) -> &str {
        "command"
    }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            return 0;
        }

        match args[0] {
            "-v" => {
                // command -v: print path to command.
                for name in &args[1..] {
                    if let Some(path) = find_in_path(name) {
                        println!("{path}");
                    } else {
                        return 1;
                    }
                }
                0
            }
            "-V" => {
                // command -V: verbose description.
                for name in &args[1..] {
                    if let Some(path) = find_in_path(name) {
                        println!("{name} is {path}");
                    } else {
                        eprintln!("frost: not found: {name}");
                        return 1;
                    }
                }
                0
            }
            _ => {
                // `command foo` — the executor handles bypassing functions.
                // This builtin can't actually exec; return 127 so executor knows.
                127
            }
        }
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
