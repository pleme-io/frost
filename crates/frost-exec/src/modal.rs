//! Modal state — typed wrapper over [`awase::BindingMap`].
//!
//! Frost-exec keeps a `ModalState` alongside [`crate::env::ShellEnv`]
//! so the interactive surface can route keystrokes through three named
//! modes:
//!
//! * **Normal** — passthrough to the PTY input (the default; behaves
//!   exactly like today's frost).
//! * **Command** — entered with `:`; accepts frost subcommands instead
//!   of writing to the PTY.
//! * **Search** — entered with `/` (forward) or `?` (backward); searches
//!   scrollback instead of running anything.
//!
//! `Esc` always drops back to Normal regardless of where you were.
//!
//! This is **additive**: the wire-in lives in the typed state machine
//! only. The frost-exec executor does not consult `ModalState` today;
//! the canonical input-side consumers (`frost-zle` / `frost-zle`'s
//! reedline engine) can opt in by calling [`ModalState::interpret_key`]
//! at their key-dispatch site without breaking existing pass-through.
//!
//! Re-uses the same primitive that mado / ayatsuri / namimado consume
//! (per the ★★ EMITTER SUBSTRATE rule) — no duplicate state machine.

use awase::{Action, BindingMap, Hotkey, Key, KeyMode, MatchContext, MatchResult, Modifiers};

/// Named mode identifiers for frost's interactive surface.
///
/// Stored as `&'static str` in the awase layer (which is the public
/// API). This enum gives frost-exec a typed border for matching on
/// the active mode without stringly comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrostMode {
    /// Default mode — keystrokes pass through to the PTY unchanged.
    Normal,
    /// `:` prefix — frost interprets the line as a frost subcommand
    /// (e.g. `:source rc.lisp`, `:set option`).
    Command,
    /// `/` forward / `?` backward — frost searches scrollback.
    Search,
}

impl FrostMode {
    /// Awase mode name. Matches the strings used in [`awase::KeyMode::new`].
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Command => "command",
            Self::Search => "search",
        }
    }

    /// Parse from the awase mode name. Returns `None` for unknown modes
    /// (a defensive border — should never happen if `ModalState` is the
    /// only constructor of the underlying [`BindingMap`]).
    #[must_use]
    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "normal" => Some(Self::Normal),
            "command" => Some(Self::Command),
            "search" => Some(Self::Search),
            _ => None,
        }
    }
}

/// Outcome of interpreting a single key event.
///
/// The interactive layer (frost-zle / frost-mcp send_keys) consults
/// this to decide whether the key should be forwarded to the PTY,
/// consumed by frost itself, or routed to a mode-specific buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyDecision {
    /// Pass the key through to the underlying surface (PTY in Normal,
    /// command-line buffer in Command, search buffer in Search).
    Passthrough,
    /// Frost consumed the key — for example, switching modes. The
    /// caller MUST NOT forward it to the PTY.
    Consumed,
    /// Mode change — frost dropped to / entered the named mode. The
    /// caller updates UI affordances (cursor shape, status line) and
    /// does NOT forward the key.
    ModeChange(FrostMode),
}

/// Modal state for frost's interactive surface.
///
/// Owns one [`BindingMap`] pre-populated with three modes (`normal`,
/// `command`, `search`) and the canonical mode-switching keys. The
/// initial mode is `Normal` — matching today's pass-through behavior
/// for callers that don't yet consult this state.
///
/// `Clone` is implemented manually because [`BindingMap`] is not
/// `Clone` upstream. The clone reconstructs a fresh state machine and
/// replays the active mode name — chord-pending state intentionally
/// resets (matches subshell-fork semantics: a forked frost starts
/// with no half-pressed chord).
#[derive(Debug)]
pub struct ModalState {
    binding_map: BindingMap,
}

impl Clone for ModalState {
    fn clone(&self) -> Self {
        let mut fresh = Self::new();
        // Replay the active mode. `new()` returns a state with all
        // three modes registered; set_mode for any of them succeeds.
        let mode_name = self.binding_map.current_mode().to_string();
        let _ = fresh.binding_map.set_mode(&mode_name);
        fresh
    }
}

impl ModalState {
    /// Build a fresh modal state with the three canonical frost modes
    /// and their mode-switching keys wired up.
    #[must_use]
    pub fn new() -> Self {
        let mut binding_map = BindingMap::new();

        // The `default` mode that BindingMap creates is unused by frost
        // — we replace it with `normal` so the names line up with
        // [`FrostMode`]. Passthrough = true so unknown keys flow to PTY.
        binding_map.add_mode(KeyMode::new("normal", true));
        binding_map.add_mode(KeyMode::new("command", false));
        binding_map.add_mode(KeyMode::new("search", false));

        // Per-mode mode-switch bindings. Each is one Hotkey + one Action
        // that does a `mode_switch` to the named target. `Esc` ALWAYS
        // drops to Normal regardless of where we were.

        // Normal -> Command via `:`
        let semicolon = Hotkey::new(Modifiers::NONE, Key::Semicolon);
        let colon = Hotkey::new(Modifiers::SHIFT, Key::Semicolon);
        let slash = Hotkey::new(Modifiers::NONE, Key::Slash);
        let qmark = Hotkey::new(Modifiers::SHIFT, Key::Slash);
        let esc = Hotkey::new(Modifiers::NONE, Key::Escape);

        if let Some(normal) = binding_map.mode_mut("normal") {
            normal.add_binding(awase::Binding::new(colon, Action::mode_switch("command")));
            // Also accept bare semicolon as a Command toggle on
            // keyboards where Shift isn't easily reportable (the
            // reedline / VT layer often hands us the produced char,
            // not the modifier set). Belt + suspenders.
            normal.add_binding(awase::Binding::new(
                semicolon,
                Action::mode_switch("command"),
            ));
            normal.add_binding(awase::Binding::new(slash, Action::mode_switch("search")));
            normal.add_binding(awase::Binding::new(qmark, Action::mode_switch("search")));
        }

        // Esc -> Normal from any non-Normal mode.
        for non_normal in ["command", "search"] {
            if let Some(mode) = binding_map.mode_mut(non_normal) {
                mode.add_binding(awase::Binding::new(esc, Action::mode_switch("normal")));
            }
        }

        // Make sure we start in Normal, not the BindingMap default.
        let _ = binding_map.set_mode("normal");

        Self { binding_map }
    }

