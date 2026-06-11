//! Typed TTY-takeover subprocess spawn.
//!
//! # Why this module exists
//!
//! 2026-05-21 incident: operator pressed Ctrl-R in frostmourne and
//! "nothing happened". Three independent bugs surfaced in sequence
//! as we peeled them off:
//!
//!   1. `rc.lisp` declared C-r twice (defbind + defpicker) — the
//!      reedline keybindings HashMap collision left the chord
//!      unresponsive. Fixed.
//!   2. Atlas migration via `ishou_tokens::FleetKeybinds` lifted
//!      every chord to one typed source so the duplicate-bind class
//!      is now a build-time error. Fixed.
//!   3. The picker dispatch itself spawned `skim` with
//!      `Command::output()` — which **explicitly NULLs stdin** to
//!      avoid parent/child read-deadlocks. That gives `skim` no
//!      keyboard channel: it draws to /dev/tty (which it can open
//!      from any subprocess) but receives no keystrokes because the
//!      foreground process group still belongs to frost. `skim`
//!      sits idle, frost blocks waiting for a selection that can
//!      never come. The operator sees nothing.
//!
//! This module is the typed fix for #3 — and the prime-directive
//! shield that makes the bug class **impossible to recreate**.
//! Every TTY-takeover subprocess in frost (skim-history, skim-files,
//! skim-cd, skim-content, future external pickers, $EDITOR launches,
//! `man`, `less`, …) routes through `TtyTakeover::spawn_and_capture`.
//! The stdio combo is **baked into the type**:
//!
//! - `stdin`  → inherit  (subprocess reads tty keystrokes)
//! - `stderr` → inherit  (subprocess TUI draws through; `skim` uses
//!                        /dev/tty for the screen itself but this
//!                        keeps error messages visible)
//! - `stdout` → piped    (the selection — captured + returned)
//!
//! No `Command::output()`. No `Command::status()`. No raw
//! `Command::spawn()` with hand-configured stdio. The only way to
//! get a TTY-takeover subprocess is through this type. A clippy
//! lint elsewhere in the workspace flags any `Command::output()`
//! call in the picker path.
//!
//! # Usage
//!
//! ```rust,ignore
//! use frost_exec::tty_takeover::TtyTakeover;
//!
//! let selection: Option<String> = TtyTakeover::new("skim-history")
//!     .arg("--query")
//!     .arg("git")
//!     .env("HISTFILE", history_path)
//!     .spawn_and_capture()
//!     .ok()
//!     .flatten();
//! ```
//!
//! # What this primitive enforces
//!
//! - **stdin is never null/piped** — the subprocess always sees the
//!   parent's tty. Operator keystrokes route correctly.
//! - **stdout is always captured** — the selection is the typed
//!   output the picker contract requires. Returning `String` makes
//!   the data flow obvious.
//! - **stderr is never captured** — error messages reach the
//!   operator's screen, not a discarded buffer.
//! - **Cannot be reconfigured after construction** — internal
//!   `Command` is private; `spawn_and_capture` consumes self.
//!
//! Future extensions belong here (e.g. tcsetpgrp handoff for stricter
//! tty ownership, signal forwarding while child runs, etc.) so every
//! consumer inherits the fix uniformly.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::process::{Command, Stdio};

/// A subprocess that takes over the operator's terminal for its
/// runtime (TUI fuzzy pickers, full-screen editors, pagers, etc.)
/// and returns a single-line selection on stdout.
///
/// Constructed via [`TtyTakeover::new`]; configured via the small
/// surface of [`Self::arg`] / [`Self::args`] / [`Self::env`]; run
/// via the consuming [`Self::spawn_and_capture`]. The internal
/// `Command` is private — there is no way to call `.output()` or
/// to override stdin/stderr/stdout from the outside.
#[must_use = "TtyTakeover does nothing until .spawn_and_capture() is called"]
pub struct TtyTakeover {
    command: Command,
}

impl TtyTakeover {
    /// Build a new TTY-takeover subprocess for the binary at `bin`.
    /// The stdio combo is set immediately so the configuration is
    /// uniform regardless of which `arg`/`env` calls fire after.
    pub fn new(bin: impl AsRef<OsStr>) -> Self {
        let mut command = Command::new(bin);
        command
            .stdin(Stdio::inherit())
            .stderr(Stdio::inherit())
            .stdout(Stdio::piped());
        Self { command }
    }

