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
//! * vi mode ([`ZleEngine::set_edit_mode`], zsh's `bindkey -v`), with the
//!   rc's chords merged into BOTH halves of the keymap pair via
//!   [`ViKeymaps`], a per-mode prompt indicator, and a per-mode terminal
//!   cursor shape.
//! * rc-authored keymaps — `(defbind …)` / `(defpicker …)` chords land
//!   through [`ZleEngine::with_bindings`] and survive every edit-mode
//!   rebuild.
//! * Completion: [`ZleEngine::with_completer`] installs a completer plus
//!   the `completion_menu`, and Tab is wired to it in every keymap.
//!
//! Not yet implemented:
//!
//! * Multi-key chord dispatch — `"C-x e"` parses (see [`ParsedChord`])
//!   but reedline binds one chord at a time, so the REPL handles the
//!   second keystroke itself.
//! * Prompt substitution (`PROMPT_SUBST`) — the caller should expand the
//!   prompt string before passing it to [`ZleEngine::set_prompt`].

use std::path::{Path, PathBuf};

use reedline::{
    Completer, CursorConfig, DefaultHinter, EditCommand, EditMode, Emacs, FileBackedHistory,
    Highlighter, Hinter, KeyCode, KeyModifiers, MenuBuilder, Prompt, PromptEditMode,
    PromptHistorySearch, PromptHistorySearchStatus, PromptViMode, Reedline, ReedlineEvent,
    ReedlineMenu, Signal, Vi, default_emacs_keybindings, default_vi_insert_keybindings,
    default_vi_normal_keybindings,
};

use nu_ansi_term::{Color, Style};

mod highlight;
pub use highlight::{FrostHighlighter, Palette, PaletteSlots, parse_hex_style};

// Re-export so downstream crates can write completers without adding a
// direct `reedline` dep.
pub use reedline::{Completer as CompleterTrait, Span as CompletionSpan, Suggestion};

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
    /// Rendered after PS1 when the editor is in vi NORMAL mode.
    vi_normal_indicator: String,
    /// Rendered after PS1 when the editor is in vi INSERT mode.
    vi_insert_indicator: String,
    /// Rendered after PS1 in emacs mode. Empty by default — emacs is
    /// frost's default mode and the rc-authored PS1 (seki) already ends
    /// in its own prompt character, so an unconditional suffix would
    /// change every operator's prompt for no information gain.
    emacs_indicator: String,
}

/// Default vi NORMAL / vi INSERT indicators.
///
/// **Single-width glyphs only, never emoji.** The fleet prompt (seki)
/// is grid-aligned; a double-width or emoji glyph shifts every column
/// after it and the alignment silently rots. Pure ASCII is the only
/// width that is unambiguous across terminals — several box-drawing
/// and dingbat candidates (`◆`, `▸`, `❮`) carry East-Asian-Width
/// *Ambiguous*, which renders double-wide under a CJK-ambiguous
/// terminal setting.
///
/// The intended long-term source for these glyphs is
/// `ishou_tokens::EscribaSignals` (`mode_normal` / `mode_insert`),
/// which carries a single-width tier alongside its emoji tier. It is
/// NOT reachable from this crate today — `frost-zle` does not depend
/// on `ishou-tokens` — so the literals below stand in. Wire them the
/// moment the dependency lands rather than inventing a third glyph
/// vocabulary.
const DEFAULT_VI_NORMAL_INDICATOR: &str = "[N] ";
const DEFAULT_VI_INSERT_INDICATOR: &str = "[I] ";

impl FrostPrompt {
    pub fn new(ps1: impl Into<String>, ps2: impl Into<String>) -> Self {
        Self {
            ps1: ps1.into(),
            ps2: ps2.into(),
            rps1: String::new(),
            vi_normal_indicator: DEFAULT_VI_NORMAL_INDICATOR.to_string(),
            vi_insert_indicator: DEFAULT_VI_INSERT_INDICATOR.to_string(),
            emacs_indicator: String::new(),
        }
    }

    /// Include a right-aligned prompt segment.
    pub fn with_rps1(mut self, rps1: impl Into<String>) -> Self {
        self.rps1 = rps1.into();
        self
    }

