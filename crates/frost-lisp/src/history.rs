//! `defhistory` — one-form history configuration.
//!
//! History knobs ordinarily land via `(defenv …)` + `(defopts …)`:
//!
//! ```lisp
//! (defenv :name "HISTFILE"   :value "$HOME/.frost_history" :export #t)
//! (defenv :name "HISTSIZE"   :value "50000"                :export #t)
//! (defenv :name "HISTIGNORE" :value "ls:pwd:exit"          :export #t)
//! (defopts :enable ("histignoredups" "histignorespace"))
//! ```
//!
//! `defhistory` collapses the pattern:
//!
//! ```lisp
//! (defhistory :file "$HOME/.frost_history"
//!             :size 50000
//!             :ignore ("ls" "pwd" "exit")
//!             :ignore-dups #t
//!             :ignore-space #t
//!             :extended #t)
//! ```
//!
//! Fields:
//!
//! | Field          | Env / opt                       |
//! |----------------|---------------------------------|
//! | `:file`        | `HISTFILE` (exported)           |
//! | `:size`        | `HISTSIZE` (exported)           |
//! | `:save-size`   | `SAVEHIST` / `HISTFILESIZE`     |
//! | `:ignore`      | `HISTIGNORE` (`:`-joined)       |
//! | `:ignore-dups` | setopt `histignoredups`         |
//! | `:ignore-space`| setopt `histignorespace`        |
//! | `:extended`    | setopt `extendedhistory`        |
//! | `:share`       | setopt `sharehistory`           |
//!
//! Tilde + `$VAR` expansion runs on `:file` at apply time so the
//! env var holds the absolute path. Unset fields leave the
//! corresponding env var / option untouched.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defhistory")]
pub struct HistorySpec {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub save_size: Option<u64>,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub ignore_dups: Option<bool>,
    #[serde(default)]
    pub ignore_space: Option<bool>,
    #[serde(default)]
    pub extended: Option<bool>,
    #[serde(default)]
    pub share: Option<bool>,
}
