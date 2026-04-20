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
mod getopts;
mod hash;
mod jobs;
mod kill;
mod misc;
mod print;
mod read;
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

    /// Declare a variable in the current (innermost) scope.
    /// Used by `typeset`/`local`/`declare` (without `-g`).
    fn declare_var(&mut self, name: &str, value: &str) {
        self.set_var(name, value);
    }

    /// Set a variable in the global scope (`typeset -g`).
    fn set_global_var(&mut self, name: &str, value: &str) {
        self.set_var(name, value);
    }

    /// Mark a variable as read-only.
    fn set_var_readonly(&mut self, _name: &str) {}

    /// Set a variable's type to integer (`typeset -i`).
    fn set_var_integer(&mut self, _name: &str) {}

    /// Set a variable's type to float (`typeset -F`).
    fn set_var_float(&mut self, _name: &str) {}

    /// Set a variable's type to indexed array (`typeset -a`).
    fn set_var_array(&mut self, _name: &str) {}

    /// Set a variable's type to associative array (`typeset -A`).
    fn set_var_associative(&mut self, _name: &str) {}
}

// ── Builtin action (replaces __FROST_* magic variables) ──────────────

/// Structured actions that builtins can return alongside an exit code.
///
/// Replaces the fragile `__FROST_EVAL_CODE`, `__FROST_SOURCE_FILE`,
/// `__FROST_SHIFT`, `__FROST_SET_POSITIONAL`, `__FROST_LET_EXPR`
/// magic variables with explicit typed variants.
#[derive(Debug, Clone)]
pub enum BuiltinAction {
    /// No special action — just the exit code.
    None,
    /// `eval` — parse and execute this code in the current shell.
    Eval(String),
    /// `source` / `.` — read and execute this file.
    Source(String),
    /// `shift N` — remove N positional parameters.
    Shift(usize),
    /// `set --` — replace positional parameters.
    SetPositional(Vec<String>),
    /// `let` — evaluate arithmetic expression.
    Let(String),
    /// `alias name=value` — define alias(es).
    DefineAlias(Vec<(String, String)>),
    /// `unalias name` — remove alias(es).
    RemoveAlias(Vec<String>),
    /// `setopt` — enable options by name.
    SetOptions(Vec<String>),
    /// `unsetopt` — disable options by name.
    UnsetOptions(Vec<String>),
    /// `exit N` — exit the shell.
    Exit(i32),
}

/// Combined result from a builtin execution.
#[derive(Debug, Clone)]
pub struct BuiltinResult {
    pub status: i32,
    pub action: BuiltinAction,
}

impl BuiltinResult {
    /// Simple success with no action.
    pub fn ok() -> Self {
        Self {
            status: 0,
            action: BuiltinAction::None,
        }
    }

    /// Simple failure with no action.
    pub fn fail(status: i32) -> Self {
        Self {
            status,
            action: BuiltinAction::None,
        }
    }

    /// Result with an action.
    pub fn with_action(status: i32, action: BuiltinAction) -> Self {
        Self { status, action }
    }
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

    /// Execute and return a structured result with optional actions.
    ///
    /// Default implementation wraps `execute()` for backward compatibility.
    fn execute_with_action(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> BuiltinResult {
        let status = self.execute(args, env);
        BuiltinResult {
            status,
            action: BuiltinAction::None,
        }
    }
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
    reg.register(Box::new(misc::Setopt));
    reg.register(Box::new(misc::Unsetopt));
    reg.register(Box::new(misc::Autoload));
    reg.register(Box::new(misc::Zmodload));
    reg.register(Box::new(misc::Functions));
    reg.register(Box::new(misc::Let));
    reg.register(Box::new(misc::Printf));
    reg.register(Box::new(read::Read));

    // Option parsing / signals / hash
    reg.register(Box::new(getopts::Getopts));
    reg.register(Box::new(kill::Kill));
    reg.register(Box::new(hash::Hash));

    // Job control
    reg.register(Box::new(jobs::Jobs));
    reg.register(Box::new(jobs::Fg));
    reg.register(Box::new(jobs::Bg));
    reg.register(Box::new(jobs::Wait));
    reg.register(Box::new(jobs::Disown));

    // Directory stack
    reg.register(Box::new(jobs::Pushd));
    reg.register(Box::new(jobs::Popd));
    reg.register(Box::new(jobs::Dirs));

    // Trap / signals
    reg.register(Box::new(misc::Trap));

    // System
    reg.register(Box::new(misc::Umask));
    reg.register(Box::new(misc::Fc));
    reg.register(Box::new(misc::Noglob));
    reg.register(Box::new(misc::Emulate));
    reg.register(Box::new(misc::Disable));
    reg.register(Box::new(misc::Enable));

    // Completion / ZLE stubs
    reg.register(Box::new(misc::Compdef));
    reg.register(Box::new(misc::Compctl));
    reg.register(Box::new(misc::Zle));
    reg.register(Box::new(misc::Bindkey));
    reg.register(Box::new(misc::Zstyle));
    reg.register(Box::new(misc::Which));

    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_contains_all_builtins() {
        let reg = default_builtins();
        for name in &[
            "cd",
            "echo",
            "print",
            "exit",
            "export",
            "true",
            "false",
            "return",
            "break",
            "continue",
            "eval",
            "source",
            ".",
            "set",
            "unset",
            "test",
            "[",
            ":",
            "shift",
            "type",
            "whence",
            "command",
            "builtin",
            "alias",
            "unalias",
            "typeset",
            "local",
            "declare",
            "integer",
            "float",
            "readonly",
            "setopt",
            "unsetopt",
            "autoload",
            "zmodload",
            "functions",
            "let",
            "printf",
            "read",
            "getopts",
            "kill",
            "hash",
            "jobs",
            "fg",
            "bg",
            "wait",
            "disown",
            "pushd",
            "popd",
            "dirs",
            "trap",
            "umask",
            "fc",
            "noglob",
            "emulate",
            "disable",
            "enable",
            "compdef",
            "compctl",
            "zle",
            "bindkey",
            "zstyle",
            "which",
        ] {
            assert!(reg.contains(name), "missing builtin: {name}");
        }
    }
}