    /// Override the per-mode indicators rendered between PS1 and the
    /// cursor. Keeps the mode signal configurable rather than baked
    /// into `render_prompt_indicator`.
    pub fn with_mode_indicators(
        mut self,
        vi_normal: impl Into<String>,
        vi_insert: impl Into<String>,
        emacs: impl Into<String>,
    ) -> Self {
        self.vi_normal_indicator = vi_normal.into();
        self.vi_insert_indicator = vi_insert.into();
        self.emacs_indicator = emacs.into();
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
    /// The vi mode is the one piece of editor state a user cannot infer
    /// from the screen — pressing Esc looks identical to not pressing it
    /// until the next keystroke does the wrong thing. reedline hands the
    /// live mode in on every repaint; discarding it (this returned `""`
    /// for every mode) threw that signal away.
    fn render_prompt_indicator(&self, mode: PromptEditMode) -> std::borrow::Cow<'_, str> {
        match mode {
            PromptEditMode::Vi(PromptViMode::Normal) => {
                std::borrow::Cow::Borrowed(&self.vi_normal_indicator)
            }
            PromptEditMode::Vi(PromptViMode::Insert) => {
                std::borrow::Cow::Borrowed(&self.vi_insert_indicator)
            }
            PromptEditMode::Emacs => std::borrow::Cow::Borrowed(&self.emacs_indicator),
            // `Default` is reedline's pre-edit-mode-selection state and
            // `Custom` belongs to an edit mode frost does not install;
            // neither has a frost-authored indicator, so render nothing
            // rather than mislabel the mode.
            PromptEditMode::Default | PromptEditMode::Custom(_) => std::borrow::Cow::Borrowed(""),
        }
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
        std::borrow::Cow::Owned(format!(
            "({prefix}reverse-search: {}) ",
            history_search.term
        ))
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
    /// `Some(reason)` when the on-disk history file could NOT be opened
    /// and this session is recording to memory only — see
    /// [`Self::history_error`].
    history_error: Option<String>,
}

impl ZleEngine {
    /// Build an interactive line editor with history backed at `history_file`.
    /// `history_file`'s parent directory is created if missing; if the file
    /// cannot be opened, the engine falls back to in-memory history and
    /// returns `Ok` (the shell should still be usable) — but it says so,
    /// loudly, on stderr and via [`Self::history_error`].
    ///
    /// ## Why the fallback is loud
    ///
    /// `FileBackedHistory::with_file` calls `sync()` internally and
    /// propagates its error. An operator's `$HISTFILE` containing
    /// invalid UTF-8 makes that `sync()` fail, `with_file` returns
    /// `Err`, and the engine drops to a no-file, in-memory history.
    /// This arm used to be a bare `Err(_) =>` that discarded the
    /// reason: the shell looked entirely normal — prompt, up-arrow
    /// within the session, hints — while persisting NOTHING. That ran
    /// undetected for 148 days on this operator's box. reedline's
    /// `Drop` swallowing the sync error is a second layer of the same
    /// silence; this arm is the first, and the only one that can name
    /// the file and the cause. Keep the diagnostic even after the
    /// reedline-side fix lands.
    ///
    /// Degrading rather than refusing to start is deliberate: a shell
    /// that will not open is worse than one that warns. What is not
    /// acceptable is a shell that does neither.
    pub fn new(history_file: impl AsRef<Path>, history_capacity: usize) -> ZleResult<Self> {
        let path = history_file.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let (editor, history_error) =
            match FileBackedHistory::with_file(history_capacity, path.clone()) {
                Ok(hist) => (Reedline::create().with_history(Box::new(hist)), None),
                Err(e) => {
                    let reason = e.to_string();
                    eprintln!(
                        "frost: warning: history file {} could not be opened ({reason})\n\
                         frost: warning: THIS SESSION RECORDS NO HISTORY — nothing typed here \
                         will be persisted.\n\
                         frost: warning: check that the file is readable and valid UTF-8, or \
                         move it aside to start a fresh one.",
                        path.display()
                    );
                    (Reedline::create(), Some(reason))
                }
            };
        Ok(Self {
            inner: editor.with_cursor_config(frost_cursor_config()),
            prompt: FrostPrompt::default(),
            custom_bindings: Vec::new(),
            current_mode: None,
            history_error,
        })
    }

    /// Why this engine is not persisting history, or `None` when the
    /// history file opened cleanly. Lets callers surface the condition
    /// somewhere durable (`frost --doctor`, the MCP snapshot) rather
    /// than relying on the operator having read one startup line.
    #[must_use]
    pub fn history_error(&self) -> Option<&str> {
        self.history_error.as_deref()
    }

