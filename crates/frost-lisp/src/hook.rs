//! `defhook` — Lisp-authored shell lifecycle hooks.
//!
//! Unlike zsh's convention of defining a function named `precmd` / `preexec`
//! / `chpwd` and having the shell call it implicitly, `defhook` surfaces the
//! intent explicitly. The `body` is parsed as shell source at load time and
//! stored as a function under a well-known name (`__frost_hook_precmd`,
//! etc.); the REPL invokes the function at the right lifecycle point.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// One lifecycle hook.
///
/// Events:
///
/// * `precmd`  — run after each command, before the next prompt
/// * `preexec` — run after input is accepted, before the command executes
/// * `chpwd`   — run when the working directory changes
///
/// ```lisp
/// (defhook :event "precmd"
///          :body "print -P '%F{244}%D{%H:%M:%S}%f'")
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defhook")]
pub struct HookSpec {
    pub event: String,
    pub body: String,
}

/// Event name → the function name frost stores the body under. Returns
/// `None` for unknown events so the applicator can error cleanly.
pub fn hook_function_name(event: &str) -> Option<&'static str> {
    match event {
        "precmd"   => Some("__frost_hook_precmd"),
        "preexec"  => Some("__frost_hook_preexec"),
        "chpwd"    => Some("__frost_hook_chpwd"),
        _          => None,
    }
}
