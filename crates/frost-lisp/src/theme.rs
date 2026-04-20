//! `deftheme` — declarative color scheme for frost's interactive surface.
//!
//! Colors previously lived scattered across the codebase:
//!
//!   * `frost-zle::FrostHighlighter` — hardcoded Nord-adjacent palette
//!   * `frost-zle::ZleEngine::with_history_hints` — hardcoded Color::Fixed(244)
//!   * `frostmourne/lisp/61-tools-skim.lisp` — Nord values spelled into
//!     SKIM_DEFAULT_OPTIONS
//!   * `frostmourne/lisp/10-prompt.lisp` — %F{green}/%F{blue}/…
//!
//! `(deftheme :name "nord" :hint "#4C566A" :command "#A3BE8C" …)`
//! collapses those knobs into one authoring surface. Unset fields
//! fall back to built-in Nord defaults, so a partial theme works.
//!
//! Consumers are resolution-late: the REPL queries
//! `ApplySummary::theme` when building the highlighter / hinter.
//! No runtime dependency from frost-zle back onto frost-lisp; the
//! spec just provides strings + downstream crates own their own
//! parse-into-Style logic.
//!
//! ```lisp
//! (deftheme :name "nord"
//!           :command         "#A3BE8C"   ; green — known commands
//!           :unknown-command "#EBCB8B"   ; yellow — unknown names
//!           :string          "#88C0D0"   ; cyan — "quoted"
//!           :variable        "#EBCB8B"   ; yellow — $VAR / ${VAR}
//!           :reserved        "#B48EAD"   ; magenta — if / for / while
//!           :operator        "#BF616A"   ; red — | / ; / && / redirects
//!           :comment         "#4C566A"   ; dim grey — # …
//!           :hint            "#4C566A"   ; dim grey — autosuggestion overlay
//!           :broken-path     "#BF616A"   ; red — path-doesn't-exist highlight
//!           :glob            "#EBCB8B"   ; yellow — * / ?
//!           :number          "#81A1C1")  ; blue — numeric literal
//! ```

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "deftheme")]
pub struct ThemeSpec {
    /// Identifier for the theme. Multiple theme forms with different
    /// names can coexist; only the LAST one applied wins. Informational
    /// — downstream consumers don't gate on it.
    #[serde(default)]
    pub name: Option<String>,
    /// Known-command color (hex `#RRGGBB` or a short name like
    /// `"green"`). Each color slot is a bare string; downstream crates
    /// parse with their own palette.
    #[serde(default)]
    pub command: Option<String>,
    /// Unknown / external-PATH command color.
    #[serde(default)]
    pub unknown_command: Option<String>,
    /// Quoted-string color (single + double).
    #[serde(default)]
    pub string: Option<String>,
    /// `$VAR` / `${VAR}` / `$(…)` head color.
    #[serde(default)]
    pub variable: Option<String>,
    /// Reserved keyword (`if`, `for`, `while`, `do`, …) color.
    #[serde(default)]
    pub reserved: Option<String>,
    /// Pipe / semi / logical / redirect operator color.
    #[serde(default)]
    pub operator: Option<String>,
    /// `# …` comment color.
    #[serde(default)]
    pub comment: Option<String>,
    /// Autosuggestion ghost-text (reedline Hinter) color.
    #[serde(default)]
    pub hint: Option<String>,
    /// Broken-path highlight color (nonexistent path argument).
    #[serde(default)]
    pub broken_path: Option<String>,
    /// Glob character (`*`, `?`) color.
    #[serde(default)]
    pub glob: Option<String>,
    /// Number literal color.
    #[serde(default)]
    pub number: Option<String>,
}

/// Built-in Nord fallback. Mirrors the palette [`FrostHighlighter`]
/// has been using since it shipped; exposing it here so consumers
/// that want "the default Nord frost theme" don't re-spell the
/// hex values. Fields map 1:1 to [`ThemeSpec`].
pub fn nord_default() -> ThemeSpec {
    ThemeSpec {
        name: Some("nord".into()),
        command: Some("#A3BE8C".into()),
        unknown_command: Some("#EBCB8B".into()),
        string: Some("#88C0D0".into()),
        variable: Some("#EBCB8B".into()),
        reserved: Some("#B48EAD".into()),
        operator: Some("#BF616A".into()),
        comment: Some("#4C566A".into()),
        hint: Some("#4C566A".into()),
        broken_path: Some("#BF616A".into()),
        glob: Some("#EBCB8B".into()),
        number: Some("#81A1C1".into()),
    }
}

/// Gruvbox Dark — the second-most-popular terminal palette after
/// Nord. Official color table from morhetz/gruvbox.
pub fn gruvbox_dark() -> ThemeSpec {
    ThemeSpec {
        name: Some("gruvbox-dark".into()),
        command: Some("#B8BB26".into()),         // bright green
        unknown_command: Some("#FABD2F".into()), // bright yellow
        string: Some("#83A598".into()),          // bright blue
        variable: Some("#FABD2F".into()),
        reserved: Some("#D3869B".into()), // bright purple
        operator: Some("#FB4934".into()), // bright red
        comment: Some("#928374".into()),  // neutral gray
        hint: Some("#928374".into()),
        broken_path: Some("#FB4934".into()),
        glob: Some("#FABD2F".into()),
        number: Some("#83A598".into()),
    }
}

