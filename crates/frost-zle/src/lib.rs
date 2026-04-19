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
    default_emacs_keybindings, default_vi_insert_keybindings, default_vi_normal_keybindings,
    Completer, EditCommand, EditMode, Emacs, FileBackedHistory, KeyCode, KeyModifiers,
    Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, Reedline,
    ReedlineEvent, ReedlineMenu, Signal, Vi,
};

// Re-export so downstream crates can write completers without adding a
// direct `reedline` dep.
pub use reedline::{Completer as CompleterTrait, Suggestion, Span as CompletionSpan};

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

    /// Replace the completer. The provided completer implements reedline's
    /// `Completer` trait and is consulted on every Tab press. Pair this with
    /// a completion menu so suggestions are rendered below the prompt.
    pub fn with_completer(mut self, completer: Box<dyn Completer>) -> Self {
        let menu = ReedlineMenu::EngineCompleter(Box::new(
            reedline::ColumnarMenu::default(),
        ));
        self.inner = std::mem::replace(&mut self.inner, Reedline::create())
            .with_completer(completer)
            .with_menu(menu);
        self
    }

    /// Update PS1 / PS2. Callers should pre-expand any `PROMPT_SUBST`
    /// placeholders before passing strings here.
    pub fn set_prompt(&mut self, ps1: impl Into<String>, ps2: impl Into<String>) {
        self.prompt = FrostPrompt::new(ps1, ps2);
    }

    /// Switch the line editor into vi or emacs mode. Idempotent —
    /// repeating the same mode is a no-op from the user's perspective,
    /// but the reedline engine is rebuilt (retaining history + completer
    /// is the caller's responsibility; today the REPL reconstructs them
    /// per-session so this is fine).
    pub fn set_edit_mode(&mut self, mode: EditModeKind) {
        let boxed: Box<dyn EditMode> = match mode {
            EditModeKind::Emacs => Box::new(Emacs::default()),
            EditModeKind::Vi => Box::new(Vi::new(
                default_vi_insert_keybindings(),
                default_vi_normal_keybindings(),
            )),
        };
        let taken = std::mem::replace(&mut self.inner, Reedline::create());
        self.inner = taken.with_edit_mode(boxed);
    }

    /// Snapshot of the current edit buffer. Returns what the user has
    /// typed so far — useful when an ExecuteHostCommand sentinel fires
    /// mid-line and the caller wants to pre-seed an external picker
    /// (e.g., `skim-history --query "$LBUFFER"`). Returns `None` when
    /// the buffer is empty so callers can skip the `--query` flag
    /// entirely rather than passing an empty string that some pickers
    /// interpret as "match nothing".
    pub fn current_buffer_contents(&self) -> Option<String> {
        let s = self.inner.current_buffer_contents();
        if s.is_empty() { None } else { Some(s.to_string()) }
    }

    /// Clear the edit buffer and seed it with `text`. On the NEXT
    /// [`ZleEngine::read_line`] call, the user will see the prompt with
    /// `text` already inserted at the cursor, ready to edit or submit.
    ///
    /// This is the splice-from-picker hook: bind a key to
    /// `ReedlineEvent::ExecuteHostCommand("__sentinel__")`, catch the
    /// sentinel in the REPL, run an external picker (fzf, skim, …), and
    /// call `inject_prefill(&selection)` before looping back to
    /// `read_line`. Reedline's `suspended_state` restores the painter so
    /// the injection lands in the right visual spot.
    pub fn inject_prefill(&mut self, text: &str) {
        self.inner.run_edit_commands(&[
            EditCommand::Clear,
            EditCommand::InsertString(text.to_string()),
        ]);
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

/// Which line-editing model to bind. Maps to zsh's `bindkey -v` / `-e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditModeKind {
    Emacs,
    Vi,
}

// ─── Custom keybindings ─────────────────────────────────────────────────

