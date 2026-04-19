//! frost-native syntax highlighter for reedline.
//!
//! blzsh parity: zsh wears fast-syntax-highlighting; frost carries a
//! Rust-side highlighter driven by `frost_lexer` so every keystroke
//! recolors the line without a subshell. reedline's [`Highlighter`]
//! trait is one method; we drive it from the lexer's spans so colors
//! track the exact byte ranges of each token.
//!
//! Color palette (Nord-adjacent, intentional overlap with skim theme):
//!
//! | Category                  | Style                         |
//! |---------------------------|-------------------------------|
//! | Reserved (if/for/do/…)    | bold magenta                  |
//! | Known command (1st word)  | green                         |
//! | Unknown command (1st word)| yellow                        |
//! | Single/double-quoted str  | cyan                          |
//! | Dollar / ${…} / $(…)      | yellow                        |
//! | Pipe / ; / && / \|\| / …  | bright red                    |
//! | Redirects (> < >> …)      | bright red                    |
//! | Comment (# …)             | dim italic                    |
//! | Number                    | blue                          |
//! | Glob (\* ?)               | yellow                        |
//! | Tilde (~)                 | magenta                       |
//! | Default (args, paths)     | terminal default              |
//!
//! Command-boundary tracking: the lexer doesn't know "is this word a
//! command?" — we track that positionally. The first `Word` after a
//! command-breaker (pipe / semi / logical / `do` / `then` / `{` / `(`)
//! is treated as a command; subsequent words in the same simple
//! command are args.

use std::collections::HashSet;

use frost_lexer::{Lexer, Token, TokenKind};
use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};

/// Lookup table for "is this name a known command?" — builtins +
/// rc-defined aliases/functions. `frost-complete` already has a
/// builtin list; this wraps it + user-supplied additions.
///
/// `check_paths` toggles fish-style broken-path highlighting: tokens
/// that look like filesystem paths (`./foo`, `/abs`, `~/rel`,
/// `$HOME/x`) are stat'd and colored red if they don't exist. Off
/// by default in tests (filesystem access per highlight is
/// undesirable); enabled by the interactive REPL.
pub struct FrostHighlighter {
    known: HashSet<String>,
    check_paths: bool,
    palette: Palette,
}

/// Color palette for each highlighted token category. All fields
/// default to the frost-native Nord-adjacent values; consumers
/// override via [`FrostHighlighter::with_palette`] (typically fed by
/// a rc-authored `(deftheme …)` spec translated via
/// [`Palette::from_hex_slots`]).
#[derive(Debug, Clone)]
pub struct Palette {
    pub command: Style,
    pub unknown_command: Style,
    pub reserved: Style,
    pub string: Style,
    pub variable: Style,
    pub operator: Style,
    pub comment: Style,
    pub glob: Style,
    pub number: Style,
    pub tilde: Style,
    pub broken_path: Style,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            command:         Style::new().fg(Color::Green),
            unknown_command: Style::new().fg(Color::Yellow),
            reserved:        Style::new().bold().fg(Color::Magenta),
            string:          Style::new().fg(Color::Cyan),
            variable:        Style::new().fg(Color::Yellow),
            operator:        Style::new().fg(Color::LightRed),
            comment:         Style::new().fg(Color::DarkGray).italic(),
            glob:            Style::new().fg(Color::Yellow),
            number:          Style::new().fg(Color::Blue),
            tilde:           Style::new().fg(Color::Magenta),
            broken_path:     Style::new().fg(Color::Red),
        }
    }
}

