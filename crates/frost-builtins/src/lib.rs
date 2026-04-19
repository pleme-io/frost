//! Built-in shell commands for frost.
//!
//! Provides a [`Builtin`] trait, a [`ShellEnvironment`] trait (so builtins
//! stay decoupled from the executor), and a [`BuiltinRegistry`] that maps
//! command names to their implementations.

mod cd;
mod colon;
mod command;
mod echo;
mod eval;
mod exit;
mod export;
mod print;
mod read_builtin;
mod readonly;
mod return_builtin;
mod set;
mod setopt;
mod stubs;
mod test_builtin;
mod true_false;
mod typeset;
mod unset;
mod whence;

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
    reg.register(Box::new(colon::Colon));
    reg.register(Box::new(command::CommandBuiltin));
    reg.register(Box::new(echo::Echo));
    reg.register(Box::new(eval::Eval));
    reg.register(Box::new(exit::Exit));
    reg.register(Box::new(export::Export));
    reg.register(Box::new(print::Print));
    reg.register(Box::new(read_builtin::ReadBuiltin));
    reg.register(Box::new(return_builtin::Return));
    reg.register(Box::new(set::Set));
    reg.register(Box::new(set::Shift));
    reg.register(Box::new(test_builtin::Test));
    reg.register(Box::new(test_builtin::TestKeyword));
    reg.register(Box::new(true_false::True));
    reg.register(Box::new(true_false::False));
    reg.register(Box::new(typeset::Typeset));
    reg.register(Box::new(typeset::Local));
    reg.register(Box::new(typeset::Declare));
    reg.register(Box::new(unset::Unset));
    reg.register(Box::new(whence::Whence));
    reg.register(Box::new(whence::Which));
    reg.register(Box::new(setopt::Setopt));
    reg.register(Box::new(setopt::Unsetopt));
    reg.register(Box::new(readonly::Readonly));
    // Stubs — accept arguments silently and succeed.
    reg.register(Box::new(stubs::Autoload));
    reg.register(Box::new(stubs::Zmodload));
    reg.register(Box::new(stubs::Integer));
    reg.register(Box::new(stubs::Float));
    reg.register(Box::new(stubs::Let));
    reg.register(Box::new(stubs::Trap));
    reg.register(Box::new(stubs::Hash));
    reg.register(Box::new(stubs::Disable));
    reg.register(Box::new(stubs::Enable));
    reg.register(Box::new(stubs::Emulate));
    reg.register(Box::new(stubs::Zle));
    reg.register(Box::new(stubs::Bindkey));
    reg.register(Box::new(stubs::Compdef));
    reg.register(Box::new(stubs::Zstyle));
    reg.register(Box::new(stubs::Alias));
    reg.register(Box::new(stubs::Unalias));
    reg.register(Box::new(stubs::BuiltinCmd));
    reg.register(Box::new(stubs::Wait));
    reg.register(Box::new(stubs::Fg));
    reg.register(Box::new(stubs::Bg));
    reg.register(Box::new(stubs::Jobs));
    reg.register(Box::new(stubs::Suspend));
    reg.register(Box::new(stubs::Times));
    reg.register(Box::new(stubs::Umask));
    reg.register(Box::new(stubs::Ulimit));
    reg.register(Box::new(stubs::Getopts));
    reg.register(Box::new(stubs::Pushd));
    reg.register(Box::new(stubs::Popd));
    reg.register(Box::new(stubs::Dirs));
    reg.register(Box::new(stubs::Limit));
    reg.register(Box::new(stubs::Unlimit));
    reg.register(Box::new(stubs::Sched));
    reg.register(Box::new(stubs::Rehash));
    reg.register(Box::new(stubs::Noglob));
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_contains_expected_builtins() {
        let reg = default_builtins();
        assert!(reg.contains("cd"));
        assert!(reg.contains(":"));
        assert!(reg.contains("command"));
        assert!(reg.contains("echo"));
        assert!(reg.contains("eval"));
        assert!(reg.contains("exit"));
        assert!(reg.contains("export"));
        assert!(reg.contains("print"));
        assert!(reg.contains("read"));
        assert!(reg.contains("return"));
        assert!(reg.contains("set"));
        assert!(reg.contains("shift"));
        assert!(reg.contains("["));
        assert!(reg.contains("test"));
        assert!(reg.contains("true"));
        assert!(reg.contains("false"));
        assert!(reg.contains("typeset"));
        assert!(reg.contains("local"));
        assert!(reg.contains("declare"));
        assert!(reg.contains("unset"));
        assert!(reg.contains("whence"));
        assert!(reg.contains("which"));
        assert!(!reg.contains("nonexistent"));
    }
}
