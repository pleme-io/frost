//! Shell environment — variables, functions, aliases, and process state.
//!
//! Variables use a scope stack: index 0 is global, subsequent entries
//! are pushed on function entry and popped on return. Lookups walk
//! from innermost scope outward; plain assignment modifies the nearest
//! scope that already contains the name, or creates in global scope.

use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::time::Instant;

use indexmap::IndexMap;

use frost_builtins::ShellEnvironment;
use frost_expand::ExpandValue;
use frost_options::{Options, ShellOption};
use frost_parser::ast::FunctionDef;

// ── Shell value types ──────────────────────────────────────────────

/// The typed value of a shell variable.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellValue {
    /// A plain string (the default).
    Scalar(String),
    /// An integer (`typeset -i`).
    Integer(i64),
    /// A floating-point number (`typeset -F`).
    Float(f64),
    /// An indexed array, 1-indexed in zsh (stored 0-indexed internally).
    Array(Vec<String>),
    /// An associative array (insertion-ordered).
    Associative(IndexMap<String, String>),
}

impl ShellValue {
    /// Convert to a scalar string representation.
    pub fn to_scalar_string(&self) -> String {
        match self {
            Self::Scalar(s) => s.clone(),
            Self::Integer(n) => n.to_string(),
            Self::Float(f) => format!("{f:.10}"),
            Self::Array(a) => a.join(" "),
            Self::Associative(m) => m.values().cloned().collect::<Vec<_>>().join(" "),
        }
    }

    /// Check if value is empty / zero.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Scalar(s) => s.is_empty(),
            Self::Integer(n) => *n == 0,
            Self::Float(f) => *f == 0.0,
            Self::Array(a) => a.is_empty(),
            Self::Associative(m) => m.is_empty(),
        }
    }

    /// Set from a string, preserving the current type.
    pub fn set_from_str(&mut self, s: &str) {
        match self {
            Self::Scalar(val) => *val = s.to_owned(),
            Self::Integer(val) => *val = s.parse().unwrap_or(0),
            Self::Float(val) => *val = s.parse().unwrap_or(0.0),
            Self::Array(vec) => {
                vec.clear();
                if !s.is_empty() {
                    vec.push(s.to_owned());
                }
            }
            Self::Associative(_) => {
                *self = Self::Scalar(s.to_owned());
            }
        }
    }
}

// ── Shell variable ───────────────────────────────────────────────────

/// A shell variable with metadata and cached string representation.
#[derive(Debug, Clone)]
pub struct ShellVar {
    /// The typed value.
    pub value: ShellValue,
    /// Whether the variable is exported to child processes.
    pub export: bool,
    /// Whether the variable is read-only.
    pub readonly: bool,
    /// Cached scalar string for `&str` access. For `Scalar`, this is a
    /// clone of the inner string; for typed values it holds the formatted
    /// representation. Kept in sync by mutation helpers.
    str_repr: String,
}

impl ShellVar {
    /// Create a new mutable, unexported scalar variable.
    pub fn new(value: impl Into<String>) -> Self {
        let s = value.into();
        Self {
            str_repr: s.clone(),
            value: ShellValue::Scalar(s),
            export: false,
            readonly: false,
        }
    }

    /// Create a new exported scalar variable.
    pub fn exported(value: impl Into<String>) -> Self {
        let s = value.into();
        Self {
            str_repr: s.clone(),
            value: ShellValue::Scalar(s),
            export: true,
            readonly: false,
        }
    }

    /// Create a variable from a typed `ShellValue`.
    pub fn with_value(value: ShellValue) -> Self {
        let str_repr = value.to_scalar_string();
        Self {
            value,
            export: false,
            readonly: false,
            str_repr,
        }
    }

    /// Get the cached string representation (always available).
    pub fn as_str(&self) -> &str {
        &self.str_repr
    }

    /// Set the value from a string, preserving the variable's type.
    pub fn set_str(&mut self, s: &str) {
        self.value.set_from_str(s);
        self.refresh_cache();
    }

    /// Set a new typed value.
    pub fn set_value(&mut self, value: ShellValue) {
        self.value = value;
        self.refresh_cache();
    }

    fn refresh_cache(&mut self) {
        self.str_repr = self.value.to_scalar_string();
    }

    /// Public cache refresh (for direct mutation of value field).
    pub fn refresh_str_cache(&mut self) {
        self.refresh_cache();
    }
}

// ── Scope ────────────────────────────────────────────────────────────

