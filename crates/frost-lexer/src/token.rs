//! Token types for the zsh lexer.

use compact_str::CompactString;

/// Byte offset span in source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// A token produced by the lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// The raw text of the token (before any expansion).
    pub text: CompactString,
}

/// All token kinds recognized by the zsh lexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    // ── Literals ────────────────────────────────────────────
    /// Unquoted word (command name, argument, etc.)
    Word,
    /// Single-quoted string (no expansion)
    SingleQuoted,
    /// Double-quoted string (expansion inside)
    DoubleQuoted,
    /// $'...' ANSI-C quoting
    DollarSingleQuoted,
    /// Integer or float literal (in arithmetic context)
    Number,

    // ── Operators ───────────────────────────────────────────
    /// |
    Pipe,
    /// |&
    PipeAmpersand,
    /// ||
    OrOr,
    /// &&
    AndAnd,
    /// &
    Ampersand,
    /// &!  or  &|  (disown)
    Disown,
    /// ;
    Semi,
    /// ;;
    DoubleSemi,
    /// ;&
    SemiAnd,
    /// ;|
    SemiPipe,
    /// \n (significant in shell grammar)
    Newline,

    // ── Redirections ────────────────────────────────────────
    /// < (stdin)
    Less,
    /// > (stdout, clobber)
    Greater,
    /// >> (append)
    DoubleGreater,
    /// >| (clobber, noclobber override)
    GreaterPipe,
    /// >! (clobber, noclobber override — zsh-specific)
    GreaterBang,
    /// &> or >& (stdout+stderr)
    AmpGreater,
    /// &>> (append stdout+stderr)
    AmpDoubleGreater,
    /// << (heredoc)
    DoubleLess,
    /// <<< (herestring)
    TripleLess,
    /// <<- (heredoc, strip tabs)
    DoubleLessDash,
    /// <> (open read-write)
    LessGreater,
    /// <( (process substitution — read)
    ProcessSubIn,
    /// >( (process substitution — write)
    ProcessSubOut,
    /// N> (fd redirect, e.g. 2>)
    FdGreater,
    /// N< (fd redirect, e.g. 0<)
    FdLess,
    /// N>>
    FdDoubleGreater,
    /// N>&M (fd dup)
    FdDup,

    // ── Grouping ────────────────────────────────────────────
    /// (
    LeftParen,
    /// )
    RightParen,
    /// {
    LeftBrace,
    /// }
    RightBrace,
    /// [[ (conditional start)
    DoubleLeftBracket,
    /// ]] (conditional end)
    DoubleRightBracket,

    // ── Expansion markers ───────────────────────────────────
    /// $ (variable expansion start)
    Dollar,
    /// ${ (parameter expansion start)
    DollarBrace,
    /// $( (command substitution start)
    DollarParen,
    /// $(( (arithmetic expansion start)
    DollarDoubleParen,
    /// ` (backtick command substitution)
    Backtick,
    /// = (assignment or test operator)
    Equals,
    /// ~ (tilde expansion)
    Tilde,
    /// # (comment start — lexer produces this then skips to newline)
    Comment,
    /// ! (history expansion or negation)
    Bang,
    /// @ (used in ${arr[@]}, $@ etc.)
    At,
    /// * (glob or arithmetic)
    Star,
    /// ? (glob or ternary)
    Question,

    // ── Reserved words ──────────────────────────────────────
    /// if
    If,
    /// then
    Then,
    /// elif
    Elif,
    /// else
    Else,
    /// fi
    Fi,
    /// for
    For,
    /// in (for ... in)
    In,
    /// while
    While,
    /// until
    Until,
    /// do
    Do,
    /// done
    Done,
    /// case
    Case,
    /// esac
    Esac,
    /// select
    Select,
    /// function
    Function,
    /// time
    Time,
    /// coproc
    Coproc,
    /// [[ (reserved word form)
    CondStart,
    /// ]] (reserved word form)
    CondEnd,

    // ── Special ─────────────────────────────────────────────
    /// End of input
    Eof,
    /// Lexer error (unrecognized byte sequence)
    Error,
}

impl TokenKind {
    /// Whether this token kind is a reserved word.
    pub fn is_reserved_word(&self) -> bool {
        matches!(
            self,
            Self::If
                | Self::Then
                | Self::Elif
                | Self::Else
                | Self::Fi
                | Self::For
                | Self::In
                | Self::While
                | Self::Until
                | Self::Do
                | Self::Done
                | Self::Case
                | Self::Esac
                | Self::Select
                | Self::Function
                | Self::Time
                | Self::Coproc
        )
    }

    /// Whether this token kind terminates a simple command.
    pub fn is_separator(&self) -> bool {
        matches!(
            self,
            Self::Semi
                | Self::Newline
                | Self::Ampersand
                | Self::Pipe
                | Self::PipeAmpersand
                | Self::OrOr
                | Self::AndAnd
                | Self::Eof
        )
    }
}