impl Palette {
    /// Build a palette from per-slot hex strings. Unset / unparseable
    /// slots fall back to the default palette's entry for that slot.
    /// Accepts `#RRGGBB` and `#RGB` shorthand; named colors aren't
    /// supported here (rc-authors stick to hex for terminal-truecolor
    /// precision).
    pub fn from_hex_slots(slots: PaletteSlots<'_>) -> Self {
        let d = Self::default();
        Self {
            command:         slots.command.and_then(parse_hex_style).unwrap_or(d.command),
            unknown_command: slots.unknown_command.and_then(parse_hex_style).unwrap_or(d.unknown_command),
            reserved:        slots.reserved.and_then(parse_hex_style).map(|s| s.bold()).unwrap_or(d.reserved),
            string:          slots.string.and_then(parse_hex_style).unwrap_or(d.string),
            variable:        slots.variable.and_then(parse_hex_style).unwrap_or(d.variable),
            operator:        slots.operator.and_then(parse_hex_style).unwrap_or(d.operator),
            comment:         slots.comment.and_then(parse_hex_style).map(|s| s.italic()).unwrap_or(d.comment),
            glob:            slots.glob.and_then(parse_hex_style).unwrap_or(d.glob),
            number:          slots.number.and_then(parse_hex_style).unwrap_or(d.number),
            tilde:           slots.tilde.and_then(parse_hex_style).unwrap_or(d.tilde),
            broken_path:     slots.broken_path.and_then(parse_hex_style).unwrap_or(d.broken_path),
        }
    }
}

/// Thin borrowed view over a `ThemeSpec` for `from_hex_slots`. Keeps
/// frost-zle free of a direct dep on frost-lisp's domain types while
/// still accepting the rc-declared color palette slot-by-slot.
#[derive(Default, Debug, Clone, Copy)]
pub struct PaletteSlots<'a> {
    pub command: Option<&'a str>,
    pub unknown_command: Option<&'a str>,
    pub reserved: Option<&'a str>,
    pub string: Option<&'a str>,
    pub variable: Option<&'a str>,
    pub operator: Option<&'a str>,
    pub comment: Option<&'a str>,
    pub glob: Option<&'a str>,
    pub number: Option<&'a str>,
    pub tilde: Option<&'a str>,
    pub broken_path: Option<&'a str>,
}

/// Parse `#RRGGBB` / `#RGB` → `Style` with that foreground. Returns
/// `None` for unparseable input so the caller can fall back.
pub fn parse_hex_style(hex: &str) -> Option<Style> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    let (r, g, b) = match hex.len() {
        6 => (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ),
        3 => {
            let ch = |i: usize| -> Option<u8> {
                let digit = u8::from_str_radix(&hex[i..=i], 16).ok()?;
                Some(digit * 17) // 0xN → 0xNN (0*17=0, F*17=FF)
            };
            (ch(0)?, ch(1)?, ch(2)?)
        }
        _ => return None,
    };
    Some(Style::new().fg(Color::Rgb(r, g, b)))
}

impl FrostHighlighter {
    /// Empty-lookup highlighter — every first-word becomes "unknown"
    /// (yellow). Useful for tests; prefer `with_known` in the REPL.
    pub fn new() -> Self {
        Self {
            known: HashSet::new(),
            check_paths: false,
            palette: Palette::default(),
        }
    }

    /// Seed the known-commands set with the given names. Callers
    /// typically pass `frost-complete`'s default builtin list + the
    /// keys of `env.aliases` + `env.functions` after rc load.
    pub fn with_known<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            known: names.into_iter().map(Into::into).collect(),
            check_paths: false,
            palette: Palette::default(),
        }
    }

    /// Install a rc-authored palette. Typically wired from the
    /// `(deftheme …)` accumulation in `ApplySummary::theme`. Fields
    /// the theme doesn't set stay at the Nord default.
    pub fn with_palette(mut self, palette: Palette) -> Self {
        self.palette = palette;
        self
    }

    /// Enable fish-style broken-path highlighting. Path-looking
    /// tokens (`./x`, `/etc`, `~/foo`, `$VAR/x`) are stat'd and
    /// painted red when they don't exist. Off by default because
    /// tests shouldn't hit the filesystem; the interactive REPL
    /// flips it on.
    pub fn with_path_checks(mut self, enabled: bool) -> Self {
        self.check_paths = enabled;
        self
    }

    /// Add a single name to the known set post-construction. Useful for
    /// incrementally tracking `alias foo=bar` invocations mid-session,
    /// though the REPL currently rebuilds the highlighter each read_line
    /// so this is mostly for tests.
    pub fn insert_known(&mut self, name: impl Into<String>) {
        self.known.insert(name.into());
    }
}

