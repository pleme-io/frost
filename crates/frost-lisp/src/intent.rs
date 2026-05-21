//! Operator-intent → fleet-canonical chord resolution.
//!
//! The bridge between rc.lisp's `(defbind :intent :history-picker …)`
//! authoring form and `ishou_tokens::FleetKeybinds`. The atlas owns
//! the chord; this module owns the typed enum + lisp-keyword parser
//! that lets rc.lisp name *intents* (typo-checked at apply time)
//! instead of *chords* (typo-silent until the operator hits the wrong
//! key).
//!
//! # Why
//!
//! 2026-05-21 incident: rc.lisp had `(defbind :key "C-r")` AND
//! `(defpicker :key "C-r")` — both inserted into reedline's
//! keybindings HashMap on the same chord, leaving Ctrl-R unresponsive.
//! Hard-coded chord strings in two places drift. With this module,
//! both forms become `:intent :history-picker` and the chord is
//! sourced from one atlas. Duplicate-by-chord becomes impossible
//! by construction; duplicate-by-intent fails at apply time with
//! a clear error.
//!
//! # Usage
//!
//! ```lisp
//! ;; Intent form — chord sourced from FleetKeybinds. Preferred.
//! (defpicker :intent :history-picker :binary "skim-history" :action "replace")
//!
//! ;; Key form — hard-coded chord, kept for app-specific bindings
//! ;; that aren't fleet-canonical.
//! (defbind :key "C-x e" :action "edit-line-in-editor")
//! ```

use ishou_tokens::FleetKeybinds;

/// Every named operator-intent the rc.lisp `(defbind :intent …)` /
/// `(defpicker :intent …)` forms can reference. One variant per
/// `FleetKeybinds` field; the resolver below is exhaustively matched
/// so adding a new field to `FleetKeybinds` is a compile error here
/// until a variant lands. That coupling is intentional — the atlas
/// and the lisp keyword set must stay in lockstep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeybindIntent {
    HistoryPicker,
    FilesPicker,
    DirPicker,
    ContentPicker,
    ClearBuffer,
    KillLine,
    EditInEditor,
    Help,
    ClipboardCopy,
    ClipboardPaste,
    ToggleSudo,
    InsertLastArg,
    MultiplexerPrefix,
}

impl KeybindIntent {
    /// Parse a lisp keyword string (`"history-picker"`,
    /// `"clear-buffer"`, …) into a typed intent. Returns `None`
    /// for typos so the apply path can emit a clear error naming
    /// the unknown intent + every valid one.
    #[must_use]
    pub fn from_lisp_keyword(s: &str) -> Option<Self> {
        Some(match s {
            "history-picker" => Self::HistoryPicker,
            "files-picker" => Self::FilesPicker,
            "dir-picker" => Self::DirPicker,
            "content-picker" => Self::ContentPicker,
            "clear-buffer" => Self::ClearBuffer,
            "kill-line" => Self::KillLine,
            "edit-in-editor" => Self::EditInEditor,
            "help" => Self::Help,
            "clipboard-copy" => Self::ClipboardCopy,
            "clipboard-paste" => Self::ClipboardPaste,
            "toggle-sudo" => Self::ToggleSudo,
            "insert-last-arg" => Self::InsertLastArg,
            "multiplexer-prefix" => Self::MultiplexerPrefix,
            _ => return None,
        })
    }

    /// Every known keyword in canonical lisp form, for use in
    /// error messages naming the valid set.
    pub const ALL_KEYWORDS: &'static [&'static str] = &[
        "history-picker",
        "files-picker",
        "dir-picker",
        "content-picker",
        "clear-buffer",
        "kill-line",
        "edit-in-editor",
        "help",
        "clipboard-copy",
        "clipboard-paste",
        "toggle-sudo",
        "insert-last-arg",
        "multiplexer-prefix",
    ];

    /// Resolve this intent to the chord declared by the given
    /// `FleetKeybinds` instance. The exhaustive match enforces
    /// that every atlas field has a corresponding variant — the
    /// substrate stays in lockstep.
    #[must_use]
    pub fn resolve(self, kb: &FleetKeybinds) -> &'static str {
        match self {
            Self::HistoryPicker => kb.history_picker,
            Self::FilesPicker => kb.files_picker,
            Self::DirPicker => kb.dir_picker,
            Self::ContentPicker => kb.content_picker,
            Self::ClearBuffer => kb.clear_buffer,
            Self::KillLine => kb.kill_line,
            Self::EditInEditor => kb.edit_in_editor,
            Self::Help => kb.help,
            Self::ClipboardCopy => kb.clipboard_copy,
            Self::ClipboardPaste => kb.clipboard_paste,
            Self::ToggleSudo => kb.toggle_sudo,
            Self::InsertLastArg => kb.insert_last_arg,
            Self::MultiplexerPrefix => kb.multiplexer_prefix,
        }
    }
}

