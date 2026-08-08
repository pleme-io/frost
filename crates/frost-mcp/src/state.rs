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

/// The pid a state-directory entry belongs to, or `None` if the name is not
/// one of ours. Recognises all three shapes a frost process leaves behind:
/// `mcp-<pid>.sock`, `state-<pid>.json`, `state-<pid>.json.tmp`.
#[must_use]
fn owning_pid(file_name: &str) -> Option<u32> {
    let body = file_name
        .strip_prefix("mcp-")
        .and_then(|s| s.strip_suffix(".sock"))
        .or_else(|| {
            file_name.strip_prefix("state-").and_then(|s| {
                s.strip_suffix(".json.tmp")
                    .or_else(|| s.strip_suffix(".json"))
            })
        })?;
    body.parse::<u32>().ok()
}

/// Remove the three files `pid` owns in `state_dir`. Missing files are not an
/// error — a `-c` invocation never creates any, and calling this on every
/// graceful exit is simpler than tracking which ones were made.
///
/// Safe against a live sibling by construction: pids are unique among running
/// processes, so `mcp-<our pid>.sock` cannot belong to another live shell.
pub fn remove_process_files(state_dir: &std::path::Path, pid: u32) {
    for name in [
        format!("mcp-{pid}.sock"),
        format!("state-{pid}.json"),
        format!("state-{pid}.json.tmp"),
    ] {
        let _ = std::fs::remove_file(state_dir.join(name));
    }
}

/// Delete every state-directory entry whose owning pid is gone, and report
/// how many were removed.
///
/// Frost bound `~/.local/state/frost/mcp-<pid>.sock` and wrote
/// `state-<pid>.json` on every interactive start and never removed either, so
/// the directory grew without bound — measured 2026-08-07 on this operator's
/// box: 346 entries, of which 301 were snapshots and 45 sockets, accumulating
/// since June. A crash or `kill -9` still leaves files behind (no exit path
/// runs), which is why this sweep exists in addition to the graceful-exit
/// teardown.
///
/// `is_alive` is injected rather than called directly so this crate stays free
/// of a libc dependency and the sweep is testable without spawning processes.
/// It must be **conservative**: anything it cannot prove dead must be reported
/// alive, since deleting a live shell's socket silently severs its MCP
/// channel. `self_pid` is never touched regardless of what `is_alive` says.
///
/// Known limit, stated rather than papered over: a pid the OS has recycled
/// onto an unrelated process reads as alive, so its stale files survive until
/// that process exits. Leaking a file is the correct side to err on.
pub fn reap_dead(
    state_dir: &std::path::Path,
    self_pid: u32,
    is_alive: &dyn Fn(u32) -> bool,
) -> usize {
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(pid) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(owning_pid)
        else {
            continue;
        };
        if pid == self_pid || is_alive(pid) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
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
pub fn discover_latest_snapshot(state_dir: &std::path::Path) -> Option<(u32, PathBuf)> {
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

    /// The state dir grew to 346 entries (301 snapshots + 45 sockets)
    /// because nothing ever removed a dead shell's files. The sweep must
    /// clear all three shapes for a dead pid, and touch nothing else.
    #[test]
    fn reap_removes_every_shape_for_a_dead_pid() {
        let dir = fresh_dir("reap-dead");
        for name in [
            "mcp-111.sock",
            "state-111.json",
            "state-111.json.tmp",
            "mcp-222.sock",
            "state-222.json",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        // Entries that are not ours must survive untouched.
        std::fs::write(dir.join("aaa-unrelated"), b"x").unwrap();
        std::fs::write(dir.join("state-notapid.json"), b"{}").unwrap();

        let removed = reap_dead(&dir, 999, &|_| false);
        assert_eq!(removed, 5, "all five entries for dead pids must go");
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left.len(),
            2,
            "only the non-frost entries survive: {left:?}"
        );
        assert!(left.contains(&"aaa-unrelated".to_string()), "{left:?}");
        assert!(left.contains(&"state-notapid.json".to_string()), "{left:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole hazard of a sweep: deleting a LIVE shell's socket severs
    /// its MCP channel. A live pid, and our own pid regardless of what the
    /// liveness predicate says, must both survive.
    #[test]
    fn reap_never_touches_a_live_shell_or_itself() {
        let dir = fresh_dir("reap-live");
        for name in [
            "mcp-111.sock",   // live
            "state-111.json", // live
            "mcp-555.sock",   // ours
            "state-555.json", // ours
            "mcp-777.sock",   // dead
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }

        // `is_alive` lies about our own pid to prove `self_pid` is checked
        // independently rather than relying on the predicate.
        let removed = reap_dead(&dir, 555, &|pid| pid == 111);
        assert_eq!(removed, 1, "only the dead pid's socket may be removed");
        assert!(dir.join("mcp-111.sock").exists(), "live socket deleted");
        assert!(dir.join("state-111.json").exists(), "live snapshot deleted");
        assert!(dir.join("mcp-555.sock").exists(), "own socket deleted");
        assert!(dir.join("state-555.json").exists(), "own snapshot deleted");
        assert!(!dir.join("mcp-777.sock").exists(), "dead socket survived");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Graceful-exit teardown: our three files go, everyone else's stay,
    /// and an absent file is not an error (a `-c` run creates none).
    #[test]
    fn remove_process_files_is_scoped_and_idempotent() {
        let dir = fresh_dir("remove-own");
        for name in [
            "mcp-42.sock",
            "state-42.json",
            "state-42.json.tmp",
            "mcp-43.sock",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        remove_process_files(&dir, 42);
        assert!(!dir.join("mcp-42.sock").exists());
        assert!(!dir.join("state-42.json").exists());
        assert!(!dir.join("state-42.json.tmp").exists());
        assert!(
            dir.join("mcp-43.sock").exists(),
            "another pid's file removed"
        );
        // Second call on the same (now absent) files must not panic.
        remove_process_files(&dir, 42);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reaped directory must still be discoverable — the sweep must not
    /// take the snapshot the bridge is about to read.
    #[test]
    fn reap_leaves_the_live_snapshot_discoverable() {
        let dir = fresh_dir("reap-discover");
        std::fs::write(dir.join("mcp-111.sock"), b"").unwrap();
        std::fs::write(dir.join("state-111.json"), b"{}").unwrap();
        FrostState::boot(456).write_snapshot(&dir).unwrap();

        reap_dead(&dir, 456, &|pid| pid == 456);
        assert_eq!(
            discover_latest_snapshot(&dir).map(|(pid, _)| pid),
            Some(456),
            "the live shell's snapshot must survive the sweep"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owning_pid_recognises_exactly_our_three_shapes() {
        assert_eq!(owning_pid("mcp-7.sock"), Some(7));
        assert_eq!(owning_pid("state-7.json"), Some(7));
        assert_eq!(owning_pid("state-7.json.tmp"), Some(7));
        assert_eq!(owning_pid("state-notapid.json"), None);
        assert_eq!(owning_pid("mcp-.sock"), None);
        assert_eq!(owning_pid("aaa-unrelated"), None);
        assert_eq!(owning_pid("mcp-7.sock.bak"), None);
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