/// Tokyo Night — folke/tokyonight.nvim port. Cooler blues + violets.
pub fn tokyo_night() -> ThemeSpec {
    ThemeSpec {
        name: Some("tokyo-night".into()),
        command: Some("#9ECE6A".into()),         // green
        unknown_command: Some("#E0AF68".into()), // yellow
        string: Some("#7DCFFF".into()),          // cyan
        variable: Some("#E0AF68".into()),
        reserved: Some("#BB9AF7".into()), // purple
        operator: Some("#F7768E".into()), // red/pink
        comment: Some("#565F89".into()),  // muted blue-gray
        hint: Some("#565F89".into()),
        broken_path: Some("#F7768E".into()),
        glob: Some("#E0AF68".into()),
        number: Some("#FF9E64".into()), // orange
    }
}

/// Catppuccin Mocha — catppuccin/catppuccin's flagship dark flavor.
pub fn catppuccin_mocha() -> ThemeSpec {
    ThemeSpec {
        name: Some("catppuccin-mocha".into()),
        command: Some("#A6E3A1".into()),         // green
        unknown_command: Some("#F9E2AF".into()), // yellow
        string: Some("#94E2D5".into()),          // teal
        variable: Some("#F9E2AF".into()),
        reserved: Some("#CBA6F7".into()), // mauve
        operator: Some("#F38BA8".into()), // pink/red
        comment: Some("#6C7086".into()),  // overlay0
        hint: Some("#6C7086".into()),
        broken_path: Some("#F38BA8".into()),
        glob: Some("#F9E2AF".into()),
        number: Some("#FAB387".into()), // peach
    }
}

/// Look up a preset by name — used by the rc to match a user's
/// `(deftheme :name "…")` shorthand against the shipped tables.
/// Returns `None` when the name doesn't match a preset, at which
/// point consumers fall back to treating the spec as a raw
/// hex-slot override on top of Nord.
pub fn preset_by_name(name: &str) -> Option<ThemeSpec> {
    match name {
        "nord" => Some(nord_default()),
        "gruvbox" | "gruvbox-dark" => Some(gruvbox_dark()),
        "tokyo-night" | "tokyonight" | "tokyo" => Some(tokyo_night()),
        "catppuccin" | "catppuccin-mocha" | "mocha" => Some(catppuccin_mocha()),
        _ => None,
    }
}

/// Merge a partial [`ThemeSpec`] onto a base (typically
/// [`nord_default`]). Used by `apply_source` so users can ship a
/// theme with only the slots they want to override.
pub fn merge_theme(base: ThemeSpec, overlay: ThemeSpec) -> ThemeSpec {
    macro_rules! pick {
        ($f:ident) => {
            overlay.$f.or(base.$f)
        };
    }
    ThemeSpec {
        name: pick!(name),
        command: pick!(command),
        unknown_command: pick!(unknown_command),
        string: pick!(string),
        variable: pick!(variable),
        reserved: pick!(reserved),
        operator: pick!(operator),
        comment: pick!(comment),
        hint: pick!(hint),
        broken_path: pick!(broken_path),
        glob: pick!(glob),
        number: pick!(number),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nord_default_fills_every_slot() {
        let n = nord_default();
        assert!(n.command.is_some());
        assert!(n.unknown_command.is_some());
        assert!(n.string.is_some());
        assert!(n.variable.is_some());
        assert!(n.reserved.is_some());
        assert!(n.operator.is_some());
        assert!(n.comment.is_some());
        assert!(n.hint.is_some());
        assert!(n.broken_path.is_some());
        assert!(n.glob.is_some());
        assert!(n.number.is_some());
    }

    #[test]
    fn merge_theme_overlay_wins_where_set() {
        let base = nord_default();
        let overlay = ThemeSpec {
            name: Some("custom".into()),
            hint: Some("#FFFFFF".into()),
            ..Default::default()
        };
        let merged = merge_theme(base.clone(), overlay);
        assert_eq!(merged.name.as_deref(), Some("custom"));
        assert_eq!(merged.hint.as_deref(), Some("#FFFFFF"));
        // Non-overlaid slots keep the base value.
        assert_eq!(merged.command, base.command);
        assert_eq!(merged.string, base.string);
    }

    #[test]
    fn merge_theme_empty_overlay_preserves_base() {
        let base = nord_default();
        let merged = merge_theme(base.clone(), ThemeSpec::default());
        assert_eq!(merged, base);
    }

    #[test]
    fn preset_by_name_covers_shipped_themes() {
        assert!(preset_by_name("nord").is_some());
        assert!(preset_by_name("gruvbox").is_some());
        assert!(preset_by_name("gruvbox-dark").is_some());
        assert!(preset_by_name("tokyo-night").is_some());
        assert!(preset_by_name("tokyonight").is_some());
        assert!(preset_by_name("catppuccin").is_some());
        assert!(preset_by_name("catppuccin-mocha").is_some());
        assert!(preset_by_name("mocha").is_some());
        // Unknown name returns None.
        assert!(preset_by_name("solarized").is_none());
        assert!(preset_by_name("").is_none());
    }

    #[test]
    fn each_preset_fills_every_slot() {
        for spec in [
            nord_default(),
            gruvbox_dark(),
            tokyo_night(),
            catppuccin_mocha(),
        ] {
            assert!(spec.name.is_some(), "theme without a name");
            assert!(spec.command.is_some());
            assert!(spec.unknown_command.is_some());
            assert!(spec.string.is_some());
            assert!(spec.variable.is_some());
            assert!(spec.reserved.is_some());
            assert!(spec.operator.is_some());
            assert!(spec.comment.is_some());
            assert!(spec.hint.is_some());
            assert!(spec.broken_path.is_some());
            assert!(spec.glob.is_some());
            assert!(spec.number.is_some());
        }
    }
}