/// Apply rc-authored `defbind` keybindings. Each entry maps a chord
/// string (`"C-l"`, `"M-?"`, …) to the name of a shell function that
/// reedline will invoke by returning it from `read_line` as if the user
/// had typed it. The ZleEngine re-installs its edit mode with the merged
/// keybindings.
///
/// Only single-key chords with Ctrl / Alt / Shift modifiers are
/// supported today; multi-key sequences like `"C-x e"` are accepted
/// syntactically but dropped with a warning, because reedline's
/// default emacs keymap owns chord state in a way that needs deeper
/// plumbing.
///
/// Unknown chord strings are skipped (the authoring surface is stable
/// — a rc file that predates a key-name addition should still load).
pub fn parse_chord(s: &str) -> Option<(KeyModifiers, KeyCode)> {
    let mut modifier = KeyModifiers::NONE;
    let parts = s.split(|c: char| c == '-' || c == '+');
    // Every part except the last is a modifier token.
    let mut collected: Vec<String> = parts.map(|p| p.to_string()).collect();
    let key_tok = collected.pop()?;
    for m in collected {
        match m.to_ascii_uppercase().as_str() {
            "C" | "CTRL"   => modifier |= KeyModifiers::CONTROL,
            "M" | "ALT"    => modifier |= KeyModifiers::ALT,
            "S" | "SHIFT"  => modifier |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    let key_code = match key_tok.to_ascii_lowercase().as_str() {
        "tab"    => KeyCode::Tab,
        "enter"  => KeyCode::Enter,
        "esc"    => KeyCode::Esc,
        "space"  => KeyCode::Char(' '),
        "up"     => KeyCode::Up,
        "down"   => KeyCode::Down,
        "left"   => KeyCode::Left,
        "right"  => KeyCode::Right,
        "home"   => KeyCode::Home,
        "end"    => KeyCode::End,
        "pageup"   | "pgup"   => KeyCode::PageUp,
        "pagedown" | "pgdn"   => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete"    => KeyCode::Delete,
        s if s.len() == 1 => KeyCode::Char(s.chars().next().unwrap()),
        _ => return None,
    };
    Some((modifier, key_code))
}

impl ZleEngine {
    /// Install rc-authored `defbind` keybindings on top of the current
    /// edit mode (emacs by default). Each (chord → function_name) pair
    /// becomes a reedline keybinding that emits
    /// `ReedlineEvent::ExecuteHostCommand(function_name)` — reedline
    /// returns `Signal::Success(function_name)` from `read_line`, the
    /// REPL runs it as a normal command, and the user's shell-source
    /// body (stored in `env.functions` by `frost-lisp`) fires.
    pub fn with_bindings<I>(mut self, bindings: I) -> Self
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut kb = default_emacs_keybindings();
        let mut applied = 0usize;
        for (chord, fn_name) in bindings {
            let Some((modifier, key_code)) = parse_chord(&chord) else {
                eprintln!("frost-zle: skipping unparseable keybinding: {chord:?}");
                continue;
            };
            kb.add_binding(
                modifier,
                key_code,
                ReedlineEvent::ExecuteHostCommand(fn_name),
            );
            applied += 1;
        }
        if applied == 0 {
            // Nothing to install — leave the engine as-is rather than
            // rebuild it, so we don't accidentally drop existing state.
            return self;
        }
        let taken = std::mem::replace(&mut self.inner, Reedline::create());
        self.inner = taken.with_edit_mode(Box::new(Emacs::new(kb)));
        self
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

    #[test]
    fn inject_prefill_does_not_panic_on_in_memory_engine() {
        // We can't easily inspect reedline's buffer from outside, but we
        // can confirm the call path compiles and doesn't panic — that's
        // the public-API contract we owe consumers.
        let mut zle = ZleEngine::in_memory();
        zle.inject_prefill("echo hello");
        zle.inject_prefill("");
    }

    #[test]
    fn parse_chord_single_char() {
        let (m, k) = parse_chord("l").unwrap();
        assert_eq!(m, KeyModifiers::NONE);
        assert_eq!(k, KeyCode::Char('l'));
    }

    #[test]
    fn parse_chord_ctrl_char() {
        let (m, k) = parse_chord("C-l").unwrap();
        assert_eq!(m, KeyModifiers::CONTROL);
        assert_eq!(k, KeyCode::Char('l'));
        let (m, k) = parse_chord("Ctrl-L").unwrap();
        assert_eq!(m, KeyModifiers::CONTROL);
        assert_eq!(k, KeyCode::Char('l'));
    }

    #[test]
    fn parse_chord_alt_char() {
        let (m, k) = parse_chord("M-?").unwrap();
        assert_eq!(m, KeyModifiers::ALT);
        assert_eq!(k, KeyCode::Char('?'));
    }

    #[test]
    fn parse_chord_named_key() {
        let (m, k) = parse_chord("C-tab").unwrap();
        assert_eq!(m, KeyModifiers::CONTROL);
        assert_eq!(k, KeyCode::Tab);
        let (m, k) = parse_chord("M-up").unwrap();
        assert_eq!(m, KeyModifiers::ALT);
        assert_eq!(k, KeyCode::Up);
    }

    #[test]
    fn parse_chord_multiple_modifiers() {
        let (m, k) = parse_chord("C-S-a").unwrap();
        assert_eq!(m, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(k, KeyCode::Char('a'));
    }

    #[test]
    fn parse_chord_plus_separator_works_too() {
        let (m, k) = parse_chord("ctrl+a").unwrap();
        assert_eq!(m, KeyModifiers::CONTROL);
        assert_eq!(k, KeyCode::Char('a'));
    }

    #[test]
    fn parse_chord_rejects_garbage() {
        assert!(parse_chord("Z-x").is_none());
        assert!(parse_chord("C-").is_none());
        assert!(parse_chord("").is_none());
    }
}
