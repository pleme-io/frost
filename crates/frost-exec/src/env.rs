//! Shell environment — variables, functions, aliases, and process state.

use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;

use frost_builtins::ShellEnvironment;
use frost_parser::ast::FunctionDef;

// ── Shell variable ───────────────────────────────────────────────────

/// A shell variable with metadata.
#[derive(Debug, Clone)]
pub struct ShellVar {
    /// The variable's value.
    pub value: String,
    /// Whether the variable is exported to child processes.
    pub export: bool,
    /// Whether the variable is read-only.
    pub readonly: bool,
}

impl ShellVar {
    /// Create a new mutable, unexported variable.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            export: false,
            readonly: false,
        }
    }

    /// Create a new exported variable.
    pub fn exported(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            export: true,
            readonly: false,
        }
    }
}

// ── Shell environment ────────────────────────────────────────────────

/// The full shell environment state.
#[derive(Debug, Clone)]
pub struct ShellEnv {
    /// Named variables.
    pub variables: HashMap<String, ShellVar>,
    /// Shell functions (stored as AST nodes).
    pub functions: HashMap<String, FunctionDef>,
    /// Aliases: name -> replacement text.
    pub aliases: HashMap<String, String>,
    /// Exit status of the last command (`$?`).
    pub exit_status: i32,
    /// PID of the shell process (`$$`).
    pub pid: u32,
    /// Parent PID (`$PPID`).
    pub ppid: u32,
    /// Positional parameters (`$1`, `$2`, ...).
    pub positional_params: Vec<String>,
}

impl ShellEnv {
    /// Create a new environment, inheriting the OS environment variables
    /// and capturing the current PID/PPID.
    pub fn new() -> Self {
        let mut variables = HashMap::new();

        // Import process environment.
        for (key, value) in std::env::vars() {
            variables.insert(
                key,
                ShellVar {
                    value,
                    export: true,
                    readonly: false,
                },
            );
        }

        let pid = std::process::id();
        let ppid = nix::unistd::getppid().as_raw() as u32;

        Self {
            variables,
            functions: HashMap::new(),
            aliases: HashMap::new(),
            exit_status: 0,
            pid,
            ppid,
            positional_params: Vec::new(),
        }
    }

    /// Get a variable's value, if it exists.
    pub fn get_var(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(|v| v.value.as_str())
    }

    /// Set a variable. Fails silently if the variable is read-only.
    pub fn set_var(&mut self, name: &str, value: &str) {
        if let Some(existing) = self.variables.get(name) {
            if existing.readonly {
                eprintln!("frost: {name}: readonly variable");
                return;
            }
        }
        self.variables
            .entry(name.to_owned())
            .and_modify(|v| v.value = value.to_owned())
            .or_insert_with(|| ShellVar::new(value));
    }

    /// Mark a variable as exported.
    pub fn export_var(&mut self, name: &str) {
        if let Some(var) = self.variables.get_mut(name) {
            var.export = true;
        } else {
            // Export an empty variable if it doesn't exist yet.
            self.variables
                .insert(name.to_owned(), ShellVar::exported(""));
        }
    }

    /// Remove a variable. Fails silently if it is read-only.
    pub fn unset_var(&mut self, name: &str) {
        if let Some(var) = self.variables.get(name) {
            if var.readonly {
                eprintln!("frost: {name}: readonly variable");
                return;
            }
        }
        self.variables.remove(name);
    }

    /// Build the `envp` array for `execve(2)`.
    ///
    /// Returns a list of `CString`s in `KEY=VALUE` format for every
    /// exported variable.
    pub fn to_env_vec(&self) -> Vec<CString> {
        self.variables
            .iter()
            .filter(|(_, v)| v.export)
            .filter_map(|(k, v)| {
                let entry = format!("{k}={}", v.value);
                CString::new(entry).ok()
            })
            .collect()
    }

    /// Build an `argv` from an `OsStr` slice (convenience for fork/exec).
    pub fn to_argv(words: &[impl AsRef<OsStr>]) -> Vec<CString> {
        words
            .iter()
            .filter_map(|w| CString::new(w.as_ref().as_bytes()).ok())
            .collect()
    }
}

impl Default for ShellEnv {
    fn default() -> Self {
        Self::new()
    }
}

// ── ShellEnvironment trait impl ──────────────────────────────────────

/// Implements the `frost_builtins::ShellEnvironment` trait so that
/// built-in commands can interact with the shell state.
impl ShellEnvironment for ShellEnv {
    fn get_var(&self, name: &str) -> Option<&str> {
        self.get_var(name)
    }

    fn set_var(&mut self, name: &str, value: &str) {
        self.set_var(name, value);
    }

    fn export_var(&mut self, name: &str) {
        self.export_var(name);
    }

    fn unset_var(&mut self, name: &str) {
        self.unset_var(name);
    }

    fn exit_status(&self) -> i32 {
        self.exit_status
    }

    fn set_exit_status(&mut self, status: i32) {
        self.exit_status = status;
    }

    fn chdir(&mut self, path: &str) -> Result<(), String> {
        std::env::set_current_dir(path).map_err(|e| e.to_string())?;
        self.set_var("PWD", path);
        Ok(())
    }

    fn home_dir(&self) -> Option<&str> {
        self.get_var("HOME")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn set_and_get_var() {
        let mut env = ShellEnv::new();
        env.set_var("FOO", "bar");
        assert_eq!(env.get_var("FOO"), Some("bar"));
    }

    #[test]
    fn export_var_appears_in_env_vec() {
        let mut env = ShellEnv::new();
        // Clear inherited env for a clean test.
        env.variables.clear();
        env.set_var("MY_VAR", "hello");
        env.export_var("MY_VAR");
        let vec = env.to_env_vec();
        let entry = CString::new("MY_VAR=hello").unwrap();
        assert!(vec.contains(&entry));
    }

    #[test]
    fn unexported_var_not_in_env_vec() {
        let mut env = ShellEnv::new();
        env.variables.clear();
        env.set_var("HIDDEN", "secret");
        let vec = env.to_env_vec();
        assert!(vec.is_empty());
    }

    #[test]
    fn readonly_var_cannot_be_set() {
        let mut env = ShellEnv::new();
        env.variables.insert(
            "RO".into(),
            ShellVar {
                value: "locked".into(),
                export: false,
                readonly: true,
            },
        );
        env.set_var("RO", "changed");
        assert_eq!(env.get_var("RO"), Some("locked"));
    }

    #[test]
    fn readonly_var_cannot_be_unset() {
        let mut env = ShellEnv::new();
        env.variables.insert(
            "RO".into(),
            ShellVar {
                value: "locked".into(),
                export: false,
                readonly: true,
            },
        );
        env.unset_var("RO");
        assert_eq!(env.get_var("RO"), Some("locked"));
    }

    #[test]
    fn unset_var_removes_it() {
        let mut env = ShellEnv::new();
        env.set_var("GONE", "bye");
        env.unset_var("GONE");
        assert_eq!(env.get_var("GONE"), None);
    }

    #[test]
    fn positional_params() {
        let mut env = ShellEnv::new();
        env.positional_params = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(env.positional_params.len(), 3);
        assert_eq!(env.positional_params[0], "a");
    }
}