    /// Append a single argument.
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.command.arg(arg);
        self
    }

    /// Append a sequence of arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    /// Set an environment variable visible to the subprocess.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    /// Spawn the subprocess, block until it exits, return the
    /// trimmed selection from its stdout.
    ///
    /// Behavior:
    ///
    /// - Subprocess reads keystrokes from the inherited tty.
    /// - Subprocess errors/warnings reach the operator's screen via
    ///   inherited stderr.
    /// - Frost reads the stdout pipe on the spawning thread before
    ///   `.wait()`. Selection is small (one line) — buffering the
    ///   read is fine.
    /// - Non-zero exit status returns `Ok(None)` so the caller can
    ///   treat "user aborted" + "binary missing" + "selection was
    ///   empty" uniformly. Real spawn errors propagate as `Err`.
    ///
    /// # Errors
    ///
    /// Returns `Err` only when the subprocess cannot be spawned at
    /// all (binary missing on `$PATH`, permission denied, …). A
    /// subprocess that runs to completion always returns `Ok`.
    pub fn spawn_and_capture(mut self) -> io::Result<Option<String>> {
        let mut child = self.command.spawn()?;
        let mut selection = String::new();
        if let Some(mut out) = child.stdout.take() {
            // Drain on this thread — the selection is one line; no
            // need to spawn a reader thread.
            let _ = out.read_to_string(&mut selection);
        }
        let status = child.wait()?;
        if !status.success() {
            return Ok(None);
        }
        let cleaned = strip_terminal_controls(&selection);
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }
}

