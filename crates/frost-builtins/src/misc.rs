//! Miscellaneous builtins: command, builtin, type/whence, shift, colon/true.

use crate::{Builtin, ShellEnvironment};

/// : (colon) — do nothing, return 0.
pub struct Colon;
impl Builtin for Colon {
    fn name(&self) -> &str { ":" }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 { 0 }
}

/// shift — shift positional parameters.
pub struct Shift;
impl Builtin for Shift {
    fn name(&self) -> &str { "shift" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let n: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
        // Signal to executor via special var
        env.set_var("__FROST_SHIFT", &n.to_string());
        0
    }
}

/// type/whence — identify commands.
pub struct Type;
impl Builtin for Type {
    fn name(&self) -> &str { "type" }
    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        for arg in args {
            // Search PATH
            if let Ok(path_var) = std::env::var("PATH") {
                let found = path_var.split(':').any(|dir| {
                    std::path::Path::new(dir).join(arg).is_file()
                });
                if found {
                    println!("{arg} is an external command");
                    continue;
                }
            }
            println!("{arg} not found");
            return 1;
        }
        0
    }
}

/// whence — alias for type with different output format.
pub struct Whence;
impl Builtin for Whence {
    fn name(&self) -> &str { "whence" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        Type.execute(args, env)
    }
}

/// command — run command bypassing shell functions.
pub struct CommandBuiltin;
impl Builtin for CommandBuiltin {
    fn name(&self) -> &str { "command" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() { return 0; }
        // Signal to executor to skip function lookup
        env.set_var("__FROST_COMMAND_BYPASS", args[0]);
        0
    }
}

/// builtin — run builtin bypassing functions.
pub struct BuiltinCmd;
impl Builtin for BuiltinCmd {
    fn name(&self) -> &str { "builtin" }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 { 0 }
}

/// alias/unalias (simplified stubs).
pub struct Alias;
impl Builtin for Alias {
    fn name(&self) -> &str { "alias" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        for arg in args {
            if let Some((name, value)) = arg.split_once('=') {
                env.set_var(&format!("__FROST_ALIAS_{name}"), value);
            }
        }
        0
    }
}

pub struct Unalias;
impl Builtin for Unalias {
    fn name(&self) -> &str { "unalias" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        for arg in args {
            env.unset_var(&format!("__FROST_ALIAS_{arg}"));
        }
        0
    }
}

/// typeset/local/declare stubs — need full implementation later.
pub struct Typeset;
impl Builtin for Typeset {
    fn name(&self) -> &str { "typeset" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        // Simplified: handle VAR=value assignments
        for arg in args {
            if arg.starts_with('-') { continue; } // skip flags for now
            if let Some((name, value)) = arg.split_once('=') {
                env.set_var(name, value);
            }
        }
        0
    }
}

pub struct Local;
impl Builtin for Local {
    fn name(&self) -> &str { "local" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        Typeset.execute(args, env)
    }
}

pub struct Declare;
impl Builtin for Declare {
    fn name(&self) -> &str { "declare" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        Typeset.execute(args, env)
    }
}

pub struct Integer;
impl Builtin for Integer {
    fn name(&self) -> &str { "integer" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        Typeset.execute(args, env)
    }
}

pub struct Float;
impl Builtin for Float {
    fn name(&self) -> &str { "float" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        Typeset.execute(args, env)
    }
}

pub struct Readonly;
impl Builtin for Readonly {
    fn name(&self) -> &str { "readonly" }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        Typeset.execute(args, env)
    }
}
