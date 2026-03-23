//! eval builtin — execute arguments as a command in the current shell.
//!
//! The actual parsing and execution happens in the executor. This builtin
//! concatenates args and signals that eval should happen.

use crate::{Builtin, BuiltinAction, BuiltinResult, ShellEnvironment};

/// Special exit code signaling "eval this string". The executor checks for this.
pub const EVAL_SIGNAL: i32 = 211;

pub struct Eval;

impl Builtin for Eval {
    fn name(&self) -> &str { "eval" }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            return 0;
        }
        let code = args.join(" ");
        env.set_var("__FROST_EVAL_CODE", &code);
        EVAL_SIGNAL
    }

    fn execute_with_action(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> BuiltinResult {
        if args.is_empty() {
            return BuiltinResult::ok();
        }
        let code = args.join(" ");
        BuiltinResult::with_action(0, BuiltinAction::Eval(code))
    }
}
