//! Runtime trace parsing.
//!
//! A trace is a JSONL file (one JSON object per line) written during
//! shell startup by the tracing wrapper around `source`/`.`.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// A single trace entry — one `source` call recorded at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    pub seq: u32,
    #[serde(default)]
    pub ts: f64,
    pub path: String,
    #[serde(default)]
    pub real_path: String,
    #[serde(default)]
    pub sha256: String,
}

/// Load a trace from a JSONL file. Skips malformed lines.
pub fn load_trace(path: &Path) -> Result<Vec<TraceEntry>, String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("read trace: {path:?}: {e}"))?;
    Ok(parse_trace(&data))
}

/// Parse trace entries from JSONL content. Skips malformed lines.
pub fn parse_trace(jsonl: &str) -> Vec<TraceEntry> {
    jsonl
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_trace() {
        let jsonl = r#"{"seq":0,"ts":1709827200.123,"path":"~/.zshenv","real_path":"/nix/store/abc","sha256":"aaa"}
{"seq":1,"ts":1709827200.130,"path":"~/.config/shell/groups/common/settings.zsh","real_path":"/nix/store/def","sha256":"bbb"}"#;

        let entries = parse_trace(jsonl);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[0].path, "~/.zshenv");
        assert_eq!(entries[1].seq, 1);
    }

    #[test]
    fn skip_malformed_lines() {
        let jsonl = r#"{"seq":0,"path":"ok","sha256":"abc"}
not valid json
{"seq":1,"path":"also ok","sha256":"def"}"#;

        let entries = parse_trace(jsonl);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn empty_trace() {
        let entries = parse_trace("");
        assert!(entries.is_empty());
    }

    #[test]
    fn missing_optional_fields() {
        let jsonl = r#"{"seq":0,"path":"test.zsh"}"#;
        let entries = parse_trace(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sha256, "");
    }
}
