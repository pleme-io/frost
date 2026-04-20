//! `defbind` — Lisp-authored ZLE keybindings.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// One keybinding. `key` is a human-readable combo (`"C-x"`, `"M-up"`,
/// `"ctrl+r"`, `"tab"`). `action` is shell source that runs when the
/// combo fires. The applicator stores the body under
/// `__frost_bind_<KEY>` in `env.functions`; the interactive ZLE loop
/// consults that name when dispatching widgets.
///
/// ```lisp
/// (defbind :key "C-x e" :action "frost -c $EDITOR")
/// (defbind :key "M-?"   :action "help")
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defbind")]
pub struct BindSpec {
    pub key: String,
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
        assert!(!is_widget_action("__frost_widget_edit_line"));  // no trailing __
    }

    #[test]
    fn bind_function_name_strips_whitespace_and_uppercases() {
        assert_eq!(bind_function_name("C-l"), "__frost_bind_C-L");
        assert_eq!(bind_function_name("C-x e"), "__frost_bind_C-XE");
        assert_eq!(bind_function_name("M-?"), "__frost_bind_M-?");
    }
}
