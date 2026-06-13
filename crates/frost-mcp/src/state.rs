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
///
/// Non-snapshot entries are SKIPPED, never fatal. The state dir is
/// shared with the per-pid UDS sockets (`mcp-<pid>.sock`) and the
/// atomic-write temp files (`state-<pid>.json.tmp`); the original
/// implementation used `?` inside the per-entry loop, so whichever
/// of those `read_dir` happened to enumerate first aborted the whole
/// discovery — the 2026-06-11 "running_shells: 0 while a live frost
/// exists" gap.
#[must_use]
pub fn discover_latest_snapshot(
    state_dir: &std::path::Path,
) -> Option<(u32, PathBuf)> {
    let entries = std::fs::read_dir(state_dir).ok()?;
    let mut best: Option<(SystemTime, u32, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(pid) = name
            .strip_prefix("state-")
            .and_then(|s| s.strip_suffix(".json"))
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(mtime) = entry.metadata().and_then(|m| m.modified()).ok() else {
            continue;
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("frost-mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discovery_skips_non_snapshot_entries() {
        // 2026-06-11 gap: the state dir also holds `mcp-<pid>.sock`
        // and `state-<pid>.json.tmp`; a `?` inside the scan loop made
        // the first such entry abort discovery entirely, so frost MCP
        // reported running_shells:0 while a live shell's snapshot sat
        // right next to its socket. Decoys created FIRST so insertion
        // order enumerates them before the snapshot.
        let dir = fresh_dir("discover-decoys");
        std::fs::write(dir.join("aaa-unrelated"), b"x").unwrap();
        std::fs::write(dir.join("mcp-123.sock"), b"").unwrap();
        std::fs::write(dir.join("state-77.json.tmp"), b"{").unwrap();
        std::fs::write(dir.join("state-notapid.json"), b"{}").unwrap();
        let snapshot = FrostState::boot(456);
        snapshot.write_snapshot(&dir).unwrap();

        let found = discover_latest_snapshot(&dir);
        assert_eq!(
            found.as_ref().map(|(pid, _)| *pid),
            Some(456),
            "discovery must skip sockets/tmp/garbage and find the snapshot, got {found:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_picks_newest_snapshot() {
        let dir = fresh_dir("discover-newest");
        FrostState::boot(100).write_snapshot(&dir).unwrap();
        // mtime resolution on APFS/ext4 is ≥1ns but clock granularity
        // can be coarse — sleep past it.
        std::thread::sleep(std::time::Duration::from_millis(20));
        FrostState::boot(200).write_snapshot(&dir).unwrap();

        let found = discover_latest_snapshot(&dir).map(|(pid, _)| pid);
        assert_eq!(found, Some(200), "newest snapshot must win");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_round_trips_through_load() {
        let dir = fresh_dir("roundtrip");
        let mut st = FrostState::boot(4242);
        st.rc_loaded = true;
        st.alias_count = 7;
        let path = st.write_snapshot(&dir).unwrap();
        let loaded = load_snapshot(&path).unwrap();
        assert_eq!(loaded.pid, 4242);
        assert!(loaded.rc_loaded);
        assert_eq!(loaded.alias_count, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
