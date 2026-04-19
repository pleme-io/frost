//! The `eval` builtin — evaluate arguments as shell code.
//!
//! Note: actual eval execution happens in the executor, which detects
//! the `eval` builtin specially and re-parses/executes the joined args.
//! This stub just joins args and signals the executor.

use crate::{Builtin, ShellEnvironment};

pub struct Eval;

impl Builtin for Eval {
    fn name(&self) -> &str {
        "eval"
    }

    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        // The actual eval is handled by the executor which joins args
        // and re-parses them. This builtin should not be reached directly.
        0
    }
}