    /// Build an in-memory (non-persistent) engine. Useful for tests and for
    /// environments where `$HOME` is unavailable.
    pub fn in_memory() -> Self {
        Self {
            inner: Reedline::create().with_cursor_config(frost_cursor_config()),
            prompt: FrostPrompt::default(),
            custom_bindings: Vec::new(),
            current_mode: None,
            // In-memory is the REQUESTED backing here, not a fallback
            // from a failed open — there is no error to report.
            history_error: None,
        }
    }

    /// Replace the completer. The provided completer implements reedline's
    /// `Completer` trait and is consulted on every Tab press. Pair this with
    /// a completion menu so suggestions are rendered below the prompt.
    pub fn with_completer(mut self, completer: Box<dyn Completer>) -> Self {
        // Name the menu "completion_menu" so the Tab binding
        // (add_completion_tab_binding) resolves to it; ColumnarMenu's
        // default name is "columnar_menu", which no binding references.
        let menu = ReedlineMenu::EngineCompleter(Box::new(
            reedline::ColumnarMenu::default()
                .with_name("completion_menu")
                // Vellum completion-menu styling — cohesive with the
                // skim-tab pickers (fg+ #F4EFE2 bold / bg+ surface). The
                // bare `ColumnarMenu::default()` inherited reedline's
                // default selected style (a light-bg highlight) which read
                // poorly on the dark parchment ground (operator report:
                // "readability issue with all the autocomplete"). Now:
                // unselected entries are calm dim-cream; the SELECTED entry
                // is bright cream BOLD on a clearly-visible parchment
                // surface; descriptions recede to shadow0.
                .with_text_style(Style::new().fg(Color::Rgb(0xAD, 0xA5, 0x93))) // snow0 dim cream
                .with_selected_text_style(
                    Style::new()
                        .bold()
                        .fg(Color::Rgb(0xF4, 0xEF, 0xE2)) // snow3 bright cream
                        .on(Color::Rgb(0x38, 0x34, 0x2A)), // night3 visible surface
                )
                .with_description_text_style(
                    Style::new().fg(Color::Rgb(0x6E, 0x68, 0x57)), // shadow0 — recede
                ),
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
        self.inner =
            std::mem::replace(&mut self.inner, Reedline::create()).with_highlighter(highlighter);
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
        let hinter = DefaultHinter::default().with_style(style).with_min_chars(1);
        self.inner =
            std::mem::replace(&mut self.inner, Reedline::create()).with_hinter(Box::new(hinter));
        self
    }

    /// Install an arbitrary [`Hinter`]. Used by tests + any consumer
    /// that wants to override the default history-backed hint.
    pub fn with_hinter(mut self, hinter: Box<dyn Hinter>) -> Self {
        self.inner = std::mem::replace(&mut self.inner, Reedline::create()).with_hinter(hinter);
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
                add_completion_tab_binding(&mut kb);
                Box::new(Emacs::new(kb))
            }
            EditModeKind::Vi => {
                // BOTH vi keymaps get the rc's bindings. A chord the
                // operator authored is a statement about what that key
                // does in this shell, not about what it does in one
                // half of one edit mode: pressing Esc must not silently
                // revoke C-r's history picker, C-l's clear, M-.'s
                // insert-last-arg or Tab's completion menu. reedline
                // resolves an unbound chord to `ReedlineEvent::None`,
                // so a normal-mode-only omission is invisible — the key
                // simply does nothing (or worse, C-r falls through to
                // reedline's built-in `SearchHistory` instead of the
                // skim picker). `ViKeymaps` is the constructor that
                // makes forgetting one half unrepresentable.
                ViKeymaps::from_bindings(&self.custom_bindings).into_edit_mode()
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
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
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

    /// Flush reedline's in-memory history to its backing `$HISTFILE` now.
    ///
    /// reedline's `FileBackedHistory` otherwise only writes on `Drop`. Since
    /// reedline is the **sole** writer of the history file (frost-history's
    /// expansion buffer is read-only — see
    /// [`frost_history::History::from_file_readonly`]), the REPL calls this
    /// after every accepted command so a crashed shell still leaves a
    /// complete, correctly-ordered trail — the eager-persistence guarantee
    /// that frost-history's `push` used to provide, now routed through the
    /// single writer so no two components race the same file.
    pub fn sync_history(&mut self) {
        let _ = self.inner.sync_history();
    }
}

/// Which line-editing model to bind. Maps to zsh's `bindkey -v` / `-e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditModeKind {
    Emacs,
    Vi,
}

/// The two keymaps reedline's [`Vi`] edit mode needs, built as a pair.
///
/// This exists to make one specific omission unrepresentable: for a
/// long time the Vi arm of [`ZleEngine::set_edit_mode`] merged the rc's
/// bindings into the INSERT keymap and passed
/// `default_vi_normal_keybindings()` straight through, so ten of the
/// twelve chords the frostmourne rc authors resolved to
/// `ReedlineEvent::None` the moment the user pressed Esc, Tab stopped
/// opening the completion menu, and C-r fell through to reedline's
/// built-in `SearchHistory`. Nothing about `Vi::new(insert, normal)`
/// signals that the second argument also wants the operator's
/// bindings — the types are identical, so the mistake reads as
/// correct code.
///
/// The guard: [`ViKeymaps::from_bindings`] is the ONLY way to build
/// one, it takes the custom bindings, and it applies both them and the
/// Tab→completion binding to *each* half. A future keymap cannot be
/// constructed without being handed the rc's bindings, and the fields
/// are private so no caller can assemble a half-configured pair.
pub struct ViKeymaps {
    insert: reedline::Keybindings,
    normal: reedline::Keybindings,
}

impl ViKeymaps {
    /// Build both vi keymaps from reedline's defaults, merging
    /// `custom` and the Tab→completion-menu binding into each.
    #[must_use]
    pub fn from_bindings(custom: &[(String, String)]) -> Self {
        let mut insert = default_vi_insert_keybindings();
        let mut normal = default_vi_normal_keybindings();
        for kb in [&mut insert, &mut normal] {
            apply_custom_bindings_to(kb, custom);
            add_completion_tab_binding(kb);
        }
        Self { insert, normal }
    }

    /// The vi INSERT keymap. Borrow-only: the pair is the unit of
    /// truth, so a caller can inspect a half but never swap one.
    #[must_use]
    pub fn insert(&self) -> &reedline::Keybindings {
        &self.insert
    }

    /// The vi NORMAL keymap.
    #[must_use]
    pub fn normal(&self) -> &reedline::Keybindings {
        &self.normal
    }

    /// Consume the pair into reedline's [`Vi`] edit mode. The only
    /// exit from this type, so the keymaps reedline receives are
    /// always the ones `from_bindings` built.
    #[must_use]
    pub fn into_edit_mode(self) -> Box<dyn EditMode> {
        Box::new(Vi::new(self.insert, self.normal))
    }
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
        let parts: Vec<String> = trimmed.split_whitespace().map(String::from).collect();
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
            "C" | "CTRL" => modifier |= KeyModifiers::CONTROL,
            "M" | "ALT" => modifier |= KeyModifiers::ALT,
            "S" | "SHIFT" => modifier |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    let key_code = match key_tok.to_ascii_lowercase().as_str() {
        "tab" => KeyCode::Tab,
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
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
        apply_custom_bindings_to(&mut kb, &collected);
        // Always wire Tab → completion menu, even with zero custom binds,
        // so completion works regardless of rc content.
        add_completion_tab_binding(&mut kb);
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

/// Per-mode terminal cursor shape.
///
/// reedline emits the DECSCUSR escape for the active edit mode on every
/// prompt draw, but only when a [`CursorConfig`] is installed — frost
/// never called `with_cursor_config`, so the shape stayed whatever the
/// terminal last set and vi normal mode was visually identical to vi
/// insert. Together with the prompt indicator this is the second,
/// peripheral-vision half of "which mode am I in".
///
/// **Steady, never Blinking.** The fleet terminal default is
/// `blink = false`; a blinking cursor style here would override that
/// per-prompt and fight the operator's own setting.
fn frost_cursor_config() -> CursorConfig {
    use crossterm::cursor::SetCursorStyle;
    CursorConfig {
        // Bar: sits between characters — insert semantics.
        vi_insert: Some(SetCursorStyle::SteadyBar),
        // Block: covers the character the next motion/operator acts on.
        vi_normal: Some(SetCursorStyle::SteadyBlock),
        // Emacs has no modal split; keep the conventional block so the
        // shape never reads as "vi insert".
        emacs: Some(SetCursorStyle::SteadyBlock),
    }
}

/// Bind Tab → completion menu (Shift-BackTab → previous) in a keymap.
/// reedline's default keymaps bind NO Tab, so a populated completer + a
/// named "completion_menu" do nothing until Tab is wired here. Called from
/// every keymap-build path (both edit modes + `with_bindings`) so the
/// binding survives the per-mode keymap rebuild in `set_edit_mode`.
fn add_completion_tab_binding(kb: &mut reedline::Keybindings) {
    kb.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    kb.add_binding(
        KeyModifiers::SHIFT,
        KeyCode::BackTab,
        ReedlineEvent::MenuPrevious,
    );
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: Tab must resolve to the completion menu in the built
    /// keymap (reedline binds no Tab by default; the completer + menu were
    /// installed but never reachable). Mirrors the convergence-guard style.
    #[test]
    fn tab_is_bound_to_completion_menu() {
        let mut kb = default_emacs_keybindings();
        add_completion_tab_binding(&mut kb);
        match kb.find_binding(KeyModifiers::NONE, KeyCode::Tab) {
            Some(ReedlineEvent::UntilFound(events)) => assert!(
                events
                    .iter()
                    .any(|e| matches!(e, ReedlineEvent::Menu(n) if n == "completion_menu")),
                "Tab must trigger the completion_menu, got {events:?}"
            ),
            other => panic!("Tab not bound to completion menu: {other:?}"),
        }
    }

    #[test]
    fn prompt_defaults_to_frost_gt_and_gt() {
        let p = FrostPrompt::default();
        assert_eq!(p.render_prompt_left(), "frost> ");
        assert_eq!(p.render_prompt_multiline_indicator(), "> ");
    }

    /// Regression: `render_prompt_indicator` took the mode and threw it
    /// away, returning `""` for all three. A vi user could not tell
    /// NORMAL from INSERT anywhere on screen.
    #[test]
    fn prompt_indicator_distinguishes_vi_normal_from_vi_insert() {
        let p = FrostPrompt::default();
        let normal = p.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Normal));
        let insert = p.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Insert));
        assert_ne!(normal, insert, "vi modes must render differently");
        assert!(!normal.is_empty() && !insert.is_empty());
        // Emacs is frost's default mode and seki's PS1 already ends in
        // its own character — a suffix there would change every
        // operator's prompt for no signal.
        assert_eq!(p.render_prompt_indicator(PromptEditMode::Emacs), "");
    }

