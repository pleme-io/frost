//! Shared state visible to every MCP tool.
//!
//! Populated by frost main during startup (rc-load summary) + updated
//! by the REPL loop (last command, last keystroke). MCP tools read
//! through the `Arc<RwLock<>>` — writes happen behind frost main's own
//! sync boundaries, never inside the tokio runtime.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use serde::Serialize;
use tokio::sync::RwLock;

/// Convenience alias for the `Arc<RwLock<FrostState>>` shape every
/// caller threads through.
pub type SharedState = Arc<RwLock<FrostState>>;

/// Snapshot of the live frost shell. Every field is what an MCP tool
/// would want to surface to the caller. Keep the struct flat — nested
/// types here become MCP tool response shapes.
#[derive(Debug, Default, Clone, Serialize)]
pub struct FrostState {
    /// PID of the running frost process.
    pub pid: u32,
    /// UTC wall-clock time the shell process started. Tools render
    /// `uptime_secs` from `now - started_at`.
    pub started_at: Option<SystemTime>,
    /// Absolute path of the rc file that was loaded (`FROSTRC` value
    /// or the default `~/.frostrc.lisp`). `None` if no rc was loaded.
    pub rc_path: Option<PathBuf>,
    /// rc load was successful.
    pub rc_loaded: bool,
    /// First-line error message if the rc failed to load.
    pub rc_error: Option<String>,
    /// Final reedline keybindings — `(chord, fn_name)` pairs. Includes
    /// every `defbind` and every `defpicker` (each picker pushes one
    /// `(key, sentinel)` entry).
    pub bindings: Vec<(String, String)>,
    /// All `(defpicker …)` forms registered. The MCP `frost_pickers`
    /// tool returns this verbatim.
    pub pickers: Vec<PickerInfo>,
    /// All widgets registered via `(defbind :action "__frost_widget_*")`.
    pub widgets: Vec<String>,
    /// Current `HISTFILE` value (`None` if `defhistory` never ran).
    pub history_file: Option<PathBuf>,
    /// Total alias count (cheap surface check; full alias map dumps
    /// would land in M2's expanded tool set).
    pub alias_count: usize,
    /// Subcmd / flag / posit / abbreviation counts so an operator can
    /// see at a glance how rich the completion surface is.
    pub subcmd_count: usize,
    pub flag_count: usize,
    pub positional_count: usize,
    pub abbreviation_count: usize,
}

/// MCP-facing description of one picker. Mirrors `frost_lisp::PickerSpec`
/// but lives here so consumers don't need to depend on frost-lisp just
/// to use the MCP client.
#[derive(Debug, Clone, Serialize)]
pub struct PickerInfo {
    pub name: String,
    pub key: String,
    pub binary: String,
    pub action: String,
}

impl FrostState {
    /// Construct an empty state with `pid` + `started_at` pre-filled.
    /// Every other field is `Default::default()` until the rc loads.
    #[must_use]
    pub fn boot(pid: u32) -> Self {
        Self {
            pid,
            started_at: Some(SystemTime::now()),
            ..Default::default()
        }
    }
}

/// Per-pid socket path. Returns `~/.local/state/frost/mcp-<pid>.sock`.
/// `None` if `$HOME` is unset (impossible on any reasonable host but
/// callers should handle gracefully — frost still starts without
/// MCP if this fails).
#[must_use]
pub fn default_socket_path(pid: u32) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push(".local/state/frost");
    p.push(format!("mcp-{pid}.sock"));
    Some(p)
}
