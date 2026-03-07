//! The `export` builtin — mark variables for export to child processes.
//!
//! Supports:
//! - `export VAR=value` — set and export
//! - `export VAR` — export existing variable
//! - `export -p` — print all exported variables

use crate::{Builtin, ShellEnvironment};

pub struct Export;

impl Builtin for Export {
    fn name(&self) -> &str {
        "export"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() || (args.len() == 1 && args[0] == "-p") {
            // `export` or `export -p`: list exported variables.
            // The ShellEnvironment trait doesn't expose an export
            // iterator, so this is a no-op stub for now.
            return 0;
        }

        for arg in args {
            if *arg == "-p" {
                continue;
            }

            if let Some((name, value)) = arg.split_once('=') {
                if !is_valid_identifier(name) {
                    eprintln!("export: `{name}': not a valid identifier");
                    return 1;
                }
                env.set_var(name, value);
                env.export_var(name);
            } else {
                if !is_valid_identifier(arg) {
                    eprintln!("export: `{arg}': not a valid identifier");
                    return 1;
                }
                env.export_var(arg);
            }
        }

        0
    }
}

/// A valid shell identifier: starts with a letter or underscore,
/// followed by letters, digits, or underscores.
fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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
    fn export_sets_and_exports() {
        let mut env = MockEnv::new();
        let export = Export;
        let status = export.execute(&["FOO=bar"], &mut env);
        assert_eq!(status, 0);
        assert_eq!(env.get_var("FOO"), Some("bar"));
        assert!(env.exported.contains(&"FOO".to_owned()));
    }

    #[test]
    fn export_name_only() {
        let mut env = MockEnv::new();
        env.set_var("BAZ", "qux");
        let export = Export;
        let status = export.execute(&["BAZ"], &mut env);
        assert_eq!(status, 0);
        assert!(env.exported.contains(&"BAZ".to_owned()));
    }

    #[test]
    fn export_invalid_identifier() {
        let mut env = MockEnv::new();
        let export = Export;
        let status = export.execute(&["123BAD=val"], &mut env);
        assert_eq!(status, 1);
    }

    #[test]
    fn export_p_is_no_op() {
        let mut env = MockEnv::new();
        let export = Export;
        let status = export.execute(&["-p"], &mut env);
        assert_eq!(status, 0);
    }

    #[test]
    fn valid_identifiers() {
        assert!(is_valid_identifier("FOO"));
        assert!(is_valid_identifier("_bar"));
        assert!(is_valid_identifier("a1_2"));
        assert!(!is_valid_identifier("1abc"));
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("foo-bar"));
    }
}