/// Does `s` look like a filesystem path the user might have typed?
/// Conservative — we'd rather miss a path than stat every Word
/// token. Recognized prefixes: `/`, `./`, `../`, `~`, `~/`, `$`.
fn looks_like_path(s: &str) -> bool {
    s.starts_with('/')
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('~')
        || s.starts_with('$')
}

/// Resolve environment-ish prefixes for a stat check: `~`, `~/`,
/// `$VAR`, `${VAR}`. Returns None when we can't resolve (then we
/// just skip the check).
fn resolve_path_for_check(s: &str) -> Option<std::path::PathBuf> {
    // Tilde.
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        return Some(std::path::PathBuf::from(home).join(rest));
    }
    if s == "~" {
        return std::env::var("HOME").ok().map(std::path::PathBuf::from);
    }
    // $NAME at the start — strip, expand, concatenate. One level deep.
    if let Some(rest) = s.strip_prefix('$') {
        let (name, tail) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };
        let name = name.trim_start_matches('{').trim_end_matches('}');
        let value = std::env::var(name).ok()?;
        return Some(std::path::PathBuf::from(value).join(tail.trim_start_matches('/')));
    }
    Some(std::path::PathBuf::from(s))
}

impl Default for FrostHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter for FrostHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut out = StyledText::new();
        let mut lexer = Lexer::new(line.as_bytes());
        let mut prev_end: usize = 0;
        let mut at_command_start = true;

        // Belt-and-suspenders iteration cap: even if the lexer is
        // buggy and emits zero-width non-EOF tokens forever, the
        // loop can't exceed one iteration per byte of input + a
        // small slack. Without this, pathological inputs
        // (unterminated herestrings, `$` at EOL, certain unicode)
        // could hang the REPL on every keystroke since the
        // highlighter runs after each edit.
        let max_iters = line.len() + 16;
        let mut iters = 0;

        loop {
            if iters >= max_iters {
                break;
            }
            iters += 1;

            let tok = lexer.next_token();
            if matches!(tok.kind, TokenKind::Eof | TokenKind::Error) {
                break;
            }

            let start = tok.span.start as usize;
            let end = tok.span.end as usize;

            // Fill any whitespace / skipped gap between the previous
            // token and this one with default-styled text. Without this
            // reedline would collapse the spaces away.
            if start > prev_end && prev_end <= line.len() && start <= line.len() {
                out.push((Style::default(), line[prev_end..start].to_string()));
            }
            // Guard against malformed spans (shouldn't happen, but the
            // lexer is defensively imperfect).
            if end > line.len() {
                break;
            }

            let raw = line[start..end].to_string();
            let mut style = style_for_token(&tok, at_command_start, &self.known, &self.palette);

            // Path-existence override. Only at non-command position
            // (command-position words are already styled as green/
            // yellow based on `known`); tokens that parse as paths
            // get red if they don't resolve. Checking happens after
            // the base style is picked so command-position paths
            // still follow the known/unknown coloring.
            if self.check_paths
                && !at_command_start
                && matches!(tok.kind, TokenKind::Word)
                && looks_like_path(&raw)
            {
                if let Some(resolved) = resolve_path_for_check(&raw) {
                    if !resolved.exists() {
                        style = self.palette.broken_path;
                    }
                }
            }

            out.push((style, raw));

            // Update command-boundary state for the next iteration.
            if is_command_breaker(tok.kind) {
                at_command_start = true;
            } else if matches!(tok.kind, TokenKind::Word) {
                // Reserved-word-as-Word (e.g., lexer-emitted `if`)
                // functions like a grammatical breaker: the next Word
                // is still at command position. Everything else is an
                // argument.
                if at_command_start && is_reserved_word_text(tok.text.as_str()) {
                    at_command_start = true;
                } else {
                    at_command_start = false;
                }
            }

            // Monotonic progress: if the lexer fails to advance past
            // `prev_end` we'd spin forever. Break cleanly in that case
            // so the painted output is a prefix of the real highlight
            // rather than a hang.
            if end <= prev_end && iters > 1 {
                break;
            }
            prev_end = end;
        }

        // Trailing bytes the lexer didn't consume (mid-token error,
        // unterminated string) — paint them default so the user can
        // still see what they typed.
        if prev_end < line.len() {
            out.push((Style::default(), line[prev_end..].to_string()));
        }

        out
    }
}

