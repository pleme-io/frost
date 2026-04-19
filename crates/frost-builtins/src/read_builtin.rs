//! The `read` builtin — read a line from stdin.

use std::io::{self, BufRead};

use crate::{Builtin, ShellEnvironment};

pub struct ReadBuiltin;

impl Builtin for ReadBuiltin {
    fn name(&self) -> &str {
        "read"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let mut prompt = None;
        let mut var_names = Vec::new();
        let mut raw = false;
        let mut i = 0;

        while i < args.len() {
            match args[i] {
                "-r" => raw = true,
                "-p" => {
                    i += 1;
                    if i < args.len() {
                        prompt = Some(args[i]);
                    }
                }
                arg if arg.starts_with('-') => {
                    // Skip unknown flags.
                }
                name => var_names.push(name),
            }
            i += 1;
        }

        if let Some(p) = prompt {
            eprint!("{p}");
        }

        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) => return 1, // EOF
            Ok(_) => {}
            Err(_) => return 1,
        }

        // Remove trailing newline.
        if line.ends_with('\n') {
            line.pop();
        }

        if !raw {
            // Handle backslash continuations (simplified: just remove \<newline>).
            line = line.replace("\\\n", "");
        }

        if var_names.is_empty() {
            env.set_var("REPLY", &line);
        } else if var_names.len() == 1 {
            env.set_var(var_names[0], &line);
        } else {
            // Split by IFS (default: space/tab/newline).
            let fields: Vec<&str> = line.splitn(var_names.len(), |c: char| c.is_whitespace()).collect();
            for (j, name) in var_names.iter().enumerate() {
                let value = fields.get(j).unwrap_or(&"");
                env.set_var(name, value);
            }
        }

        0
    }
}
