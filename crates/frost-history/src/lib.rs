//! Zsh-compatible history storage and `!`-expansion.
//!
//! Scope:
//!
//! * [`History`] — append-only command buffer. Load from `$HISTFILE`,
//!   push accepted commands, persist on drop or on-demand.
//! * [`expand`] — pure function that rewrites a raw input line per zsh's
//!   `HIST_EXPAND` rules. Runs *before* the parser so the executor sees
//!   the rewritten command.
//!
//! Supported expansions:
//!
//! | Token   | Meaning |
//! |---------|---------|
//! | `!!`    | entire previous command |
//! | `!$`    | last word of the previous command |
//! | `!^`    | first word (argv[1]) of the previous command |
//! | `!*`    | all args of the previous command (everything after argv[0]) |
//! | `!n`    | the nth command from the start (1-indexed) |
//! | `!-n`   | the nth command from the end |
//! | `!str`  | most recent command starting with `str` |
//! | `!?str` | most recent command containing `str` |
//!
//! Quoting rules: a `!` inside a single-quoted segment is literal; all
//! other contexts (double-quoted, bare) expand. `\!` escapes a `!`.
//! Anchors `^str^repl^` (quick substitution) are future work.

use std::path::{Path, PathBuf};

pub type HistoryResult<T> = Result<T, HistoryError>;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("io error on history file: {0}")]
    Io(#[from] std::io::Error),
    #[error("history reference `!{0}` has no match")]
    NoMatch(String),
    #[error("history reference `!{0}` out of range")]
    OutOfRange(String),
}

/// An append-only history buffer with optional file backing.
pub struct History {
    /// Ordered list of accepted command lines.
    entries: Vec<String>,
    /// If set, `save` appends to this path on drop / explicit flush.
    backing: Option<PathBuf>,
}

impl History {
    /// Empty in-memory history.
    pub fn new() -> Self {
        Self { entries: Vec::new(), backing: None }
    }

    /// Load a history file. Missing files are treated as empty (not an
    /// error) so `frost` can bootstrap fresh `$HISTFILE`s.
    pub fn from_file(path: impl Into<PathBuf>) -> HistoryResult<Self> {
        let backing = path.into();
        let mut entries = Vec::new();
        if backing.exists() {
            let content = std::fs::read_to_string(&backing)?;
            for line in content.lines() {
                let trimmed = line.trim_end();
                if !trimmed.is_empty() { entries.push(trimmed.to_string()); }
            }
        }
        Ok(Self { entries, backing: Some(backing) })
    }

    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
    pub fn entries(&self) -> &[String] { &self.entries }

    /// Append a command and, if file-backed, persist it eagerly so a
    /// crashed shell still leaves a complete trail.
    pub fn push(&mut self, line: impl Into<String>) -> HistoryResult<()> {
        let line = line.into();
        if line.trim().is_empty() { return Ok(()); }
        self.entries.push(line.clone());
        if let Some(path) = &self.backing {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true).append(true).open(path)?;
            writeln!(f, "{line}")?;
        }
        Ok(())
    }

    /// `!!` — the most recent command.
    pub fn previous(&self) -> Option<&str> {
        self.entries.last().map(String::as_str)
    }

    /// Resolve `!n` / `!-n`.
    pub fn at(&self, n: i64) -> Option<&str> {
        if n > 0 {
            self.entries.get((n - 1) as usize).map(String::as_str)
        } else if n < 0 {
            let len = self.entries.len();
            let idx = len.checked_sub((-n) as usize)?;
            self.entries.get(idx).map(String::as_str)
        } else {
            None
        }
    }

    /// Most recent command starting with `prefix`.
    pub fn find_prefix(&self, prefix: &str) -> Option<&str> {
        self.entries.iter().rev().find(|e| e.starts_with(prefix)).map(String::as_str)
    }

    /// Most recent command containing `needle`.
    pub fn find_contains(&self, needle: &str) -> Option<&str> {
        self.entries.iter().rev().find(|e| e.contains(needle)).map(String::as_str)
    }
}

impl Default for History {
    fn default() -> Self { Self::new() }
}

