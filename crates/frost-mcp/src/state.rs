//! Shared state visible to every MCP tool.
//!
//! Populated by frost main during startup (rc-load summary) + updated
//! by the REPL loop (last command, last keystroke). MCP tools read
//! through one of two channels:
//!
//! 1. **JSON snapshot file** at `~/.local/state/frost/state-${pid}.json`
//!    — written on rc-load and on every REPL iteration. This is the
//!    canonical M1 IPC: simple, atomic, debuggable, survives process
//!    crashes long enough for diagnostics. The bridge subcommand
//!    (`frost --mcp`) reads these files and serves the MCP tools
//!    over stdio.
//!
//! 2. **In-process `SharedState`** held by the running shell — used
//!    by an optional UDS server (kept for future live mutation paths)
//!    and by the snapshot writer.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Convenience alias for the `Arc<RwLock<FrostState>>` shape every
/// caller threads through.
pub type SharedState = Arc<RwLock<FrostState>>;

/// Snapshot of the live frost shell. Every field is what an MCP tool
/// would want to surface to the caller. `Deserialize` is derived so
/// the bridge subcommand can read snapshots written by running shells.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FrostState {
    /// PID of the running frost process.
    pub pid: u32,
    /// UTC wall-clock time the shell process started. Tools render
    /// `uptime_secs` from `now - started_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<SystemTime>,
    /// Absolute path of the rc file that was loaded (`FROSTRC` value
    /// or the default `~/.frostrc.lisp`). `None` if no rc was loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rc_path: Option<PathBuf>,
    /// rc load was successful.
    pub rc_loaded: bool,
    /// First-line error message if the rc failed to load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rc_error: Option<String>,
    /// Final reedline keybindings — `(chord, fn_name)` pairs. Includes
    /// every `defbind` and every `defpicker` (each picker pushes one
    /// `(key, sentinel)` entry).
    #[serde(default)]
    pub bindings: Vec<(String, String)>,
    /// All `(defpicker …)` forms registered. The MCP `frost_pickers`
    /// tool returns this verbatim.
    #[serde(default)]
    pub pickers: Vec<PickerInfo>,
    /// All widgets registered via `(defbind :action "__frost_widget_*")`.
    #[serde(default)]
    pub widgets: Vec<String>,
    /// Current `HISTFILE` value (`None` if `defhistory` never ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Write a JSON snapshot of this state to
    /// `<state_dir>/state-<pid>.json`. Returns the path written.
    /// Atomic via write-temp + rename so concurrent readers never see
    /// a partial file.
    ///
    /// # Errors
    ///
    /// Returns io::Error if the directory can't be created, the
    /// temp file can't be written, or rename fails.
    pub fn write_snapshot(&self, state_dir: &std::path::Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(state_dir)?;
        let path = state_dir.join(format!("state-{}.json", self.pid));
        let tmp = state_dir.join(format!("state-{}.json.tmp", self.pid));
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
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

/// Canonical state directory: `~/.local/state/frost`. Both snapshots
/// and UDS sockets live here.
#[must_use]
pub fn default_state_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push(".local/state/frost");
    Some(p)
}

/// Find the most-recently-modified `state-*.json` snapshot under
/// `state_dir`. Returns `(pid, path)` or `None` if none exist.
#[must_use]
pub fn discover_latest_snapshot(
    state_dir: &std::path::Path,
) -> Option<(u32, PathBuf)> {
    let entries = std::fs::read_dir(state_dir).ok()?;
    let mut best: Option<(SystemTime, u32, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_str()?.to_string();
        let pid_str = name.strip_prefix("state-")?.strip_suffix(".json")?;
        let pid: u32 = pid_str.parse().ok()?;
        let mtime = entry.metadata().ok()?.modified().ok()?;
        match &best {
            None => best = Some((mtime, pid, path)),
            Some((t, _, _)) if mtime > *t => best = Some((mtime, pid, path)),
            _ => {}
        }
    }
    best.map(|(_, pid, path)| (pid, path))
}

/// Load a snapshot from a JSON file. Returns the parsed `FrostState`
/// or an io/json error.
///
/// # Errors
///
/// Returns io::Error for missing/unreadable files, or a wrapped
/// serde_json error if the file isn't valid JSON.
pub fn load_snapshot(path: &std::path::Path) -> std::io::Result<FrostState> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice::<FrostState>(&bytes).map_err(std::io::Error::other)
}
