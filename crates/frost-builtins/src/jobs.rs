//! Job control and directory stack builtins.
//!
//! Provides `jobs`, `fg`, `bg`, `wait`, `disown` (job control stubs) and
//! `pushd`, `popd`, `dirs` (directory stack).

use crate::{Builtin, ShellEnvironment};

// ── Job control stubs ──────────────────────────────────────────────

/// `jobs` — list current jobs.
pub struct Jobs;

impl Builtin for Jobs {
    fn name(&self) -> &str {
        "jobs"
    }

    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        // The real job table lives in the executor (frost-exec) and is not
        // accessible from builtins.  For now we just report "no jobs".
        println!("no jobs");
        0
    }
}

/// `fg` — foreground a job (stub).
pub struct Fg;

impl Builtin for Fg {
    fn name(&self) -> &str {
        "fg"
    }

    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        eprintln!("fg: job control not yet supported");
        1
    }
}

/// `bg` — background a job (stub).
pub struct Bg;

impl Builtin for Bg {
    fn name(&self) -> &str {
        "bg"
    }

    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        eprintln!("bg: job control not yet supported");
        1
    }
}

/// `wait` — wait for background jobs (stub).
pub struct Wait;

impl Builtin for Wait {
    fn name(&self) -> &str {
        "wait"
    }

    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        // Nothing to wait for yet.
        0
    }
}

/// `disown` — remove jobs from job table (stub).
pub struct Disown;

impl Builtin for Disown {
    fn name(&self) -> &str {
        "disown"
    }

    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        // Nothing to disown yet.
        0
    }
}

// ── Directory stack ────────────────────────────────────────────────

/// Internal variable used to store the directory stack.
const DIRSTACK_VAR: &str = "__FROST_DIRSTACK";

/// `pushd` — push directory onto the stack and cd to the argument.
pub struct Pushd;

impl Builtin for Pushd {
    fn name(&self) -> &str {
        "pushd"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let target = match args.first() {
            Some(dir) => (*dir).to_owned(),
            None => {
                eprintln!("pushd: no directory specified");
                return 1;
            }
        };

        // Save current directory onto the stack.
        let cwd = env.get_var("PWD").unwrap_or("/").to_owned();
        let stack = match env.get_var(DIRSTACK_VAR) {
            Some(existing) if !existing.is_empty() => format!("{cwd}\n{existing}"),
            _ => cwd.clone(),
        };
        env.set_var(DIRSTACK_VAR, &stack);

        // Change to the new directory.
        match env.chdir(&target) {
            Ok(()) => {
                env.set_var("PWD", &target);
                0
            }
            Err(e) => {
                eprintln!("pushd: {target}: {e}");
                // Undo the stack push on failure.
                let restored = stack.splitn(2, '\n').nth(1).unwrap_or("");
                if restored.is_empty() {
                    env.unset_var(DIRSTACK_VAR);
                } else {
                    env.set_var(DIRSTACK_VAR, restored);
                }
                1
            }
        }
    }
}

/// `popd` — pop the top directory from the stack and cd to it.
pub struct Popd;

impl Builtin for Popd {
    fn name(&self) -> &str {
        "popd"
    }

    fn execute(&self, _args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let stack = match env.get_var(DIRSTACK_VAR) {
            Some(s) if !s.is_empty() => s.to_owned(),
            _ => {
                eprintln!("popd: directory stack empty");
                return 1;
            }
        };

        // The first line is the top of the stack.
        let mut lines = stack.splitn(2, '\n');
        let top = lines.next().unwrap(); // always present since stack is non-empty
        let rest = lines.next().unwrap_or("");

        // Update the stack variable.
        if rest.is_empty() {
            env.unset_var(DIRSTACK_VAR);
        } else {
            env.set_var(DIRSTACK_VAR, rest);
        }

        // Change to the popped directory.
        let target = top.to_owned();
        match env.chdir(&target) {
            Ok(()) => {
                env.set_var("PWD", &target);
                0
            }
            Err(e) => {
                eprintln!("popd: {target}: {e}");
                1
            }
        }
    }
}

/// `dirs` — print the directory stack.
pub struct Dirs;

impl Builtin for Dirs {
    fn name(&self) -> &str {
        "dirs"
    }

