//! The `unset` builtin — remove shell variables or functions.

use crate::{Builtin, ShellEnvironment};

pub struct Unset;

impl Builtin for Unset {
    fn name(&self) -> &str {
        "unset"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let mut unset_func = false;
        let mut names = args;

        if let Some(&first) = args.first() {
            if first == "-v" {
                names = &args[1..];
            } else if first == "-f" {
                unset_func = true;
                names = &args[1..];
            }
        }

        for name in names {
            if unset_func {
                // Function unsetting not supported via ShellEnvironment trait yet.
                // This would need extending the trait.
            } else {
                env.unset_var(name);
            }
        }
        0
    }
}