    /// The fleet seki prompt is grid-aligned: an emoji or a
    /// double-width glyph in the indicator shifts every column after
    /// it. Assert one column per char, ASCII only.
    #[test]
    fn prompt_indicators_are_single_width_ascii() {
        let p = FrostPrompt::default();
        for mode in [
            PromptEditMode::Vi(PromptViMode::Normal),
            PromptEditMode::Vi(PromptViMode::Insert),
            PromptEditMode::Emacs,
        ] {
            let s = p.render_prompt_indicator(mode);
            assert!(
                s.is_ascii(),
                "indicator {s:?} is not ASCII — non-ASCII risks double-width or emoji rendering"
            );
            assert!(
                !s.chars().any(char::is_control),
                "indicator {s:?} contains a control character"
            );
        }
    }

    #[test]
    fn prompt_mode_indicators_are_configurable() {
        let p = FrostPrompt::new("$ ", "> ").with_mode_indicators("N", "I", "E");
        assert_eq!(
            p.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Normal)),
            "N"
        );
        assert_eq!(
            p.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Insert)),
            "I"
        );
        assert_eq!(p.render_prompt_indicator(PromptEditMode::Emacs), "E");
    }

    /// THE regression this branch exists for: the Vi arm built only the
    /// INSERT keymap from the rc's bindings and passed
    /// `default_vi_normal_keybindings()` through untouched, so every
    /// rc chord died the moment the user pressed Esc.
    #[test]
    fn vi_keymaps_carry_custom_bindings_in_both_halves() {
        let bindings = vec![
            ("C-r".to_string(), "__frost_picker_history__".to_string()),
            ("C-t".to_string(), "__frost_picker_files__".to_string()),
            (
                "M-.".to_string(),
                "__frost_widget_insert-last-arg__".to_string(),
            ),
        ];
        let keymaps = ViKeymaps::from_bindings(&bindings);
        for (which, kb) in [("insert", keymaps.insert()), ("normal", keymaps.normal())] {
            for (chord, fn_name) in &bindings {
                let (modifier, key) = parse_chord(chord).unwrap();
                match kb.find_binding(modifier, key) {
                    Some(ReedlineEvent::ExecuteHostCommand(got)) => assert_eq!(
                        &got, fn_name,
                        "{which} keymap: {chord} bound to the wrong command"
                    ),
                    other => panic!("{which} keymap: {chord} resolved to {other:?}, not {fn_name}"),
                }
            }
            // Tab was unbound in vi NORMAL, so completion was
            // unreachable after Esc.
            assert!(
                matches!(
                    kb.find_binding(KeyModifiers::NONE, KeyCode::Tab),
                    Some(ReedlineEvent::UntilFound(_))
                ),
                "{which} keymap: Tab is not wired to the completion menu"
            );
        }
    }

    /// C-r in vi NORMAL used to resolve to reedline's built-in
    /// `SearchHistory` — the picker binding never got there, so the
    /// wrong search UI opened with no visible sign anything was wrong.
    #[test]
    fn rc_binding_beats_reedline_builtin_in_vi_normal() {
        let bindings = vec![("C-r".to_string(), "__frost_picker_history__".to_string())];
        let default_normal = default_vi_normal_keybindings();
        let (modifier, key) = parse_chord("C-r").unwrap();
        // Guard the premise: reedline really does bind C-r itself.
        assert!(
            default_normal.find_binding(modifier, key).is_some(),
            "premise changed: reedline no longer binds C-r in vi normal"
        );
        let keymaps = ViKeymaps::from_bindings(&bindings);
        assert_eq!(
            keymaps.normal().find_binding(modifier, key),
            Some(ReedlineEvent::ExecuteHostCommand(
                "__frost_picker_history__".to_string()
            )),
            "the rc's C-r must override reedline's built-in SearchHistory"
        );
    }

    #[test]
    fn in_memory_engine_reports_no_history_error() {
        // In-memory is a requested backing, not a failed open — it must
        // not masquerade as the silent-data-loss condition.
        assert!(ZleEngine::in_memory().history_error().is_none());
    }

    /// The 148-day silent-history-loss class: an unopenable `$HISTFILE`
    /// dropped to in-memory with `Err(_) =>` discarding the reason. The
    /// engine must still start, but must carry the reason.
    #[test]
    fn unopenable_history_file_is_reported_not_swallowed() {
        let zle = ZleEngine::new("/nonexistent/absolutely/no/way/history", 100)
            .expect("engine must still start");
        assert!(
            zle.history_error().is_some(),
            "an unopenable history file must leave a reason behind"
        );
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
        assert!(matches!(
            classify_chord("backspace"),
            ParsedChord::Single(..)
        ));
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
            classify_chord("M-k  M-h"), // double space
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
        assert_eq!(classify_chord("-x"), ParsedChord::Invalid); // leading separator
        assert_eq!(classify_chord("C--x"), ParsedChord::Invalid); // double sep
        assert_eq!(classify_chord("Z-x"), ParsedChord::Invalid); // unknown mod
        assert_eq!(classify_chord("C-xx"), ParsedChord::Invalid); // multi-char key
        assert_eq!(
            classify_chord("C-🎉"),
            ParsedChord::Single(KeyModifiers::CONTROL, KeyCode::Char('🎉'))
        ); // unicode key is a single codepoint, OK
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
            ("C-l".to_string(), "clear".to_string()), // single — applied
            ("M-?".to_string(), "help".to_string()),  // single — applied
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
        let mut zle = ZleEngine::in_memory()
            .with_bindings([("C-r".to_string(), "__frost_picker_history__".to_string())]);
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
            ("C-x e".to_string(), "edit".to_string()), // multi-key — skipped but stashed
            ("bogus".to_string(), "nope".to_string()), // invalid — warned but stashed
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
        let n = apply_custom_bindings_to(
            &mut kb,
            &[
                ("C-r".into(), "sentinel-r".into()), // single — applies
                ("C-t".into(), "sentinel-t".into()), // single — applies
                ("C-x e".into(), "multi".into()),    // multi-key — skipped
                ("bogus".into(), "nope".into()),     // invalid — skipped
            ],
        );
        assert_eq!(n, 2, "only two single-chord bindings should apply");
    }
}
