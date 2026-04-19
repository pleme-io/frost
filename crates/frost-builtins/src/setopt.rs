//! The `setopt` and `unsetopt` builtins — shell option management.
//!
//! These accept zsh option names (case-insensitive, underscores ignored)
//! and silently succeed. The actual option state is managed by the executor
//! through the frost-options crate.

use crate::{Builtin, ShellEnvironment};

pub struct Setopt;

impl Builtin for Setopt {
    fn name(&self) -> &str {
        "setopt"
    }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        // Parse flags and option names. For now, accept everything silently.
        // Zsh setopt accepts: setopt [+-options] [option_name ...]
        for arg in args {
            if arg.starts_with('-') || arg.starts_with('+') {
                // Flags like -o, +o, etc. — accept silently.
                continue;
            }
            // Option name — accept silently. The executor can query
            // frost-options for actual state if needed.
        }
        0
    }
}

pub struct Unsetopt;

impl Builtin for Unsetopt {
    fn name(&self) -> &str {
        "unsetopt"
    }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        // Accept all option names silently.
        for arg in args {
            if arg.starts_with('-') || arg.starts_with('+') {
                continue;
            }
        }
        0
    }
}