/// Expand `!` references in `input` against `history`. Returns
/// `(expanded, changed)` — `changed` is true iff any substitution
/// occurred (callers typically re-echo the line when true, matching zsh).
///
/// If the input contains a `!` token that has no history match,
/// returns `HistoryError::NoMatch` and the caller should reject the line
/// (zsh prints `event not found`).
pub fn expand(input: &str, history: &History) -> HistoryResult<(String, bool)> {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    let mut changed = false;
    let mut in_single = false;

    while i < bytes.len() {
        let c = bytes[i];
        if in_single {
            out.push(c as char);
            if c == b'\'' { in_single = false; }
            i += 1;
            continue;
        }
        if c == b'\'' {
            out.push('\'');
            in_single = true;
            i += 1;
            continue;
        }
        if c == b'\\' && i + 1 < bytes.len() {
            // Escape passes the next byte literally — including `\!`.
            out.push(c as char);
            out.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        if c != b'!' { out.push(c as char); i += 1; continue; }

        // We're at a `!`. zsh's rule: `!` followed by whitespace, `=`, or
        // `(` is literal. Otherwise it's a history ref.
        let next = bytes.get(i + 1).copied();
        match next {
            None | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'=') | Some(b'(') => {
                out.push('!');
                i += 1;
                continue;
            }
            _ => {}
        }

        // Parse the reference token starting at i.
        let (substitute, consumed) = parse_ref(&bytes[i..], history)?;
        out.push_str(&substitute);
        changed = true;
        i += consumed;
    }

    Ok((out, changed))
}

fn parse_ref(tail: &[u8], history: &History) -> HistoryResult<(String, usize)> {
    // tail[0] == b'!'
    let rest = &tail[1..];
    let (event, event_len) = parse_event(rest, history)?;
    let after_event = &tail[1 + event_len..];
    // Optional :word-designator — support :$ :^ :*  and :n / :n-m / :n-$.
    // Keep it tight; zsh supports more but these cover day-to-day use.
    let (word_part, word_len) = parse_word_designator(after_event);
    let selected = if let Some(wd) = word_part {
        select_words(&event, wd)
    } else {
        event.clone()
    };
    Ok((selected, 1 + event_len + word_len))
}

fn parse_event(rest: &[u8], history: &History) -> HistoryResult<(String, usize)> {
    // `!!`, `!$`, `!^`, `!*` — single-byte event with an implicit word designator.
    match rest.first().copied() {
        Some(b'!') => {
            let cmd = history.previous().ok_or_else(|| HistoryError::NoMatch("!".into()))?;
            Ok((cmd.to_string(), 1))
        }
        Some(b'$') => {
            let cmd = history.previous().ok_or_else(|| HistoryError::NoMatch("$".into()))?;
            Ok((last_word(cmd).to_string(), 1))
        }
        Some(b'^') => {
            let cmd = history.previous().ok_or_else(|| HistoryError::NoMatch("^".into()))?;
            Ok((nth_word(cmd, 1).to_string(), 1))
        }
        Some(b'*') => {
            let cmd = history.previous().ok_or_else(|| HistoryError::NoMatch("*".into()))?;
            Ok((words_from(cmd, 1).to_string(), 1))
        }
        // Numeric `!n` / `!-n`
        Some(b'-') => {
            let mut len = 1usize;
            let digits_start = 1;
            while rest.get(len).copied().is_some_and(|c| c.is_ascii_digit()) { len += 1; }
            if len == digits_start { return Err(HistoryError::NoMatch("-".into())); }
            let n: i64 = std::str::from_utf8(&rest[..len]).unwrap().parse().unwrap();
            let cmd = history.at(n).ok_or_else(|| HistoryError::OutOfRange(n.to_string()))?;
            Ok((cmd.to_string(), len))
        }
        Some(c) if c.is_ascii_digit() => {
            let mut len = 0usize;
            while rest.get(len).copied().is_some_and(|c| c.is_ascii_digit()) { len += 1; }
            let n: i64 = std::str::from_utf8(&rest[..len]).unwrap().parse().unwrap();
            let cmd = history.at(n).ok_or_else(|| HistoryError::OutOfRange(n.to_string()))?;
            Ok((cmd.to_string(), len))
        }
        // `!?str?` or `!?str` (terminator optional at end of line)
        Some(b'?') => {
            let mut len = 1usize;
            while len < rest.len() && rest[len] != b'?' && !is_ref_terminator(rest[len]) { len += 1; }
            let needle = std::str::from_utf8(&rest[1..len]).unwrap_or("").to_string();
            let consumed = if rest.get(len) == Some(&b'?') { len + 1 } else { len };
            let cmd = history.find_contains(&needle).ok_or_else(|| HistoryError::NoMatch(format!("?{needle}?")))?;
            Ok((cmd.to_string(), consumed))
        }
        // `!str` — prefix search
        Some(_) => {
            let mut len = 0usize;
            while len < rest.len() && !is_ref_terminator(rest[len]) { len += 1; }
            let prefix = std::str::from_utf8(&rest[..len]).unwrap_or("");
            let cmd = history.find_prefix(prefix).ok_or_else(|| HistoryError::NoMatch(prefix.into()))?;
            Ok((cmd.to_string(), len))
        }
        None => Err(HistoryError::NoMatch(String::new())),
    }
}

fn is_ref_terminator(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b':' | b'$' | b'\'' | b'"')
}

