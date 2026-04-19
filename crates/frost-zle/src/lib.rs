//! Zsh Line Editor (ZLE) — thin wrapper around [`reedline`] providing the
//! interactive read-a-command-line surface for frost.
//!
//! Current capabilities:
//!
//! * Emacs-style line editing (Home/End, Ctrl-A/E, Ctrl-W, arrow keys,
//!   word motions, …) via reedline's default keybindings.
//! * Persistent command history backed by `$HISTFILE` (defaulting to
//!   `$HOME/.frost_history`) via [`reedline::FileBackedHistory`].
//! * Multi-line continuation: the caller owns "is this a complete command"
//!   detection; when it returns `ReadLineOutcome::Incomplete`, `read_line`
//!   re-prompts with `PS2` (default `> `) and concatenates.
//!
//! Not yet implemented:
//!
//! * vi mode / custom keymaps.
//! * Completion engine hookup (wire-up point is future `with_completer`).
//! * Prompt substitution (`PROMPT_SUBST`) — the caller should expand the
//!   prompt string before passing it to [`ZleEngine::set_prompt`].

use std::path::{Path, PathBuf};

use reedline::{
    FileBackedHistory, Prompt, PromptEditMode, PromptHistorySearch,
    PromptHistorySearchStatus, Reedline, Signal,
};

pub type ZleResult<T> = Result<T, ZleError>;

#[derive(Debug, thiserror::Error)]
pub enum ZleError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("reedline error: {0}")]
    Reedline(String),
}

/// Outcome of a single read_line pass, returned by the caller's
/// "is-this-complete" check.
pub enum InputStatus {
    /// Input is a complete shell command — hand it to the executor.
    Complete,
    /// Input looks incomplete (unclosed quote, trailing `\`, unmatched `do`,
    /// etc.) — the engine should re-prompt with the continuation prompt and
    /// concatenate the next line.
    Incomplete,
}

/// What [`ZleEngine::read_line`] returned to the caller.
pub enum ReadLineOutcome {
    /// A complete command line. Pass it to the executor.
    Input(String),
    /// User pressed Ctrl-C (line aborted) — caller should discard input.
    Interrupted,
    /// EOF / Ctrl-D — caller should exit the shell.
    Eof,
}

/// A frost prompt: `PS1` for the primary line and `PS2` for continuations.
pub struct FrostPrompt {
    ps1: String,
    ps2: String,
}

impl FrostPrompt {
    pub fn new(ps1: impl Into<String>, ps2: impl Into<String>) -> Self {
        Self { ps1: ps1.into(), ps2: ps2.into() }
    }
}

impl Default for FrostPrompt {
    fn default() -> Self {
        FrostPrompt::new("frost> ", "> ")
    }
}

impl Prompt for FrostPrompt {
    fn render_prompt_left(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(&self.ps1)
    }
    fn render_prompt_right(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("")
    }
    fn render_prompt_multiline_indicator(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(&self.ps2)
    }
    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> std::borrow::Cow<'_, str> {
        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        std::borrow::Cow::Owned(format!("({prefix}reverse-search: {}) ", history_search.term))
    }
}

/// The interactive line editor. Wraps a [`Reedline`] instance with a
/// history backend so commands persist across frost invocations.
pub struct ZleEngine {
    inner: Reedline,
    prompt: FrostPrompt,
}

impl ZleEngine {
    /// Build an interactive line editor with history backed at `history_file`.
    /// `history_file`'s parent directory is created if missing; if the file
    /// cannot be opened, the engine falls back to in-memory history and
    /// returns `Ok` (the shell should still be usable).
    pub fn new(history_file: impl AsRef<Path>, history_capacity: usize) -> ZleResult<Self> {
        let path = history_file.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let editor = match FileBackedHistory::with_file(history_capacity, path.clone()) {
            Ok(hist) => Reedline::create().with_history(Box::new(hist)),
            Err(_) => Reedline::create(),
        };
        Ok(Self {
            inner: editor,
            prompt: FrostPrompt::default(),
        })
    }

    /// Build an in-memory (non-persistent) engine. Useful for tests and for
    /// environments where `$HOME` is unavailable.
    pub fn in_memory() -> Self {
        Self {
            inner: Reedline::create(),
            prompt: FrostPrompt::default(),
        }
    }

    /// Update PS1 / PS2. Callers should pre-expand any `PROMPT_SUBST`
    /// placeholders before passing strings here.
    pub fn set_prompt(&mut self, ps1: impl Into<String>, ps2: impl Into<String>) {
        self.prompt = FrostPrompt::new(ps1, ps2);
    }

    /// Read one logical command line. `is_complete` is called after each
    /// physical line read; returning [`InputStatus::Incomplete`] causes the
    /// engine to re-prompt with PS2 and concatenate the next line.
    pub fn read_line<F>(&mut self, mut is_complete: F) -> ZleResult<ReadLineOutcome>
    where
        F: FnMut(&str) -> InputStatus,
    {
        let mut buf = String::new();
        loop {
            match self.inner.read_line(&self.prompt) {
                Ok(Signal::Success(line)) => {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(&line);
                    match is_complete(&buf) {
                        InputStatus::Complete => return Ok(ReadLineOutcome::Input(buf)),
                        InputStatus::Incomplete => continue,
                    }
                }
                Ok(Signal::CtrlC) => return Ok(ReadLineOutcome::Interrupted),
                Ok(Signal::CtrlD) => return Ok(ReadLineOutcome::Eof),
                Err(e) => return Err(ZleError::Reedline(e.to_string())),
            }
        }
    }
}

/// Resolve the history file path from `HISTFILE` if set, else
/// `$HOME/.frost_history`, else a file in the temp dir so the engine
/// still starts on unusual setups.
pub fn default_history_path() -> PathBuf {
    if let Ok(p) = std::env::var("HISTFILE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".frost_history");
    }
    std::env::temp_dir().join("frost_history")
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_defaults_to_frost_gt_and_gt() {
        let p = FrostPrompt::default();
        assert_eq!(p.render_prompt_left(), "frost> ");
        assert_eq!(p.render_prompt_multiline_indicator(), "> ");
    }

    #[test]
    fn default_history_path_is_nonempty() {
        let p = default_history_path();
        assert!(!p.as_os_str().is_empty());
    }

    #[test]
    fn in_memory_engine_constructs() {
        let _ = ZleEngine::in_memory();
    }
}
