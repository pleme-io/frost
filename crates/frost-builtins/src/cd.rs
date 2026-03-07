//! The `cd` builtin — change working directory.
//!
//! Handles `~`, `-` (OLDPWD), and CDPATH lookup.

use crate::{Builtin, ShellEnvironment};

pub struct Cd;

impl Builtin for Cd {
    fn name(&self) -> &str {
        "cd"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let target = match args.first() {
            // `cd -` → go to OLDPWD
            Some(&"-") => match env.get_var("OLDPWD") {
                Some(old) => old.to_owned(),
                None => {
                    eprintln!("cd: OLDPWD not set");
                    return 1;
                }
            },
            // `cd ~` or bare `~` prefix
            Some(arg) if arg.starts_with('~') => {
                let rest = &arg[1..];
                match env.home_dir() {
                    Some(home) => format!("{home}{rest}"),
                    None => {
                        eprintln!("cd: HOME not set");
                        return 1;
                    }
                }
            }
            // `cd <path>`
            Some(arg) => {
                // Try CDPATH if the path is relative and doesn't start with ./
                if !arg.starts_with('/') && !arg.starts_with("./") && !arg.starts_with("..") {
                    if let Some(resolved) = try_cdpath(arg, env) {
                        resolved
                    } else {
                        (*arg).to_owned()
                    }
                } else {
                    (*arg).to_owned()
                }
            }
            // `cd` with no args → go home
            None => match env.home_dir() {
                Some(home) => home.to_owned(),
                None => {
                    eprintln!("cd: HOME not set");
                    return 1;
                }
            },
        };

        // Save current directory as OLDPWD before changing.
        if let Some(pwd) = env.get_var("PWD") {
            let pwd = pwd.to_owned();
            env.set_var("OLDPWD", &pwd);
        }

        match env.chdir(&target) {
            Ok(()) => {
                // Update PWD to the new directory.
                env.set_var("PWD", &target);
                0
            }
            Err(e) => {
                eprintln!("cd: {target}: {e}");
                1
            }
        }
    }
}

/// Search CDPATH entries for a directory matching `name`.
fn try_cdpath(name: &str, env: &dyn ShellEnvironment) -> Option<String> {
    let cdpath = env.get_var("CDPATH")?;
    for dir in cdpath.split(':') {
        let candidate = if dir.is_empty() {
            name.to_owned()
        } else {
            format!("{dir}/{name}")
        };
        // We can't stat from here (no filesystem access in the trait),
        // so we return the first candidate and let chdir() validate it.
        // A real implementation would probe the filesystem, but this
        // keeps the builtin free of OS deps.
        return Some(candidate);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShellEnvironment;
    use std::collections::HashMap;

    struct MockEnv {
        vars: HashMap<String, String>,
        status: i32,
        cwd: String,
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
            self.cwd = path.into();
            Ok(())
        }
        fn home_dir(&self) -> Option<&str> {
            self.get_var("HOME")
        }
    }

    #[test]
    fn cd_no_args_goes_home() {
        let mut env = MockEnv::new();
        let cd = Cd;
        let status = cd.execute(&[], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.cwd, "/home/user");
    }

    #[test]
    fn cd_tilde_expands_home() {
        let mut env = MockEnv::new();
        let cd = Cd;
        let status = cd.execute(&["~/projects"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.cwd, "/home/user/projects");
    }

    #[test]
    fn cd_dash_uses_oldpwd() {
        let mut env = MockEnv::new();
        env.set_var("OLDPWD", "/var/log");
        let cd = Cd;
        let status = cd.execute(&["-"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.cwd, "/var/log");
    }

    #[test]
    fn cd_dash_without_oldpwd_fails() {
        let mut env = MockEnv::new();
        let cd = Cd;
        let status = cd.execute(&["-"], &mut env);
        assert_eq!(status, 1);
    }
}
