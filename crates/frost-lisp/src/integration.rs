//! `defintegration` — one-line tool integrations.
//!
//! Several tools need a consistent "init recipe" across shells:
//! zoxide wants an `add` call on every `chpwd`, direnv wants a
//! `export` on every `chpwd`, starship wants a `prompt` call on every
//! `precmd`. Writing those out as three separate `(defhook …)` forms
//! per tool produces file-sprawl and forces users to know the
//! internal conventions of each tool.
//!
//! `(defintegration :tool "zoxide")` collapses the recipe to one
//! line. At apply time we look up the tool in
//! [`KNOWN_INTEGRATIONS`] and expand it into the underlying
//! aliases/hooks/prompt forms. Missing tool ⇒ rc-load error so typos
//! don't silently no-op.
//!
//! ```lisp
//! (defintegration :tool "zoxide")
//! (defintegration :tool "direnv")
//! (defintegration :tool "starship")
//! (defintegration :tool "atuin")
//! ```
//!
//! Philosophy: `defintegration` is sugar. The underlying primitives
//! (`defalias`, `defhook`, `defprompt`) stay fully usable for anything
//! `KNOWN_INTEGRATIONS` doesn't cover. Tools in the registry can be
//! overridden locally by writing the primitive forms after the
//! `defintegration` (last writer wins in the alias/prompt paths;
//! hooks compose, which is usually what you want).

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defintegration")]
pub struct IntegrationSpec {
    /// Tool name — must appear in [`KNOWN_INTEGRATIONS`].
    pub tool: String,
}

/// What one integration expands into. Each field is independently
/// optional so integrations can contribute only the pieces they need.
#[derive(Debug, Clone)]
pub struct IntegrationRecipe {
    pub aliases: &'static [(&'static str, &'static str)],
    pub precmd_body: Option<&'static str>,
    pub preexec_body: Option<&'static str>,
    pub chpwd_body: Option<&'static str>,
    pub env: &'static [(&'static str, &'static str, bool /* export */)],
    pub prompt_command: Option<&'static str>,
    /// Named shell functions the integration injects. Each entry is
    /// `(name, body)`; the body is shell source compiled into a real
    /// function (NOT an alias) so it can wrap builtins, take
    /// arguments, and return typed exit codes. Used by zoxide to
    /// override `cd` with a smart fallback that consults the zoxide
    /// database when the literal path doesn't exist.
    pub functions: &'static [(&'static str, &'static str)],
}

