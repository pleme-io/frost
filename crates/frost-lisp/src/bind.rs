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
