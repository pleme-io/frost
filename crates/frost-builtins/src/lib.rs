//! Built-in shell commands for frost.
//!
//! Provides a [`Builtin`] trait, a [`ShellEnvironment`] trait (so builtins
//! stay decoupled from the executor), and a [`BuiltinRegistry`] that maps
//! command names to their implementations.

mod cd;
pub mod control;
mod echo;
mod eval_builtin;
mod exit;
mod export;
mod misc;
mod print;
mod set;
mod source;
mod test;
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

    // Core I/O
    reg.register(Box::new(true_false::True));
    reg.register(Box::new(true_false::False));
    reg.register(Box::new(echo::Echo));
    reg.register(Box::new(print::Print));
    reg.register(Box::new(cd::Cd));
    reg.register(Box::new(exit::Exit));
    reg.register(Box::new(export::Export));

    // Control flow
    reg.register(Box::new(control::Return));
    reg.register(Box::new(control::Break));
    reg.register(Box::new(control::Continue));

    // Eval/source
    reg.register(Box::new(eval_builtin::Eval));
    reg.register(Box::new(source::Source));
    reg.register(Box::new(source::Dot));

    // Set/unset
    reg.register(Box::new(set::Set));
    reg.register(Box::new(set::Unset));

    // Test
    reg.register(Box::new(test::Test));
    reg.register(Box::new(test::Bracket));

    // Misc
    reg.register(Box::new(misc::Colon));
    reg.register(Box::new(misc::Shift));
    reg.register(Box::new(misc::Type));
    reg.register(Box::new(misc::Whence));
    reg.register(Box::new(misc::CommandBuiltin));
    reg.register(Box::new(misc::BuiltinCmd));
    reg.register(Box::new(misc::Alias));
    reg.register(Box::new(misc::Unalias));
    reg.register(Box::new(misc::Typeset));
    reg.register(Box::new(misc::Local));
    reg.register(Box::new(misc::Declare));
    reg.register(Box::new(misc::Integer));
    reg.register(Box::new(misc::Float));
    reg.register(Box::new(misc::Readonly));

    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_contains_all_builtins() {
        let reg = default_builtins();
        for name in &[
            "cd", "echo", "print", "exit", "export", "true", "false",
            "return", "break", "continue", "eval", "source", ".",
            "set", "unset", "test", "[", ":", "shift", "type",
            "whence", "command", "builtin", "alias", "unalias",
            "typeset", "local", "declare", "integer", "float", "readonly",
        ] {
            assert!(reg.contains(name), "missing builtin: {name}");
        }
    }
}
