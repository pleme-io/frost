//! The `true` and `false` builtins.
//!
//! `true` always returns 0; `false` always returns 1.

use crate::{Builtin, ShellEnvironment};

pub struct True;

impl Builtin for True {
    fn name(&self) -> &str {
        "true"
    }

    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

pub struct False;

impl Builtin for False {
    fn name(&self) -> &str {
        "false"
    }

    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShellEnvironment;
    use std::collections::HashMap;

    struct MockEnv {
        vars: HashMap<String, String>,
        status: i32,
    }

    impl MockEnv {
        fn new() -> Self {
            Self {
                vars: HashMap::new(),
                status: 0,
            }
        }
    }

    impl ShellEnvironment for MockEnv {
        fn get_var(&self, name: &str) -> Option<&str> {
            self.vars.get(name).map(|s| s.as_str())
        }
        fn set_var(&mut self, name: &str, value: &str) {
            self.vars.insert(name.into(), value.into());
        }
        fn export_var(&mut self, _name: &str) {}
        fn unset_var(&mut self, name: &str) {
            self.vars.remove(name);
        }
        fn exit_status(&self) -> i32 {
            self.status
        }
        fn set_exit_status(&mut self, status: i32) {
            self.status = status;
        }
        fn chdir(&mut self, _path: &str) -> Result<(), String> {
            Ok(())
        }
        fn home_dir(&self) -> Option<&str> {
            None
        }
    }

    #[test]
    fn true_returns_zero() {
        let mut env = MockEnv::new();
        assert_eq!(True.execute(&[], &mut env), 0);
    }

    #[test]
    fn false_returns_one() {
        let mut env = MockEnv::new();
        assert_eq!(False.execute(&[], &mut env), 1);
    }

    #[test]
    fn true_ignores_args() {
        let mut env = MockEnv::new();
        assert_eq!(True.execute(&["whatever", "--flag"], &mut env), 0);
    }
}
