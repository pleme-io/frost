//! Tatara-Lisp ↔ frost bridge.
//!
//! Every concept in a `.zshrc` — aliases, options, environment variables,
//! prompt templates, hooks, keybindings, completions, functions — lives in
//! frost as typed Rust state inside [`frost_exec::ShellEnv`] (plus side
//! registries owned by `frost-zle` / `frost-complete` / `frost-prompt`).
//!
//! This crate lets users DECLARE that state in Lisp via tatara-lisp, then
//! applies the declarations to the shell at startup time. Per the
//! pleme-io Rust + Lisp pattern:
//!
//! - **Rust** owns the types, invariants, and execution.
//! - **Lisp** owns the authoring surface — how humans express what they
//!   want the shell to do.
//!
//! The top-level entry is [`apply_source`] / [`load_rc`]; each Lisp
//! `def…` form has a dedicated spec type with `#[derive(DeriveTataraDomain)]`.
//!
//! # Current domains
//!
//! | Keyword     | Spec type          | Effect |
//! |-------------|--------------------|--------|
//! | `defalias`  | [`AliasSpec`]      | Add an alias to `env.aliases` |
//! | `defopts`   | [`OptionSetSpec`]  | Enable/disable shell options |
//! | `defenv`    | [`EnvSpec`]        | Set (and optionally export) a variable |
//!
//! Future domains — keep adding new `#[tatara(keyword = "…")]` structs
//! plus a pass in [`apply_source`]: `defprompt`, `defhook`, `defbind`,
//! `defcompletion`, `defun`, `deftrap`.

mod alias;
mod env;
mod option;

pub use alias::AliasSpec;
pub use env::EnvSpec;
pub use option::OptionSetSpec;

use frost_exec::ShellEnv;
use std::path::Path;

pub type LispResult<T> = Result<T, LispError>;

#[derive(Debug, thiserror::Error)]
pub enum LispError {
    #[error("io error reading rc file {path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("tatara-lisp parse error: {0}")]
    Parse(String),
    #[error("unknown option name: {0}")]
    UnknownOption(String),
}

/// Summary of what a rc-application round actually changed. Returned by
/// [`apply_source`] so callers can log or validate.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplySummary {
    pub aliases: usize,
    pub options_enabled: usize,
    pub options_disabled: usize,
    pub env_vars: usize,
    pub env_exports: usize,
}

/// Parse a Lisp source string and apply every recognized form to `env`.
///
/// Forms with unknown keywords are silently ignored (tatara-lisp's
/// `compile_typed` filters by keyword, so mixing `defalias`/`defopts`/
/// `defenv` in the same file is expected).
pub fn apply_source(src: &str, env: &mut ShellEnv) -> LispResult<ApplySummary> {
    let mut summary = ApplySummary::default();

    // Aliases
    let aliases: Vec<AliasSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for a in aliases {
        env.aliases.insert(a.name, a.value);
        summary.aliases += 1;
    }

    // Shell options
    let opts: Vec<OptionSetSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for o in opts {
        for name in &o.enable {
            let opt = frost_options::Options::from_name(name)
                .ok_or_else(|| LispError::UnknownOption(name.clone()))?;
            env.set_option(opt);
            summary.options_enabled += 1;
        }
        for name in &o.disable {
            let opt = frost_options::Options::from_name(name)
                .ok_or_else(|| LispError::UnknownOption(name.clone()))?;
            env.unset_option(opt);
            summary.options_disabled += 1;
        }
    }

    // Environment variables (with optional export).
    let envs: Vec<EnvSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for e in envs {
        env.set_var(&e.name, &e.value);
        summary.env_vars += 1;
        if e.export {
            env.export_var(&e.name);
            summary.env_exports += 1;
        }
    }

    Ok(summary)
}

/// Read and apply a Lisp rc file. Missing file = Ok with empty summary
/// so callers can unconditionally call this on startup.
pub fn load_rc(path: impl AsRef<Path>, env: &mut ShellEnv) -> LispResult<ApplySummary> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(ApplySummary::default());
    }
    let src = std::fs::read_to_string(path).map_err(|e| LispError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    apply_source(&src, env)
}

/// Resolve the default rc file path — `$FROSTRC` if set, else
/// `$XDG_CONFIG_HOME/frost/rc.lisp`, else `$HOME/.frostrc.lisp`.
pub fn default_rc_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("FROSTRC") {
        if !p.is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let p = std::path::PathBuf::from(xdg).join("frost").join("rc.lisp");
        if p.exists() { return p; }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(".frostrc.lisp")
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_aliases() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defalias :name "ll" :value "ls -la")
            (defalias :name "gst" :value "git status -sb")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.aliases, 2);
        assert_eq!(env.aliases.get("ll").map(String::as_str), Some("ls -la"));
        assert_eq!(env.aliases.get("gst").map(String::as_str), Some("git status -sb"));
    }

    #[test]
    fn apply_options_enable_disable() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defopts :enable ("extendedglob" "globdots")
                     :disable ("beep"))
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.options_enabled, 2);
        assert_eq!(s.options_disabled, 1);
        assert!(env.is_option_set(frost_options::ShellOption::ExtendedGlob));
        assert!(env.is_option_set(frost_options::ShellOption::GlobDots));
    }

    #[test]
    fn apply_unknown_option_errors() {
        let mut env = ShellEnv::new();
        let src = r#"(defopts :enable ("notAnOption"))"#;
        assert!(matches!(apply_source(src, &mut env), Err(LispError::UnknownOption(_))));
    }

    #[test]
    fn apply_env_with_export() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defenv :name "EDITOR" :value "blnvim" :export #t)
            (defenv :name "PAGER"  :value "less -R")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.env_vars, 2);
        assert_eq!(s.env_exports, 1);
        assert_eq!(env.get_var("EDITOR"), Some("blnvim"));
        assert_eq!(env.get_var("PAGER"), Some("less -R"));
    }

    #[test]
    fn apply_mixed_source_in_one_pass() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defalias :name "ll" :value "ls -la")
            (defopts :enable ("globdots"))
            (defenv :name "LANG" :value "en_US.UTF-8" :export #t)
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.aliases, 1);
        assert_eq!(s.options_enabled, 1);
        assert_eq!(s.env_vars, 1);
        assert_eq!(s.env_exports, 1);
    }

    #[test]
    fn missing_rc_is_not_an_error() {
        let mut env = ShellEnv::new();
        let s = load_rc("/nonexistent/path/frostrc.lisp", &mut env).unwrap();
        assert_eq!(s, ApplySummary::default());
    }

    #[test]
    fn default_rc_path_is_nonempty() {
        assert!(!default_rc_path().as_os_str().is_empty());
    }
}
