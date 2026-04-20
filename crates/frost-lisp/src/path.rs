//! `defpath` — declarative PATH manipulation.
//!
//! The zsh idiom is `export PATH="/new/dir:$PATH"` for prepend,
//! `export PATH="$PATH:/new/dir"` for append. `defpath` encodes the
//! same intent without the `$PATH` dance — entries dedupe
//! automatically, and the spec composes across multiple forms
//! (later forms extend the list rather than overwriting).
//!
//! ```lisp
//! ;; prepend (wins over system binaries with same name)
//! (defpath :prepend ("$HOME/.local/bin" "$HOME/.cargo/bin"))
//! ;; append (system wins over these)
//! (defpath :append ("/opt/homebrew/bin"))
//! ```
//!
//! Variable expansion (`$HOME`, `$XDG_DATA_HOME`) is resolved at apply
//! time against the current `ShellEnv`, mirroring how zsh's `typeset
//! -U path` + `path=("$HOME/bin" $path)` would expand. Unknown vars
//! expand to empty — safer than leaving literal `$HOME` in PATH.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defpath")]
pub struct PathSpec {
    /// Entries to prepend — each becomes higher-priority than existing
    /// PATH content. List order is preserved, so the first entry wins
    /// over the second.
    #[serde(default)]
    pub prepend: Vec<String>,
    /// Entries to append — each becomes lower-priority than existing
    /// PATH content. List order preserved.
    #[serde(default)]
    pub append: Vec<String>,
}

/// Expand `$VAR` / `${VAR}` references in `s` against `lookup`.
/// Unknown vars expand to empty (documented behavior — safer than
/// leaving literal tokens in PATH).
///
/// The expander handles:
///   * `$NAME`             — one alphanumeric run
///   * `${NAME}`           — braced, any chars up to `}`
///   * `${NAME:-default}`  — POSIX-style fallback: default used
///                            when NAME is unset or empty. The
///                            default is itself re-expanded so you
///                            can nest (`${X:-${Y:-$HOME}}`).
///
/// No command substitution, no arithmetic, no other param modifiers.
pub fn expand_vars(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        // Braced form: ${NAME} or ${NAME:-default}
        if let Some((_, '{')) = it.peek().copied() {
            it.next();
            // Scan to the closing `}`, tracking nesting so a default
            // expression like `${X:-${Y}}` doesn't terminate early.
            let start = i + 2;
            let mut end = start;
            let mut depth: i32 = 0;
            while let Some((j, ch)) = it.peek().copied() {
                if ch == '{' {
                    depth += 1;
                    end = j + ch.len_utf8();
                    it.next();
                    continue;
                }
                if ch == '}' {
                    if depth == 0 {
                        it.next();
                        break;
                    }
                    depth -= 1;
                    end = j + ch.len_utf8();
                    it.next();
                    continue;
                }
                end = j + ch.len_utf8();
                it.next();
            }
            let body = &s[start..end];
            // Split on first `:-` for the POSIX fallback form.
            if let Some(sep) = body.find(":-") {
                let name = &body[..sep];
                let default_expr = &body[sep + 2..];
                let value = lookup(name).filter(|v| !v.is_empty());
                match value {
                    Some(v) => out.push_str(&v),
                    None => {
                        // Recursively expand the default — this makes
                        // `${X:-$HOME/.config}` work.
                        let expanded = expand_vars(default_expr, lookup);
                        out.push_str(&expanded);
                    }
                }
            } else if let Some(v) = lookup(body) {
                out.push_str(&v);
            }
            continue;
        }
        // Bare form: $NAME (alphanumeric + underscore run).
        let start = i + 1;
        let mut end = start;
        while let Some((j, ch)) = it.peek().copied() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                end = j + ch.len_utf8();
                it.next();
            } else {
                break;
            }
        }
        if end == start {
            // Dangling `$` — keep literal.
            out.push('$');
            continue;
        }
        let name = &s[start..end];
        if let Some(v) = lookup(name) {
            out.push_str(&v);
        }
    }
    out
}

