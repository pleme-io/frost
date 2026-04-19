//! Core verification algorithm.
//!
//! Compares a manifest (expected) against a trace (actual) and produces
//! a report with per-entry status and an aggregate verdict.

use std::collections::HashMap;
use std::path::Path;

use crate::manifest::{EntryKind, Manifest, ManifestEntry};
use crate::trace::TraceEntry;

/// Overall verification result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
}

/// Per-entry verification status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryStatus {
    /// Order and hash match.
    Verified,
    /// Hash mismatch.
    HashMismatch { expected: String, actual: String },
    /// Sourced but in wrong order.
    OrderMismatch { expected: u32, actual: u32 },
    /// Expected but not found in trace.
    Missing,
    /// Missing but it's a deferred plugin (warning, not error).
    MissingDeferred,
    /// Missing but it's a local override (warning, not error).
    MissingLocal,
    /// Found in trace but not in manifest.
    Unexpected,
}

/// Verification result for a single entry.
#[derive(Debug, Clone)]
pub struct EntryResult {
    pub path: String,
    pub status: EntryStatus,
    pub kind: Option<EntryKind>,
    pub name: Option<String>,
}

/// Complete verification report.
#[derive(Debug, Clone)]
pub struct Report {
    pub verdict: Verdict,
    pub verified: usize,
    pub mismatched: usize,
    pub missing: usize,
    pub warnings: usize,
    pub unexpected: usize,
    pub entries: Vec<EntryResult>,
}

/// Verify a trace against a manifest.
pub fn verify(manifest: &Manifest, trace: &[TraceEntry]) -> Report {
    let trace_by_path: HashMap<&str, &TraceEntry> =
        trace.iter().map(|e| (e.path.as_str(), e)).collect();

    let mut entries = Vec::new();
    let mut verified = 0usize;
    let mut mismatched = 0usize;
    let mut missing = 0usize;
    let mut warnings = 0usize;

    for m_entry in &manifest.entries {
        let path = normalize_path(&m_entry.path);
        match trace_by_path.get(path.as_str()) {
            Some(t_entry) => {
                let status = check_entry(m_entry, t_entry);
                match &status {
                    EntryStatus::Verified => verified += 1,
                    EntryStatus::HashMismatch { .. } | EntryStatus::OrderMismatch { .. } => {
                        mismatched += 1
                    }
                    _ => {}
                }
                entries.push(EntryResult {
                    path: m_entry.path.clone(),
                    status,
                    kind: Some(m_entry.kind),
                    name: m_entry.name.clone(),
                });
            }
            None => {
                let status = if m_entry.deferred {
                    warnings += 1;
                    EntryStatus::MissingDeferred
                } else if m_entry.kind == EntryKind::LocalOverride {
                    warnings += 1;
                    EntryStatus::MissingLocal
                } else {
                    missing += 1;
                    EntryStatus::Missing
                };
                entries.push(EntryResult {
                    path: m_entry.path.clone(),
                    status,
                    kind: Some(m_entry.kind),
                    name: m_entry.name.clone(),
                });
            }
        }
    }

    // Find unexpected trace entries (not in manifest).
    let manifest_paths: std::collections::HashSet<String> = manifest
        .entries
        .iter()
        .map(|e| normalize_path(&e.path))
        .collect();

    let mut unexpected = 0usize;
    for t_entry in trace {
        let path = normalize_path(&t_entry.path);
        if !manifest_paths.contains(&path) {
            unexpected += 1;
            entries.push(EntryResult {
                path: t_entry.path.clone(),
                status: EntryStatus::Unexpected,
                kind: None,
                name: None,
            });
        }
    }

    let verdict = if missing > 0 || mismatched > 0 {
        Verdict::Fail
    } else if warnings > 0 || unexpected > 0 {
        Verdict::Warn
    } else {
        Verdict::Pass
    };

    Report {
        verdict,
        verified,
        mismatched,
        missing,
        warnings,
        unexpected,
        entries,
    }
}

/// Rehash mode: verify manifest entries against files on disk.
/// No trace needed — reads each file and computes SHA-256.
pub fn rehash(manifest: &Manifest) -> Report {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut entries = Vec::new();
    let mut verified = 0usize;
    let mut mismatched = 0usize;
    let mut missing = 0usize;
    let mut warnings = 0usize;

    for m_entry in &manifest.entries {
        let expanded = m_entry.path.replace('~', &home);
        let path = Path::new(&expanded);

        if !path.exists() {
            let status = if m_entry.deferred {
                warnings += 1;
                EntryStatus::MissingDeferred
            } else if m_entry.kind == EntryKind::LocalOverride {
                warnings += 1;
                EntryStatus::MissingLocal
            } else {
                missing += 1;
                EntryStatus::Missing
            };
            entries.push(EntryResult {
                path: m_entry.path.clone(),
                status,
                kind: Some(m_entry.kind),
                name: m_entry.name.clone(),
            });
            continue;
        }

        let actual_hash = match hash_file(path) {
            Ok(h) => h,
            Err(_) => {
                missing += 1;
                entries.push(EntryResult {
                    path: m_entry.path.clone(),
                    status: EntryStatus::Missing,
                    kind: Some(m_entry.kind),
                    name: m_entry.name.clone(),
                });
                continue;
            }
        };

        if actual_hash == m_entry.blake3 {
            verified += 1;
            entries.push(EntryResult {
                path: m_entry.path.clone(),
                status: EntryStatus::Verified,
                kind: Some(m_entry.kind),
                name: m_entry.name.clone(),
            });
        } else {
            mismatched += 1;
            entries.push(EntryResult {
                path: m_entry.path.clone(),
                status: EntryStatus::HashMismatch {
                    expected: m_entry.blake3.clone(),
                    actual: actual_hash,
                },
                kind: Some(m_entry.kind),
                name: m_entry.name.clone(),
            });
        }
    }

    let verdict = if missing > 0 || mismatched > 0 {
        Verdict::Fail
    } else if warnings > 0 {
        Verdict::Warn
    } else {
        Verdict::Pass
    };

    Report {
        verdict,
        verified,
        mismatched,
        missing,
        warnings,
        unexpected: 0,
        entries,
    }
}

