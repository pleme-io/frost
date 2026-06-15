//! Regression: the history file has exactly ONE writer (reedline's
//! `FileBackedHistory`). frost-history's `!`-expansion buffer is read-only
//! on disk (`from_file_readonly`).
//!
//! ## The bug this pins (operator report, mado/frostmourne stack)
//!
//! "Ctrl-R doesn't prioritize bringing up my most-recently-typed command."
//!
//! ## Root cause (proven below)
//!
//! frost ran TWO writers against one `$HISTFILE`:
//!
//! 1. `frost_history::History::push` — eager, line-by-line append.
//! 2. `reedline::FileBackedHistory` — saves accepted lines in-memory, then
//!    on `Drop`/`sync()` re-reads the WHOLE file as "foreign" entries and
//!    re-appends its own buffer after them.
//!
//! Interleaving the two corrupts the file: every command is duplicated, and
//! as the file grows across sessions reedline re-appends its in-memory
//! buffer (which may hold older commands) AFTER the newest eager append, so
//! the genuinely most-recent command is not reliably the last/highest-counter
//! line. skim-tab's Ctrl-R picker dedups + sorts most-recent-first off that
//! file, so a corrupted order means the most-recent command is not on top.
//!
//! The fix removes the second writer. This test proves: (a) two writers
//! duplicate, (b) one writer is clean and most-recent-last.

use reedline::{FileBackedHistory, History, HistoryItem};
use std::io::Write;

fn read_lines(p: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Reproduces the OLD dual-writer corruption deterministically: every
/// command ends up duplicated on disk. This is the bug the fix eliminates;
/// kept here as the falsifiable record of WHY a single writer is required.
#[test]
fn dual_writer_corrupts_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h");

    let mut reedline = FileBackedHistory::with_file(10_000, path.clone()).unwrap();
    for cmd in ["echo one", "echo two", "echo three"] {
        // Writer #1: frost_history's old eager append.
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{cmd}").unwrap();
        drop(f);
        // Writer #2: reedline saves in-memory (flushed on drop).
        reedline.save(HistoryItem::from_command_line(cmd)).unwrap();
    }
    drop(reedline); // FileBackedHistory::sync(): foreign + own.

    let lines = read_lines(&path);
    assert_eq!(
        lines.len(),
        6,
        "two writers duplicate every command — the corruption Ctrl-R tripped over: {lines:?}"
    );
    assert_eq!(
        lines,
        vec![
            "echo one", "echo two", "echo three", "echo one", "echo two", "echo three"
        ]
    );
}

/// The fix: reedline is the SOLE writer (eager `sync()` after each save, as
/// the frost REPL now does via `ZleEngine::sync_history`). No duplication;
/// the most-recently-run command is the last line — so skim-tab's Ctrl-R
/// feed (most-recent-first) lands the cursor on it.
#[test]
fn single_writer_is_clean_and_most_recent_last() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h");

    let mut reedline = FileBackedHistory::with_file(10_000, path.clone()).unwrap();
    for cmd in ["echo one", "echo two", "echo three"] {
        reedline.save(HistoryItem::from_command_line(cmd)).unwrap();
        reedline.sync().unwrap(); // eager flush — the only writer.
    }
    drop(reedline);

    let lines = read_lines(&path);
    assert_eq!(
        lines,
        vec!["echo one", "echo two", "echo three"],
        "single writer ⇒ no duplication, run-order preserved"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("echo three"),
        "most-recently-run command must be the LAST line (skim-tab puts it on top)"
    );
}

/// Cross-session: a second reedline (the next frost launch) opening the file
/// must read the prior session's commands and keep appending in order — no
/// re-duplication of the already-on-disk entries.
#[test]
fn reopen_does_not_reduplicate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h");

    let mut s1 = FileBackedHistory::with_file(10_000, path.clone()).unwrap();
    for cmd in ["a", "b"] {
        s1.save(HistoryItem::from_command_line(cmd)).unwrap();
        s1.sync().unwrap();
    }
    drop(s1);

    let mut s2 = FileBackedHistory::with_file(10_000, path.clone()).unwrap();
    s2.save(HistoryItem::from_command_line("c")).unwrap();
    s2.sync().unwrap();
    drop(s2);

    assert_eq!(read_lines(&path), vec!["a", "b", "c"]);
}
