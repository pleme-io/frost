//! `defbind` — Lisp-authored ZLE keybindings.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// One keybinding. `key` is a human-readable combo (`"C-x"`, `"M-up"`,
/// `"ctrl+r"`, `"tab"`). `action` is shell source that runs when the
/// combo fires. The applicator stores the body under
/// `__frost_bind_<KEY>` in `env.functions`; the interactive ZLE loop
/// consults that name when dispatching widgets.
///
/// Two authoring forms:
///
/// ```lisp
/// ;; Intent form — chord sourced from FleetKeybinds atlas. Preferred
/// ;; for any chord that is fleet-canonical (history picker, clear
/// ;; buffer, multiplexer prefix, …).
/// (defbind :intent :clear-buffer :action "__frost_widget_clear__")
///
/// ;; Key form — hard-coded chord, kept for app-specific bindings
/// ;; not covered by the atlas.
/// (defbind :key "C-x e" :action "frost -c $EDITOR")
/// ```
///
/// Resolution rules at apply time:
///
/// * `intent` set, `key` empty: atlas chord wins.
/// * `intent` set, `key` set: atlas chord wins; `key` is logged + ignored
///   (the typed source beats the literal).
/// * `intent` empty, `key` set: `key` wins (legacy form).
/// * Both empty: form is dropped with a warning.
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defbind")]
pub struct BindSpec {
    /// Hard-coded chord. Either this or [`Self::intent`] must be set.
    /// Empty string when intent is supplied.
    #[serde(default)]
    pub key: String,
    /// Operator-intent keyword sourced from `ishou_tokens::FleetKeybinds`
    /// (e.g. `":history-picker"`). When set, the chord is resolved
    /// through the atlas at apply time and stored back into `key`.
    /// Empty string when key is supplied directly.
    #[serde(default)]
    pub intent: String,
    /// Shell source (or sentinel like `__frost_widget_clear__`) that
    /// runs when the chord fires.
    pub action: String,
}

/// Canonicalize a key name for function-table storage: uppercase, no
/// whitespace, `+` and `-` kept. Purely cosmetic — what matters is that
/// the author and the dispatcher agree on the same string.
pub fn bind_function_name(key: &str) -> String {
    format!(
        "__frost_bind_{}",
        key.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase()
    )
}

/// True when `action` is a widget sentinel (`__frost_widget_<name>__`) —
/// the REPL's widget dispatcher pattern-matches on this prefix. Used
/// by apply_source to route defbinds with widget actions directly
/// into bind_map instead of wrapping them in a shell function.
pub fn is_widget_action(action: &str) -> bool {
    action.starts_with("__frost_widget_") && action.ends_with("__")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_action_detector() {
        assert!(is_widget_action("__frost_widget_edit_line__"));
        assert!(is_widget_action("__frost_widget_clear__"));
        assert!(!is_widget_action("__frost_picker_history__"));
        assert!(!is_widget_action("echo hi"));
        assert!(!is_widget_action(""));
        assert!(!is_widget_action("__frost_widget_edit_line")); // no trailing __
    }

    #[test]
    fn bind_function_name_strips_whitespace_and_uppercases() {
        assert_eq!(bind_function_name("C-l"), "__frost_bind_C-L");
        assert_eq!(bind_function_name("C-x e"), "__frost_bind_C-XE");
        assert_eq!(bind_function_name("M-?"), "__frost_bind_M-?");
    }
}
