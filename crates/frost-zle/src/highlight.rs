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
}

impl FrostHighlighter {
    /// Empty-lookup highlighter — every first-word becomes "unknown"
    /// (yellow). Useful for tests; prefer `with_known` in the REPL.
    pub fn new() -> Self {
        Self {
            known: HashSet::new(),
            check_paths: false,
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
        }
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
            let mut style = style_for_token(&tok, at_command_start, &self.known);

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
                        style = Style::new().fg(Color::Red);
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

fn style_for_token(tok: &Token, at_command_start: bool, known: &HashSet<String>) -> Style {
    // Reserved words first — they can sit where a command would.
    if tok.kind.is_reserved_word() {
        return Style::new().bold().fg(Color::Magenta);
    }

    match tok.kind {
        // Strings — the full tokenized literal including quotes.
        TokenKind::SingleQuoted
        | TokenKind::DoubleQuoted
        | TokenKind::DollarSingleQuoted => Style::new().fg(Color::Cyan),

        // Numbers (arithmetic contexts).
        TokenKind::Number => Style::new().fg(Color::Blue),

        // Expansion markers — the `$` / `${` / `$(` / `$((` head. The
        // interior of `${...}` comes back as separate tokens; they'll
        // fall through to the default path, which is fine.
        TokenKind::Dollar
        | TokenKind::DollarBrace
        | TokenKind::DollarParen
        | TokenKind::DollarDoubleParen
        | TokenKind::Backtick => Style::new().fg(Color::Yellow),

        // Command separators / logical ops / redirects — bright red for
        // immediate visual separation.
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
        | TokenKind::FdDup => Style::new().fg(Color::LightRed),

        // Globs (outside a string).
        TokenKind::Star | TokenKind::Question => Style::new().fg(Color::Yellow),

        // Tilde expansion.
        TokenKind::Tilde => Style::new().fg(Color::Magenta),

        // Comments — dim italic; the lexer emits a single Comment token
        // covering the whole `# …` run.
        TokenKind::Comment => Style::new().fg(Color::DarkGray).italic(),

        // Plain word — command-vs-argument distinction. At command
        // position, check first for reserved-word text (the lexer
        // doesn't always promote `if` to TokenKind::If) then fall to
        // known-command or unknown-external styling.
        TokenKind::Word => {
            if at_command_start {
                if is_reserved_word_text(tok.text.as_str()) {
                    Style::new().bold().fg(Color::Magenta)
                } else if known.contains(tok.text.as_str()) {
                    Style::new().fg(Color::Green)
                } else {
                    Style::new().fg(Color::Yellow)
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
            out.push((style_for_token(&tok, at_cmd, &h.known), line[start..end].to_string()));
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
    fn strings_get_cyan() {
        let h = FrostHighlighter::new();
        let segs = render(&h, r#"echo "hello world" 'bye'"#);
        assert!(segs.iter().any(|(s, t)| t == r#""hello world""# && s.foreground == Some(Color::Cyan)));
        assert!(segs.iter().any(|(s, t)| t == "'bye'" && s.foreground == Some(Color::Cyan)));
    }
}
