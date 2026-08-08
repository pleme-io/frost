//! `FrostShellState` — the aggregator the kanshou server exposes
//! for a running frostmourne shell.
//!
//! Hand-implemented [`Introspect`] over the shell's static atomics +
//! lock-protected state. The MCP server (`frost --mcp`) plus any
//! external operator tool can connect to the kanshou socket and read
//! the live posture: what rc files loaded, what the last prompt
//! computed, whether the shell is currently waiting for a VT
//! response from its terminal emulator, what command is executing.
//!
//! Tonight's diagnostic value: when frost is "blank" inside mado,
//! `kanshou query frost.<pid>.pending_vt_responses` answers "I sent
//! a DSR query and mado never wrote back" without any archaeology.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kanshou::{Introspect, Query, QueryError, QueryResult};

/// Live aggregator. Each leaf is one match-arm; add a new query
/// surface by extending the arms below + the schema array. Hand-
/// implemented because the leaves are static atomics + accessor
/// closures the derive macro doesn't cover.
pub struct FrostShellState {
    pub rc_loaded: Arc<AtomicBool>,
    pub rc_path: Arc<parking_lot::RwLock<Option<String>>>,
    pub started_at_unix_ms: u64,
    pub commands_executed: Arc<AtomicU64>,
    pub current_command: Arc<parking_lot::RwLock<Option<String>>>,
    pub last_prompt_render_us: Arc<AtomicU64>,
    pub pending_vt_responses: Arc<AtomicU64>,
    pub history_path: Arc<parking_lot::RwLock<Option<String>>>,
}

impl FrostShellState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rc_loaded: Arc::new(AtomicBool::new(false)),
            rc_path: Arc::new(parking_lot::RwLock::new(None)),
            started_at_unix_ms: now_unix_ms(),
            commands_executed: Arc::new(AtomicU64::new(0)),
            current_command: Arc::new(parking_lot::RwLock::new(None)),
            last_prompt_render_us: Arc::new(AtomicU64::new(0)),
            pending_vt_responses: Arc::new(AtomicU64::new(0)),
            history_path: Arc::new(parking_lot::RwLock::new(None)),
        }
    }
}

impl Default for FrostShellState {
    fn default() -> Self {
        Self::new()
    }
}

impl Introspect for FrostShellState {
    fn query(&self, q: &Query) -> QueryResult {
        let Some(first) = q.path.first().map(String::as_str) else {
            return Err(QueryError::unknown_field(String::new()));
        };
        match first {
            "rc" => Ok(serde_json::json!({
                "loaded": self.rc_loaded.load(Ordering::Relaxed),
                "path": self.rc_path.read().as_deref(),
            })),
            "current_command" => Ok(serde_json::json!({
                "command": self.current_command.read().as_deref(),
                "commands_executed": self.commands_executed.load(Ordering::Relaxed),
            })),
            "prompt" => Ok(serde_json::json!({
                "last_render_us": self.last_prompt_render_us.load(Ordering::Relaxed),
            })),
            "vt" => Ok(serde_json::json!({
                "pending_responses": self.pending_vt_responses.load(Ordering::Relaxed),
            })),
            "history" => Ok(serde_json::json!({
                "path": self.history_path.read().as_deref(),
            })),
            "process" => Ok(serde_json::json!({
                "pid": std::process::id(),
                "binary": std::env::current_exe()
                    .ok()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                "started_at_unix_ms": self.started_at_unix_ms,
                "uptime_ms": now_unix_ms().saturating_sub(self.started_at_unix_ms),
                "version": env!("CARGO_PKG_VERSION"),
            })),
            other => Err(QueryError::unknown_field(other.to_string())),
        }
    }

    fn schema(&self) -> &'static [&'static str] {
        &[
            "rc",
            "current_command",
            "prompt",
            "vt",
            "history",
            "process",
        ]
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Spawn the kanshou server in a tokio task. Returns the path the
/// server bound to. Best-effort: bind failure is non-fatal — the
/// shell runs without introspection and the operator sees a tracing
/// warn-level log explaining why.
pub fn spawn_server(
    app_name: &str,
    state: Arc<FrostShellState>,
) -> std::io::Result<std::path::PathBuf> {
    let server = kanshou::Server::new(app_name, state)?;
    let socket_path = server.socket_path().to_path_buf();
    tokio::spawn(async move {
        if let Err(e) = server.serve().await {
            tracing::warn!(error = ?e, "frost kanshou server exited with error");
        }
    });
    Ok(socket_path)
}
