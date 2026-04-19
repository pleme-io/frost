//! `defsource` — inline-load another Lisp file at rc-apply time.
//!
//! Large rc files get unwieldy fast, especially when auto-generated
//! completion trees (649+ `(defsubcmd …)` forms from the skim-tab
//! YAMLs, or equivalent per-tool exports) live alongside hand-tuned
//! bindings and prompts. `defsource` splits them out:
//!
//! ```lisp
//! ;; ~/.frostrc.lisp
//! (defalias :name "ll" :value "ls -la")
//! (defsource :path "~/.config/frost/rc.d/kubectl.lisp")
//! (defsource :path "${XDG_CONFIG_HOME:-$HOME/.config}/frost/rc.d/git.lisp")
//! ```
//!
//! Semantics:
//!
//! * The sourced file's `(def*)` forms run in the SAME apply pass as
//!   the outer rc — hook composition, alias-last-writer-wins, and
//!   summary accounting all work the same way as if the forms were
//!   inlined.
//! * Relative paths resolve against the sourced file's directory
//!   (not cwd) — stable across where `frost` is launched from.
//! * `$VAR` / `${VAR}` expansion in the path honors the current env,
//!   with `HOME` + `XDG_CONFIG_HOME` falling back to `std::env::var`.
//! * Missing file is **an error** (not a silent no-op) — a typo in a
//!   sourced path shouldn't degrade silently.
//! * Circular sourcing is guarded: the apply pass tracks visited
//!   canonical paths and skips re-sourcing.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defsource")]
pub struct SourceSpec {
    /// Path to another Lisp file. `~` + `$VAR` expanded at apply time.
    pub path: String,
}
