//! The `getopts` builtin — POSIX-style option parsing.
//!
//! Usage: `getopts optstring name [arg...]`
//!
//! Parses options from positional parameters (or explicit `arg` list).
//! Tracks position via `OPTIND` and stores option arguments in `OPTARG`.
//! If `optstring` starts with `:`, uses silent error reporting.

use crate::{Builtin, ShellEnvironment};

pub struct Getopts;

impl Builtin for Getopts {
    fn name(&self) -> &str {
        "getopts"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        // getopts optstring name [arg...]
        if args.len() < 2 {
            eprintln!("getopts: usage: getopts optstring name [arg ...]");
            return 2;
        }

        let raw_optstring = args[0];
        let var_name = args[1];

        // If optstring starts with ':', use silent error mode
        let (silent, optstring) = if raw_optstring.starts_with(':') {
            (true, &raw_optstring[1..])
        } else {
            (false, raw_optstring)
        };

        // Remaining args after optstring and name are the arg list to parse.
        // If none provided, the executor should have placed positional
        // parameters as the remaining args.
        let opt_args: Vec<&str> = if args.len() > 2 {
            args[2..].to_vec()
        } else {
            // No explicit args — try to read positional parameters from env.
            // The executor typically passes them, but as a fallback we
            // look at __FROST_POSITIONAL_COUNT.
            let count: usize = env
                .get_var("__FROST_POSITIONAL_COUNT")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let mut positionals = Vec::new();
            for i in 1..=count {
                if let Some(val) = env.get_var(&i.to_string()) {
                    positionals.push(val.to_string());
                }
            }
            // We need &str but own Strings — store in env and re-read.
            // Actually, just return owned and convert below.
            // For simplicity, return empty if no args provided.
            if positionals.is_empty() {
                env.set_var(var_name, "?");
                return 1;
            }
            // We cannot return references to local Strings, so we
            // handle this path separately.
            return execute_getopts_owned(&positionals, optstring, var_name, silent, env);
        };

        execute_getopts(opt_args.as_slice(), optstring, var_name, silent, env)
    }
}

