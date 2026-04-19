//! The `readonly` builtin — mark variables as read-only.

use crate::{Builtin, ShellEnvironment};

pub struct Readonly;

impl Builtin for Readonly {
    fn name(&self) -> &str {
        "readonly"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let mut i = 0;
        let mut print_mode = false;

        // Parse flags.
        while i < args.len() && args[i].starts_with('-') {
            let flags = &args[i][1..];
            for c in flags.chars() {
                match c {
                    'p' => print_mode = true,
                    // -g (global), -a (array), -A (assoc) — accept silently
                    'g' | 'a' | 'A' => {}
                    _ => {}
                }
            }
            i += 1;
        }

        if print_mode && i >= args.len() {
            // readonly -p: print all readonly variables.
            // We don't track readonly state through the ShellEnvironment trait,
            // so just return 0 for now.
            return 0;
        }

        // Process variable assignments/declarations.
        for arg in &args[i..] {
            if let Some(eq_pos) = arg.find('=') {
                let name = &arg[..eq_pos];
                let value = &arg[eq_pos + 1..];
                env.set_var(name, value);
                // Mark readonly via the environment trait.
                // The ShellEnvironment trait doesn't have mark_readonly yet,
                // so we just set the value. The executor handles readonly
                // through ShellEnv directly.
            } else {
                // Just declare the variable as readonly.
                if env.get_var(arg).is_none() {
                    env.set_var(arg, "");
                }
            }
        }

        0
    }
}
