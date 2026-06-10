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
        // `--` ends option processing — `cd -- <path>` forces the next
        // word to be treated as a directory (so a path that begins with
        // `-` isn't mistaken for `cd -`). The zoxide cd-integration
        // override relies on this: it finalizes the jump with
        // `builtin cd -- "$target"`. Without stripping `--`, frost tried
        // to chdir into a literal `--` and the jump silently failed.
        let args = match args.first() {
            Some(&"--") => &args[1..],
            _ => args,
        };
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

        // `chdir` is the single surface that owns the full cd contract:
        // it resolves the path to its absolute LOGICAL form (no symlink
        // chasing — zsh default), performs the OS chdir, saves the prior
        // `$PWD` as `OLDPWD`, then updates `$PWD`. We do NOT touch `PWD` /
        // `OLDPWD` here — that would clobber the logical value or duplicate
        // the save. AUTO_CD routes through the same `chdir`, so both paths
        // keep cwd / `$PWD` / `OLDPWD` in lock-step.
        match env.chdir(&target) {
            Ok(()) => 0,
            Err(e) => {
                // Smart-cd: when a *bare* token isn't a real directory, fall
                // back to the wadachi (轍) frecency store — `cd wad` jumps to
                // the most-worn path matching "wad". Only plain tokens get this
                // treatment; absolute / `./` / `..` / `~` / `-` forms must fail
                // honestly. This is the in-process replacement for zoxide's
                // `cd`-override shell function.
                if let Some(needle) = frecency_needle(args) {
                    if let Some(hit) = env.resolve_frecency(needle) {
                        if env.chdir(&hit).is_ok() {
                            return 0;
                        }
                    }
                }
                eprintln!("cd: {target}: {e}");
                1
            }
        }
    }
}

/// The bare-token frecency needle, if `args` is eligible for smart-cd. Returns
/// `None` for `-`, `~…`, absolute, `./…`, and `..…` forms, which must resolve
/// literally or fail honestly.
fn frecency_needle<'a>(args: &'a [&'a str]) -> Option<&'a str> {
    let arg = *args.first()?;
    if arg == "-"
        || arg.starts_with('~')
        || arg.starts_with('/')
        || arg.starts_with("./")
        || arg.starts_with("..")
    {
        return None;
    }
    Some(arg)
}

/// The `pwd` builtin — print the working directory. `-L` (default)
/// prints the logical `$PWD` (no symlink chasing, matching zsh's default
/// and the `cd` builtin's logical-path contract); `-P` prints the
/// physical, symlink-resolved path. Everyday command that was missing
/// from the registry.
pub struct Pwd;

impl Builtin for Pwd {
    fn name(&self) -> &str {
        "pwd"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let physical = args.iter().any(|a| *a == "-P");
        let dir = if physical {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            // Logical: prefer $PWD (kept in lock-step by chdir), fall back
            // to the OS cwd if $PWD is somehow unset.
            env.get_var("PWD").map(str::to_owned).or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
            })
        };
        match dir {
            Some(d) => {
                println!("{d}");
                0
            }
            None => {
                eprintln!("pwd: cannot determine current directory");
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

    #[test]
    fn cd_double_dash_strips_separator() {
        // `cd -- <path>` — the `--` ends option processing; the next word
        // is the directory. The zoxide cd-integration override finalizes
        // jumps with `builtin cd -- "$target"`, so without this `cd` tried
        // to chdir into a literal `--` and the jump silently failed.
        let mut env = MockEnv::new();
        let cd = Cd;
        let status = cd.execute(&["--", "/var/log"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.cwd, "/var/log");
    }

    #[test]
    fn cd_double_dash_alone_goes_home() {
        let mut env = MockEnv::new();
        let cd = Cd;
        let status = cd.execute(&["--"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.cwd, "/home/user");
    }

    #[test]
    fn pwd_logical_and_physical_succeed() {
        // pwd (-L default, reads $PWD) and pwd -P (physical OS cwd) both
        // resolve a directory and exit 0. MockEnv seeds PWD=/tmp.
        let mut env = MockEnv::new();
        let pwd = Pwd;
        assert_eq!(pwd.execute(&[], &mut env), 0);
        assert_eq!(pwd.execute(&["-L"], &mut env), 0);
        assert_eq!(pwd.execute(&["-P"], &mut env), 0);
    }
}
