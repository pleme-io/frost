//! Main lexer implementation.

use compact_str::CompactString;

use crate::cursor::Cursor;
use crate::token::{Span, Token, TokenKind};

/// Zsh-compatible lexer.
///
/// Converts a byte slice into a stream of [`Token`]s. Call [`Lexer::next_token`]
/// repeatedly until [`TokenKind::Eof`] is returned.
pub struct Lexer<'src> {
    cursor: Cursor<'src>,
    src: &'src [u8],
    /// Whether the next word can be a reserved word (start of command position).
    command_position: bool,
    /// Brace nesting depth (for ${...} — suppress comment inside).
    brace_depth: u32,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src [u8]) -> Self {
        Self {
            cursor: Cursor::new(src),
            src,
            command_position: true,
            brace_depth: 0,
        }
    }

    /// Produce the next token.
    pub fn next_token(&mut self) -> Token {
        self.skip_whitespace();

        let start = self.cursor.pos() as u32;

        let Some(b) = self.cursor.peek() else {
            return self.make_token(TokenKind::Eof, start);
        };

        let kind = match b {
            b'#' if self.brace_depth == 0 => self.lex_comment(),
            b'#' => {
                // Inside ${...}, # is not a comment — treat as word
                self.cursor.advance();
                self.cursor.eat_while(|b| !is_meta(b));
                TokenKind::Word
            }
            b'\n' => {
                self.cursor.advance();
                self.command_position = true;
                TokenKind::Newline
            }
            b'\'' => self.lex_single_quoted(),
            b'"' => self.lex_double_quoted(),
            b'$' => self.lex_dollar(),
            b'`' => {
                self.cursor.advance();
                TokenKind::Backtick
            }
            b'|' => self.lex_pipe(),
            b'&' => self.lex_ampersand(),
            b';' => self.lex_semicolon(),
            b'(' => {
                self.cursor.advance();
                TokenKind::LeftParen
            }
            b')' => {
                self.cursor.advance();
                TokenKind::RightParen
            }
            b'{' => {
                self.cursor.advance();
                TokenKind::LeftBrace
            }
            b'}' => {
                self.cursor.advance();
                if self.brace_depth > 0 {
                    self.brace_depth -= 1;
                }
                TokenKind::RightBrace
            }
            b'<' => self.lex_less(),
            b'>' => self.lex_greater(),
            b'~' => {
                self.cursor.advance();
                TokenKind::Tilde
            }
            b'!' => {
                self.cursor.advance();
                TokenKind::Bang
            }
            b'=' => {
                self.cursor.advance();
                TokenKind::Equals
            }
            b'*' => {
                self.cursor.advance();
                TokenKind::Star
            }
            b'?' => {
                self.cursor.advance();
                TokenKind::Question
            }
            b'@' => {
                self.cursor.advance();
                TokenKind::At
            }
            b'0'..=b'9' if self.peek_fd_redirect_suffix().is_some() => self.lex_fd_redirect(),
            _ => self.lex_word(),
        };

        self.make_token(kind, start)
    }

    fn make_token(&self, kind: TokenKind, start: u32) -> Token {
        let end = self.cursor.pos() as u32;
        let text = String::from_utf8_lossy(&self.src[start as usize..end as usize]);
        Token {
            kind,
            span: Span::new(start, end),
            text: CompactString::new(&text),
        }
    }

    fn skip_whitespace(&mut self) {
        loop {
            self.cursor.eat_while(|b| b == b' ' || b == b'\t');
            // Backslash-newline continuation
            if self.cursor.peek() == Some(b'\\') && self.cursor.peek_nth(1) == Some(b'\n') {
                self.cursor.skip(2);
            } else {
                break;
            }
        }
    }

    fn lex_comment(&mut self) -> TokenKind {
        self.cursor.advance(); // skip #
        self.cursor.eat_while(|b| b != b'\n');
        TokenKind::Comment
    }

    fn lex_single_quoted(&mut self) -> TokenKind {
        self.cursor.advance(); // skip opening '
        loop {
            match self.cursor.advance() {
                Some(b'\'') => break,
                Some(_) => continue,
                None => break, // unterminated — let parser handle error
            }
        }
        self.command_position = false;
        TokenKind::SingleQuoted
    }

    fn lex_double_quoted(&mut self) -> TokenKind {
        self.cursor.advance(); // skip opening "
        loop {
            match self.cursor.advance() {
                Some(b'"') => break,
                Some(b'\\') => {
                    self.cursor.advance(); // skip escaped char
                }
                Some(_) => continue,
                None => break, // unterminated
            }
        }
        self.command_position = false;
        TokenKind::DoubleQuoted
    }

    fn lex_dollar(&mut self) -> TokenKind {
        self.cursor.advance(); // skip $
        match self.cursor.peek() {
            Some(b'{') => {
                self.cursor.advance();
                self.brace_depth += 1;
                TokenKind::DollarBrace
            }
            Some(b'(') => {
                if self.cursor.peek_nth(1) == Some(b'(') {
                    self.cursor.skip(2);
                    TokenKind::DollarDoubleParen
                } else {
                    self.cursor.advance();
                    TokenKind::DollarParen
                }
            }
            Some(b'\'') => {
                self.cursor.advance(); // skip '
                loop {
                    match self.cursor.advance() {
                        Some(b'\\') => {
                            self.cursor.advance();
                        }
                        Some(b'\'') => break,
                        Some(_) => continue,
                        None => break,
                    }
                }
                TokenKind::DollarSingleQuoted
            }
            _ => TokenKind::Dollar,
        }
    }

    fn lex_pipe(&mut self) -> TokenKind {
        self.cursor.advance(); // skip |
        match self.cursor.peek() {
            Some(b'|') => {
                self.cursor.advance();
                self.command_position = true;
                TokenKind::OrOr
            }
            Some(b'&') => {
                self.cursor.advance();
                self.command_position = true;
                TokenKind::PipeAmpersand
            }
            _ => {
                self.command_position = true;
                TokenKind::Pipe
            }
        }
    }

    fn lex_ampersand(&mut self) -> TokenKind {
        self.cursor.advance(); // skip &
        match self.cursor.peek() {
            Some(b'&') => {
                self.cursor.advance();
                self.command_position = true;
                TokenKind::AndAnd
            }
            Some(b'>') => {
                self.cursor.advance();
                if self.cursor.peek() == Some(b'>') {
                    self.cursor.advance();
                    TokenKind::AmpDoubleGreater
                } else {
                    TokenKind::AmpGreater
                }
            }
            Some(b'!' | b'|') => {
                self.cursor.advance();
                TokenKind::Disown
            }
            _ => {
                self.command_position = true;
                TokenKind::Ampersand
            }
        }
    }

    fn lex_semicolon(&mut self) -> TokenKind {
        self.cursor.advance(); // skip ;
        match self.cursor.peek() {
            Some(b';') => {
                self.cursor.advance();
                TokenKind::DoubleSemi
            }
            Some(b'&') => {
                self.cursor.advance();
                TokenKind::SemiAnd
            }
            Some(b'|') => {
                self.cursor.advance();
                TokenKind::SemiPipe
            }
            _ => {
                self.command_position = true;
                TokenKind::Semi
            }
        }
    }

    fn lex_less(&mut self) -> TokenKind {
        self.cursor.advance(); // skip <
        match self.cursor.peek() {
            Some(b'<') => {
                self.cursor.advance();
                if self.cursor.peek() == Some(b'<') {
                    self.cursor.advance();
                    TokenKind::TripleLess
                } else if self.cursor.peek() == Some(b'-') {
                    self.cursor.advance();
                    TokenKind::DoubleLessDash
                } else {
                    TokenKind::DoubleLess
                }
            }
            Some(b'>') => {
                self.cursor.advance();
                TokenKind::LessGreater
            }
            Some(b'(') => {
                self.cursor.advance();
                TokenKind::ProcessSubIn
            }
            _ => TokenKind::Less,
        }
    }

    fn lex_greater(&mut self) -> TokenKind {
        self.cursor.advance(); // skip >
        match self.cursor.peek() {
            Some(b'>') => {
                self.cursor.advance();
                TokenKind::DoubleGreater
            }
            Some(b'|') => {
                self.cursor.advance();
                TokenKind::GreaterPipe
            }
            Some(b'!') => {
                self.cursor.advance();
                TokenKind::GreaterBang
            }
            Some(b'(') => {
                self.cursor.advance();
                TokenKind::ProcessSubOut
            }
            Some(b'&') => {
                self.cursor.advance();
                TokenKind::AmpGreater
            }
            _ => TokenKind::Greater,
        }
    }

    /// Look-ahead: the current cursor byte is a digit — is this the
    /// start of a fd-prefixed redirect (`2>`, `2>>`, `2<`, `2>&`)? If
    /// so, return the resulting `TokenKind` without moving the cursor.
    /// A digit with whitespace or a non-redirect char after is NOT an
    /// fd-prefix — that's a regular word/argument.
    fn peek_fd_redirect_suffix(&self) -> Option<TokenKind> {
        // Count the digit run starting at the cursor.
        let mut i = 0;
        while let Some(b) = self.cursor.peek_nth(i) {
            if b.is_ascii_digit() {
                i += 1;
            } else {
                break;
            }
        }
        if i == 0 {
            return None;
        }
        // Peek past the digit run for the operator.
        match self.cursor.peek_nth(i)? {
            b'>' => match self.cursor.peek_nth(i + 1) {
                Some(b'>') => Some(TokenKind::FdDoubleGreater),
                Some(b'&') => Some(TokenKind::FdDup),
                _ => Some(TokenKind::FdGreater),
            },
            b'<' => match self.cursor.peek_nth(i + 1) {
                Some(b'&') => Some(TokenKind::FdDup),
                _ => Some(TokenKind::FdLess),
            },
            _ => None,
        }
    }

    /// Consume a fd-prefixed redirect token (`2>`, `2>>`, `2<`, `2>&`,
    /// `2<&`). Cursor is positioned at the first digit; advances past
    /// the full operator. The token text captures the fd digits + op
    /// so the parser's `parse_fd_prefix` can recover the fd number.
    fn lex_fd_redirect(&mut self) -> TokenKind {
        // Consume digit run.
        self.cursor.eat_while(|b| b.is_ascii_digit());
        let kind = match self.cursor.peek() {
            Some(b'>') => {
                self.cursor.advance();
                match self.cursor.peek() {
                    Some(b'>') => {
                        self.cursor.advance();
                        TokenKind::FdDoubleGreater
                    }
                    Some(b'&') => {
                        self.cursor.advance();
                        TokenKind::FdDup
                    }
                    _ => TokenKind::FdGreater,
                }
            }
            Some(b'<') => {
                self.cursor.advance();
                match self.cursor.peek() {
                    Some(b'&') => {
                        self.cursor.advance();
                        TokenKind::FdDup
                    }
                    _ => TokenKind::FdLess,
                }
            }
            // Peek-predicate guaranteed we'd see one of the above.
            _ => unreachable!("peek_fd_redirect_suffix agreed but lex_fd_redirect saw nothing"),
        };
        self.command_position = false;
        kind
    }

    fn lex_word(&mut self) -> TokenKind {
        loop {
            self.cursor.eat_while(|b| !is_meta(b));
            // Handle backslash escape in words: \c makes c literal
            if self.cursor.peek() == Some(b'\\') {
                if self.cursor.peek_nth(1) == Some(b'\n') {
                    // Backslash-newline: line continuation, skip both
                    self.cursor.skip(2);
                    continue;
                } else if self.cursor.peek_nth(1).is_some() {
                    // Backslash-escape: consume both chars
                    self.cursor.skip(2);
                    continue;
                }
            }
            break;
        }
        let was_command = self.command_position;
        self.command_position = false;

        if was_command {
            let start = self.cursor.pos();
            // Check if this word is a reserved word (only in command position)
            let text = &self.src[start..self.cursor.pos()];
            return match text {
                b"if" => TokenKind::If,
                b"then" => TokenKind::Then,
                b"elif" => TokenKind::Elif,
                b"else" => TokenKind::Else,
                b"fi" => TokenKind::Fi,
                b"for" => TokenKind::For,
                b"in" => TokenKind::In,
                b"while" => TokenKind::While,
                b"until" => TokenKind::Until,
                b"do" => TokenKind::Do,
                b"done" => TokenKind::Done,
                b"case" => TokenKind::Case,
                b"esac" => TokenKind::Esac,
                b"select" => TokenKind::Select,
                b"function" => TokenKind::Function,
                b"time" => TokenKind::Time,
                b"coproc" => TokenKind::Coproc,
                _ => TokenKind::Word,
            };
        }

        TokenKind::Word
    }
}

