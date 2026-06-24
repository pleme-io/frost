//! set/unset builtins.

use crate::{Builtin, BuiltinAction, BuiltinResult, ShellEnvironment};

pub struct Set;
pub struct Unset;

/// Map a POSIX/zsh single-letter `set` flag to its long option name.
///
/// Only the unambiguous "enable means turn this on" letters are mapped;
/// paired options with inverted letters (`-f` noglob, `-C` noclobber) are
/// reached via the fully general `-o <name>` / `+o <name>` form instead, so
/// the `-`/`+` sign keeps its plain enable/disable meaning here.
fn flag_letter_name(c: char) -> Option<&'static str> {
    match c {
        'e' => Some("errexit"),
        'x' => Some("xtrace"),
        'v' => Some("verbose"),
        'm' => Some("monitor"),
        'b' => Some("notify"),
        _ => None,
    }
}

/// Build the `no`-prefixed form of an option name without `format!`
/// (TYPED EMISSION — `std::format` is banned fleet-wide). `SetOptions`
/// resolves a `no`-prefixed name to *unsetting* the underlying option, so a
/// single `SetOptions` action expresses both enables and disables.
fn negated_name(name: &str) -> String {
    let mut s = String::with_capacity(name.len() + 2);
    s.push_str("no");
    s.push_str(name);
    s
}

impl Builtin for Set {
    fn name(&self) -> &str {
        "set"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            // Print all variables (simplified)
            return 0;
        }
        if args[0] == "--" {
            // set -- args: set positional parameters
            // Store as __FROST_POSITIONAL for executor to pick up
            let params = args[1..].join("\x1f");
            env.set_var("__FROST_SET_POSITIONAL", &params);
            return 0;
        }
        // Option toggling happens via execute_with_action (the action path
        // owns ShellEnv.options); nothing to do on the legacy path.
        0
    }

    fn execute_with_action(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> BuiltinResult {
        if args.is_empty() {
            return BuiltinResult::ok();
        }

        // Accumulate option toggles as option names for one `SetOptions`
        // action: a plain name enables, a `no`-prefixed name disables.
        let mut names: Vec<String> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let arg = args[i];
            if arg == "--" {
                // Everything after `--` is positional parameters.
                let params: Vec<String> = args[i + 1..].iter().map(|s| s.to_string()).collect();
                return BuiltinResult::with_action(0, BuiltinAction::SetPositional(params));
            } else if let Some(rest) = arg.strip_prefix('-') {
                if rest.is_empty() {
                    i += 1; // lone "-": no-op flag terminator
                    continue;
                }
                if rest == "o" {
                    if let Some(name) = args.get(i + 1) {
                        names.push((*name).to_string());
                        i += 2;
                        continue;
                    }
                    i += 1;
                    continue;
                }
                for c in rest.chars() {
                    if let Some(n) = flag_letter_name(c) {
                        names.push(n.to_string());
                    }
                }
                i += 1;
            } else if let Some(rest) = arg.strip_prefix('+') {
                if rest == "o" {
                    if let Some(name) = args.get(i + 1) {
                        names.push(negated_name(name));
                        i += 2;
                        continue;
                    }
                    i += 1;
                    continue;
                }
                for c in rest.chars() {
                    if let Some(n) = flag_letter_name(c) {
                        names.push(negated_name(n));
                    }
                }
                i += 1;
            } else {
                // A bare argument (no -/+ prefix): POSIX `set a b c` assigns
                // the positional parameters from here on.
                let params: Vec<String> = args[i..].iter().map(|s| s.to_string()).collect();
                return BuiltinResult::with_action(0, BuiltinAction::SetPositional(params));
            }
        }

        if names.is_empty() {
            return BuiltinResult::ok();
        }
        BuiltinResult::with_action(0, BuiltinAction::SetOptions(names))
    }
}

impl Builtin for Unset {
    fn name(&self) -> &str {
        "unset"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        for arg in args {
            if *arg == "-f" || *arg == "-v" {
                continue; // flags, skip
            }
            env.unset_var(arg);
        }
        0
    }
}
