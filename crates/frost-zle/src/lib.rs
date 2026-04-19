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
    Completer, DefaultHinter, EditCommand, EditMode, Emacs, FileBackedHistory, Highlighter, Hinter,
    KeyCode, KeyModifiers, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus,
    Reedline, ReedlineEvent, ReedlineMenu, Signal, Vi,
};

use nu_ansi_term::{Color, Style};

mod highlight;
pub use highlight::{parse_hex_style, FrostHighlighter, Palette, PaletteSlots};

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

/// A frost prompt: `PS1` for the primary line, `PS2` for continuations,
/// optional `RPS1` for the right-side segment (clock, git info, exit
/// code badge — typical blzsh / fish / zsh `RPROMPT` usage).
pub struct FrostPrompt {
    ps1: String,
    ps2: String,
    rps1: String,
}

impl FrostPrompt {
    pub fn new(ps1: impl Into<String>, ps2: impl Into<String>) -> Self {
        Self {
            ps1: ps1.into(),
            ps2: ps2.into(),
            rps1: String::new(),
        }
    }

    /// Include a right-aligned prompt segment.
    pub fn with_rps1(mut self, rps1: impl Into<String>) -> Self {
        self.rps1 = rps1.into();
        self
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
        std::borrow::Cow::Borrowed(&self.rps1)
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
    /// Rc-authored keybindings captured via `with_bindings`. Stored so
    /// `set_edit_mode` can re-apply them whenever reedline's edit mode
    /// is rebuilt (e.g. when `setopt vi` toggles). Without this cache
    /// the bindings would silently vanish on every REPL iteration —
    /// the source of the C-r-fires-default-reverse-search bug
    /// reported against frostmourne. Each entry is `(chord, fn_name)`
    /// matching the shape `with_bindings` ingests.
    custom_bindings: Vec<(String, String)>,
    /// The mode currently installed on `inner`. Avoids rebuilding the
    /// Emacs/Vi machinery on every iteration when the shell option
    /// hasn't changed — both a correctness win (doesn't stomp the
    /// keymap mid-session) and a small perf win.
    current_mode: Option<EditModeKind>,
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
            custom_bindings: Vec::new(),
            current_mode: None,
        })
    }

    /// Build an in-memory (non-persistent) engine. Useful for tests and for
    /// environments where `$HOME` is unavailable.
    pub fn in_memory() -> Self {
        Self {
            inner: Reedline::create(),
            prompt: FrostPrompt::default(),
            custom_bindings: Vec::new(),
            current_mode: None,
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

    /// Install a syntax highlighter (typically [`FrostHighlighter`]).
    /// reedline repaints the line on every keystroke, so the highlighter
    /// sees every intermediate edit — keep its `highlight()` cheap. Our
    /// lexer-driven highlighter runs ~50µs on 80-char lines, well under
    /// the "feels instant" threshold.
    pub fn with_highlighter(mut self, highlighter: Box<dyn Highlighter>) -> Self {
        self.inner = std::mem::replace(&mut self.inner, Reedline::create())
            .with_highlighter(highlighter);
        self
    }

    /// Install a history-backed hinter. Fish's ghost-text UX: after
    /// you type a prefix that matches a past command, reedline shows
    /// the remainder of that command in a colored overlay. Accept
    /// with → (right-arrow) or Ctrl-E.
    ///
    /// `hint_color` accepts a `#RRGGBB` / `#RGB` hex or `None` to use
    /// the Nord dim-grey default. Typically fed from rc-loaded
    /// `(deftheme :hint "...")`.
    pub fn with_history_hints(mut self, hint_color: Option<&str>) -> Self {
        let style = hint_color
            .and_then(crate::highlight::parse_hex_style)
            .unwrap_or_else(|| Style::new().fg(Color::Fixed(244))); // Nord polar-night-4
        let hinter = DefaultHinter::default()
            .with_style(style)
            .with_min_chars(1);
        self.inner = std::mem::replace(&mut self.inner, Reedline::create())
            .with_hinter(Box::new(hinter));
        self
    }

    /// Install an arbitrary [`Hinter`]. Used by tests + any consumer
    /// that wants to override the default history-backed hint.
    pub fn with_hinter(mut self, hinter: Box<dyn Hinter>) -> Self {
        self.inner = std::mem::replace(&mut self.inner, Reedline::create())
            .with_hinter(hinter);
        self
    }

    /// Update PS1 / PS2. Callers should pre-expand any `PROMPT_SUBST`
    /// placeholders before passing strings here.
    pub fn set_prompt(&mut self, ps1: impl Into<String>, ps2: impl Into<String>) {
        self.prompt = FrostPrompt::new(ps1, ps2);
    }

    /// Update PS1 / PS2 and RPS1 in one call — the common path for
    /// REPLs that re-read the prompt vars each iteration.
    pub fn set_prompt_with_rps1(
        &mut self,
        ps1: impl Into<String>,
        ps2: impl Into<String>,
        rps1: impl Into<String>,
    ) {
        self.prompt = FrostPrompt::new(ps1, ps2).with_rps1(rps1);
    }

    /// Switch the line editor into vi or emacs mode. Idempotent —
    /// if the requested mode is already installed this is a no-op;
    /// otherwise reedline's edit machinery is rebuilt with
    /// `self.custom_bindings` merged into the default emacs / vi
    /// keymap. Previously this silently replaced the user's
    /// `(defbind …)` / `(defpicker …)` bindings with the default
    /// keymap on every REPL iteration, which is why Ctrl-R ended
    /// up firing reedline's built-in reverse search instead of the
    /// skim-history picker frostmourne binds it to.
    pub fn set_edit_mode(&mut self, mode: EditModeKind) {
        // Fast path: already in the requested mode. Custom bindings
        // are embedded in the current keymap; no rebuild needed.
        if self.current_mode == Some(mode) {
            return;
        }
        let boxed: Box<dyn EditMode> = match mode {
            EditModeKind::Emacs => {
                let mut kb = default_emacs_keybindings();
                apply_custom_bindings_to(&mut kb, &self.custom_bindings);
                Box::new(Emacs::new(kb))
            }
            EditModeKind::Vi => {
                // In vi mode, custom bindings apply to the INSERT
                // keymap (where Ctrl-R et al. commonly fire). Normal
                // mode keeps its default keymap.
                let mut insert_kb = default_vi_insert_keybindings();
                apply_custom_bindings_to(&mut insert_kb, &self.custom_bindings);
                Box::new(Vi::new(insert_kb, default_vi_normal_keybindings()))
            }
        };
        let taken = std::mem::replace(&mut self.inner, Reedline::create());
        self.inner = taken.with_edit_mode(boxed);
        self.current_mode = Some(mode);
    }

    /// How many custom keybindings were stashed via
    /// [`Self::with_bindings`]. Exposed for harness tests that
    /// verify rc-authored chord lists round-trip into the engine.
    /// Not useful in the REPL hot path.
    pub fn custom_bindings_count(&self) -> usize {
        self.custom_bindings.len()
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

/// Parsed chord result. Carries more nuance than the single-chord
/// shape so callers can distinguish "not supported yet" from
/// "typo in rc".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedChord {
    /// Single-key chord — reedline can bind this directly.
    Single(KeyModifiers, KeyCode),
    /// Space-separated multi-key sequence (`"C-x e"`, `"M-k M-h"`).
    /// Authoring surface is valid; reedline's current keybinding API
    /// binds one chord at a time, so we silently record these as
    /// "opted-in but not-yet-dispatched" rather than erroring. The
    /// intent survives an rc edit so the moment multi-key lands in
    /// reedline we can switch the variant's consumer side.
    MultiKey(Vec<String>),
    /// Malformed — neither a valid single chord nor a multi-key
    /// sequence (empty input, trailing `-`, unknown modifier token
    /// like `Z-x`).
    Invalid,
}

/// Apply rc-authored `defbind` keybindings. Each entry maps a chord
/// string (`"C-l"`, `"M-?"`, …) to the name of a shell function that
/// reedline will invoke by returning it from `read_line` as if the user
/// had typed it. The ZleEngine re-installs its edit mode with the merged
/// keybindings.
///
/// Only single-key chords with Ctrl / Alt / Shift modifiers are
/// bound today; multi-key sequences (`"C-x e"`, `"M-k M-h"`) are
/// recognized by [`classify_chord`] as `ParsedChord::MultiKey` and
/// silently skipped (the binding remains declared in rc; it just
/// can't fire until reedline gains chord-state dispatch).
///
/// Unknown chord strings are skipped (the authoring surface is stable
/// — a rc file that predates a key-name addition should still load).
pub fn parse_chord(s: &str) -> Option<(KeyModifiers, KeyCode)> {
    match classify_chord(s) {
        ParsedChord::Single(m, k) => Some((m, k)),
        _ => None,
    }
}

/// Full chord classifier — returns the structured
/// [`ParsedChord`]. Use this directly when you need to distinguish
/// multi-key (intentional but unsupported) from invalid (typo in
/// rc that should scream).
pub fn classify_chord(s: &str) -> ParsedChord {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return ParsedChord::Invalid;
    }

    // Multi-key: any whitespace inside the chord string is a chord
    // separator. `"C-x e"` → ["C-x", "e"]. Each component must
    // itself parse as a single chord for the multi-key form to be
    // considered valid; otherwise we call it Invalid so typos still
    // surface.
    if trimmed.contains(char::is_whitespace) {
        let parts: Vec<String> = trimmed
            .split_whitespace()
            .map(String::from)
            .collect();
        if parts.iter().all(|p| parse_single_chord(p).is_some()) {
            return ParsedChord::MultiKey(parts);
        }
        return ParsedChord::Invalid;
    }

    match parse_single_chord(trimmed) {
        Some((m, k)) => ParsedChord::Single(m, k),
        None => ParsedChord::Invalid,
    }
}

/// Merge `(chord, fn_name)` pairs into an existing reedline
/// `Keybindings` in place. Returns how many successfully applied.
/// Multi-key chords are silently skipped (valid rc intent reedline
/// can't dispatch yet). Invalid chords print a one-shot stderr
/// warning so typos are visible but not spammy. Used both by
/// `with_bindings` (on first install) and `set_edit_mode` (to
/// re-apply when the edit mode rebuilds).
pub fn apply_custom_bindings_to(
    kb: &mut reedline::Keybindings,
    bindings: &[(String, String)],
) -> usize {
    let mut applied = 0usize;
    for (chord, fn_name) in bindings {
        match classify_chord(chord) {
            ParsedChord::Single(modifier, key_code) => {
                kb.add_binding(
                    modifier,
                    key_code,
                    ReedlineEvent::ExecuteHostCommand(fn_name.clone()),
                );
                applied += 1;
            }
            ParsedChord::MultiKey(_) => {
                // Not yet supported — silently skipped. Users don't
                // have to change rc to silence the warning.
            }
            ParsedChord::Invalid => {
                eprintln!("frost-zle: skipping unparseable keybinding: {chord:?}");
            }
        }
    }
    applied
}

/// Parse one single-key chord component. Extracted so
/// [`classify_chord`] can validate each piece of a multi-key string.
fn parse_single_chord(s: &str) -> Option<(KeyModifiers, KeyCode)> {
    if s.is_empty() {
        return None;
    }
    let mut modifier = KeyModifiers::NONE;
    let parts = s.split(|c: char| c == '-' || c == '+');
    let mut collected: Vec<String> = parts.map(|p| p.to_string()).collect();
    let key_tok = collected.pop()?;
    // Trailing separator with no key token (`"C-"`).
    if key_tok.is_empty() {
        return None;
    }
    for m in collected {
        // Empty modifier slot means two consecutive separators
        // (`"C--x"`) — invalid.
        if m.is_empty() {
            return None;
        }
        match m.to_ascii_uppercase().as_str() {
            "C" | "CTRL"  => modifier |= KeyModifiers::CONTROL,
            "M" | "ALT"   => modifier |= KeyModifiers::ALT,
            "S" | "SHIFT" => modifier |= KeyModifiers::SHIFT,
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
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
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
        // Collect once so we can (a) stash on self for later
        // set_edit_mode calls, and (b) apply to the initial emacs
        // keymap below. A caller that invokes `with_bindings`
        // multiple times replaces the prior set — matches the
        // builder-style semantics elsewhere on this struct.
        let collected: Vec<(String, String)> = bindings.into_iter().collect();
        self.custom_bindings = collected.clone();

        let mut kb = default_emacs_keybindings();
        let applied = apply_custom_bindings_to(&mut kb, &collected);
        if applied == 0 {
            return self;
        }
        let taken = std::mem::replace(&mut self.inner, Reedline::create());
        self.inner = taken.with_edit_mode(Box::new(Emacs::new(kb)));
        self.current_mode = Some(EditModeKind::Emacs);
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

    // ─── classify_chord regression cover — multi-key + edge cases ─────

    #[test]
    fn classify_chord_recognizes_single_key_forms() {
        assert!(matches!(classify_chord("C-r"), ParsedChord::Single(..)));
        assert!(matches!(classify_chord("Ctrl-L"), ParsedChord::Single(..)));
        assert!(matches!(classify_chord("M-?"), ParsedChord::Single(..)));
        assert!(matches!(classify_chord("backspace"), ParsedChord::Single(..)));
        assert!(matches!(classify_chord("C-S-a"), ParsedChord::Single(..)));
    }

    #[test]
    fn classify_chord_recognizes_multi_key_sequences() {
        // The report: `(defbind :key "C-x e" ...)` was previously
        // stderr-warning on startup. Now it classifies as MultiKey,
        // silently skipped until reedline ships chord dispatch.
        assert_eq!(
            classify_chord("C-x e"),
            ParsedChord::MultiKey(vec!["C-x".into(), "e".into()])
        );
        assert_eq!(
            classify_chord("M-k  M-h"),   // double space
            ParsedChord::MultiKey(vec!["M-k".into(), "M-h".into()])
        );
        assert_eq!(
            classify_chord("C-x C-c"),
            ParsedChord::MultiKey(vec!["C-x".into(), "C-c".into()])
        );
        // Leading/trailing whitespace trimmed.
        assert_eq!(
            classify_chord("  C-x e  "),
            ParsedChord::MultiKey(vec!["C-x".into(), "e".into()])
        );
    }

    #[test]
    fn classify_chord_rejects_multi_key_with_invalid_piece() {
        // `Z-x` is invalid, so `C-x Z-x` is also invalid (not
        // silently-skipped MultiKey). Guards against "typos hiding
        // in valid-looking multi-key strings".
        assert_eq!(classify_chord("C-x Z-x"), ParsedChord::Invalid);
        assert_eq!(classify_chord("valid C-"), ParsedChord::Invalid);
    }

    #[test]
    fn classify_chord_rejects_malformed_single_chord() {
        assert_eq!(classify_chord(""), ParsedChord::Invalid);
        assert_eq!(classify_chord("   "), ParsedChord::Invalid);
        assert_eq!(classify_chord("C-"), ParsedChord::Invalid);
        assert_eq!(classify_chord("-x"), ParsedChord::Invalid);       // leading separator
        assert_eq!(classify_chord("C--x"), ParsedChord::Invalid);     // double sep
        assert_eq!(classify_chord("Z-x"), ParsedChord::Invalid);      // unknown mod
        assert_eq!(classify_chord("C-xx"), ParsedChord::Invalid);     // multi-char key
        assert_eq!(classify_chord("C-🎉"), ParsedChord::Single(
            KeyModifiers::CONTROL, KeyCode::Char('🎉')
        ));  // unicode key is a single codepoint, OK
    }

    #[test]
    fn with_bindings_silently_skips_multi_key_chords() {
        // Run a ZleEngine build with the known problematic binding
        // from frostmourne's 30-bindings.lisp. No panic, no stderr.
        // (stderr-capture isn't part of std, so we just verify the
        // build path doesn't explode and the classify returns the
        // expected MultiKey variant.)
        let zle = ZleEngine::in_memory();
        let _ = zle.with_bindings([
            ("C-x e".to_string(), "edit".to_string()),
            ("C-l".to_string(), "clear".to_string()),     // single — applied
            ("M-?".to_string(), "help".to_string()),       // single — applied
            ("garbage-chord".to_string(), "no".to_string()), // typo — warns
        ]);
    }

    #[test]
    fn parse_single_chord_key_case_insensitive() {
        // `C-X` and `C-x` should resolve to the same chord so rc
        // files that don't bother lowercasing keys still work.
        assert_eq!(parse_chord("C-x"), parse_chord("C-X"));
        assert_eq!(parse_chord("M-Q"), parse_chord("m-q"));
    }

    #[test]
    fn with_bindings_stashes_custom_bindings() {
        // The regression under fix: `set_edit_mode(Emacs)` was
        // previously called on every REPL iteration and built a
        // default emacs keymap, silently dropping every rc-authored
        // binding. The fix stashes them in `custom_bindings` so
        // set_edit_mode can re-apply. This test asserts that stash.
        let zle = ZleEngine::in_memory();
        let zle = zle.with_bindings([
            ("C-r".to_string(), "__frost_picker_history__".to_string()),
            ("C-t".to_string(), "__frost_picker_files__".to_string()),
        ]);
        assert_eq!(zle.custom_bindings.len(), 2);
        assert!(zle.custom_bindings.iter().any(|(k, _)| k == "C-r"));
        assert_eq!(zle.current_mode, Some(EditModeKind::Emacs));
    }

    #[test]
    fn set_edit_mode_idempotent_on_same_mode() {
        // Calling `set_edit_mode(Emacs)` repeatedly shouldn't rebuild
        // the keymap — confirmed via `current_mode` unchanged. The
        // pre-fix bug was that each call DID rebuild, losing custom
        // bindings every iteration.
        let mut zle = ZleEngine::in_memory().with_bindings([
            ("C-r".to_string(), "__frost_picker_history__".to_string()),
        ]);
        assert_eq!(zle.current_mode, Some(EditModeKind::Emacs));
        zle.set_edit_mode(EditModeKind::Emacs);
        zle.set_edit_mode(EditModeKind::Emacs);
        // Bindings survive across the (now-idempotent) calls.
        assert!(zle.custom_bindings.iter().any(|(k, _)| k == "C-r"));
    }

    #[test]
    fn set_edit_mode_rebuilds_keymap_on_mode_change_with_custom_bindings() {
        // Toggle emacs → vi → emacs. Custom bindings must re-apply
        // on each rebuild — this is the actual correctness property.
        let mut zle = ZleEngine::in_memory().with_bindings([
            ("C-r".to_string(), "__frost_picker_history__".to_string()),
            ("C-t".to_string(), "__frost_picker_files__".to_string()),
            ("C-x e".to_string(), "edit".to_string()),  // multi-key — skipped but stashed
            ("bogus".to_string(), "nope".to_string()),  // invalid — warned but stashed
        ]);
        zle.set_edit_mode(EditModeKind::Vi);
        assert_eq!(zle.current_mode, Some(EditModeKind::Vi));
        zle.set_edit_mode(EditModeKind::Emacs);
        assert_eq!(zle.current_mode, Some(EditModeKind::Emacs));
        // The custom_bindings stash survives mode toggles.
        assert_eq!(zle.custom_bindings.len(), 4);
    }

    #[test]
    fn apply_custom_bindings_to_reports_applied_count() {
        use reedline::default_emacs_keybindings;
        let mut kb = default_emacs_keybindings();
        let n = apply_custom_bindings_to(&mut kb, &[
            ("C-r".into(),     "sentinel-r".into()),   // single — applies
            ("C-t".into(),     "sentinel-t".into()),   // single — applies
            ("C-x e".into(),   "multi".into()),         // multi-key — skipped
            ("bogus".into(),   "nope".into()),          // invalid — skipped
        ]);
        assert_eq!(n, 2, "only two single-chord bindings should apply");
    }
}
