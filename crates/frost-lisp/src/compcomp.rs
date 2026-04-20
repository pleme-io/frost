//! Rich completion DSL — `defsubcmd`, `defflag`, `defposit`.
//!
//! The original `defcompletion` gives a flat candidate list per
//! command. That covers the 80% case of "what subcommands exist" but
//! can't express:
//!
//!   * per-subcommand flags (`git commit -m`, `kubectl apply -f`)
//!   * flags that take an argument with a particular value type
//!     (`$EDITOR` picks a file, `--namespace` picks a namespace)
//!   * positional arguments with type constraints
//!   * descriptions on flags + positionals (zsh's `[desc]` brackets)
//!
//! This module adds three flat Lisp forms whose specs compose into a
//! tree keyed by dotted paths:
//!
//! ```lisp
//! ;; Register a subcommand under a parent.
//! (defsubcmd :path "git" :name "commit" :description "record changes")
//! (defsubcmd :path "git" :name "checkout" :description "switch branches")
//!
//! ;; Flags under a (sub)command path.
//! (defflag :path "git.commit" :name "-m" :takes "string"
//!          :description "commit message")
//! (defflag :path "git.commit" :name "--amend"
//!          :description "replace last commit")
//!
//! ;; Positional with a type constraint.
//! (defposit :path "git.commit" :index 1 :takes "files"
//!           :description "paths to commit")
//! ```
//!
//! The `:takes` slot names a **value type**. Supported values:
//!
//! | Value            | Meaning                                 |
//! |------------------|-----------------------------------------|
//! | omitted          | bool flag / bare positional             |
//! | `"string"`       | free text                               |
//! | `"integer"`      | numeric                                 |
//! | `"file"`         | a file path (filesystem completion)     |
//! | `"files"`        | one or more file paths                  |
//! | `"dir"`          | directory path                          |
//! | `"dirs"`         | one or more directory paths             |
//! | `"choice:a,b,c"` | one of the comma-separated choices      |
//!
//! Keeping the forms flat (three single-struct specs rather than one
//! deeply-nested map) keeps them tatara-lisp-friendly: each one maps
//! to a plain `DeriveTataraDomain` struct, the same pattern every
//! other def-form uses. The tree is assembled in frost-complete at
//! apply time by joining on `path`.
//!
//! Zsh intent parity: every `_arguments` spec maps to one or more of
//! these three primitives. A `'-m+[commit message]:msg:'` spec
//! becomes `(defflag :path "git.commit" :name "-m" :takes "string"
//! :description "commit message")`. Mutual-exclusion groups and
//! state-based completers aren't covered yet — tracked for a second
//! pass.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defsubcmd")]
pub struct SubcmdSpec {
    /// Parent path (dotted). `"git"` for a top-level subcommand of git,
    /// `"git.remote"` for a sub-sub-command of `git remote`.
    pub path: String,
    /// Subcommand name — what the user types.
    pub name: String,
    /// One-line description shown in the Tab menu.
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defflag")]
pub struct FlagSpec {
    /// Path where the flag lives. `"git.commit"` means the flag is
    /// offered after `git commit` has been typed.
    pub path: String,
    /// Flag literal — `"-m"`, `"--amend"`, `"-v"`. Long/short doesn't
    /// matter for the completer; both styles go through the same match.
    pub name: String,
    /// Value type the flag accepts. See module docs for the table.
    /// `None` = bool flag.
    #[serde(default)]
    pub takes: Option<String>,
    /// Description shown in the Tab menu.
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defposit")]
pub struct PositSpec {
    /// Path where the positional lives.
    pub path: String,
    /// 1-based argument position. Multiple `(defposit …)` with the
    /// same `:path` and different `:index` build a positional
    /// sequence (`kubectl apply -f FILE RESOURCE`).
    #[serde(default = "one")]
    pub index: u32,
    /// Value type — see module docs.
    #[serde(default)]
    pub takes: Option<String>,
    /// Description shown in the Tab menu.
    #[serde(default)]
    pub description: Option<String>,
}

fn one() -> u32 {
    1
}

/// Typed value kind parsed from a `takes:` string. Used by
/// frost-complete to drive the right completer at each arg position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueKind {
    /// Free text — no enumeration, accept anything.
    String,
    /// Numeric.
    Integer,
    /// Single file path.
    File,
    /// One or more file paths (consumed greedily until the next flag).
    Files,
    /// Single directory.
    Dir,
    /// One or more directories.
    Dirs,
    /// One of a fixed set.
    Choice(Vec<String>),
}

impl ValueKind {
    /// Parse a `takes:` string. Unknown kinds fall back to `String` —
    /// keeps rc files working even if we add a new kind later and the
    /// user hasn't updated their specs.
    pub fn parse(s: &str) -> Self {
        if let Some(rest) = s.strip_prefix("choice:") {
            let choices: Vec<String> = rest
                .split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            return Self::Choice(choices);
        }
        match s {
            "string" => Self::String,
            "integer" => Self::Integer,
            "file" => Self::File,
            "files" => Self::Files,
            "dir" => Self::Dir,
            "dirs" => Self::Dirs,
            _ => Self::String,
        }
    }

    /// True if this kind should trigger filesystem completion at Tab
    /// time. Used by frost-complete to decide whether to walk the
    /// filesystem or just list the finite choice set.
    pub fn completes_from_fs(&self) -> bool {
        matches!(self, Self::File | Self::Files | Self::Dir | Self::Dirs)
    }

    /// True if this kind should only offer directories (not files).
    pub fn directories_only(&self) -> bool {
        matches!(self, Self::Dir | Self::Dirs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_kind_parses_known_strings() {
        assert_eq!(ValueKind::parse("string"), ValueKind::String);
        assert_eq!(ValueKind::parse("integer"), ValueKind::Integer);
        assert_eq!(ValueKind::parse("file"), ValueKind::File);
        assert_eq!(ValueKind::parse("files"), ValueKind::Files);
        assert_eq!(ValueKind::parse("dir"), ValueKind::Dir);
        assert_eq!(ValueKind::parse("dirs"), ValueKind::Dirs);
    }

    #[test]
    fn value_kind_parses_choice() {
        let k = ValueKind::parse("choice:json,yaml,text");
        assert_eq!(
            k,
            ValueKind::Choice(vec!["json".into(), "yaml".into(), "text".into()])
        );
    }

    #[test]
    fn value_kind_unknown_falls_back_to_string() {
        assert_eq!(ValueKind::parse("bogus"), ValueKind::String);
        assert_eq!(ValueKind::parse(""), ValueKind::String);
    }

    #[test]
    fn fs_completion_predicates() {
        assert!(ValueKind::File.completes_from_fs());
        assert!(ValueKind::Dir.completes_from_fs());
        assert!(!ValueKind::String.completes_from_fs());
        assert!(!ValueKind::Choice(vec![]).completes_from_fs());
        assert!(ValueKind::Dir.directories_only());
        assert!(!ValueKind::File.directories_only());
    }
}