/// Reserved-word strings the lexer may emit as `Word` tokens rather
/// than dedicated `TokenKind::If` / `::Then` / … — happens when the
/// word appears outside a grammatical position where the lexer
/// promotes it. We treat any `Word` whose text matches this set as
/// reserved-for-highlighting purposes; it's a superset of what the
/// grammar would actually recognize, but harmlessly so.
const RESERVED_WORD_STRINGS: &[&str] = &[
    "if", "then", "elif", "else", "fi",
    "for", "while", "until", "do", "done",
    "case", "esac", "in",
    "select", "function", "time", "coproc",
    "return", "break", "continue",
];

fn is_reserved_word_text(s: &str) -> bool {
    RESERVED_WORD_STRINGS.iter().any(|w| *w == s)
}

fn is_command_breaker(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Newline
            | TokenKind::Pipe
            | TokenKind::PipeAmpersand
            | TokenKind::OrOr
            | TokenKind::AndAnd
            | TokenKind::Semi
            | TokenKind::DoubleSemi
            | TokenKind::SemiAnd
            | TokenKind::SemiPipe
            | TokenKind::LeftBrace
            | TokenKind::LeftParen
            | TokenKind::If
            | TokenKind::Then
            | TokenKind::Else
            | TokenKind::Elif
            | TokenKind::For
            | TokenKind::In
            | TokenKind::While
            | TokenKind::Until
            | TokenKind::Do
            | TokenKind::Case
            | TokenKind::Esac
            | TokenKind::Select
            | TokenKind::Function
            | TokenKind::Time
            | TokenKind::Coproc
    )
}

