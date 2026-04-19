//! `defcompletion` — Lisp-authored per-command completion spec.
//!
//! A first cut that captures the most useful 80%: a command name plus a
//! flat list of argument names or flag strings to offer. Rich compsys
//! features (positional dispatch, `_arguments` spec DSL, per-state
//! lookups) can layer on top of this struct later.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// ```lisp
/// (defcompletion :command "git"
///                :args ("status" "diff" "log" "commit" "push" "pull"))
///
/// (defcompletion :command "kubectl"
///                :args ("get" "describe" "apply" "delete" "logs" "exec"))
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defcompletion")]
pub struct CompletionSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Short description shown alongside the command in Tab menus.
    #[serde(default)]
    pub description: Option<String>,
}