/// Whether a byte is a shell metacharacter (terminates a word).
fn is_meta(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\n'
            | b'|'
            | b'&'
            | b';'
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'\''
            | b'"'
            | b'`'
            | b'$'
            | b'#'
            | b'='
            | b'{'
            | b'}'
            | b'~'
            | b'!'
            | b'*'
            | b'?'
            | b'@'
            | b'\\'
    )
}

/// Tokenize an entire source string into a Vec of tokens.
#[allow(dead_code)] // Tatara-lisp/debug tooling calls this; the runtime uses the streaming API.
pub fn tokenize(src: &[u8]) -> Vec<Token> {
    let mut lexer = Lexer::new(src);
    let mut tokens = Vec::new();
    loop {
        let tok = lexer.next_token();
        let is_eof = tok.kind == TokenKind::Eof;
        tokens.push(tok);
        if is_eof {
            break;
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src.as_bytes())
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn simple_command() {
        assert_eq!(
            kinds("ls -la"),
            vec![TokenKind::Word, TokenKind::Word, TokenKind::Eof]
        );
    }

    #[test]
    fn pipe_chain() {
        assert_eq!(
            kinds("cat foo | grep bar"),
            vec![
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Pipe,
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn redirections() {
        assert_eq!(
            kinds("echo hello > out.txt 2>&1"),
            vec![
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Greater,
                TokenKind::Word,
                TokenKind::FdDup, // 2>& — lexed as one fd-prefixed op
                TokenKind::Word,  // 1
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn fd_prefix_redirects() {
        // Regression for the `cat foo 2>/dev/null` bug where `2` was
        // swallowed as a word argument. Each form below must produce
        // one fd-prefixed token, not a digit-word + operator.
        assert_eq!(
            kinds("cmd 2>/dev/null"),
            vec![
                TokenKind::Word,
                TokenKind::FdGreater,
                TokenKind::Word,
                TokenKind::Eof,
            ],
            "2> must be a single FdGreater token",
        );
        assert_eq!(
            kinds("cmd 2>>log"),
            vec![
                TokenKind::Word,
                TokenKind::FdDoubleGreater,
                TokenKind::Word,
                TokenKind::Eof,
            ],
            "2>> must be a single FdDoubleGreater token",
        );
        assert_eq!(
            kinds("cmd 2<input"),
            vec![
                TokenKind::Word,
                TokenKind::FdLess,
                TokenKind::Word,
                TokenKind::Eof,
            ],
            "2< must be a single FdLess token",
        );
        // A digit followed by whitespace is NOT an fd-prefix — it's
        // just an argument.
        assert_eq!(
            kinds("cmd 2 > out"),
            vec![
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Greater,
                TokenKind::Word,
                TokenKind::Eof,
            ],
            "digit-space-greater must NOT be an fd redirect",
        );
    }

    #[test]
    fn single_quoted() {
        assert_eq!(
            kinds("echo 'hello world'"),
            vec![TokenKind::Word, TokenKind::SingleQuoted, TokenKind::Eof]
        );
    }

    #[test]
    fn double_quoted() {
        assert_eq!(
            kinds(r#"echo "hello $USER""#),
            vec![TokenKind::Word, TokenKind::DoubleQuoted, TokenKind::Eof]
        );
    }

    #[test]
    fn command_substitution() {
        assert_eq!(
            kinds("echo $(date)"),
            vec![
                TokenKind::Word,
                TokenKind::DollarParen,
                TokenKind::Word,
                TokenKind::RightParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn and_or() {
        assert_eq!(
            kinds("true && echo yes || echo no"),
            vec![
                TokenKind::Word,
                TokenKind::AndAnd,
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::OrOr,
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn heredoc_marker() {
        assert_eq!(
            kinds("cat <<EOF"),
            vec![
                TokenKind::Word,
                TokenKind::DoubleLess,
                TokenKind::Word,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn herestring() {
        assert_eq!(
            kinds("cat <<<'hello'"),
            vec![
                TokenKind::Word,
                TokenKind::TripleLess,
                TokenKind::SingleQuoted,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn process_substitution() {
        assert_eq!(
            kinds("diff <(cmd1) >(cmd2)"),
            vec![
                TokenKind::Word,
                TokenKind::ProcessSubIn,
                TokenKind::Word,
                TokenKind::RightParen,
                TokenKind::ProcessSubOut,
                TokenKind::Word,
                TokenKind::RightParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn comment() {
        assert_eq!(
            kinds("echo hi # this is a comment\necho bye"),
            vec![
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Comment,
                TokenKind::Newline,
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn dollar_single_quoted() {
        assert_eq!(
            kinds(r"echo $'\n'"),
            vec![
                TokenKind::Word,
                TokenKind::DollarSingleQuoted,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn background() {
        assert_eq!(
            kinds("sleep 10 &"),
            vec![
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Ampersand,
                TokenKind::Eof,
            ]
        );
    }
}
