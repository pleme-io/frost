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
/// The expander is deliberately simple: `$NAME` is one alphanumeric
/// run, `${NAME}` is anything up to `}`. No command substitution, no
/// arithmetic, no parameter modifiers. Paths don't need that.
pub fn expand_vars(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        // Braced form: ${NAME}
        if let Some((_, '{')) = it.peek().copied() {
            it.next();
            let start = i + 2;
            let mut end = start;
            while let Some((j, ch)) = it.peek().copied() {
                if ch == '}' {
                    it.next();
                    break;
                }
                end = j + ch.len_utf8();
                it.next();
            }
            let name = &s[start..end];
            if let Some(v) = lookup(name) {
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
pub fn apply_path(current: &str, spec: &PathSpec, lookup: &dyn Fn(&str) -> Option<String>) -> String {
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
    for entry in new_prepend.into_iter()
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
