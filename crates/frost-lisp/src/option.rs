//! `defopts` — batch enable/disable of shell options.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// One `(defopts …)` form expresses "turn these on, turn these off" as
/// a single transaction. Each name goes through
/// [`frost_options::ShellOption::from_name`] which accepts both zsh-style
/// short names (`"globdots"`) and no-prefixed forms (`"nobeep"`).
///
/// ```lisp
/// (defopts :enable ("extendedglob" "globdots" "promptsubst")
///          :disable ("beep" "nomatch"))
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defopts")]
pub struct OptionSetSpec {
    #[serde(default)]
    pub enable: Vec<String>,
    #[serde(default)]
    pub disable: Vec<String>,
}
