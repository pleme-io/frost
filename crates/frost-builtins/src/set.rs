//! set/unset builtins.

use crate::{Builtin, ShellEnvironment};

pub struct Set;
pub struct Unset;

impl Builtin for Set {
    fn name(&self) -> &str { "set" }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            // Print all variables (simplified)
            return 0;
        }
        if args[0] == "--" {
            // set -- args: set positional parameters
            // Store as __FROST_POSITIONAL for executor to pick up
            let params = args[1..].join("\x1f");
            env.set_var("__FROST_SET_POSITIONAL", &params);
            return 0;
        }
        // set -o / set +o options (simplified)
        0
    }
}

impl Builtin for Unset {
    fn name(&self) -> &str { "unset" }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        for arg in args {
            if *arg == "-f" || *arg == "-v" {
                continue; // flags, skip
            }
            env.unset_var(arg);
        }
        0
    }
}