/// Core getopts logic operating on a slice of `&str` arguments.
fn execute_getopts(
    opt_args: &[&str],
    optstring: &str,
    var_name: &str,
    silent: bool,
    env: &mut dyn ShellEnvironment,
) -> i32 {
    // OPTIND is 1-based. It points to the next argument to process.
    let optind: usize = env
        .get_var("OPTIND")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    // Convert to 0-based index into opt_args
    let idx = optind.saturating_sub(1);

    if idx >= opt_args.len() {
        // No more arguments to process
        env.set_var(var_name, "?");
        return 1;
    }

    let arg = opt_args[idx];

    // Check if this is an option argument (starts with '-')
    if !arg.starts_with('-') || arg == "-" {
        env.set_var(var_name, "?");
        return 1;
    }

    // "--" signals end of options
    if arg == "--" {
        env.set_var("OPTIND", &(optind + 1).to_string());
        env.set_var(var_name, "?");
        return 1;
    }

    // Get the current character position within the current argument.
    // __FROST_OPTPOS tracks multi-char args like -abc.
    let char_pos: usize = env
        .get_var("__FROST_OPTPOS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let arg_bytes = arg.as_bytes();

    if char_pos >= arg_bytes.len() {
        // Move to next argument
        env.set_var("OPTIND", &(optind + 1).to_string());
        env.unset_var("__FROST_OPTPOS");
        env.set_var(var_name, "?");
        return 1;
    }

    let opt_char = arg_bytes[char_pos] as char;

    // Look up the option character in optstring
    let opt_idx = optstring.find(opt_char);

    match opt_idx {
        Some(pos) => {
            // Valid option found
            let needs_arg = optstring.as_bytes().get(pos + 1) == Some(&b':');

            if needs_arg {
                // Option requires an argument
                if char_pos + 1 < arg_bytes.len() {
                    // Argument is the rest of this token: -fvalue
                    let opt_arg = &arg[char_pos + 1..];
                    env.set_var("OPTARG", opt_arg);
                    env.set_var("OPTIND", &(optind + 1).to_string());
                    env.unset_var("__FROST_OPTPOS");
                } else {
                    // Argument is the next token
                    let next_idx = idx + 1;
                    if next_idx < opt_args.len() {
                        env.set_var("OPTARG", opt_args[next_idx]);
                        env.set_var("OPTIND", &(optind + 2).to_string());
                        env.unset_var("__FROST_OPTPOS");
                    } else {
                        // Missing argument
                        if silent {
                            env.set_var(var_name, ":");
                            env.set_var("OPTARG", &opt_char.to_string());
                        } else {
                            eprintln!("getopts: option requires an argument -- '{opt_char}'");
                            env.set_var(var_name, "?");
                            env.unset_var("OPTARG");
                        }
                        env.set_var("OPTIND", &(optind + 1).to_string());
                        env.unset_var("__FROST_OPTPOS");
                        return if silent { 0 } else { 0 };
                    }
                }
            } else {
                // Option does not require an argument
                env.unset_var("OPTARG");
                if char_pos + 1 < arg_bytes.len() {
                    // More option chars in this token: advance position
                    env.set_var("__FROST_OPTPOS", &(char_pos + 1).to_string());
                } else {
                    // Done with this token
                    env.set_var("OPTIND", &(optind + 1).to_string());
                    env.unset_var("__FROST_OPTPOS");
                }
            }

            env.set_var(var_name, &opt_char.to_string());
            0
        }
        None => {
            // Unknown option
            if silent {
                env.set_var(var_name, "?");
                env.set_var("OPTARG", &opt_char.to_string());
            } else {
                eprintln!("getopts: illegal option -- '{opt_char}'");
                env.set_var(var_name, "?");
                env.unset_var("OPTARG");
            }

            if char_pos + 1 < arg_bytes.len() {
                env.set_var("__FROST_OPTPOS", &(char_pos + 1).to_string());
            } else {
                env.set_var("OPTIND", &(optind + 1).to_string());
                env.unset_var("__FROST_OPTPOS");
            }

            0
        }
    }
}

/// Variant of `execute_getopts` for owned `String` arguments.
fn execute_getopts_owned(
    opt_args: &[String],
    optstring: &str,
    var_name: &str,
    silent: bool,
    env: &mut dyn ShellEnvironment,
) -> i32 {
    let refs: Vec<&str> = opt_args.iter().map(String::as_str).collect();
    execute_getopts(&refs, optstring, var_name, silent, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShellEnvironment;
    use std::collections::HashMap;

    struct MockEnv {
        vars: HashMap<String, String>,
        exported: Vec<String>,
        status: i32,
    }

    impl MockEnv {
        fn new() -> Self {
            Self {
                vars: HashMap::new(),
                exported: Vec::new(),
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
        fn export_var(&mut self, name: &str) {
            if !self.exported.contains(&name.to_owned()) {
                self.exported.push(name.into());
            }
        }
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
    fn getopts_simple_flag() {
        let mut env = MockEnv::new();
        let getopts = Getopts;
        // getopts "ab" opt -a
        let status = getopts.execute(&["ab", "opt", "-a"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("opt"), Some("a"));
    }

    #[test]
    fn getopts_with_argument() {
        let mut env = MockEnv::new();
        let getopts = Getopts;
        // getopts "f:" opt -f myfile
        let status = getopts.execute(&["f:", "opt", "-f", "myfile"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("opt"), Some("f"));
        assert_eq!(env.get_var("OPTARG"), Some("myfile"));
    }

    #[test]
    fn getopts_attached_argument() {
        let mut env = MockEnv::new();
        let getopts = Getopts;
        // getopts "f:" opt -fmyfile
        let status = getopts.execute(&["f:", "opt", "-fmyfile"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("opt"), Some("f"));
        assert_eq!(env.get_var("OPTARG"), Some("myfile"));
    }

    #[test]
    fn getopts_unknown_option_silent() {
        let mut env = MockEnv::new();
        let getopts = Getopts;
        // getopts ":ab" opt -z
        let status = getopts.execute(&[":ab", "opt", "-z"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("opt"), Some("?"));
        assert_eq!(env.get_var("OPTARG"), Some("z"));
    }

    #[test]
    fn getopts_end_of_options() {
        let mut env = MockEnv::new();
        let getopts = Getopts;
        // getopts "ab" opt -- (should return 1, no more options)
        let status = getopts.execute(&["ab", "opt", "--"], &mut env);
        assert_eq!(status, 1);
    }

    #[test]
    fn getopts_no_args_returns_error() {
        let mut env = MockEnv::new();
        let getopts = Getopts;
        let status = getopts.execute(&[], &mut env);
        assert_eq!(status, 2);
    }

    #[test]
    fn getopts_combined_flags() {
        let mut env = MockEnv::new();
        let getopts = Getopts;
        // First call: getopts "abc" opt -ab
        let status = getopts.execute(&["abc", "opt", "-ab"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("opt"), Some("a"));
        // OPTPOS should be set for next char
        assert_eq!(env.get_var("__FROST_OPTPOS"), Some("2"));

        // Second call (same OPTIND, next char)
        let status = getopts.execute(&["abc", "opt", "-ab"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("opt"), Some("b"));
    }
}
