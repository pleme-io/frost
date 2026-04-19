//! `defenv` — Lisp-authored environment variable assignment.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// Sets a shell variable. When `export` is true, also marks it exported
/// so subprocesses inherit it.
///
/// ```lisp
/// (defenv :name "EDITOR" :value "blnvim" :export #t)
/// (defenv :name "LANG"   :value "en_US.UTF-8" :export #t)
/// (defenv :name "internal_counter" :value "0")  ; not exported
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defenv")]
pub struct EnvSpec {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub export: bool,
}
