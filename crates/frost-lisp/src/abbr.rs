//! `defabbr` — fish-style command abbreviations.
//!
//! Aliases expand at exec time inside the parser's alias-table lookup;
//! abbreviations expand in the input line **before** execution, so the
//! expansion is echoed (like `!`-expansion) and recorded in history as
//! the expanded form. Semantic difference:
//!
//! ```lisp
//! (defalias :name "gco" :value "git checkout")   ; hidden substitution
//! (defabbr  :name "gco" :expansion "git checkout") ; visible expansion
//! ```
//!
//! The abbreviation fires only when `gco` is the **first word** of the
//! submitted line — matching fish behavior. A multi-token abbrev is
//! simply a multi-token expansion (`:expansion "git commit --amend"`).
//!
//! Collected into `ApplySummary::abbreviations` at rc apply time; the
//! REPL consumes the map and rewrites the input at submit time.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defabbr")]
pub struct AbbrSpec {
    /// Name the user types (first-word matched).
    pub name: String,
    /// Verbatim text inserted in place of `name`. Can span multiple
    /// tokens — the expander just rewrites `<name> …` → `<expansion> …`.
    pub expansion: String,
}

/// Rewrite `line` if its first word matches `name` in `abbreviations`;
/// return `(rewritten, expanded?)`. When not rewritten, returns the
/// original `line` and `false`.
pub fn expand_abbreviation(
    line: &str,
    abbreviations: &std::collections::HashMap<String, String>,
) -> (String, bool) {
    if abbreviations.is_empty() {
        return (line.to_string(), false);
    }
    // Fast path: find the first word without allocating. We match
    // ASCII-whitespace boundaries (mirrors frost's executor parsing
    // of the command position).
    let leading_ws: usize = line
        .chars()
        .take_while(|c| c.is_ascii_whitespace())
        .map(|c| c.len_utf8())
        .sum();
    let remainder = &line[leading_ws..];
    let first_len: usize = remainder
        .chars()
        .take_while(|c| !c.is_ascii_whitespace())
        .map(|c| c.len_utf8())
        .sum();
    if first_len == 0 {
        return (line.to_string(), false);
    }
    let first_word = &remainder[..first_len];
    let Some(expansion) = abbreviations.get(first_word) else {
        return (line.to_string(), false);
    };
    let tail = &remainder[first_len..];
    let leading = &line[..leading_ws];
    (format!("{leading}{expansion}{tail}"), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("gco".into(), "git checkout".into());
        m.insert("k".into(), "kubectl".into());
        m
    }

    #[test]
    fn expands_first_word_only() {
        let (line, changed) = expand_abbreviation("gco main", &sample());
        assert!(changed);
        assert_eq!(line, "git checkout main");
    }

    #[test]
    fn preserves_leading_whitespace() {
        let (line, changed) = expand_abbreviation("  k get pods", &sample());
        assert!(changed);
        assert_eq!(line, "  kubectl get pods");
    }

    #[test]
    fn does_not_expand_non_leading_occurrence() {
        let (line, changed) = expand_abbreviation("echo gco", &sample());
        assert!(!changed);
        assert_eq!(line, "echo gco");
    }

    #[test]
    fn unknown_first_word_unchanged() {
        let (line, changed) = expand_abbreviation("ls -la", &sample());
        assert!(!changed);
        assert_eq!(line, "ls -la");
    }

    #[test]
    fn empty_map_is_no_op() {
        let empty = HashMap::new();
        let (line, changed) = expand_abbreviation("anything", &empty);
        assert!(!changed);
        assert_eq!(line, "anything");
    }

    #[test]
    fn empty_line_is_no_op() {
        let (line, changed) = expand_abbreviation("", &sample());
        assert!(!changed);
        assert_eq!(line, "");
    }
}
