//! `deftrap` — Lisp-authored signal handler.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// Signal name (`INT`, `TERM`, `HUP`, `USR1`, …) or a zsh pseudo-signal
/// (`EXIT`, `DEBUG`, `ERR`, `ZERR`). Body is shell source; the
/// applicator stores it under `env.traps` so it fires when the
/// executor delivers the corresponding signal.
///
/// ```lisp
/// (deftrap :signal "INT"  :body "echo interrupted; return 130")
/// (deftrap :signal "EXIT" :body "echo goodbye")
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "deftrap")]
pub struct TrapSpec {
    pub signal: String,
    pub body: String,
}
