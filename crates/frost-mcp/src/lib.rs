//! frost-mcp — live MCP introspection + control surface for a running frost shell.
//!
//! Every long-lived frost process opens a Unix domain socket at
//! `~/.local/state/frost/mcp-${pid}.sock`. MCP clients (claude-code,
//! kaname, ad-hoc rmcp clients) connect over UDS and call typed tools
//! that read from [`FrostState`] — a shared snapshot of the rc-load
//! result + per-iteration REPL telemetry.
//!
//! Mirrors mado's in-process MCP server. The compounding move: every
//! pleme-io operator-facing tool grows the SAME MCP shape so an
//! operator (or an agent) can introspect every running tool the same
//! way without learning per-tool RPC.
//!
//! M1 scope: introspection tools only (status, bindings, pickers,
//! history_path). M2 adds last-event diagnostics; M3 adds live config
//! mutation; M4 adds skim probes.

mod server;
mod state;

pub use server::{serve_stdio, serve_uds, FrostMcp};
pub use state::{
    default_socket_path, default_state_dir, discover_latest_snapshot, load_snapshot, reap_dead,
    remove_process_files, FrostState, PickerInfo, SharedState,
};