/// Lookup table. Keep recipes minimal — the canonical "what does each
/// integration want"? If a user needs more, they can append primitive
/// forms in their rc. Ordering: recipe fires BEFORE any user forms in
/// the same file (apply pass runs `defintegration` first in the
/// consumer-facing order doesn't matter because of hook composition).
pub fn lookup_integration(tool: &str) -> Option<IntegrationRecipe> {
    match tool {
        "zoxide" => Some(IntegrationRecipe {
            // zoxide's bash/zsh init defines shell functions `__zoxide_z`
            // etc. that wrap `zoxide query` with smart cd semantics.
            // frost doesn't run that init, so we alias directly to
            // `zoxide query` — one subprocess per jump, same UX.
            aliases: &[("z", "zoxide query"), ("zi", "zoxide query -i")],
            precmd_body: None,
            preexec_body: None,
            // zoxide needs to record every directory change.
            chpwd_body: Some(r#"command -v zoxide >/dev/null 2>&1 && zoxide add -- "$PWD""#),
            env: &[],
            prompt_command: None,
            // Smart-cd override. `cd <arg>` tries the builtin first
            // (no args = $HOME, literal path = use it); falls back to
            // `zoxide query` so frequently-visited matches like
            // `cd nix` resolve to e.g. ~/code/github/pleme-io/nix.
            // The `command cd` invocation bypasses any aliases on cd
            // (cycle-safe). Zero args, single-arg dash, and absolute
            // paths short-circuit through the builtin without
            // consulting zoxide — preserves $OLDPWD semantics.
            functions: &[(
                "cd",
                r#"if [ $# -eq 0 ]; then
  builtin cd
elif [ "$1" = "-" ]; then
  builtin cd -
elif builtin cd "$@" 2>/dev/null; then
  :
elif command -v zoxide >/dev/null 2>&1; then
  local __frost_z_target
  __frost_z_target=$(zoxide query -- "$@" 2>/dev/null) && builtin cd -- "$__frost_z_target"
else
  builtin cd "$@"
fi"#,
            )],
        }),

        "direnv" => Some(IntegrationRecipe {
            aliases: &[],
            precmd_body: None,
            preexec_body: None,
            // `direnv export` re-sources .envrc. This still goes through
            // `eval` of direnv's BASH-format output, which works only because
            // frost implements `$'…'` as of 2026-08-07 — before that it failed
            // silently on every line and .envrc variables never applied at all.
            //
            // The typed replacement EXISTS and is tested:
            // `frost_builtins::direnv::DirenvApply` (`__frost_direnv_apply`)
            // runs `direnv export json` and applies the delta natively, with no
            // eval and no shell. It is deliberately NOT wired here yet — its
            // ENTER path is correct and its LEAVE path is not. Measured: after
            // `cd` out of a directory, `direnv export json` from the builtin
            // returns the LOAD delta again rather than the leave-delta of
            // nulls, so an .envrc variable survives leaving its own tree.
            //
            // Ruled out while chasing it, so nobody re-walks these: the child
            // DOES inherit the exported DIRENV_* state (verified with `env`),
            // and frost's `cd` DOES change the process cwd (verified: a child
            // `/bin/pwd` reports the new directory). Something else in the
            // DIRENV_DIFF/DIRENV_WATCHES round-trip differs from what bash's
            // own eval produces.
            //
            // Shipping the swap in that state would trade a working
            // integration for a leaking one, so the shell form stays until the
            // leave path is understood.
            chpwd_body: Some(r#"eval "$(direnv export bash 2>/dev/null)""#),
            env: &[],
            prompt_command: None,
            functions: &[],
        }),

        "starship" => Some(IntegrationRecipe {
            aliases: &[],
            precmd_body: None,
            preexec_body: None,
            chpwd_body: None,
            env: &[],
            // Delegates the entire prompt rendering to `starship
            // prompt`. frost-lisp's defprompt :command synthesizes a
            // precmd hook so the output lands in PS1 each prompt.
            prompt_command: Some(r#"starship prompt --status="$?""#),
            functions: &[],
        }),

        "atuin" => Some(IntegrationRecipe {
            aliases: &[
                ("h", "atuin search -i"),
                ("hist-stats", "atuin stats"),
                ("hist-import", "atuin import auto"),
            ],
            precmd_body: None,
            // Tell atuin when a command finishes so it records status/
            // duration. atuin has its own hook script normally; we use
            // the `end` command directly.
            preexec_body: None,
            chpwd_body: None,
            // Disable atuin's default keybinding takeover — frostmourne
            // binds C-r to the skim-history picker instead.
            env: &[("ATUIN_NOBIND", "true", true)],
            prompt_command: None,
            functions: &[],
        }),

        _ => None,
    }
}

/// All tool names the registry recognizes. Exposed for testing and
/// for a future `defintegration :list #t` debugging form.
pub const KNOWN_INTEGRATIONS: &[&str] = &["zoxide", "direnv", "starship", "atuin"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_integrations_resolve() {
        for tool in KNOWN_INTEGRATIONS {
            assert!(lookup_integration(tool).is_some(), "{tool} missing");
        }
    }

    #[test]
    fn unknown_tool_returns_none() {
        assert!(lookup_integration("not-a-real-tool").is_none());
    }

    #[test]
    fn starship_recipe_emits_prompt_command() {
        let r = lookup_integration("starship").unwrap();
        assert!(r.prompt_command.is_some());
        assert!(r.prompt_command.unwrap().contains("starship prompt"));
    }

    #[test]
    fn zoxide_recipe_has_chpwd_hook() {
        let r = lookup_integration("zoxide").unwrap();
        assert!(r.chpwd_body.is_some());
        assert!(r.chpwd_body.unwrap().contains("zoxide add"));
    }
}
