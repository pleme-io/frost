//! Quote-aware word splitting for non-AST contexts.
//!
//! The canonical consumer is frost-exec's alias splice: an alias value
//! like `cd '/a b'` must re-tokenize to `[cd, /a b]` — quote removal +
//! backslash escapes applied — NOT `[cd, '/a, b']`. Splitting alias
//! values on raw whitespace shipped the 2026-06-11 defmark incident:
//! `(defmark :name "nix" …)` registered `nix` → `cd '/Users/…/nix'`,
//! the literal quotes survived the splice, and the cd builtin failed
//! with ENOENT on a path that exists.
//!
//! This is intentionally NOT a full shell parse — no expansion, no
//! command substitution, no globbing. `$VAR`, `~`, and operator
//! characters pass through as literal text exactly like the historical
//! whitespace splitter; only QUOTING (single, double, `$'…'`) and
//! backslash escapes are interpreted, because those are the lexical
//! word-boundary concerns an alias body legitimately uses.

use crate::token::{Token, TokenKind};

/// Split `value` into shell words with quote removal and backslash
/// escapes applied. Adjacent quoted/unquoted segments join into one
/// word (`'/a'\''b'` → `/a'b`), matching the zsh word rules.
///
/// Values containing no quoting characters take a fast path that is
/// byte-for-byte the historical `split_whitespace` behavior, so plain
/// aliases (`ls -la`, `grep --color=auto`) are provably unchanged.
pub fn split_words(value: &str) -> Vec<String> {
    if !value.contains(['\'', '"', '\\']) {
        return value.split_whitespace().map(str::to_string).collect();
    }

    let tokens = crate::lexer::tokenize(value.as_bytes());
    let mut words: Vec<String> = Vec::new();
    // The word being assembled from span-adjacent tokens, plus the
    // byte offset its last token ended at. A gap between spans means
    // the lexer skipped whitespace there — word boundary.
    let mut current: Option<(String, u32)> = None;

    for tok in tokens {
        match tok.kind {
            TokenKind::Eof => break,
            // Newlines separate words but contribute no text.
            TokenKind::Newline => {
                if let Some((w, _)) = current.take() {
                    words.push(w);
                }
                continue;
            }
            // A comment swallows everything to end-of-line. Preserve
            // the historical splitter's shape (each blank-separated
            // run is its own word) so values that mix quotes and `#`
            // don't silently drop trailing text.
            TokenKind::Comment => {
                if let Some((w, _)) = current.take() {
                    words.push(w);
                }
                words.extend(tok.text.split_whitespace().map(str::to_string));
                continue;
            }
            _ => {}
        }

        let piece = word_piece(&tok);
        current = Some(match current.take() {
            Some((mut w, end)) if tok.span.start == end => {
                w.push_str(&piece);
                (w, tok.span.end)
            }
            other => {
                if let Some((w, _)) = other {
                    words.push(w);
                }
                (piece, tok.span.end)
            }
        });
    }
    if let Some((w, _)) = current.take() {
        words.push(w);
    }
    words
}

/// The text a single token contributes to the word being assembled —
/// quotes stripped, escapes resolved. Operator/expansion tokens pass
/// through as raw text: this splitter does not evaluate them, it only
/// preserves them as literal argv content (identical to the historical
/// whitespace splitter's treatment).
fn word_piece(tok: &Token) -> String {
    match tok.kind {
        TokenKind::SingleQuoted => strip_quotes(&tok.text, '\'').to_string(),
        TokenKind::DoubleQuoted => unescape_double_quoted(strip_quotes(&tok.text, '"')),
        TokenKind::DollarSingleQuoted => {
            let inner = tok.text.strip_prefix("$'").unwrap_or(&tok.text);
            let inner = inner.strip_suffix('\'').unwrap_or(inner);
            unescape_ansi_c(inner)
        }
        TokenKind::Word => unescape_word(&tok.text),
        _ => tok.text.to_string(),
    }
}

/// Strip one layer of surrounding `quote` chars. The closing quote is
/// optional (the lexer tolerates unterminated quotes at EOF).
fn strip_quotes(raw: &str, quote: char) -> &str {
    let inner = raw.strip_prefix(quote).unwrap_or(raw);
    inner.strip_suffix(quote).unwrap_or(inner)
}

/// Backslash escapes in an unquoted word: `\X` → `X`, backslash-newline
/// is a line continuation (both dropped), a lone trailing `\` stays.
fn unescape_word(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\n') => {}
            Some(next) => out.push(next),
            None => out.push('\\'),
        }
    }
    out
}

/// Backslash escapes inside double quotes (zsh rules): only `$`, `` ` ``,
/// `"`, `\`, and newline are escapable; any other `\X` keeps the
/// backslash literally.
fn unescape_double_quoted(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('$' | '`' | '"' | '\\') => {
                out.push(chars.next().expect("peeked"));
            }
            Some('\n') => {
                chars.next();
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// ANSI-C escapes for `$'…'` — the common subset. Unknown escapes keep
/// both characters (zsh prints `$'\q'` as `\q`).
fn unescape_ansi_c(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('a') => out.push('\u{07}'),
            Some('b') => out.push('\u{08}'),
            Some('e') => out.push('\u{1b}'),
            Some('f') => out.push('\u{0c}'),
            Some('v') => out.push('\u{0b}'),
            Some('0') => out.push('\u{00}'),
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn unquoted_values_match_whitespace_splitting() {
        assert_eq!(split_words("ls -la"), vec!["ls", "-la"]);
        assert_eq!(
            split_words("grep --color=auto"),
            vec!["grep", "--color=auto"]
        );
        assert_eq!(split_words(""), Vec::<String>::new());
        assert_eq!(split_words("   "), Vec::<String>::new());
    }

    #[test]
    fn quoted_segments_have_quotes_removed() {
        assert_eq!(split_words("cd '/a/b'"), vec!["cd", "/a/b"]);
        assert_eq!(split_words("cd '/a b/c'"), vec!["cd", "/a b/c"]);
        assert_eq!(split_words(r#"cd "/a b""#), vec!["cd", "/a b"]);
    }

    #[test]
    fn adjacent_segments_join_into_one_word() {
        // `'/a'\''b'` — the POSIX way to embed a single quote.
        assert_eq!(split_words(r"cd '/a'\''b'"), vec!["cd", "/a'b"]);
        // Quoted + unquoted adjacency.
        assert_eq!(split_words("echo 'a'b\"c\""), vec!["echo", "abc"]);
    }

    #[test]
    fn backslash_escapes_resolve_in_words() {
        assert_eq!(split_words(r"cd /a\ b"), vec!["cd", "/a b"]);
    }

    #[test]
    fn dollar_and_operators_pass_through_literally() {
        // No expansion here — `$HOME` stays literal text, matching the
        // historical splitter (expansion is the executor's concern).
        assert_eq!(split_words("echo '$x' $HOME"), vec!["echo", "$x", "$HOME"]);
    }
}