    /// Currently active mode.
    #[must_use]
    pub fn current_mode(&self) -> FrostMode {
        FrostMode::from_str(self.binding_map.current_mode()).unwrap_or(FrostMode::Normal)
    }

    /// Whether the current mode is passthrough (Normal).
    ///
    /// Convenience for the interactive layer: callers that only want to
    /// know "should this key go to the PTY by default?" can branch on
    /// this without inspecting the mode enum.
    #[must_use]
    pub fn is_passthrough(&self) -> bool {
        self.binding_map.current_mode_passthrough()
    }

    /// Interpret a single key event. Returns a [`KeyDecision`].
    ///
    /// Errors from the underlying awase layer (unknown mode names,
    /// missing modes) are flattened to `Passthrough` — frost must
    /// never lose a keystroke because the modal layer is mis-configured.
    pub fn interpret_key(&mut self, hotkey: Hotkey) -> KeyDecision {
        let ctx = MatchContext::default();
        let result = self.binding_map.match_key(hotkey, &ctx);
        match result {
            MatchResult::NoMatch => KeyDecision::Passthrough,
            MatchResult::Remapped { to } => {
                // Treat remap as a regenerated key event; the caller's
                // input layer can re-feed `to` if it wants to. For now,
                // we treat it as a passthrough so behavior stays
                // backwards-compatible.
                let _ = to;
                KeyDecision::Passthrough
            }
            MatchResult::ChordPending { .. } => KeyDecision::Consumed,
            MatchResult::Matched { action, .. } => match action {
                Action::ModeSwitch(target) => {
                    if self.binding_map.set_mode(&target).is_ok() {
                        let mode = FrostMode::from_str(&target).unwrap_or(FrostMode::Normal);
                        KeyDecision::ModeChange(mode)
                    } else {
                        KeyDecision::Passthrough
                    }
                }
                _ => KeyDecision::Consumed,
            },
        }
    }

    /// Force a mode change (used by external triggers — e.g. `:` typed
    /// into a Command-mode buffer that wants to escape back to Normal).
    pub fn set_mode(&mut self, mode: FrostMode) -> Result<(), awase::AwaseError> {
        self.binding_map.set_mode(mode.as_str())
    }
}

impl Default for ModalState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_in_normal_mode() {
        let modal = ModalState::new();
        assert_eq!(modal.current_mode(), FrostMode::Normal);
        assert!(modal.is_passthrough());
    }

    #[test]
    fn colon_enters_command_mode() {
        let mut modal = ModalState::new();
        let colon = Hotkey::new(Modifiers::SHIFT, Key::Semicolon);
        let decision = modal.interpret_key(colon);
        assert!(matches!(
            decision,
            KeyDecision::ModeChange(FrostMode::Command)
        ));
        assert_eq!(modal.current_mode(), FrostMode::Command);
        assert!(!modal.is_passthrough());
    }

    #[test]
    fn slash_enters_search_mode() {
        let mut modal = ModalState::new();
        let slash = Hotkey::new(Modifiers::NONE, Key::Slash);
        let decision = modal.interpret_key(slash);
        assert!(matches!(
            decision,
            KeyDecision::ModeChange(FrostMode::Search)
        ));
        assert_eq!(modal.current_mode(), FrostMode::Search);
    }

    #[test]
    fn esc_returns_to_normal_from_command() {
        let mut modal = ModalState::new();
        modal.set_mode(FrostMode::Command).unwrap();
        let esc = Hotkey::new(Modifiers::NONE, Key::Escape);
        let decision = modal.interpret_key(esc);
        assert!(matches!(
            decision,
            KeyDecision::ModeChange(FrostMode::Normal)
        ));
        assert_eq!(modal.current_mode(), FrostMode::Normal);
    }

    #[test]
    fn unknown_key_in_normal_passes_through() {
        let mut modal = ModalState::new();
        let a = Hotkey::new(Modifiers::NONE, Key::A);
        let decision = modal.interpret_key(a);
        assert_eq!(decision, KeyDecision::Passthrough);
        // Mode unchanged.
        assert_eq!(modal.current_mode(), FrostMode::Normal);
    }

    #[test]
    fn frost_mode_str_roundtrip() {
        for mode in [FrostMode::Normal, FrostMode::Command, FrostMode::Search] {
            assert_eq!(FrostMode::from_str(mode.as_str()), Some(mode));
        }
        assert_eq!(FrostMode::from_str("bogus"), None);
    }
}
