//! `defmark` — declarative directory bookmarks.
//!
//! Blackmatter-shell ships bookmarks as hand-written aliases:
//! `alias code='cd "$HOME/code/github/pleme-io"'`. Declarative `defmark`
//! makes this one-line AND resolves the path at rc-load time (so
//! the alias body holds the expanded absolute path, not a
//! lazy `$HOME` that might differ between sessions or platforms):
//!
//! ```lisp
//! (defmark :name "code"    :path "$HOME/code/github/pleme-io")
//! (defmark :name "dotfiles" :path "~/.config")
//! (defmark :name "nix"     :path "$HOME/code/github/pleme-io/nix")
//! ```
//!
//! Each form registers an alias with the same name as the mark:
//! `code` → `cd <expanded>`. Composable with the rest of the alias
//! machinery — later `(defalias …)` wins, the mark shows up in
//! completion suggestions, etc.
//!
//! `$VAR` and `~` expansion both happen at rc-load (via the same
//! `path::expand_vars` the defpath form uses) with `HOME` /
//! `XDG_*` falling through to `std::env::var`. Unknown variables
//! expand to empty; trailing slashes trimmed.
//!
//! Why not just use `defalias`? The semantic clarity alone is
//! worth a form:
//!
//!   1. Marks compose into a registry (`ApplySummary::marks`) that
//!      future tooling can read — a `marks` builtin that lists all
//!      registered bookmarks, a Tab completion that offers mark
//!      names, a `jump-to-mark` picker widget.
//!   2. Cross-machine config stays portable: the SAME rc file
//!      produces correctly-expanded aliases on both Darwin and
//!      NixOS because expansion reads the live env.
//!   3. Tilde expansion is always safe — a hand-written alias of
//!      `cd ~/code` only works in zsh if aliases expand tildes,
//!      which frost doesn't do yet (the alias body goes through
//!      the parser, which treats `~` as a Tilde token only in
//!      word-initial position).

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defmark")]
pub struct MarkSpec {
    /// Name the user types (also becomes the alias name).
    pub name: String,
    /// Path to cd to. Supports `$VAR` / `${VAR}` and leading-`~`
    /// expansion; resolved at rc-load.
    pub path: String,
}

/// Expand `~` + `$VAR` in a mark path. Returns the resolved path as
/// a string. Unknown vars / no-HOME → best-effort (segments drop).
pub fn expand_mark_path(raw: &str) -> String {
    let expanded = crate::path::expand_vars(raw, &|name| std::env::var(name).ok());
    if let Some(rest) = expanded.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{}", rest.trim_end_matches('/'));
        }
    }
    if expanded == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
    }
    expanded.trim_end_matches('/').to_string()
}

/// Quote a path for safe embedding inside a `cd` alias body.
/// Single-quote + escape any embedded `'`. Matches how zsh's
/// `alias foo='cd /some/path'` escapes metacharacters.
pub fn shell_quote_path(path: &str) -> String {
    let escaped = path.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// What a mark name resolves to on the command surface WITHOUT the
/// cd-alias. A mark whose name resolves here would alias-swallow every
/// invocation of that command (the 2026-06-11 incident: a mark named
/// `nix` made `nix run …` unrunnable by construction), so apply skips
/// the alias and surfaces this as the typed payload of
/// [`crate::ApplyWarning::MarkShadowsCommand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResolution {
    /// A frost builtin (e.g. `cd`, `echo`).
    Builtin,
    /// A shell function already registered in the environment.
    Function,
    /// An executable on `$PATH`, with the resolved absolute path.
    PathBinary { path: String },
}

impl std::fmt::Display for CommandResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin => write!(f, "builtin"),
            Self::Function => write!(f, "shell function"),
            Self::PathBinary { path } => write!(f, "{path}"),
        }
    }
}

/// First executable regular file named `name` in a `:`-separated
/// `path_var`. Pure in its inputs (the PATH string is a parameter, not
/// read from the process env) so tests control the search space.
pub fn resolve_in_path(name: &str, path_var: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    for dir in path_var.split(':').filter(|d| !d.is_empty()) {
        let candidate = std::path::Path::new(dir).join(name);
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn expand_mark_path_handles_tilde() {
        // Set HOME explicitly for the test.
        let prev = env::var("HOME").ok();
        unsafe {
            env::set_var("HOME", "/Users/sample");
        }
        assert_eq!(expand_mark_path("~/code"), "/Users/sample/code");
        assert_eq!(expand_mark_path("~"), "/Users/sample");
        if let Some(h) = prev {
            unsafe {
                env::set_var("HOME", h);
            }
        }
    }

    #[test]
    fn expand_mark_path_handles_env_var() {
        let prev = env::var("X_TEST_MARK_DIR").ok();
        unsafe {
            env::set_var("X_TEST_MARK_DIR", "/opt/x");
        }
        assert_eq!(expand_mark_path("$X_TEST_MARK_DIR/sub"), "/opt/x/sub");
        assert_eq!(expand_mark_path("${X_TEST_MARK_DIR}/a"), "/opt/x/a");
        if let Some(p) = prev {
            unsafe {
                env::set_var("X_TEST_MARK_DIR", p);
            }
        } else {
            unsafe {
                env::remove_var("X_TEST_MARK_DIR");
            }
        }
    }

    #[test]
    fn shell_quote_path_escapes_interior_quote() {
        assert_eq!(shell_quote_path("/a/b"), "'/a/b'");
        assert_eq!(shell_quote_path("/a'b"), "'/a'\\''b'");
    }
}
