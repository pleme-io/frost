//! Control flow builtins: return, break, continue.
//!
//! These builtins use exit codes with special meaning:
//!   200 = return
//!   201 = break
//!   202 = continue
//!   203+ = break/continue with level
//! The executor checks for these codes and converts them to control flow signals.

use crate::{Builtin, ShellEnvironment};

/// Special exit codes used for control flow signaling.
pub const RETURN_SIGNAL: i32 = 200;
pub const BREAK_SIGNAL: i32 = 201;
pub const CONTINUE_SIGNAL: i32 = 202;

pub struct Return;
pub struct Break;
pub struct Continue;

impl Builtin for Return {
    fn name(&self) -> &str { "return" }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let code = args.first()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or_else(|| env.exit_status());
        // Store the return value and signal return via special exit code
        env.set_exit_status(code);
        RETURN_SIGNAL
    }
}

impl Builtin for Break {
    fn name(&self) -> &str { "break" }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        let levels = args.first()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(1)
            .max(1);
        BREAK_SIGNAL + levels - 1
    }
}

impl Builtin for Continue {
    fn name(&self) -> &str { "continue" }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        let levels = args.first()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(1)
            .max(1);
        CONTINUE_SIGNAL + levels - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEnv { exit_status: i32 }
    impl ShellEnvironment for MockEnv {
        fn get_var(&self, _: &str) -> Option<&str> { None }
        fn set_var(&mut self, _: &str, _: &str) {}
        fn export_var(&mut self, _: &str) {}
        fn unset_var(&mut self, _: &str) {}
        fn exit_status(&self) -> i32 { self.exit_status }
        fn set_exit_status(&mut self, s: i32) { self.exit_status = s; }
        fn chdir(&mut self, _: &str) -> Result<(), String> { Ok(()) }
        fn home_dir(&self) -> Option<&str> { None }
    }

    #[test]
    fn return_default_uses_last_status() {
        let mut env = MockEnv { exit_status: 42 };
        let code = Return.execute(&[], &mut env);
        assert_eq!(code, RETURN_SIGNAL);
        assert_eq!(env.exit_status, 42);
    }

    #[test]
    fn return_with_code() {
        let mut env = MockEnv { exit_status: 0 };
        let code = Return.execute(&["5"], &mut env);
        assert_eq!(code, RETURN_SIGNAL);
        assert_eq!(env.exit_status, 5);
    }

    #[test]
    fn break_default_one_level() {
        let mut env = MockEnv { exit_status: 0 };
        assert_eq!(Break.execute(&[], &mut env), BREAK_SIGNAL);
    }

    #[test]
    fn break_multiple_levels() {
        let mut env = MockEnv { exit_status: 0 };
        assert_eq!(Break.execute(&["3"], &mut env), BREAK_SIGNAL + 2);
    }

    #[test]
    fn continue_default() {
        let mut env = MockEnv { exit_status: 0 };
        assert_eq!(Continue.execute(&[], &mut env), CONTINUE_SIGNAL);
    }
}