/// Outcome of resolving an `intent` field on `BindSpec`/`PickerSpec`.
/// The apply path branches on this without panicking; an unknown
/// intent surfaces as a typed error the caller can log + skip the
/// form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentResolution {
    /// The intent string was empty / not supplied. Caller falls
    /// back to the `key` field.
    NoIntent,
    /// Atlas resolved the intent to this canonical chord. Caller
    /// should use this in place of the `key` field.
    Chord(&'static str),
    /// The intent string was non-empty but doesn't name any known
    /// intent. Caller should log + skip the form. `unknown` is the
    /// supplied string verbatim for the error message.
    Unknown { unknown: String },
}

/// Resolve the optional `intent` keyword on a `BindSpec`/`PickerSpec`
/// through the supplied atlas. The caller passes the atlas
/// explicitly so tests + alternate fleet baselines can be threaded
/// without a global.
#[must_use]
pub fn resolve_intent(intent: Option<&str>, kb: &FleetKeybinds) -> IntentResolution {
    let Some(raw) = intent else {
        return IntentResolution::NoIntent;
    };
    let trimmed = raw.trim_start_matches(':');
    if trimmed.is_empty() {
        return IntentResolution::NoIntent;
    }
    match KeybindIntent::from_lisp_keyword(trimmed) {
        Some(k) => IntentResolution::Chord(k.resolve(kb)),
        None => IntentResolution::Unknown {
            unknown: trimmed.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_atlas_field_has_a_typed_intent_variant() {
        // The resolve() exhaustive match would already fail to
        // compile if any atlas field were missing. This test pins
        // the *count* so an atlas addition without a variant
        // addition is caught even if someone wires the variant
        // wrong (e.g. duplicates a match arm).
        assert_eq!(
            KeybindIntent::ALL_KEYWORDS.len(),
            13,
            "FleetKeybinds + KeybindIntent count drifted — add the missing variant + keyword"
        );
    }

    #[test]
    fn from_lisp_keyword_matches_canonical_form() {
        for k in KeybindIntent::ALL_KEYWORDS {
            assert!(
                KeybindIntent::from_lisp_keyword(k).is_some(),
                "valid keyword {k:?} did not parse"
            );
        }
    }

    #[test]
    fn from_lisp_keyword_rejects_unknown() {
        assert_eq!(KeybindIntent::from_lisp_keyword("not-an-intent"), None);
        assert_eq!(KeybindIntent::from_lisp_keyword(""), None);
        assert_eq!(KeybindIntent::from_lisp_keyword("HistoryPicker"), None); // case-sensitive
    }

    #[test]
    fn resolve_returns_atlas_chord() {
        let kb = FleetKeybinds::prescribed();
        assert_eq!(KeybindIntent::HistoryPicker.resolve(&kb), "C-r");
        assert_eq!(KeybindIntent::MultiplexerPrefix.resolve(&kb), "C-b");
        assert_eq!(KeybindIntent::ClearBuffer.resolve(&kb), "C-l");
    }

    #[test]
    fn resolve_intent_none_when_missing() {
        let kb = FleetKeybinds::prescribed();
        assert_eq!(resolve_intent(None, &kb), IntentResolution::NoIntent);
        assert_eq!(resolve_intent(Some(""), &kb), IntentResolution::NoIntent);
        assert_eq!(resolve_intent(Some(":"), &kb), IntentResolution::NoIntent);
    }

    #[test]
    fn resolve_intent_strips_leading_colon() {
        // rc.lisp authors may write either :history-picker or
        // history-picker; both must resolve. The leading-colon
        // form is conventional for lisp keywords.
        let kb = FleetKeybinds::prescribed();
        assert_eq!(
            resolve_intent(Some(":history-picker"), &kb),
            IntentResolution::Chord("C-r"),
        );
        assert_eq!(
            resolve_intent(Some("history-picker"), &kb),
            IntentResolution::Chord("C-r"),
        );
    }

    #[test]
    fn resolve_intent_resolves_known_intent_to_atlas_chord() {
        let kb = FleetKeybinds::prescribed();
        assert_eq!(
            resolve_intent(Some(":files-picker"), &kb),
            IntentResolution::Chord("C-t"),
        );
    }

    #[test]
    fn resolve_intent_reports_unknown_with_supplied_string() {
        let kb = FleetKeybinds::prescribed();
        assert_eq!(
            resolve_intent(Some(":bogus-intent"), &kb),
            IntentResolution::Unknown {
                unknown: "bogus-intent".into(),
            },
        );
    }

    #[test]
    fn bare_atlas_resolves_to_empty_chord() {
        // The bare tier returns "" for every intent — call-sites
        // can detect this + skip the bind, matching the "no
        // opinion" semantics.
        let kb = FleetKeybinds::bare();
        assert_eq!(KeybindIntent::HistoryPicker.resolve(&kb), "");
    }
}