/// Compute hex-encoded BLAKE3 hash of a file.
pub fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    let data = std::fs::read(path)?;
    Ok(blake3::hash(&data).to_hex().to_string())
}

fn check_entry(manifest: &ManifestEntry, trace: &TraceEntry) -> EntryStatus {
    if !trace.blake3.is_empty() && trace.blake3 != manifest.blake3 {
        return EntryStatus::HashMismatch {
            expected: manifest.blake3.clone(),
            actual: trace.blake3.clone(),
        };
    }

    // Check order.
    if trace.seq != manifest.order {
        return EntryStatus::OrderMismatch {
            expected: manifest.order,
            actual: trace.seq,
        };
    }

    EntryStatus::Verified
}

/// Normalize a path for comparison (expand ~ if needed, canonicalize).
fn normalize_path(path: &str) -> String {
    // For comparison we keep paths as-is; both manifest and trace
    // should use the same format.
    path.to_string()
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Pass => write!(f, "PASS"),
            Verdict::Warn => write!(f, "WARN"),
            Verdict::Fail => write!(f, "FAIL"),
        }
    }
}

impl Report {
    /// Format the report as human-readable text.
    pub fn display(&self, verbose: bool) -> String {
        let mut out = String::new();
        out.push_str("=== Shell Config Verification ===\n\n");

        let total = self.verified + self.mismatched + self.missing + self.warnings;
        out.push_str(&format!("  Verified:    {}/{total}\n", self.verified));
        if self.warnings > 0 {
            out.push_str(&format!("  Warnings:    {}\n", self.warnings));
        }
        if self.mismatched > 0 {
            out.push_str(&format!("  Mismatched:  {}\n", self.mismatched));
        }
        if self.missing > 0 {
            out.push_str(&format!("  Missing:     {}\n", self.missing));
        }
        if self.unexpected > 0 {
            out.push_str(&format!("  Unexpected:  {}\n", self.unexpected));
        }

        out.push_str(&format!("\n  Verdict: {}\n", self.verdict));

        if verbose {
            out.push('\n');
            for entry in &self.entries {
                let status = match &entry.status {
                    EntryStatus::Verified => "ok".to_string(),
                    EntryStatus::HashMismatch { expected, actual } => {
                        format!(
                            "HASH MISMATCH (expected {}, got {})",
                            &expected[..8],
                            &actual[..8.min(actual.len())]
                        )
                    }
                    EntryStatus::OrderMismatch { expected, actual } => {
                        format!("ORDER MISMATCH (expected {expected}, got {actual})")
                    }
                    EntryStatus::Missing => "MISSING".to_string(),
                    EntryStatus::MissingDeferred => "missing (deferred)".to_string(),
                    EntryStatus::MissingLocal => "missing (local override)".to_string(),
                    EntryStatus::Unexpected => "unexpected".to_string(),
                };

                let label = entry
                    .name
                    .as_deref()
                    .unwrap_or_else(|| entry.path.rsplit('/').next().unwrap_or(&entry.path));

                out.push_str(&format!("  [{status}] {label}\n"));
            }
        }

        out
    }

