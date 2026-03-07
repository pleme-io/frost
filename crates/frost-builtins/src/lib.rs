//! Built-in shell commands for frost.
//!
//! Provides a [`Builtin`] trait, a [`ShellEnvironment`] trait (so builtins
//! stay decoupled from the executor), and a [`BuiltinRegistry`] that maps
//! command names to their implementations.

mod cd;
mod echo;
mod exit;
mod export;
mod true_false;

use std::collections::HashMap;

// ── Shell environment trait ──────────────────────────────────────────

/// Trait that builtins use to interact with the shell environment.
///
/// Defined here (rather than in `frost-exec`) so builtins have no
/// dependency on the execution engine.
pub trait ShellEnvironment {
    fn get_var(&self, name: &str) -> Option<&str>;
    fn set_var(&mut self, name: &str, value: &str);
    fn export_var(&mut self, name: &str);
    fn unset_var(&mut self, name: &str);
    fn exit_status(&self) -> i32;
    fn set_exit_status(&mut self, status: i32);
    fn chdir(&mut self, path: &str) -> Result<(), String>;
    fn home_dir(&self) -> Option<&str>;
}

// ── Builtin trait ────────────────────────────────────────────────────

/// A single built-in command.
pub trait Builtin: Send + Sync {
    /// The command name (e.g. `"cd"`, `"echo"`).
    fn name(&self) -> &str;

    /// Execute the builtin with the given arguments and environment.
    ///
    /// Returns an exit status (0 = success).
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32;
}

// ── Registry ─────────────────────────────────────────────────────────

/// A registry mapping command names to [`Builtin`] implementations.
pub struct BuiltinRegistry {
    builtins: HashMap<String, Box<dyn Builtin>>,
}

impl BuiltinRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            builtins: HashMap::new(),
        }
    }

    /// Register a builtin.
    pub fn register(&mut self, builtin: Box<dyn Builtin>) {
        self.builtins.insert(builtin.name().to_owned(), builtin);
    }

    /// Look up a builtin by name.
    pub fn get(&self, name: &str) -> Option<&dyn Builtin> {
        self.builtins.get(name).map(|b| b.as_ref())
    }

    /// Whether `name` is a registered builtin.
    pub fn contains(&self, name: &str) -> bool {
        self.builtins.contains_key(name)
    }

    /// Iterate over all registered builtins.
    pub fn iter(&self) -> impl Iterator<Item = &dyn Builtin> {
        self.builtins.values().map(|b| b.as_ref())
    }
}

impl Default for BuiltinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Default set ──────────────────────────────────────────────────────

/// Build a registry populated with the standard builtins.
pub fn default_builtins() -> BuiltinRegistry {
    let mut reg = BuiltinRegistry::new();
    reg.register(Box::new(cd::Cd));
    reg.register(Box::new(echo::Echo));
    reg.register(Box::new(exit::Exit));
    reg.register(Box::new(export::Export));
    reg.register(Box::new(true_false::True));
    reg.register(Box::new(true_false::False));
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_contains_expected_builtins() {
        let reg = default_builtins();
        assert!(reg.contains("cd"));
        assert!(reg.contains("echo"));
        assert!(reg.contains("exit"));
        assert!(reg.contains("export"));
        assert!(reg.contains("true"));
        assert!(reg.contains("false"));
        assert!(!reg.contains("nonexistent"));
    }
}
