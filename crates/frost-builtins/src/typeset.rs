//! The `typeset`/`local`/`declare` builtins — variable declaration.

use crate::{Builtin, ShellEnvironment};

pub struct Typeset;

impl Builtin for Typeset {
    fn name(&self) -> &str {
        "typeset"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        typeset_impl(args, env)
    }
}

pub struct Local;

impl Builtin for Local {
    fn name(&self) -> &str {
        "local"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        typeset_impl(args, env)
    }
}

pub struct Declare;

impl Builtin for Declare {
    fn name(&self) -> &str {
        "declare"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        typeset_impl(args, env)
    }
}

fn typeset_impl(args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
    let mut export = false;
    let mut i = 0;

    // Parse flags.
    while i < args.len() && args[i].starts_with('-') {
        let flags = &args[i][1..];
        for c in flags.chars() {
            match c {
                'x' => export = true,
                'g' | 'i' | 'r' | 'a' | 'A' | 'l' | 'u' => {
                    // Ignore flags we don't implement yet.
                }
                _ => {}
            }
        }
        i += 1;
    }

    // Process variable assignments.
    for arg in &args[i..] {
        if let Some(eq_pos) = arg.find('=') {
            let name = &arg[..eq_pos];
            let value = &arg[eq_pos + 1..];
            env.set_var(name, value);
            if export {
                env.export_var(name);
            }
        } else {
            // Declare without value — just ensure it exists.
            if env.get_var(arg).is_none() {
                env.set_var(arg, "");
            }
            if export {
                env.export_var(arg);
            }
        }
    }

    0
}
