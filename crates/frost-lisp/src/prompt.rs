//! `defprompt` — set PS1 / PS2 and `PROMPT_SUBST` in one form.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// Prompt configuration. `ps1` / `ps2` are literal template strings —
/// `frost-prompt::render` expands them each iteration (see that crate for
/// the supported `%…` escapes). `prompt-subst` toggles the `PROMPT_SUBST`
/// shell option so `$VAR` expansion in the template is also applied.
///
/// ```lisp
/// (defprompt :ps1 "%F{green}%n%f@%F{blue}%m%f %~ %# " :ps2 "> " :prompt-subst #t)
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defprompt")]
pub struct PromptSpec {
    #[serde(default)]
    pub ps1: Option<String>,
    #[serde(default)]
    pub ps2: Option<String>,
    /// If true, enable `PROMPT_SUBST` so `$VAR` inside the template
    /// expands. Defaults to unchanged (None).
    #[serde(default)]
    pub prompt_subst: Option<bool>,
}