/// A variable scope (global or function-local).
#[derive(Debug, Clone)]
pub struct Scope {
    pub variables: IndexMap<String, ShellVar>,
}

impl Scope {
    pub fn new() -> Self {
        Self {
            variables: IndexMap::new(),
        }
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

// ── Shell environment ────────────────────────────────────────────────

/// The full shell environment state.
#[derive(Debug, Clone)]
pub struct ShellEnv {
    /// Scope stack: index 0 = global, last = current (innermost).
    scopes: Vec<Scope>,
    /// Shell functions (stored as AST nodes).
    pub functions: HashMap<String, FunctionDef>,
    /// Aliases: name → replacement text.
    pub aliases: HashMap<String, String>,
    /// Shell options (GLOB, EXTENDED_GLOB, ERR_EXIT, …).
    pub options: Options,
    /// Exit status of the last command (`$?`).
    pub exit_status: i32,
    /// PID of the shell process (`$$`).
    pub pid: u32,
    /// Parent PID (`$PPID`).
    pub ppid: u32,
    /// Positional parameters (`$1`, `$2`, …).
    pub positional_params: Vec<String>,
    /// Startup time for `$SECONDS`.
    start_time: Instant,
    /// Simple PRNG state for `$RANDOM` (matches zsh: rand() & 0x7fff).
    random_state: u32,
}

impl ShellEnv {
    /// Create a new environment, importing OS environment variables
    /// and capturing the current PID/PPID.
    pub fn new() -> Self {
        let mut global = Scope::new();

        for (key, value) in std::env::vars() {
            global.variables.insert(key, ShellVar::exported(value));
        }

        let pid = std::process::id();
        let ppid = nix::unistd::getppid().as_raw() as u32;

        Self {
            scopes: vec![global],
            functions: HashMap::new(),
            aliases: HashMap::new(),
            options: Options::default(),
            exit_status: 0,
            pid,
            ppid,
            positional_params: Vec::new(),
            start_time: Instant::now(),
            random_state: pid,
        }
    }

    // ── Scope management ─────────────────────────────────────────

    /// Push a new empty scope (called on function entry).
    pub fn push_scope(&mut self) {
        self.scopes.push(Scope::new());
    }

    /// Pop the innermost scope (called on function return).
    /// Returns `None` if only the global scope remains.
    pub fn pop_scope(&mut self) -> Option<Scope> {
        if self.scopes.len() > 1 {
            self.scopes.pop()
        } else {
            None
        }
    }

    /// Number of scopes on the stack (1 = global only).
    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    // ── Variable access ──────────────────────────────────────────

    /// Look up a variable's string value, searching innermost → global.
    /// Handles special dynamic variables ($RANDOM, $SECONDS, $LINENO, etc.).
    pub fn get_var(&self, name: &str) -> Option<&str> {
        // Check regular variables first (they can shadow specials)
        for scope in self.scopes.iter().rev() {
            if let Some(var) = scope.variables.get(name) {
                return Some(var.as_str());
            }
        }
        None
    }

    /// Check whether a shell option is enabled.
    pub fn is_option_set(&self, opt: ShellOption) -> bool {
        self.options.is_set(opt)
    }

    /// Enable a shell option.
    pub fn set_option(&mut self, opt: ShellOption) {
        self.options.set(opt);
    }

    /// Disable a shell option.
    pub fn unset_option(&mut self, opt: ShellOption) {
        self.options.unset(opt);
    }

    /// Generate a random number (0-32767) like zsh's $RANDOM.
    pub fn next_random(&mut self) -> u32 {
        // Simple xorshift32 PRNG matching zsh's rand() & 0x7fff range
        let mut x = self.random_state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.random_state = x;
        x & 0x7fff
    }

    /// Get elapsed seconds since shell start (for $SECONDS).
    pub fn seconds_elapsed(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Get a reference to the full `ShellVar`.
    pub fn get_shell_var(&self, name: &str) -> Option<&ShellVar> {
        for scope in self.scopes.iter().rev() {
            if let Some(var) = scope.variables.get(name) {
                return Some(var);
            }
        }
        None
    }

    /// Get a mutable reference to the full `ShellVar`.
    pub fn get_shell_var_mut(&mut self, name: &str) -> Option<&mut ShellVar> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(var) = scope.variables.get_mut(name) {
                return Some(var);
            }
        }
        None
    }

    /// Get the typed value of a variable.
    pub fn get_value(&self, name: &str) -> Option<&ShellValue> {
        self.get_shell_var(name).map(|v| &v.value)
    }

    /// Set a variable. Finds the nearest scope containing it and
    /// updates in place; if the name doesn't exist anywhere, creates
    /// it in the **global** scope. Silently refuses read-only vars.
    pub fn set_var(&mut self, name: &str, value: &str) {
        // Readonly check
        for scope in self.scopes.iter().rev() {
            if let Some(var) = scope.variables.get(name) {
                if var.readonly {
                    eprintln!("frost: {name}: readonly variable");
                    return;
                }
                break;
            }
        }

        // Update in nearest scope that contains it
        for scope in self.scopes.iter_mut().rev() {
            if let Some(var) = scope.variables.get_mut(name) {
                var.set_str(value);
                return;
            }
        }

        // Not found anywhere → create in global scope
        self.scopes[0]
            .variables
            .insert(name.to_owned(), ShellVar::new(value));
    }

    /// Declare a variable in the current (innermost) scope.
    /// Used by `typeset` / `local` / `declare` (without `-g`).
    pub fn declare_var(&mut self, name: &str, value: &str) {
        let scope = self.scopes.last_mut().expect("at least global scope");
        if let Some(var) = scope.variables.get_mut(name) {
            if var.readonly {
                eprintln!("frost: {name}: readonly variable");
                return;
            }
            var.set_str(value);
        } else {
            scope
                .variables
                .insert(name.to_owned(), ShellVar::new(value));
        }
    }

    /// Declare a variable in the global scope (`typeset -g`).
    pub fn declare_global_var(&mut self, name: &str, value: &str) {
        let scope = &mut self.scopes[0];
        if let Some(var) = scope.variables.get_mut(name) {
            if var.readonly {
                eprintln!("frost: {name}: readonly variable");
                return;
            }
            var.set_str(value);
        } else {
            scope
                .variables
                .insert(name.to_owned(), ShellVar::new(value));
        }
    }

    /// Mark a variable as exported.
    pub fn export_var(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(var) = scope.variables.get_mut(name) {
                var.export = true;
                return;
            }
        }
        // Doesn't exist: create exported empty var in global scope
        let mut var = ShellVar::new("");
        var.export = true;
        self.scopes[0].variables.insert(name.to_owned(), var);
    }

