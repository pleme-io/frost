//! `defprompt` — set PS1 / PS2 and `PROMPT_SUBST` in one form.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// Prompt configuration. `ps1` / `ps2` are literal template strings —
/// `frost-prompt::render` expands them each iteration (see that crate for
/// the supported `%…` escapes). `prompt-subst` toggles the `PROMPT_SUBST`
/// shell option so `$VAR` expansion in the template is also applied.
///
/// `command` delegates the prompt to an external binary — the rc-load
/// layer synthesizes a `precmd` hook that runs `$(command)` and assigns
/// the output to `PS1` on every prompt. Clean integration point for
/// starship / oh-my-posh / any-prompt-generator without needing a
/// tool-specific form.
///
/// ```lisp
/// ;; A literal PS1 template.
/// (defprompt :ps1 "%F{green}%n%f@%F{blue}%m%f %~ %# " :ps2 "> " :prompt-subst #t)
///
/// ;; Delegate to starship.
/// (defprompt :command "starship prompt --status=$?")
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defprompt")]
pub struct PromptSpec {
    #[serde(default)]
    pub ps1: Option<String>,
    #[serde(default)]
    pub ps2: Option<String>,
    /// Right-aligned prompt (`RPROMPT` in zsh, `RPS1`). Drawn on the
    /// same line as `ps1` but flush-right. Common uses: clock, git
    /// branch, exit-code badge. Empty = no right prompt.
    #[serde(default)]
    pub rps1: Option<String>,
    /// If true, enable `PROMPT_SUBST` so `$VAR` inside the template
    /// expands. Defaults to unchanged (None).
    #[serde(default)]
    pub prompt_subst: Option<bool>,
    /// Shell command whose stdout becomes `PS1` each prompt. When set,
    /// the rc loader synthesizes a `precmd` hook that captures the
    /// command's output. If `ps1` is also set, the `command`-driven
    /// hook wins (it's the later writer).
    #[serde(default)]
    pub command: Option<String>,
}