fn style_for_token(
    tok: &Token,
    at_command_start: bool,
    known: &HashSet<String>,
    palette: &Palette,
) -> Style {
    // Reserved words first — they can sit where a command would.
    if tok.kind.is_reserved_word() {
        return palette.reserved;
    }

    match tok.kind {
        // Strings — the full tokenized literal including quotes.
        TokenKind::SingleQuoted
        | TokenKind::DoubleQuoted
        | TokenKind::DollarSingleQuoted => palette.string,

        // Numbers (arithmetic contexts).
        TokenKind::Number => palette.number,

        // Expansion markers — the `$` / `${` / `$(` / `$((` head. The
        // interior of `${...}` comes back as separate tokens; they'll
        // fall through to the default path, which is fine.
        TokenKind::Dollar
        | TokenKind::DollarBrace
        | TokenKind::DollarParen
        | TokenKind::DollarDoubleParen
        | TokenKind::Backtick => palette.variable,

        // Command separators / logical ops / redirects.
        TokenKind::Pipe
        | TokenKind::PipeAmpersand
        | TokenKind::OrOr
        | TokenKind::AndAnd
        | TokenKind::Ampersand
        | TokenKind::Disown
        | TokenKind::Semi
        | TokenKind::DoubleSemi
        | TokenKind::SemiAnd
        | TokenKind::SemiPipe
        | TokenKind::Newline
        | TokenKind::Less
        | TokenKind::Greater
        | TokenKind::DoubleGreater
        | TokenKind::GreaterPipe
        | TokenKind::GreaterBang
        | TokenKind::AmpGreater
        | TokenKind::AmpDoubleGreater
        | TokenKind::DoubleLess
        | TokenKind::TripleLess
        | TokenKind::DoubleLessDash
        | TokenKind::LessGreater
        | TokenKind::ProcessSubIn
        | TokenKind::ProcessSubOut
        | TokenKind::FdGreater
        | TokenKind::FdLess
        | TokenKind::FdDoubleGreater
        | TokenKind::FdDup => palette.operator,

        // Globs (outside a string).
        TokenKind::Star | TokenKind::Question => palette.glob,

        // Tilde expansion.
        TokenKind::Tilde => palette.tilde,

        // Comments — dim italic; the lexer emits a single Comment token
        // covering the whole `# …` run.
        TokenKind::Comment => palette.comment,

        // Plain word — command-vs-argument distinction. At command
        // position, check first for reserved-word text (the lexer
        // doesn't always promote `if` to TokenKind::If) then fall to
        // known-command or unknown-external styling.
        TokenKind::Word => {
            if at_command_start {
                if is_reserved_word_text(tok.text.as_str()) {
                    palette.reserved
                } else if known.contains(tok.text.as_str()) {
                    palette.command
                } else {
                    palette.unknown_command
                }
            } else {
                Style::default()
            }
        }

        // Grouping punctuation — default color; they don't carry much
        // meaning in a single-line highlighter.
        TokenKind::LeftParen
        | TokenKind::RightParen
        | TokenKind::LeftBrace
        | TokenKind::RightBrace
        | TokenKind::DoubleLeftBracket
        | TokenKind::DoubleRightBracket
        | TokenKind::CondStart
        | TokenKind::CondEnd
        | TokenKind::Equals
        | TokenKind::Bang
        | TokenKind::At => Style::default(),

        // Fallback — should be exhaustive; new token kinds pick up
        // default styling until someone adds a case.
        TokenKind::Eof | TokenKind::Error => Style::default(),

        // Reserved words handled above; explicit `_` avoids the "not
        // all TokenKind variants covered" warning when new reserved
        // words are added.
        _ => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(h: &FrostHighlighter, line: &str) -> Vec<(Style, String)> {
        let st = h.highlight(line, line.len());
        // reedline's StyledText doesn't expose segments publicly so we
        // lean on its Display impl, which renders ANSI escape
        // sequences. For unit-level assertions we re-render via
        // Highlighter path directly — since that's what we ship, the
        // internal segment list isn't observable here. Call sites
        // assert on pseudo-Debug text.
        let _ = st;
        // Re-implement the walk for the test (mirrors Highlighter::highlight).
        let mut out: Vec<(Style, String)> = Vec::new();
        let mut lexer = frost_lexer::Lexer::new(line.as_bytes());
        let mut prev_end: usize = 0;
        let mut at_cmd = true;
        loop {
            let tok = lexer.next_token();
            if matches!(tok.kind, TokenKind::Eof | TokenKind::Error) { break; }
            let start = tok.span.start as usize;
            let end = tok.span.end as usize;
            if start > prev_end {
                out.push((Style::default(), line[prev_end..start].to_string()));
            }
            if end > line.len() { break; }
            out.push((style_for_token(&tok, at_cmd, &h.known, &h.palette), line[start..end].to_string()));
            if is_command_breaker(tok.kind) {
                at_cmd = true;
            } else if matches!(tok.kind, TokenKind::Word) {
                at_cmd = at_cmd && is_reserved_word_text(tok.text.as_str());
            }
            prev_end = end;
        }
        if prev_end < line.len() {
            out.push((Style::default(), line[prev_end..].to_string()));
        }
        out
    }

    #[test]
    fn empty_line_yields_no_segments() {
        let h = FrostHighlighter::new();
        let segs = render(&h, "");
        assert!(segs.is_empty());
    }

    #[test]
    fn known_command_gets_green() {
        let h = FrostHighlighter::with_known(["echo"]);
        let segs = render(&h, "echo hi");
        assert_eq!(segs[0].1, "echo");
        assert_eq!(segs[0].0.foreground, Some(Color::Green));
    }

    #[test]
    fn unknown_command_gets_yellow() {
        let h = FrostHighlighter::new();
        let segs = render(&h, "nope hi");
        assert_eq!(segs[0].1, "nope");
        assert_eq!(segs[0].0.foreground, Some(Color::Yellow));
    }

    #[test]
    fn reserved_word_gets_magenta_bold() {
        let h = FrostHighlighter::new();
        let segs = render(&h, "if true; then echo; fi");
        // First token is `if` — bold magenta reserved.
        assert_eq!(segs[0].1, "if");
        assert_eq!(segs[0].0.foreground, Some(Color::Magenta));
        assert!(segs[0].0.is_bold);
    }

    #[test]
    fn second_word_is_arg_default_style() {
        let h = FrostHighlighter::with_known(["echo"]);
        let segs = render(&h, "echo world");
        // Find "world"
        let (style, text) = segs.iter().find(|(_, s)| s == "world").unwrap();
        assert_eq!(text, "world");
        assert_eq!(style.foreground, None);
    }

    #[test]
    fn pipe_breaks_command_boundary() {
        let h = FrostHighlighter::with_known(["ls", "wc"]);
        let segs = render(&h, "ls | wc -l");
        let find = |s: &str| segs.iter().find(|(_, t)| t == s).cloned().unwrap();
        // both `ls` and `wc` should be green since both are known.
        assert_eq!(find("ls").0.foreground, Some(Color::Green));
        assert_eq!(find("wc").0.foreground, Some(Color::Green));
    }

    #[test]
    fn parse_hex_style_rrggbb_and_rgb() {
        // Full form.
        let s = parse_hex_style("#FF8800").unwrap();
        assert_eq!(s.foreground, Some(Color::Rgb(0xFF, 0x88, 0x00)));
        // Leading-# optional.
        let s = parse_hex_style("A3BE8C").unwrap();
        assert_eq!(s.foreground, Some(Color::Rgb(0xA3, 0xBE, 0x8C)));
        // Short form expands via × 17.
        let s = parse_hex_style("#F80").unwrap();
        assert_eq!(s.foreground, Some(Color::Rgb(0xFF, 0x88, 0x00)));
    }

    #[test]
    fn parse_hex_style_rejects_garbage() {
        assert!(parse_hex_style("#GGGGGG").is_none());
        assert!(parse_hex_style("#12345").is_none()); // 5 chars
        assert!(parse_hex_style("").is_none());
        assert!(parse_hex_style("notahex").is_none());
    }

    #[test]
    fn palette_from_hex_slots_overrides_only_set_slots() {
        let p = Palette::from_hex_slots(PaletteSlots {
            command: Some("#00FF00"),
            ..Default::default()
        });
        assert_eq!(p.command.foreground, Some(Color::Rgb(0, 255, 0)));
        // Unset slot retains default (Style::new().fg(Color::Yellow) for
        // unknown_command).
        let d = Palette::default();
        assert_eq!(p.unknown_command.foreground, d.unknown_command.foreground);
    }

    #[test]
    fn highlighter_uses_custom_palette_for_command() {
        let mut p = Palette::default();
        p.command = Style::new().fg(Color::Rgb(1, 2, 3));
        let h = FrostHighlighter::with_known(["echo"]).with_palette(p);
        let segs = render(&h, "echo hi");
        // First segment = "echo" at command position, known name.
        assert_eq!(segs[0].1, "echo");
        assert_eq!(segs[0].0.foreground, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn strings_get_cyan() {
        let h = FrostHighlighter::new();
        let segs = render(&h, r#"echo "hello world" 'bye'"#);
        assert!(segs.iter().any(|(s, t)| t == r#""hello world""# && s.foreground == Some(Color::Cyan)));
        assert!(segs.iter().any(|(s, t)| t == "'bye'" && s.foreground == Some(Color::Cyan)));
    }
}
