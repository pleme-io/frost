//! `defalias` — Lisp-authored shell alias.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// An alias entry. Name and value are raw strings; value is split on
/// whitespace at expansion time (see `frost-exec::expand_aliases`).
///
/// ```lisp
/// (defalias :name "ll"  :value "ls -la")
/// (defalias :name "gst" :value "git status -sb")
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defalias")]
pub struct AliasSpec {
    pub name: String,
    pub value: String,
}