    fn execute(&self, _args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let cwd = env.get_var("PWD").unwrap_or("/");
        print!("{cwd}");
        if let Some(stack) = env.get_var(DIRSTACK_VAR) {
            if !stack.is_empty() {
                for dir in stack.split('\n') {
                    print!(" {dir}");
                }
            }
        }
        println!();
        0
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShellEnvironment;
    use std::collections::HashMap;

    struct MockEnv {
        vars: HashMap<String, String>,
        status: i32,
        cwd: String,
        /// If set, chdir() will fail for this path.
        fail_path: Option<String>,
    }

    impl MockEnv {
        fn new() -> Self {
            Self {
                vars: HashMap::from([
                    ("HOME".into(), "/home/user".into()),
                    ("PWD".into(), "/tmp".into()),
                ]),
                status: 0,
                cwd: "/tmp".into(),
                fail_path: None,
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
        fn chdir(&mut self, path: &str) -> Result<(), String> {
            if self.fail_path.as_deref() == Some(path) {
                return Err("No such file or directory".into());
            }
            self.cwd = path.into();
            Ok(())
        }
        fn home_dir(&self) -> Option<&str> {
            self.get_var("HOME")
        }
    }

    // ── jobs / fg / bg / wait / disown ────────────────────────────

    #[test]
    fn jobs_returns_zero() {
        let mut env = MockEnv::new();
        assert_eq!(Jobs.execute(&[], &mut env), 0);
    }

    #[test]
    fn fg_returns_one() {
        let mut env = MockEnv::new();
        assert_eq!(Fg.execute(&[], &mut env), 1);
    }

    #[test]
    fn bg_returns_one() {
        let mut env = MockEnv::new();
        assert_eq!(Bg.execute(&[], &mut env), 1);
    }

    #[test]
    fn wait_returns_zero() {
        let mut env = MockEnv::new();
        assert_eq!(Wait.execute(&[], &mut env), 0);
    }

    #[test]
    fn disown_returns_zero() {
        let mut env = MockEnv::new();
        assert_eq!(Disown.execute(&[], &mut env), 0);
    }

    // ── pushd / popd / dirs ───────────────────────────────────────

    #[test]
    fn pushd_no_arg_fails() {
        let mut env = MockEnv::new();
        assert_eq!(Pushd.execute(&[], &mut env), 1);
    }

    #[test]
    fn pushd_changes_directory() {
        let mut env = MockEnv::new();
        assert_eq!(Pushd.execute(&["/var/log"], &mut env), 0);
        assert_eq!(env.cwd, "/var/log");
        assert_eq!(env.get_var("PWD"), Some("/var/log"));
    }

    #[test]
    fn pushd_saves_old_dir_on_stack() {
        let mut env = MockEnv::new();
        // cwd starts at /tmp
        Pushd.execute(&["/var/log"], &mut env);
        let stack = env.get_var(DIRSTACK_VAR).unwrap();
        assert!(
            stack.starts_with("/tmp"),
            "stack should contain old dir: {stack}"
        );
    }

    #[test]
    fn pushd_popd_round_trip() {
        let mut env = MockEnv::new();
        // /tmp -> /var/log -> /home
        Pushd.execute(&["/var/log"], &mut env);
        Pushd.execute(&["/home"], &mut env);
        assert_eq!(env.cwd, "/home");

        // popd -> /var/log
        assert_eq!(Popd.execute(&[], &mut env), 0);
        assert_eq!(env.cwd, "/var/log");

        // popd -> /tmp
        assert_eq!(Popd.execute(&[], &mut env), 0);
        assert_eq!(env.cwd, "/tmp");
    }

    #[test]
    fn popd_empty_stack_fails() {
        let mut env = MockEnv::new();
        assert_eq!(Popd.execute(&[], &mut env), 1);
    }

    #[test]
    fn dirs_returns_zero() {
        let mut env = MockEnv::new();
        assert_eq!(Dirs.execute(&[], &mut env), 0);
    }

    #[test]
    fn pushd_failure_restores_stack() {
        let mut env = MockEnv::new();
        env.fail_path = Some("/bad/dir".into());
        assert_eq!(Pushd.execute(&["/bad/dir"], &mut env), 1);
        // Stack should not have been modified.
        assert!(env.get_var(DIRSTACK_VAR).is_none());
    }
}
