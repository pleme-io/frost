//! set/unset builtins.

use crate::{Builtin, BuiltinAction, BuiltinResult, ShellEnvironment};

pub struct Set;
pub struct Unset;

impl Builtin for Set {
    fn name(&self) -> &str {
        "set"
    }

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

    fn execute_with_action(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> BuiltinResult {
        if args.is_empty() {
            return BuiltinResult::ok();
        }
        if args[0] == "--" {
            let params: Vec<String> = args[1..].iter().map(|s| s.to_string()).collect();
            return BuiltinResult::with_action(0, BuiltinAction::SetPositional(params));
        }
        BuiltinResult::ok()
    }
}

impl Builtin for Unset {
    fn name(&self) -> &str {
        "unset"
    }

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
