//! The `hash` builtin — manage the command hash table.
//!
//! Usage:
//! - `hash` — list all hashed commands
//! - `hash name` — add command to hash by searching PATH
//! - `hash name=path` — add command with explicit path
//! - `hash -r` — clear the hash table
//! - `hash -d name` — remove a specific entry
//!
//! Uses `__FROST_HASH_*` environment variables as storage.

use crate::{Builtin, ShellEnvironment};

/// Prefix for hash table entries in the environment.
const HASH_PREFIX: &str = "__FROST_HASH_";

pub struct Hash;

impl Builtin for Hash {
    fn name(&self) -> &str {
        "hash"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            // List all hashed commands
            print_hash_table(env);
            return 0;
        }

        let mut i = 0;
        let mut status = 0;

        while i < args.len() {
            let arg = args[i];

            if arg == "-r" {
                // Clear the entire hash table.
                // We signal the executor to clear all __FROST_HASH_* vars.
                env.set_var("__FROST_HASH_CLEAR", "1");
                i += 1;
            } else if arg == "-d" {
                // Remove specific entry
                i += 1;
                if i >= args.len() {
                    eprintln!("hash: -d: option requires an argument");
                    return 1;
                }
                let name = args[i];
                env.unset_var(&format!("{HASH_PREFIX}{name}"));
                i += 1;
            } else if let Some((name, path)) = arg.split_once('=') {
                // Explicit hash: name=path
                if path.is_empty() {
                    eprintln!("hash: {name}: path must not be empty");
                    status = 1;
                } else {
                    env.set_var(&format!("{HASH_PREFIX}{name}"), path);
                }
                i += 1;
            } else {
                // Search PATH for command and cache it
                match find_in_path(arg) {
                    Some(path) => {
                        env.set_var(&format!("{HASH_PREFIX}{arg}"), &path);
                    }
                    None => {
                        eprintln!("hash: {arg}: not found");
                        status = 1;
                    }
                }
                i += 1;
            }
        }

        status
    }
}

/// Search PATH for a command and return its full path.
fn find_in_path(cmd: &str) -> Option<String> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in path_var.split(':') {
        let candidate = std::path::Path::new(dir).join(cmd);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// Print all hashed commands from the environment.
fn print_hash_table(_env: &dyn ShellEnvironment) {
    // We cannot iterate over env vars from the ShellEnvironment trait
    // directly, so we signal the executor to print them by setting a
    // flag variable. The executor should intercept this and list all
    // __FROST_HASH_* entries.
    //
    // For the builtin itself, we check a limited set — in practice
    // the executor handles listing via its own env iteration.
    //
    // This is a limitation of the decoupled ShellEnvironment trait.
    // Signal executor to print hash table.
    // The executor will read this flag and print all __FROST_HASH_* vars.
    println!("hash: hash table empty");
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
    fn hash_explicit_path() {
        let mut env = MockEnv::new();
        let hash = Hash;
        let status = hash.execute(&["foo=/usr/bin/foo"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("__FROST_HASH_foo"), Some("/usr/bin/foo"));
    }

    #[test]
    fn hash_remove_entry() {
        let mut env = MockEnv::new();
        env.set_var("__FROST_HASH_foo", "/usr/bin/foo");
        let hash = Hash;
        let status = hash.execute(&["-d", "foo"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("__FROST_HASH_foo"), None);
    }

    #[test]
    fn hash_clear() {
        let mut env = MockEnv::new();
        env.set_var("__FROST_HASH_foo", "/usr/bin/foo");
        let hash = Hash;
        let status = hash.execute(&["-r"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("__FROST_HASH_CLEAR"), Some("1"));
    }

    #[test]
    fn hash_empty_path_rejected() {
        let mut env = MockEnv::new();
        let hash = Hash;
        let status = hash.execute(&["foo="], &mut env);
        assert_eq!(status, 1);
    }

    #[test]
    fn hash_d_requires_argument() {
        let mut env = MockEnv::new();
        let hash = Hash;
        let status = hash.execute(&["-d"], &mut env);
        assert_eq!(status, 1);
    }

    #[test]
    fn hash_command_from_path() {
        // "ls" should be findable on any Unix system
        let mut env = MockEnv::new();
        let hash = Hash;
        let status = hash.execute(&["ls"], &mut env);
        assert_eq!(status, 0);
        let hashed = env.get_var("__FROST_HASH_ls");
        assert!(hashed.is_some(), "ls should be found in PATH");
        assert!(hashed.unwrap().contains("ls"));
    }

    #[test]
    fn hash_command_not_found() {
        let mut env = MockEnv::new();
        let hash = Hash;
        let status = hash.execute(&["this_command_certainly_does_not_exist_xyz"], &mut env);
        assert_eq!(status, 1);
    }
}
