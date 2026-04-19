//! The `:` (colon/noop) builtin — always succeeds.

use crate::{Builtin, ShellEnvironment};

pub struct Colon;

impl Builtin for Colon {
    fn name(&self) -> &str {
        ":"
    }

    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}
