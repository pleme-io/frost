//! Two MCP server shapes:
//!
//! 1. [`serve_uds`] — the per-shell UDS server. One instance lives
//!    inside each frostmourne process, bound to
//!    `~/.local/state/frost/mcp-${pid}.sock`. Holds live `SharedState`
//!    for direct connections (future M3 live-mutation path).
//!
//! 2. [`serve_stdio`] — the **bridge MCP server**. Spawned by Claude
//!    Code (or any other rmcp client) as `frost --mcp`. Reads the
//!    latest `state-*.json` snapshot file written by a running
//!    frostmourne and surfaces it via the same typed tools. Critically,
//!    `serve_stdio` ALWAYS starts cleanly — when no snapshot exists,
//!    tools return `{"running_shells": 0}` instead of crashing the MCP
//!    server.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use tokio::net::UnixListener;

use crate::state::{
    SharedState, default_state_dir, discover_latest_snapshot, load_snapshot,
};

/// The frost MCP server. One instance per connection; serves the same
/// four introspection tools regardless of source (UDS or stdio bridge).
///
/// `state` is `Some` for the per-shell UDS server (reads from live
/// `Arc<RwLock<>>`). For the stdio bridge it's `None` — tools fall back
/// to reading the latest `state-*.json` snapshot on each call.
#[derive(Debug, Clone)]
pub struct FrostMcp {
    tool_router: ToolRouter<Self>,
    state: Option<SharedState>,
}

#[tool_router]
impl FrostMcp {
    /// Per-shell UDS server constructor — `state` is the live
    /// `Arc<RwLock<>>` populated by frost main.
    #[must_use]
    pub fn with_live_state(state: SharedState) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state: Some(state),
        }
    }

    /// Stdio bridge constructor — no live state; tools read from
    /// snapshot files at call time.
    #[must_use]
    pub fn new_bridge() -> Self {
        Self {
            tool_router: Self::tool_router(),
            state: None,
        }
    }

    /// Resolve the current snapshot. Live state wins if present; else
    /// reads the latest JSON snapshot from disk.
    async fn resolve_state(&self) -> Option<crate::state::FrostState> {
        if let Some(live) = &self.state {
            return Some(live.read().await.clone());
        }
        let dir = default_state_dir()?;
        let (_pid, path) = discover_latest_snapshot(&dir)?;
        load_snapshot(&path).ok()
    }

    #[tool(
        description = "Get frost shell status — pid, uptime in seconds, rc path, whether rc loaded successfully. Equivalent to a live `frost --doctor` header. Returns JSON. If no frostmourne is running, returns {\"running_shells\": 0}."
    )]
    async fn frost_status(&self) -> String {
        let Some(st) = self.resolve_state().await else {
            return serde_json::json!({
                "running_shells": 0,
                "note": "No running frostmourne — open a window in mado/terminal to populate this surface.",
            })
            .to_string();
        };
        let uptime_secs = st
            .started_at
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .map(|d| d.as_secs());
        serde_json::json!({
            "status": "running",
            "app": "frost",
            "pid": st.pid,
            "uptime_secs": uptime_secs,
            "rc_path": st.rc_path,
            "rc_loaded": st.rc_loaded,
            "rc_error": st.rc_error,
            "history_file": st.history_file,
            "counts": {
                "bindings": st.bindings.len(),
                "pickers": st.pickers.len(),
                "widgets": st.widgets.len(),
                "aliases": st.alias_count,
                "subcmds": st.subcmd_count,
                "flags": st.flag_count,
                "positionals": st.positional_count,
                "abbreviations": st.abbreviation_count,
            },
        })
        .to_string()
    }

    #[tool(
        description = "List every reedline keybinding installed in the running frost shell. Each entry is {chord, action} where action is either a shell function name (`__frost_bind_*`), a widget sentinel (`__frost_widget_*`), or a picker sentinel (`__frost_picker_*`). Use to verify that an rc-authored `(defbind ...)` or `(defpicker ...)` actually reached reedline. Returns {\"running_shells\": 0} when no frostmourne is open."
    )]
    async fn frost_bindings(&self) -> String {
        let Some(st) = self.resolve_state().await else {
            return serde_json::json!({"running_shells": 0}).to_string();
        };
        let bindings: Vec<_> = st
            .bindings
            .iter()
            .map(|(chord, action)| {
                serde_json::json!({ "chord": chord, "action": action })
            })
            .collect();
        serde_json::json!({ "ok": true, "count": bindings.len(), "bindings": bindings })
            .to_string()
    }

    #[tool(
        description = "List every (defpicker …) form registered. Each entry has {name, key, binary, action}. Pickers are skim-tab integrations bound to a key chord — pressing the chord spawns the binary in the freed terminal. Use to verify picker registration + diagnose Ctrl-R / Ctrl-T / M-c / Ctrl-F behavior. Returns {\"running_shells\": 0} when no frostmourne is open."
    )]
    async fn frost_pickers(&self) -> String {
        let Some(st) = self.resolve_state().await else {
            return serde_json::json!({"running_shells": 0}).to_string();
        };
        let pickers: Vec<_> = st
            .pickers
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "key": p.key,
                    "binary": p.binary,
                    "action": p.action,
                })
            })
            .collect();
        serde_json::json!({ "ok": true, "count": pickers.len(), "pickers": pickers })
            .to_string()
    }

    #[tool(
        description = "Get the current HISTFILE path, its size in bytes, and whether it exists. Skim-history reads this file directly, so divergence between rc-configured path and this live path is a common Ctrl-R failure mode. Returns JSON; falls back to env-var-derived path when no frostmourne is open."
    )]
    async fn frost_history_path(&self) -> String {
        let st = self.resolve_state().await;
        let env_path = std::env::var("HISTFILE").ok();
        let rc_path = st.as_ref().and_then(|s| s.history_file.clone());
        let resolved: Option<PathBuf> = rc_path
            .clone()
            .or_else(|| env_path.clone().map(PathBuf::from));
        let (size_bytes, exists) = resolved
            .as_ref()
            .map(|p| {
                let meta = std::fs::metadata(p).ok();
                (
                    meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    meta.is_some(),
                )
            })
            .unwrap_or((0, false));
        serde_json::json!({
            "ok": true,
            "running_shells": if st.is_some() { 1 } else { 0 },
            "rc_history_file": rc_path,
            "env_HISTFILE": env_path,
            "resolved": resolved,
            "exists": exists,
            "size_bytes": size_bytes,
        })
        .to_string()
    }
}

