//! `defun` — Lisp-authored shell function.
//!
//! The body is shell source (not Lisp), same as `defhook` — so authors
//! write familiar `if` / `for` / `echo` constructs while the enclosing
//! form is declarative. The applicator parses the body once into the
//! frost AST and stores it under `env.functions`, making the function
//! callable by name from anywhere else in the shell.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// ```lisp
/// (defun :name "mkcd"
///        :body "mkdir -p \"$1\" && cd \"$1\"")
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defun")]
pub struct FunctionSpec {
    pub name: String,
    pub body: String,
}
