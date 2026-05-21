//! UDS-transported MCP server. Accepts on
//! `~/.local/state/frost/mcp-${pid}.sock`; spawns one rmcp server per
//! client connection.
//!
//! rmcp 0.15's `transport-io` consumes any `AsyncRead + AsyncWrite`
//! pair, so a `tokio::net::UnixStream` is dropped in directly — no
//! custom transport needed.

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

use crate::state::SharedState;

/// The frost MCP server. One instance per UDS connection — every tool
/// reads through the shared `state`.
#[derive(Debug, Clone)]
pub struct FrostMcp {
    tool_router: ToolRouter<Self>,
    state: SharedState,
}

#[tool_router]
impl FrostMcp {
    /// Construct with an externally-owned shared-state handle. Every
    /// connection clones the `Arc<RwLock<_>>` so reads see the
    /// latest writes done by the REPL loop.
    #[must_use]
    pub fn new(state: SharedState) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state,
        }
    }

    #[tool(
        description = "Get frost shell status — pid, uptime in seconds, rc path, whether rc loaded successfully. Equivalent to a live `frost --doctor` header. Returns JSON."
    )]
    async fn frost_status(&self) -> String {
        let st = self.state.read().await;
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
        description = "List every reedline keybinding installed in the running frost shell. Each entry is {chord, action} where action is either a shell function name (`__frost_bind_*`), a widget sentinel (`__frost_widget_*`), or a picker sentinel (`__frost_picker_*`). Use to verify that an rc-authored `(defbind ...)` or `(defpicker ...)` actually reached reedline."
    )]
    async fn frost_bindings(&self) -> String {
        let st = self.state.read().await;
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
        description = "List every (defpicker …) form registered. Each entry has {name, key, binary, action}. Pickers are skim-tab integrations bound to a key chord — pressing the chord spawns the binary in the freed terminal. Use to verify picker registration + diagnose Ctrl-R / Ctrl-T / M-c / Ctrl-F behavior."
    )]
    async fn frost_pickers(&self) -> String {
        let st = self.state.read().await;
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
        description = "Get the current HISTFILE path, its size in bytes, and whether it exists. Skim-history reads this file directly, so divergence between rc-configured path and this live path is a common Ctrl-R failure mode. Returns JSON."
    )]
    async fn frost_history_path(&self) -> String {
        let st = self.state.read().await;
        let path: Option<&Path> = st.history_file.as_deref();
        let env_path = std::env::var("HISTFILE").ok();
        let resolved: Option<PathBuf> =
            path.map(Path::to_path_buf).or_else(|| env_path.clone().map(PathBuf::from));
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
            "rc_history_file": st.history_file,
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
                "frost — live shell introspection. Tools surface the running shell's rc-load state, keybindings, pickers, and history-file resolution. Read-only in M1; mutation lands in M3."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

/// Bind a `UnixListener` at `socket_path`, accept connections forever,
/// and serve one `FrostMcp` per connection. The socket is removed
/// on startup (in case a prior crash left it behind) and on graceful
/// shutdown via [`cleanup_socket`].
///
/// # Errors
///
/// Returns the underlying io::Error if the socket can't be bound
/// (e.g. directory doesn't exist + can't be created, permission
/// denied). Returning Err means MCP is disabled for this frost
/// process; frost itself keeps running.
pub async fn serve_uds(socket_path: PathBuf, state: SharedState) -> std::io::Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A leftover socket from a prior crash makes bind() fail with
    // EADDRINUSE. Best-effort remove; if it's a live socket someone
    // else owns, the bind itself will fail with a clearer error.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!(socket = %socket_path.display(), "frost-mcp listening");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "frost-mcp accept failed; continuing");
                continue;
            }
        };
        let mcp = FrostMcp::new(Arc::clone(&state));
        tokio::spawn(async move {
            // Split UnixStream into read/write halves; rmcp 0.15's
            // transport-io accepts the (R, W) tuple.
            let (reader, writer) = stream.into_split();
            match mcp.serve((reader, writer)).await {
                Ok(server) => {
                    if let Err(e) = server.waiting().await {
                        tracing::debug!(error = %e, "frost-mcp client session ended");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "frost-mcp serve failed"),
            }
        });
    }
}

/// Remove the socket file at shutdown so a re-launched frost gets a
/// clean bind() on the same path. Idempotent; missing file is fine.
/// Public so frost main can call it from a trap handler when the
/// REPL exits — without this a clean exit would leave a stale
/// socket that the next frost on the same PID can't bind over.
#[allow(dead_code)]
pub fn cleanup_socket(socket_path: &Path) {
    let _ = std::fs::remove_file(socket_path);
}