#[tool_handler]
impl ServerHandler for FrostMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "frost — live shell introspection. Tools surface the running shell's rc-load state, keybindings, pickers, and history-file resolution. If no frostmourne is open, tools return {\"running_shells\": 0} instead of failing — open a window in mado/terminal to populate the snapshot."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

/// Bind a `UnixListener` at `socket_path`, accept connections forever,
/// and serve one `FrostMcp` per connection. The socket is removed
/// on startup (in case a prior crash left it behind).
///
/// # Errors
///
/// Returns the underlying io::Error if the socket can't be bound.
/// Returning Err means MCP is disabled for this frost process;
/// frost itself keeps running.
pub async fn serve_uds(socket_path: PathBuf, state: SharedState) -> std::io::Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!(socket = %socket_path.display(), "frost-mcp UDS listening");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "frost-mcp accept failed; continuing");
                continue;
            }
        };
        let mcp = FrostMcp::with_live_state(Arc::clone(&state));
        tokio::spawn(async move {
            let (reader, writer) = stream.into_split();
            match mcp.serve((reader, writer)).await {
                Ok(server) => {
                    if let Err(e) = server.waiting().await {
                        tracing::debug!(error = %e, "frost-mcp UDS client session ended");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "frost-mcp UDS serve failed"),
            }
        });
    }
}

/// Run the **bridge MCP server** over stdio. Called by `frost --mcp`.
/// Always starts cleanly; tools fall back to `{"running_shells": 0}`
/// when no `state-*.json` snapshot exists.
///
/// # Errors
///
/// Returns an io::Error wrapping any rmcp transport failure.
pub async fn serve_stdio() -> std::io::Result<()> {
    let mcp = FrostMcp::new_bridge();
    let server = mcp
        .serve(rmcp::transport::stdio())
        .await
        .map_err(std::io::Error::other)?;
    server.waiting().await.map_err(std::io::Error::other)?;
    Ok(())
}

/// Remove the socket file at shutdown so a re-launched frost gets a
/// clean bind() on the same path. Idempotent; missing file is fine.
#[allow(dead_code)]
pub fn cleanup_socket(socket_path: &Path) {
    let _ = std::fs::remove_file(socket_path);
}
