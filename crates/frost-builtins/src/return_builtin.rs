//! The `return` builtin — exit from a function.

use crate::{Builtin, ShellEnvironment};

pub struct Return;

impl Builtin for Return {
    fn name(&self) -> &str {
        "return"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let code = match args.first() {
            Some(s) => s.parse::<i32>().unwrap_or_else(|_| {
                eprintln!("return: {s}: numeric argument required");
                2
            }) & 0xFF,
            None => env.exit_status(),
        };
        env.set_exit_status(code);
        code
    }
}
