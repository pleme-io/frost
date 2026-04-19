//! `defpicker` — declarative skim-tab picker registration.
//!
//! A picker is a terminal-takeover widget (history, files, cd, content,
//! …) that the REPL spawns when a bound key is pressed, then splices
//! the selection back into the edit buffer via reedline's
//! `run_edit_commands`. `(defpicker …)` encodes the full convention —
//! key chord, binary to spawn, how to consume the result — in one form.
//!
//! ```lisp
//! (defpicker :name "history" :key "C-r" :binary "skim-history" :action "replace")
//! (defpicker :name "files"   :key "C-t" :binary "skim-files"   :action "append")
//! (defpicker :name "cd"      :key "M-c" :binary "skim-cd"      :action "cd-submit")
//! (defpicker :name "content" :key "C-f" :binary "skim-content" :action "submit")
//! ```
//!
//! Each form:
//!
//! 1. **Registers a keybinding** so reedline returns the sentinel
//!    `__frost_picker_<name>__` verbatim on key press — no wrapper
//!    function, so the REPL's sentinel dispatcher sees the sentinel
//!    directly and can intercept it before parse/exec.
//! 2. **Records the spec** in `ApplySummary::pickers` so the REPL can
//!    build its dispatch table. The binary name stays in Lisp where the
//!    user authored it; frost itself has no hardcoded knowledge of
//!    which pickers exist.
//!
//! Valid `action` values:
//!
//! | Action      | Semantics |
//! |-------------|-----------|
//! | `replace`   | selection replaces the edit buffer; user reviews + Enter |
//! | `append`    | selection appends to the buffer (file-picker UX) |
//! | `cd-submit` | buffer becomes `cd <selection>` and auto-submits |
//! | `submit`    | selection becomes the command verbatim and auto-submits |

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defpicker")]
pub struct PickerSpec {
    /// Short widget name (`"history"`). Becomes the sentinel
    /// `__frost_picker_<name>__`. Must be a valid shell word (no
    /// whitespace / metachars) so the round-trip through reedline's
    /// ExecuteHostCommand is clean.
    pub name: String,
    /// Key chord (`"C-r"`, `"M-c"`, `"ctrl+t"`). Parsed by
    /// `frost-zle::parse_chord`; unrecognized chords are dropped with a
    /// warning at bind time, not a hard error.
    pub key: String,
    /// Binary to spawn (`"skim-history"`). Must resolve via `$PATH`.
    /// Missing binary at run-time = no-op (picker returns `Nothing`).
    pub binary: String,
    /// How the REPL handles the selection. See module docs.
    pub action: String,
}

/// Canonical sentinel string for a picker named `name`. The REPL
/// compares incoming `ExecuteHostCommand` payloads against this.
pub fn picker_sentinel(name: &str) -> String {
    format!("__frost_picker_{name}__")
}

/// Accepted `action` strings. Used to validate at rc-load.
pub const VALID_ACTIONS: &[&str] = &["replace", "append", "cd-submit", "submit"];

/// True when `action` is one of [`VALID_ACTIONS`].
pub fn is_valid_action(action: &str) -> bool {
    VALID_ACTIONS.iter().any(|a| *a == action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_format_is_stable() {
        assert_eq!(picker_sentinel("history"), "__frost_picker_history__");
        assert_eq!(picker_sentinel("files"),   "__frost_picker_files__");
    }

    #[test]
    fn valid_actions_known() {
        assert!(is_valid_action("replace"));
        assert!(is_valid_action("append"));
        assert!(is_valid_action("cd-submit"));
        assert!(is_valid_action("submit"));
        assert!(!is_valid_action("Replace"));
        assert!(!is_valid_action("submit-it"));
    }
}
