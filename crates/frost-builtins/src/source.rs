//! source/. builtin — execute a file in the current shell context.
//!
//! Note: actual file reading and execution happens in the executor.
//! This builtin just validates arguments and signals that source should happen.

use crate::{Builtin, ShellEnvironment};

/// Special exit code signaling "source this file". The executor checks for this.
pub const SOURCE_SIGNAL: i32 = 210;

pub struct Source;
pub struct Dot;

impl Builtin for Source {
    fn name(&self) -> &str { "source" }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            eprintln!("frost: source: filename argument required");
            return 1;
        }
        // Store the filename for the executor to pick up
        env.set_var("__FROST_SOURCE_FILE", args[0]);
        SOURCE_SIGNAL
    }
}

impl Builtin for Dot {
    fn name(&self) -> &str { "." }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        Source.execute(args, env)
    }
}
