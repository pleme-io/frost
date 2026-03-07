//! The `exit` builtin — terminate the shell.

use crate::{Builtin, ShellEnvironment};

pub struct Exit;

impl Builtin for Exit {
    fn name(&self) -> &str {
        "exit"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let code = match args.first() {
            Some(s) => match s.parse::<i32>() {
                Ok(n) => n & 0xFF, // Clamp to 0..255 like POSIX.
                Err(_) => {
                    eprintln!("exit: {s}: numeric argument required");
                    2
                }
            },
            // No argument: use the last command's exit status.
            None => env.exit_status(),
        };

        // Signal the executor by setting exit status. The executor
        // must check for the "exit requested" condition after running
        // the `exit` builtin.
        env.set_exit_status(code);
        code
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
        fn new(status: i32) -> Self {
            Self {
                vars: HashMap::new(),
                status,
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
    fn exit_with_code() {
        let mut env = MockEnv::new(0);
        let exit = Exit;
        let status = exit.execute(&["42"], &mut env);
        assert_eq!(status, 42);
        assert_eq!(env.exit_status(), 42);
    }

    #[test]
    fn exit_no_args_uses_last_status() {
        let mut env = MockEnv::new(7);
        let exit = Exit;
        let status = exit.execute(&[], &mut env);
        assert_eq!(status, 7);
    }

    #[test]
    fn exit_clamps_to_byte() {
        let mut env = MockEnv::new(0);
        let exit = Exit;
        let status = exit.execute(&["256"], &mut env);
        assert_eq!(status, 0); // 256 & 0xFF == 0
    }

    #[test]
    fn exit_bad_arg() {
        let mut env = MockEnv::new(0);
        let exit = Exit;
        let status = exit.execute(&["abc"], &mut env);
        assert_eq!(status, 2);
    }
}