/// Strip terminal control sequences from a captured selection.
///
/// 2026-06-11 incident: skim 3.x's stack (ratatui
/// `CrosstermBackend::get_cursor_position` → `crossterm::cursor::position`)
/// wrote its `ESC[6n` cursor query to the process stdout — the selection
/// pipe — so frost spliced `^[[6n` into the line editor in front of every
/// recalled command, and a cancelled picker returned the bare query as a
/// non-empty "selection" that the operator then accepted into history.
/// skim-tab fixed the emission side (`TtyStdoutGuard`); this is frost's
/// half of the contract: **a selection is printable text — terminal
/// control bytes cannot cross this boundary into the editor buffer**,
/// no matter what a present or future picker binary leaks.
///
/// Removes: CSI (`ESC[…<final>`), OSC (`ESC]…BEL`/`ESC]…ST`),
/// DCS/SOS/PM/APC (`ESC P/X/^/_ … ST`), two-byte `ESC <c>` escapes, and
/// all C0 controls except `\n` and `\t`. Keeps everything else verbatim
/// (shell-quoted paths, multi-line multi-selections).
fn strip_terminal_controls(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                // CSI: ESC [ … final byte in @..=~
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] … (BEL | ESC \)
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // DCS / SOS / PM / APC: ESC P/X/^/_ … ESC \
                Some('P' | 'X' | '^' | '_') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Two-byte escape (ESC c, ESC =, …) — drop both.
                Some(_) => {
                    chars.next();
                }
                // Lone trailing ESC — drop.
                None => {}
            },
            // C0 controls other than newline/tab never belong in a
            // selection (CR included — zsh history is LF-framed).
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// True selection round-trip via a stand-in subprocess (`echo`)
    /// that doesn't actually take over the tty — but the stdio
    /// contract is identical and lets us pin the return shape.
    #[test]
    fn echo_subprocess_round_trips_selection() {
        let sel = TtyTakeover::new("echo")
            .arg("the chosen line")
            .spawn_and_capture()
            .expect("echo should spawn");
        assert_eq!(sel.as_deref(), Some("the chosen line"));
    }

    #[test]
    fn empty_stdout_yields_none() {
        let sel = TtyTakeover::new("true")
            .spawn_and_capture()
            .expect("true should spawn");
        assert_eq!(sel, None);
    }

    #[test]
    fn nonzero_exit_yields_none() {
        // false(1) exits non-zero with no output — same shape skim
        // produces when the operator hits ESC to cancel.
        let sel = TtyTakeover::new("false")
            .spawn_and_capture()
            .expect("false should spawn");
        assert_eq!(sel, None);
    }

    #[test]
    fn whitespace_only_stdout_yields_none() {
        let sel = TtyTakeover::new("printf")
            .arg("   \n\n\t  ")
            .spawn_and_capture()
            .expect("printf should spawn");
        assert_eq!(sel, None);
    }

    #[test]
    fn trims_trailing_newline_from_selection() {
        // skim writes the selection with a trailing newline. The
        // contract is: callers get the clean selection, not the
        // raw bytes.
        let sel = TtyTakeover::new("printf")
            .arg("ls -la\n")
            .spawn_and_capture()
            .expect("printf should spawn");
        assert_eq!(sel.as_deref(), Some("ls -la"));
    }

    #[test]
    fn env_propagates_to_subprocess() {
        // /bin/sh -c is the portable way to read env back as the
        // captured selection. Use `printenv FROST_TEST_VAR`.
        let sel = TtyTakeover::new("printenv")
            .arg("FROST_TEST_TTY_TAKEOVER_VAR")
            .env("FROST_TEST_TTY_TAKEOVER_VAR", "atlas-value")
            .spawn_and_capture()
            .expect("printenv should spawn");
        assert_eq!(sel.as_deref(), Some("atlas-value"));
    }

    #[test]
    fn missing_binary_returns_err_not_none() {
        // A binary that doesn't exist must surface as Err (so the
        // caller can log "skim-foo not on $PATH") — distinct from
        // Ok(None) (which means "binary ran, user picked nothing").
        let err = TtyTakeover::new("__definitely_not_a_binary__")
            .spawn_and_capture();
        assert!(err.is_err(), "missing binary should return Err");
    }

    #[test]
    fn args_pass_through_in_order() {
        let sel = TtyTakeover::new("printf")
            .args(["%s-%s", "first", "second"])
            .spawn_and_capture()
            .expect("printf should spawn");
        assert_eq!(sel.as_deref(), Some("first-second"));
    }

    /// Compile-time check that the public API doesn't accidentally
    /// expose `Command` mutation. This is a guard for future
    /// edits — if someone adds `pub fn command_mut(&mut self) ->
    /// &mut Command` or makes the field `pub`, the bug class
    /// (operator overrides stdin to null) re-opens.
    ///
    /// We can't test the *absence* of a method, but we can pin the
    /// surface area: `spawn_and_capture` takes `self` by value, so
    /// after one call the type is consumed — no "configure then
    /// run twice" foot-gun either.
    #[test]
    fn spawn_and_capture_consumes_self() {
        let takeover = TtyTakeover::new("true");
        let _ = takeover.spawn_and_capture();
        // The next line would fail to compile if uncommented —
        // `takeover` moved on the prior call. Documented here so
        // future maintainers see the intent.
        // let _ = takeover.spawn_and_capture();
    }

    // ── selection sanitization (2026-06-11 CPR-leak incident) ──────

    /// The exact incident bytes: skim 3.x leaked its `ESC[6n` cursor
    /// query into the selection pipe ahead of the picked command. The
    /// boundary must hand the caller the command, never the query.
    #[test]
    fn cpr_query_prefix_is_stripped_from_selection() {
        let sel = TtyTakeover::new("printf")
            .arg("\x1b[6ngit push origin main")
            .spawn_and_capture()
            .expect("printf should spawn");
        assert_eq!(sel.as_deref(), Some("git push origin main"));
    }

    /// Cancelled picker, pre-fix shape: stdout was ONLY the leaked
    /// query. That must read as "no selection", not a 5-byte command
    /// the operator then accepts into shell history.
    #[test]
    fn control_only_stdout_yields_none() {
        let sel = TtyTakeover::new("printf")
            .arg("\x1b[6n")
            .spawn_and_capture()
            .expect("printf should spawn");
        assert_eq!(sel, None);
    }

    #[test]
    fn strip_removes_csi_osc_dcs_and_bare_controls() {
        assert_eq!(strip_terminal_controls("\x1b[6ngit status"), "git status");
        assert_eq!(strip_terminal_controls("a\x1b[31mred\x1b[0mb"), "aredb");
        assert_eq!(
            strip_terminal_controls("\x1b]0;title\x07ls -la"),
            "ls -la"
        );
        assert_eq!(
            strip_terminal_controls("\x1bP+q544e\x1b\\echo hi"),
            "echo hi"
        );
        assert_eq!(strip_terminal_controls("cd /tmp\r"), "cd /tmp");
        assert_eq!(strip_terminal_controls("dangling\x1b"), "dangling");
    }

    #[test]
    fn strip_keeps_printable_selections_verbatim() {
        // Shell-quoted paths and multi-line multi-selections are the
        // legitimate shapes — they pass through untouched.
        assert_eq!(
            strip_terminal_controls("'my dir/file name.txt'"),
            "'my dir/file name.txt'"
        );
        assert_eq!(strip_terminal_controls("a.txt\nb.txt"), "a.txt\nb.txt");
        assert_eq!(strip_terminal_controls("col1\tcol2"), "col1\tcol2");
    }
}