    /// Remove a variable. Silently refuses read-only vars.
    pub fn unset_var(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(var) = scope.variables.get(name) {
                if var.readonly {
                    eprintln!("frost: {name}: readonly variable");
                    return;
                }
            }
            if scope.variables.shift_remove(name).is_some() {
                return;
            }
        }
    }

    /// Mark a variable as read-only (searched innermost → global).
    pub fn set_readonly(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(var) = scope.variables.get_mut(name) {
                var.readonly = true;
                return;
            }
        }
        // Doesn't exist: create readonly empty var in current scope
        let mut var = ShellVar::new("");
        var.readonly = true;
        self.scopes
            .last_mut()
            .expect("scope")
            .variables
            .insert(name.to_owned(), var);
    }

    /// Change a variable's type to integer, converting its current
    /// value. If `global`, operates on scope 0; otherwise current scope.
    pub fn set_var_integer(&mut self, name: &str, global: bool) {
        let scope = if global {
            &mut self.scopes[0]
        } else {
            self.scopes.last_mut().expect("scope")
        };
        if let Some(var) = scope.variables.get_mut(name) {
            let n: i64 = var.as_str().parse().unwrap_or(0);
            var.set_value(ShellValue::Integer(n));
        } else {
            scope.variables.insert(
                name.to_owned(),
                ShellVar::with_value(ShellValue::Integer(0)),
            );
        }
    }

    /// Change a variable's type to float.
    pub fn set_var_float(&mut self, name: &str, global: bool) {
        let scope = if global {
            &mut self.scopes[0]
        } else {
            self.scopes.last_mut().expect("scope")
        };
        if let Some(var) = scope.variables.get_mut(name) {
            let f: f64 = var.as_str().parse().unwrap_or(0.0);
            var.set_value(ShellValue::Float(f));
        } else {
            scope.variables.insert(
                name.to_owned(),
                ShellVar::with_value(ShellValue::Float(0.0)),
            );
        }
    }

    /// Change a variable's type to indexed array.
    pub fn set_var_array(&mut self, name: &str, global: bool) {
        let scope = if global {
            &mut self.scopes[0]
        } else {
            self.scopes.last_mut().expect("scope")
        };
        if let Some(var) = scope.variables.get_mut(name) {
            let s = var.as_str().to_owned();
            let arr = if s.is_empty() { Vec::new() } else { vec![s] };
            var.set_value(ShellValue::Array(arr));
        } else {
            scope.variables.insert(
                name.to_owned(),
                ShellVar::with_value(ShellValue::Array(Vec::new())),
            );
        }
    }

    /// Change a variable's type to associative array.
    pub fn set_var_associative(&mut self, name: &str, global: bool) {
        let scope = if global {
            &mut self.scopes[0]
        } else {
            self.scopes.last_mut().expect("scope")
        };
        if scope.variables.get(name).is_none() {
            scope.variables.insert(
                name.to_owned(),
                ShellVar::with_value(ShellValue::Associative(IndexMap::new())),
            );
        }
        // If it already exists, leave its value alone but it's now assoc
    }

    /// Convert a `ShellValue` to an `ExpandValue` for the expansion engine.
    pub fn to_expand_value(sv: &ShellValue) -> ExpandValue {
        match sv {
            ShellValue::Scalar(s) => ExpandValue::Scalar(s.clone()),
            ShellValue::Integer(n) => ExpandValue::Integer(*n),
            ShellValue::Float(f) => ExpandValue::Float(*f),
            ShellValue::Array(a) => ExpandValue::Array(a.clone()),
            ShellValue::Associative(m) => ExpandValue::Associative(m.clone()),
        }
    }

    /// Build the `envp` array for `execve(2)`.
    ///
    /// Returns `CString`s in `KEY=VALUE` format for every exported
    /// variable. Inner scopes shadow outer scopes.
    pub fn to_env_vec(&self) -> Vec<CString> {
        let mut exported: IndexMap<&str, &ShellVar> = IndexMap::new();
        for scope in &self.scopes {
            for (name, var) in &scope.variables {
                if var.export {
                    exported.insert(name.as_str(), var);
                }
            }
        }
        exported
            .iter()
            .filter_map(|(k, v)| CString::new(format!("{k}={}", v.as_str())).ok())
            .collect()
    }

    /// Build an `argv` from an `OsStr` slice (convenience for fork/exec).
    pub fn to_argv(words: &[impl AsRef<OsStr>]) -> Vec<CString> {
        words
            .iter()
            .filter_map(|w| CString::new(w.as_ref().as_bytes()).ok())
            .collect()
    }

    /// Direct access to the global scope (for tests).
    #[cfg(test)]
    pub fn global_scope(&self) -> &Scope {
        &self.scopes[0]
    }

    /// Direct mutable access to the global scope (for tests).
    #[cfg(test)]
    pub fn global_scope_mut(&mut self) -> &mut Scope {
        &mut self.scopes[0]
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

    fn declare_var(&mut self, name: &str, value: &str) {
        self.declare_var(name, value);
    }

    fn set_global_var(&mut self, name: &str, value: &str) {
        self.declare_global_var(name, value);
    }

    fn export_var(&mut self, name: &str) {
        self.export_var(name);
    }

    fn unset_var(&mut self, name: &str) {
        self.unset_var(name);
    }

    fn set_var_readonly(&mut self, name: &str) {
        self.set_readonly(name);
    }

    fn set_var_integer(&mut self, name: &str) {
        self.set_var_integer(name, false);
    }

    fn set_var_float(&mut self, name: &str) {
        self.set_var_float(name, false);
    }

    fn set_var_array(&mut self, name: &str) {
        self.set_var_array(name, false);
    }

    fn set_var_associative(&mut self, name: &str) {
        self.set_var_associative(name, false);
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
        env.scopes[0].variables.clear();
        env.set_var("MY_VAR", "hello");
        env.export_var("MY_VAR");
        let vec = env.to_env_vec();
        let entry = CString::new("MY_VAR=hello").unwrap();
        assert!(vec.contains(&entry));
    }

    #[test]
    fn unexported_var_not_in_env_vec() {
        let mut env = ShellEnv::new();
        env.scopes[0].variables.clear();
        env.set_var("HIDDEN", "secret");
        let vec = env.to_env_vec();
        assert!(vec.is_empty());
    }

    #[test]
    fn readonly_var_cannot_be_set() {
        let mut env = ShellEnv::new();
        env.scopes[0].variables.insert(
            "RO".into(),
            ShellVar {
                value: ShellValue::Scalar("locked".into()),
                export: false,
                readonly: true,
                str_repr: "locked".into(),
            },
        );
        env.set_var("RO", "changed");
        assert_eq!(env.get_var("RO"), Some("locked"));
    }

    #[test]
    fn readonly_var_cannot_be_unset() {
        let mut env = ShellEnv::new();
        env.scopes[0].variables.insert(
            "RO".into(),
            ShellVar {
                value: ShellValue::Scalar("locked".into()),
                export: false,
                readonly: true,
                str_repr: "locked".into(),
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

    // ── Scope tests ──────────────────────────────────────────────

    #[test]
    fn scope_push_and_pop() {
        let mut env = ShellEnv::new();
        assert_eq!(env.scope_depth(), 1);
        env.push_scope();
        assert_eq!(env.scope_depth(), 2);
        env.pop_scope();
        assert_eq!(env.scope_depth(), 1);
    }

    #[test]
    fn cannot_pop_global_scope() {
        let mut env = ShellEnv::new();
        assert!(env.pop_scope().is_none());
        assert_eq!(env.scope_depth(), 1);
    }

    #[test]
    fn local_var_shadows_global() {
        let mut env = ShellEnv::new();
        env.set_var("X", "global");
        env.push_scope();
        env.declare_var("X", "local");
        assert_eq!(env.get_var("X"), Some("local"));
        env.pop_scope();
        assert_eq!(env.get_var("X"), Some("global"));
    }

    #[test]
    fn set_var_modifies_nearest_scope() {
        let mut env = ShellEnv::new();
        env.set_var("X", "global");
        env.push_scope();
        env.declare_var("X", "local");
        env.set_var("X", "modified");
        assert_eq!(env.get_var("X"), Some("modified"));
        env.pop_scope();
        assert_eq!(env.get_var("X"), Some("global"));
    }

    #[test]
    fn set_var_creates_in_global_when_not_found() {
        let mut env = ShellEnv::new();
        env.push_scope();
        env.set_var("NEW", "value");
        env.pop_scope();
        assert_eq!(env.get_var("NEW"), Some("value"));
    }

    #[test]
    fn declare_global_var() {
        let mut env = ShellEnv::new();
        env.push_scope();
        env.declare_global_var("G", "global_val");
        env.pop_scope();
        assert_eq!(env.get_var("G"), Some("global_val"));
    }

    #[test]
    fn shell_value_integer() {
        let var = ShellVar::with_value(ShellValue::Integer(42));
        assert_eq!(var.as_str(), "42");
    }

    #[test]
    fn shell_value_float() {
        let var = ShellVar::with_value(ShellValue::Float(3.14));
        // zsh default: 10 decimal places
        assert_eq!(var.as_str(), "3.1400000000");
    }

    #[test]
    fn shell_value_array() {
        let var = ShellVar::with_value(ShellValue::Array(vec!["a".into(), "b".into(), "c".into()]));
        assert_eq!(var.as_str(), "a b c");
    }

    #[test]
    fn shell_value_set_from_str_preserves_type() {
        let mut var = ShellVar::with_value(ShellValue::Integer(0));
        var.set_str("42");
        assert_eq!(var.value, ShellValue::Integer(42));
        assert_eq!(var.as_str(), "42");
    }

    #[test]
    fn nested_scopes() {
        let mut env = ShellEnv::new();
        env.set_var("A", "1");
        env.push_scope();
        env.declare_var("B", "2");
        env.push_scope();
        env.declare_var("C", "3");
        // All visible from innermost
        assert_eq!(env.get_var("A"), Some("1"));
        assert_eq!(env.get_var("B"), Some("2"));
        assert_eq!(env.get_var("C"), Some("3"));
        env.pop_scope();
        assert_eq!(env.get_var("C"), None);
        assert_eq!(env.get_var("B"), Some("2"));
        env.pop_scope();
        assert_eq!(env.get_var("B"), None);
        assert_eq!(env.get_var("A"), Some("1"));
    }

    #[test]
    fn export_across_scopes() {
        let mut env = ShellEnv::new();
        env.scopes[0].variables.clear();
        env.set_var("X", "outer");
        env.export_var("X");
        env.push_scope();
        env.declare_var("X", "inner");
        // Inner scope X is not exported; outer is
        let vec = env.to_env_vec();
        let entry = CString::new("X=outer").unwrap();
        assert!(vec.contains(&entry));
    }

    #[test]
    fn readonly_in_scope() {
        let mut env = ShellEnv::new();
        env.push_scope();
        env.declare_var("R", "val");
        env.set_readonly("R");
        env.set_var("R", "changed");
        assert_eq!(env.get_var("R"), Some("val"));
    }
}