    /// Serialize the report as JSON.
    pub fn to_json(&self) -> String {
        let entries: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                let mut obj = serde_json::json!({
                    "path": e.path,
                    "status": format!("{:?}", e.status),
                });
                if let Some(kind) = &e.kind {
                    obj["kind"] = serde_json::json!(kind.to_string());
                }
                if let Some(name) = &e.name {
                    obj["name"] = serde_json::json!(name);
                }
                obj
            })
            .collect();

        serde_json::to_string_pretty(&serde_json::json!({
            "verdict": self.verdict.to_string(),
            "verified": self.verified,
            "mismatched": self.mismatched,
            "missing": self.missing,
            "warnings": self.warnings,
            "unexpected": self.unexpected,
            "entries": entries,
        }))
        .unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{EntryKind, ManifestEntry, Phase};

    fn entry(order: u32, path: &str, hash: &str) -> ManifestEntry {
        ManifestEntry {
            phase: Phase::Zshrc,
            order,
            path: path.to_string(),
            kind: EntryKind::Group,
            blake3: hash.to_string(),
            name: None,
            priority: None,
            deferred: false,
        }
    }

    fn trace_entry(seq: u32, path: &str, hash: &str) -> TraceEntry {
        TraceEntry {
            seq,
            ts: 0.0,
            path: path.to_string(),
            real_path: String::new(),
            blake3: hash.to_string(),
        }
    }

    #[test]
    fn perfect_match() {
        let manifest = Manifest {
            version: crate::manifest::MANIFEST_VERSION,
            shell: "zsh".into(),
            root: None,
            entries: vec![entry(0, "a.zsh", "aaa"), entry(1, "b.zsh", "bbb")],
        };
        let trace = vec![
            trace_entry(0, "a.zsh", "aaa"),
            trace_entry(1, "b.zsh", "bbb"),
        ];

        let report = verify(&manifest, &trace);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.verified, 2);
        assert_eq!(report.missing, 0);
        assert_eq!(report.mismatched, 0);
    }

    #[test]
    fn hash_mismatch_fails() {
        let manifest = Manifest {
            version: crate::manifest::MANIFEST_VERSION,
            shell: "zsh".into(),
            root: None,
            entries: vec![entry(0, "a.zsh", "expected")],
        };
        let trace = vec![trace_entry(0, "a.zsh", "actual")];

        let report = verify(&manifest, &trace);
        assert_eq!(report.verdict, Verdict::Fail);
        assert_eq!(report.mismatched, 1);
    }

    #[test]
    fn missing_entry_fails() {
        let manifest = Manifest {
            version: crate::manifest::MANIFEST_VERSION,
            shell: "zsh".into(),
            root: None,
            entries: vec![entry(0, "a.zsh", "aaa"), entry(1, "missing.zsh", "bbb")],
        };
        let trace = vec![trace_entry(0, "a.zsh", "aaa")];

        let report = verify(&manifest, &trace);
        assert_eq!(report.verdict, Verdict::Fail);
        assert_eq!(report.missing, 1);
    }

    #[test]
    fn deferred_missing_is_warning() {
        let manifest = Manifest {
            version: crate::manifest::MANIFEST_VERSION,
            shell: "zsh".into(),
            root: None,
            entries: vec![ManifestEntry {
                deferred: true,
                ..entry(0, "deferred.zsh", "aaa")
            }],
        };
        let trace = vec![];

        let report = verify(&manifest, &trace);
        assert_eq!(report.verdict, Verdict::Warn);
        assert_eq!(report.warnings, 1);
        assert_eq!(report.missing, 0);
    }

    #[test]
    fn local_override_missing_is_warning() {
        let manifest = Manifest {
            version: crate::manifest::MANIFEST_VERSION,
            shell: "zsh".into(),
            root: None,
            entries: vec![ManifestEntry {
                kind: EntryKind::LocalOverride,
                ..entry(0, "local.zsh", "aaa")
            }],
        };
        let trace = vec![];

        let report = verify(&manifest, &trace);
        assert_eq!(report.verdict, Verdict::Warn);
        assert_eq!(report.warnings, 1);
    }

    #[test]
    fn unexpected_entry_is_warning() {
        let manifest = Manifest {
            version: crate::manifest::MANIFEST_VERSION,
            shell: "zsh".into(),
            root: None,
            entries: vec![entry(0, "a.zsh", "aaa")],
        };
        let trace = vec![
            trace_entry(0, "a.zsh", "aaa"),
            trace_entry(1, "extra.zsh", "xxx"),
        ];

        let report = verify(&manifest, &trace);
        assert_eq!(report.verdict, Verdict::Warn);
        assert_eq!(report.unexpected, 1);
    }

    #[test]
    fn empty_manifest_empty_trace_passes() {
        let manifest = Manifest {
            version: crate::manifest::MANIFEST_VERSION,
            shell: "zsh".into(),
            root: None,
            entries: vec![],
        };
        let report = verify(&manifest, &[]);
        assert_eq!(report.verdict, Verdict::Pass);
    }

    #[test]
    fn order_mismatch_fails() {
        let manifest = Manifest {
            version: crate::manifest::MANIFEST_VERSION,
            shell: "zsh".into(),
            root: None,
            entries: vec![entry(0, "a.zsh", "aaa"), entry(1, "b.zsh", "bbb")],
        };
        // b.zsh loaded before a.zsh
        let trace = vec![
            trace_entry(1, "a.zsh", "aaa"),
            trace_entry(0, "b.zsh", "bbb"),
        ];

        let report = verify(&manifest, &trace);
        assert_eq!(report.verdict, Verdict::Fail);
        assert_eq!(report.mismatched, 2);
    }

    #[test]
    fn hash_file_works() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello\n").unwrap();
        let hash = hash_file(&file).unwrap();
        // BLAKE3 of "hello\n"
        assert_eq!(
            hash,
            "8e4c7c1b99dbfd50e7a95185fead5ee1448fa904a2fdd778eaf5f2dbfd629a99"
        );
    }
}