/// Apply a path spec to a colon-delimited PATH string and return the
/// new value. Dedupes: if an entry already appears in `current`, the
/// spec's copy wins (so prepend actually bumps priority) while a
/// single copy is kept.
pub fn apply_path(
    current: &str,
    spec: &PathSpec,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> String {
    let expand = |v: &str| expand_vars(v, lookup);

    let existing: Vec<String> = current
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    let new_prepend: Vec<String> = spec
        .prepend
        .iter()
        .map(|s| expand(s))
        .filter(|s| !s.is_empty())
        .collect();
    let new_append: Vec<String> = spec
        .append
        .iter()
        .map(|s| expand(s))
        .filter(|s| !s.is_empty())
        .collect();

    // Build the union with prepend > existing > append priority,
    // deduplicating in-order.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for entry in new_prepend
        .into_iter()
        .chain(existing.into_iter())
        .chain(new_append.into_iter())
    {
        if seen.insert(entry.clone()) {
            out.push(entry);
        }
    }
    out.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from<'a>(
        map: &'a HashMap<&'a str, &'a str>,
    ) -> Box<dyn Fn(&str) -> Option<String> + 'a> {
        Box::new(|name: &str| map.get(name).map(|s| s.to_string()))
    }

    #[test]
    fn expand_known_and_unknown() {
        let m: HashMap<&str, &str> = [("HOME", "/Users/me")].into_iter().collect();
        let f = lookup_from(&m);
        assert_eq!(expand_vars("$HOME/.bin", &f), "/Users/me/.bin");
        assert_eq!(expand_vars("${HOME}/.bin", &f), "/Users/me/.bin");
        // Unknown → empty (not literal).
        assert_eq!(expand_vars("$NOPE/x", &f), "/x");
        // Dangling $ kept literal (each one individually).
        assert_eq!(expand_vars("echo $$", &f), "echo $$");
        assert_eq!(expand_vars("$", &f), "$");
    }

    #[test]
    fn expand_posix_fallback_unset_uses_default() {
        let m: HashMap<&str, &str> = [("HOME", "/Users/me")].into_iter().collect();
        let f = lookup_from(&m);
        // Unset variable falls back to the literal default.
        assert_eq!(
            expand_vars("${XDG_CONFIG_HOME:-/fallback}", &f),
            "/fallback"
        );
        // Default expression itself gets expanded — the canonical
        // `~/.config` idiom: use XDG_CONFIG_HOME if set, else $HOME/.config.
        assert_eq!(
            expand_vars("${XDG_CONFIG_HOME:-$HOME/.config}", &f),
            "/Users/me/.config"
        );
        // Braced HOME inside default.
        assert_eq!(
            expand_vars("${XDG_CONFIG_HOME:-${HOME}/.config}", &f),
            "/Users/me/.config"
        );
    }

    #[test]
    fn expand_posix_fallback_set_wins_over_default() {
        let m: HashMap<&str, &str> = [("HOME", "/Users/me"), ("XDG_CONFIG_HOME", "/Users/me/cfg")]
            .into_iter()
            .collect();
        let f = lookup_from(&m);
        // Set variable overrides the default completely.
        assert_eq!(
            expand_vars("${XDG_CONFIG_HOME:-$HOME/.config}", &f),
            "/Users/me/cfg"
        );
    }

    #[test]
    fn expand_posix_fallback_empty_treated_as_unset() {
        let m: HashMap<&str, &str> = [("HOME", "/Users/me"), ("EMPTY", "")].into_iter().collect();
        let f = lookup_from(&m);
        // POSIX `:-` treats empty as unset, so default applies.
        assert_eq!(expand_vars("${EMPTY:-fallback}", &f), "fallback");
        assert_eq!(expand_vars("${EMPTY:-$HOME/x}", &f), "/Users/me/x");
    }

    #[test]
    fn expand_posix_fallback_nested() {
        let m: HashMap<&str, &str> = [("HOME", "/Users/me")].into_iter().collect();
        let f = lookup_from(&m);
        // `${X:-${Y:-$HOME}}` — both outer and inner unset, innermost default wins.
        assert_eq!(expand_vars("${X:-${Y:-$HOME}}", &f), "/Users/me");
    }

    #[test]
    fn apply_prepend_puts_entry_first() {
        let m: HashMap<&str, &str> = HashMap::new();
        let f = lookup_from(&m);
        let spec = PathSpec {
            prepend: vec!["/new/bin".into()],
            append: vec![],
        };
        let out = apply_path("/usr/bin:/bin", &spec, &f);
        assert_eq!(out, "/new/bin:/usr/bin:/bin");
    }

    #[test]
    fn apply_append_puts_entry_last() {
        let m: HashMap<&str, &str> = HashMap::new();
        let f = lookup_from(&m);
        let spec = PathSpec {
            prepend: vec![],
            append: vec!["/opt/homebrew/bin".into()],
        };
        let out = apply_path("/usr/bin:/bin", &spec, &f);
        assert_eq!(out, "/usr/bin:/bin:/opt/homebrew/bin");
    }

    #[test]
    fn apply_dedupes_and_prepend_wins_priority() {
        let m: HashMap<&str, &str> = HashMap::new();
        let f = lookup_from(&m);
        let spec = PathSpec {
            prepend: vec!["/usr/local/bin".into()],
            append: vec![],
        };
        // `/usr/local/bin` was in the middle; prepend must move it to the front, leaving only one copy.
        let out = apply_path("/usr/bin:/usr/local/bin:/bin", &spec, &f);
        assert_eq!(out, "/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn apply_expands_home_in_spec() {
        let m: HashMap<&str, &str> = [("HOME", "/U")].into_iter().collect();
        let f = lookup_from(&m);
        let spec = PathSpec {
            prepend: vec!["$HOME/.local/bin".into(), "${HOME}/.cargo/bin".into()],
            append: vec![],
        };
        let out = apply_path("/usr/bin", &spec, &f);
        assert_eq!(out, "/U/.local/bin:/U/.cargo/bin:/usr/bin");
    }
}
