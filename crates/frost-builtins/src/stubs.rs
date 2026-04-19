//! Stub builtins that accept arguments silently and succeed.
//!
//! These are needed to pass zsh test suite prep blocks that call
//! commands like `autoload`, `zmodload`, `integer`, `float`, `let`,
//! `trap`, `hash`, `disable`, `enable`, `emulate`, `zle`, `bindkey`,
//! `compdef`, `zstyle`, `alias`, `unalias`, `builtin`, `wait`, `fg`,
//! `bg`, `jobs`, `suspend`, `times`, `umask`, `ulimit`, `getopts`,
//! `pushd`, `popd`, `dirs`, `limit`, `unlimit`, `sched`, `rehash`,
//! `noglob`.

use crate::{Builtin, ShellEnvironment};

macro_rules! stub_builtin {
    ($struct_name:ident, $cmd_name:expr) => {
        pub struct $struct_name;

        impl Builtin for $struct_name {
            fn name(&self) -> &str {
                $cmd_name
            }

            fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
                0
            }
        }
    };
}

stub_builtin!(Autoload, "autoload");
stub_builtin!(Zmodload, "zmodload");
stub_builtin!(Integer, "integer");
stub_builtin!(Float, "float");
stub_builtin!(Let, "let");
stub_builtin!(Trap, "trap");
stub_builtin!(Hash, "hash");
stub_builtin!(Disable, "disable");
stub_builtin!(Enable, "enable");
stub_builtin!(Emulate, "emulate");
stub_builtin!(Zle, "zle");
stub_builtin!(Bindkey, "bindkey");
stub_builtin!(Compdef, "compdef");
stub_builtin!(Zstyle, "zstyle");
stub_builtin!(Alias, "alias");
stub_builtin!(Unalias, "unalias");
stub_builtin!(BuiltinCmd, "builtin");
stub_builtin!(Wait, "wait");
stub_builtin!(Fg, "fg");
stub_builtin!(Bg, "bg");
stub_builtin!(Jobs, "jobs");
stub_builtin!(Suspend, "suspend");
stub_builtin!(Times, "times");
stub_builtin!(Umask, "umask");
stub_builtin!(Ulimit, "ulimit");
stub_builtin!(Getopts, "getopts");
stub_builtin!(Pushd, "pushd");
stub_builtin!(Popd, "popd");
stub_builtin!(Dirs, "dirs");
stub_builtin!(Limit, "limit");
stub_builtin!(Unlimit, "unlimit");
stub_builtin!(Sched, "sched");
stub_builtin!(Rehash, "rehash");
stub_builtin!(Noglob, "noglob");