fn parse_word_designator(tail: &[u8]) -> (Option<char>, usize) {
    if tail.first() != Some(&b':') { return (None, 0); }
    match tail.get(1).copied() {
        Some(b'$') => (Some('$'), 2),
        Some(b'^') => (Some('^'), 2),
        Some(b'*') => (Some('*'), 2),
        _ => (None, 0),
    }
}

fn select_words(cmd: &str, designator: char) -> String {
    match designator {
        '$' => last_word(cmd).to_string(),
        '^' => nth_word(cmd, 1).to_string(),
        '*' => words_from(cmd, 1).to_string(),
        _ => cmd.to_string(),
    }
}

fn last_word(s: &str) -> &str {
    s.split_whitespace().next_back().unwrap_or("")
}
fn nth_word(s: &str, n: usize) -> &str {
    s.split_whitespace().nth(n).unwrap_or("")
}
fn words_from(s: &str, start: usize) -> &str {
    // Return the substring starting at the `start`-th whitespace-separated
    // word. Keeps interior whitespace as the user typed it.
    let mut count = 0usize;
    let mut in_ws = true;
    for (i, ch) in s.char_indices() {
        let is_ws = ch.is_whitespace();
        if in_ws && !is_ws {
            if count == start { return &s[i..]; }
            count += 1;
            in_ws = false;
        } else if !in_ws && is_ws {
            in_ws = true;
        }
    }
    ""
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(entries: &[&str]) -> History {
        let mut h = History::new();
        for e in entries { h.push(*e).unwrap(); }
        h
    }

    #[test]
    fn bang_bang_replays_previous() {
        let h = hist(&["ls -la /tmp", "cd /etc"]);
        let (out, changed) = expand("!!", &h).unwrap();
        assert_eq!(out, "cd /etc");
        assert!(changed);
    }

    #[test]
    fn bang_dollar_picks_last_word() {
        let h = hist(&["cp src/lib.rs src/main.rs"]);
        let (out, _) = expand("vi !$", &h).unwrap();
        assert_eq!(out, "vi src/main.rs");
    }

    #[test]
    fn bang_caret_picks_first_arg() {
        let h = hist(&["cp src/lib.rs src/main.rs"]);
        let (out, _) = expand("cat !^", &h).unwrap();
        assert_eq!(out, "cat src/lib.rs");
    }

    #[test]
    fn bang_star_picks_all_args() {
        let h = hist(&["cp a b c d"]);
        let (out, _) = expand("echo !*", &h).unwrap();
        assert_eq!(out, "echo a b c d");
    }

    #[test]
    fn numeric_index() {
        let h = hist(&["one", "two", "three"]);
        let (out, _) = expand("!1", &h).unwrap();
        assert_eq!(out, "one");
        let (out, _) = expand("!3", &h).unwrap();
        assert_eq!(out, "three");
    }

    #[test]
    fn negative_numeric_counts_from_end() {
        let h = hist(&["one", "two", "three"]);
        let (out, _) = expand("!-1", &h).unwrap();
        assert_eq!(out, "three");
        let (out, _) = expand("!-2", &h).unwrap();
        assert_eq!(out, "two");
    }

    #[test]
    fn prefix_match() {
        let h = hist(&["cd /tmp", "ls /etc", "cd /var"]);
        let (out, _) = expand("!cd", &h).unwrap();
        assert_eq!(out, "cd /var");
    }

    #[test]
    fn contains_match() {
        let h = hist(&["cd /tmp", "rg TODO src/"]);
        let (out, _) = expand("!?TODO", &h).unwrap();
        assert_eq!(out, "rg TODO src/");
    }

    #[test]
    fn single_quoted_bang_is_literal() {
        let h = hist(&["yes"]);
        let (out, changed) = expand("echo '!!'", &h).unwrap();
        assert_eq!(out, "echo '!!'");
        assert!(!changed);
    }

    #[test]
    fn escaped_bang_is_literal() {
        let h = hist(&["yes"]);
        let (out, changed) = expand(r"echo \!!", &h).unwrap();
        assert_eq!(out, r"echo \!!");
        assert!(!changed);
    }

    #[test]
    fn bang_followed_by_space_is_literal() {
        let h = hist(&["yes"]);
        let (out, _) = expand("echo !", &h).unwrap();
        assert_eq!(out, "echo !");
    }

    #[test]
    fn no_match_errors() {
        let h = hist(&["ls"]);
        assert!(matches!(expand("!qwerty", &h), Err(HistoryError::NoMatch(_))));
    }

    #[test]
    fn empty_history_bangs_fail() {
        let h = History::new();
        assert!(matches!(expand("!!", &h), Err(HistoryError::NoMatch(_))));
    }
}
