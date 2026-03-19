//! Recursive descent parser for zsh-compatible shell grammar.
//!
//! Grammar hierarchy:
//!   Program          → CompleteCommand*
//!   CompleteCommand   → List [&]
//!   List              → Pipeline ((&& | ||) Pipeline)*
//!   Pipeline          → [!] Command (| Command)*
//!   Command           → SimpleCommand | CompoundCommand
//!   CompoundCommand   → Subshell | BraceGroup | If | For | While | Until | Case | Select | FunctionDef
//!   SimpleCommand     → (Assignment | Word | Redirect)*

use crate::ast::*;
use compact_str::CompactString;
use frost_lexer::{Span, Token, TokenKind};

/// Parse error with position context.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("unexpected token {kind:?} at position {pos}, expected {expected}")]
    Unexpected {
        kind: TokenKind,
        pos: usize,
        expected: String,
    },

    #[error("unexpected end of input, expected {expected}")]
    UnexpectedEof { expected: String },
}

pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse(&mut self) -> Program {
        let commands = self.parse_program();
        Program { commands }
    }

    // ── Helpers ────────────────────────────────────────────────

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&self.tokens[self.tokens.len() - 1])
    }

    fn kind(&self) -> TokenKind {
        self.peek().kind
    }

    fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos.min(self.tokens.len() - 1)];
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.kind() == kind || self.word_matches_keyword(kind)
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// The lexer may produce `Word("if")` instead of `TokenKind::If` depending on
    /// command_position context. This helper matches either form.
    fn word_matches_keyword(&self, kind: TokenKind) -> bool {
        if self.kind() != TokenKind::Word {
            return false;
        }
        let text = self.peek().text.as_str();
        matches!(
            (text, kind),
            ("if", TokenKind::If)
                | ("then", TokenKind::Then)
                | ("elif", TokenKind::Elif)
                | ("else", TokenKind::Else)
                | ("fi", TokenKind::Fi)
                | ("for", TokenKind::For)
                | ("in", TokenKind::In)
                | ("while", TokenKind::While)
                | ("until", TokenKind::Until)
                | ("do", TokenKind::Do)
                | ("done", TokenKind::Done)
                | ("case", TokenKind::Case)
                | ("esac", TokenKind::Esac)
                | ("select", TokenKind::Select)
                | ("function", TokenKind::Function)
                | ("time", TokenKind::Time)
                | ("coproc", TokenKind::Coproc)
        )
    }

    fn expect(&mut self, kind: TokenKind) {
        if !self.eat(kind) {
            // Best-effort: skip the unexpected token and continue
            self.advance();
        }
    }

    fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.advance();
        }
    }

    fn at_eof(&self) -> bool {
        self.at(TokenKind::Eof)
    }

    fn span(&self) -> Span {
        self.peek().span
    }

    /// Whether the current token can start a command.
    fn at_command_start(&self) -> bool {
        // Check if it's a keyword word like Word("if")
        if self.kind() == TokenKind::Word {
            let text = self.peek().text.as_str();
            if matches!(text, "if" | "for" | "while" | "until" | "case" | "select" | "function" | "time" | "coproc") {
                return true;
            }
        }
        matches!(
            self.kind(),
            TokenKind::Word
                | TokenKind::SingleQuoted
                | TokenKind::DoubleQuoted
                | TokenKind::DollarSingleQuoted
                | TokenKind::Dollar
                | TokenKind::DollarBrace
                | TokenKind::DollarParen
                | TokenKind::DollarDoubleParen
                | TokenKind::Backtick
                | TokenKind::Tilde
                | TokenKind::Star
                | TokenKind::Question
                | TokenKind::At
                | TokenKind::Bang
                | TokenKind::Less
                | TokenKind::Greater
                | TokenKind::DoubleGreater
                | TokenKind::AmpGreater
                | TokenKind::AmpDoubleGreater
                | TokenKind::GreaterPipe
                | TokenKind::GreaterBang
                | TokenKind::DoubleLess
                | TokenKind::TripleLess
                | TokenKind::LessGreater
                | TokenKind::FdGreater
                | TokenKind::FdLess
                | TokenKind::FdDoubleGreater
                | TokenKind::FdDup
                | TokenKind::Number
                | TokenKind::LeftParen
                | TokenKind::LeftBrace
                | TokenKind::If
                | TokenKind::For
                | TokenKind::While
                | TokenKind::Until
                | TokenKind::Case
                | TokenKind::Select
                | TokenKind::Function
                | TokenKind::Time
                | TokenKind::Coproc
        )
    }

    /// Whether the current token is a word-like token (can be part of a Word).
    fn at_word(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Word
                | TokenKind::SingleQuoted
                | TokenKind::DoubleQuoted
                | TokenKind::DollarSingleQuoted
                | TokenKind::Dollar
                | TokenKind::DollarBrace
                | TokenKind::DollarParen
                | TokenKind::DollarDoubleParen
                | TokenKind::Backtick
                | TokenKind::Tilde
                | TokenKind::Star
                | TokenKind::Question
                | TokenKind::At
                | TokenKind::Number
                | TokenKind::Equals
        )
    }

    fn at_redirect(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Less
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
                | TokenKind::FdGreater
                | TokenKind::FdLess
                | TokenKind::FdDoubleGreater
                | TokenKind::FdDup
        )
    }

    // ── Program ────────────────────────────────────────────────

    fn parse_program(&mut self) -> Vec<CompleteCommand> {
        let mut commands = Vec::new();
        self.skip_newlines();

        while !self.at_eof() {
            if self.at_command_start() {
                commands.push(self.parse_complete_command());
            }
            // Consume separators between commands
            if !self.eat(TokenKind::Semi) && !self.eat(TokenKind::Newline) {
                if !self.at_eof() {
                    self.skip_newlines();
                }
            }
            self.skip_newlines();
        }
        commands
    }

    // ── CompleteCommand ────────────────────────────────────────

    fn parse_complete_command(&mut self) -> CompleteCommand {
        let list = self.parse_list();
        let is_async = self.eat(TokenKind::Ampersand);
        CompleteCommand { list, is_async }
    }

    // ── List ───────────────────────────────────────────────────

    fn parse_list(&mut self) -> List {
        let first = self.parse_pipeline();
        let mut rest = Vec::new();

        loop {
            let op = if self.eat(TokenKind::AndAnd) {
                Some(ListOp::And)
            } else if self.eat(TokenKind::OrOr) {
                Some(ListOp::Or)
            } else {
                None
            };

            match op {
                Some(op) => {
                    self.skip_newlines();
                    rest.push((op, self.parse_pipeline()));
                }
                None => break,
            }
        }

        List { first, rest }
    }

    // ── Pipeline ───────────────────────────────────────────────

    fn parse_pipeline(&mut self) -> Pipeline {
        let bang = self.eat(TokenKind::Bang);
        let first = self.parse_command();
        let mut commands = vec![first];
        let mut pipe_stderr = Vec::new();

        loop {
            if self.eat(TokenKind::Pipe) {
                pipe_stderr.push(false);
                self.skip_newlines();
                commands.push(self.parse_command());
            } else if self.eat(TokenKind::PipeAmpersand) {
                pipe_stderr.push(true);
                self.skip_newlines();
                commands.push(self.parse_command());
            } else {
                break;
            }
        }

        Pipeline { bang, commands, pipe_stderr }
    }

    // ── Command ────────────────────────────────────────────────

    fn parse_command(&mut self) -> Command {
        // Check both TokenKind and Word text for keywords
        if self.at(TokenKind::LeftParen) { return self.parse_subshell(); }
        if self.at(TokenKind::LeftBrace) { return self.parse_brace_group(); }
        if self.at(TokenKind::If) { return self.parse_if(); }
        if self.at(TokenKind::For) { return self.parse_for(); }
        if self.at(TokenKind::While) { return self.parse_while(); }
        if self.at(TokenKind::Until) { return self.parse_until(); }
        if self.at(TokenKind::Case) { return self.parse_case(); }
        if self.at(TokenKind::Select) { return self.parse_select(); }
        if self.at(TokenKind::Function) { return self.parse_function_def(); }
        if self.at(TokenKind::Time) { return self.parse_time(); }
        if self.at(TokenKind::Coproc) { return self.parse_coproc(); }

        match self.kind() {
            _ => {
                // Check for function definition: name () { ... }
                if self.is_function_def_ahead() {
                    return self.parse_function_def_short();
                }
                Command::Simple(self.parse_simple_command())
            }
        }
    }

    fn is_function_def_ahead(&self) -> bool {
        // name () — function definition without 'function' keyword
        if self.kind() == TokenKind::Word {
            if let Some(next) = self.tokens.get(self.pos + 1) {
                if next.kind == TokenKind::LeftParen {
                    if let Some(after) = self.tokens.get(self.pos + 2) {
                        return after.kind == TokenKind::RightParen;
                    }
                }
            }
        }
        false
    }

    // ── SimpleCommand ──────────────────────────────────────────

    fn parse_simple_command(&mut self) -> SimpleCommand {
        let mut assignments = Vec::new();
        let mut words = Vec::new();
        let mut redirects = Vec::new();

        // Parse leading assignments (before any command word)
        while self.is_assignment() && words.is_empty() {
            assignments.push(self.parse_assignment());
        }

        // Parse words and redirects
        while !self.at_eof()
            && !self.kind().is_separator()
            && !matches!(
                self.kind(),
                TokenKind::RightParen
                    | TokenKind::RightBrace
                    | TokenKind::DoubleSemi
                    | TokenKind::SemiAnd
                    | TokenKind::SemiPipe
                    | TokenKind::Then
                    | TokenKind::Elif
                    | TokenKind::Else
                    | TokenKind::Fi
                    | TokenKind::Do
                    | TokenKind::Done
                    | TokenKind::Esac
            )
            // Also check word-based keywords
            && !self.at(TokenKind::Then)
            && !self.at(TokenKind::Elif)
            && !self.at(TokenKind::Else)
            && !self.at(TokenKind::Fi)
            && !self.at(TokenKind::Do)
            && !self.at(TokenKind::Done)
            && !self.at(TokenKind::Esac)
        {
            if self.at_redirect() {
                redirects.push(self.parse_redirect());
            } else if self.at_word() {
                words.push(self.parse_word());
            } else {
                break;
            }
        }

        SimpleCommand { assignments, words, redirects }
    }

    fn is_assignment(&self) -> bool {
        // Pattern: Word Equals [Word] — the lexer splits FOO=bar into three tokens
        if self.kind() != TokenKind::Word {
            return false;
        }
        let name = &self.peek().text;
        let is_ident = !name.is_empty()
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !name.bytes().next().unwrap_or(b'0').is_ascii_digit();
        if !is_ident {
            return false;
        }
        // Check if next token is Equals
        self.tokens.get(self.pos + 1).is_some_and(|t| t.kind == TokenKind::Equals)
    }

    fn parse_assignment(&mut self) -> Assignment {
        let name_tok = self.advance().clone(); // Word (the name)
        let eq_span = self.peek().span;
        self.expect(TokenKind::Equals); // =

        // Value is the next word token (if present and not a separator)
        let value = if self.at_word() {
            Some(self.parse_word())
        } else {
            None
        };

        Assignment {
            name: name_tok.text.clone(),
            op: AssignOp::Assign,
            value,
            span: Span::new(name_tok.span.start, eq_span.end),
        }
    }

    // ── Word ───────────────────────────────────────────────────

    fn parse_word(&mut self) -> Word {
        let start_span = self.span();
        let mut parts = Vec::new();

        // A Word is one or more adjacent word-like tokens with no whitespace separation.
        // The lexer handles whitespace splitting, so each call here produces one Word.
        // For now, we consume a single word-like token and convert it.
        let tok = self.advance().clone();
        match tok.kind {
            TokenKind::Word | TokenKind::Number => {
                parts.push(WordPart::Literal(tok.text.clone()));
            }
            TokenKind::SingleQuoted => {
                // Strip surrounding quotes
                let inner = strip_quotes(&tok.text, '\'');
                parts.push(WordPart::SingleQuoted(inner));
            }
            TokenKind::DoubleQuoted | TokenKind::DollarSingleQuoted => {
                let inner = strip_quotes(&tok.text, '"');
                parts.extend(parse_double_quoted_parts(&inner));
            }
            TokenKind::Dollar => {
                // $VAR — the next token should be the variable name
                if self.kind() == TokenKind::Word {
                    let name_tok = self.advance();
                    parts.push(WordPart::DollarVar(name_tok.text.clone()));
                } else {
                    parts.push(WordPart::Literal(CompactString::from("$")));
                }
            }
            TokenKind::DollarBrace => {
                // ${...} — consume until matching }
                let param = if self.kind() == TokenKind::Word {
                    let t = self.advance();
                    t.text.clone()
                } else {
                    CompactString::default()
                };
                // Skip to closing brace (simplified — doesn't handle nested braces)
                while !self.at_eof() && !self.at(TokenKind::RightBrace) {
                    self.advance();
                }
                self.eat(TokenKind::RightBrace);
                parts.push(WordPart::DollarBrace {
                    param,
                    operator: None,
                    arg: None,
                });
            }
            TokenKind::DollarParen => {
                // $(cmd) — for now, capture as CommandSub with empty program
                // Full implementation would recursively parse the inner tokens
                parts.push(WordPart::CommandSub(Box::new(Program {
                    commands: vec![],
                })));
                // Skip to matching )
                let mut depth = 1u32;
                while !self.at_eof() && depth > 0 {
                    if self.at(TokenKind::LeftParen) || self.at(TokenKind::DollarParen) {
                        depth += 1;
                    } else if self.at(TokenKind::RightParen) {
                        depth -= 1;
                        if depth == 0 {
                            self.advance();
                            break;
                        }
                    }
                    self.advance();
                }
            }
            TokenKind::DollarDoubleParen => {
                // $((expr)) — arithmetic substitution
                let mut expr = String::new();
                while !self.at_eof() {
                    // Look for ))
                    if self.at(TokenKind::RightParen) {
                        self.advance();
                        if self.eat(TokenKind::RightParen) {
                            break;
                        }
                        expr.push(')');
                    } else {
                        expr.push_str(&self.advance().text);
                    }
                }
                parts.push(WordPart::ArithSub(CompactString::from(expr)));
            }
            TokenKind::Backtick => {
                parts.push(WordPart::CommandSub(Box::new(Program {
                    commands: vec![],
                })));
            }
            TokenKind::Star => parts.push(WordPart::Glob(GlobKind::Star)),
            TokenKind::Question => parts.push(WordPart::Glob(GlobKind::Question)),
            TokenKind::At => parts.push(WordPart::Glob(GlobKind::At)),
            TokenKind::Tilde => {
                // ~user or just ~
                let user = if self.kind() == TokenKind::Word {
                    let t = self.advance();
                    t.text.clone()
                } else {
                    CompactString::default()
                };
                parts.push(WordPart::Tilde(user));
            }
            TokenKind::Equals => {
                parts.push(WordPart::Literal(CompactString::from("=")));
            }
            _ => {
                // Fallback: treat as literal
                parts.push(WordPart::Literal(tok.text.clone()));
            }
        }

        Word {
            parts,
            span: start_span,
        }
    }

    // ── Redirect ───────────────────────────────────────────────

    fn parse_redirect(&mut self) -> Redirect {
        let redir_tok = self.advance().clone();
        let (fd, op) = match redir_tok.kind {
            TokenKind::Less => (None, RedirectOp::Less),
            TokenKind::Greater => (None, RedirectOp::Greater),
            TokenKind::DoubleGreater => (None, RedirectOp::DoubleGreater),
            TokenKind::GreaterPipe => (None, RedirectOp::GreaterPipe),
            TokenKind::GreaterBang => (None, RedirectOp::GreaterBang),
            TokenKind::AmpGreater => (None, RedirectOp::AmpGreater),
            TokenKind::AmpDoubleGreater => (None, RedirectOp::AmpDoubleGreater),
            TokenKind::DoubleLess => (None, RedirectOp::DoubleLess),
            TokenKind::TripleLess => (None, RedirectOp::TripleLess),
            TokenKind::DoubleLessDash => (None, RedirectOp::DoubleLessDash),
            TokenKind::LessGreater => (None, RedirectOp::LessGreater),
            TokenKind::FdGreater => {
                let fd_num = parse_fd_prefix(&redir_tok.text);
                (Some(fd_num), RedirectOp::Greater)
            }
            TokenKind::FdLess => {
                let fd_num = parse_fd_prefix(&redir_tok.text);
                (Some(fd_num), RedirectOp::Less)
            }
            TokenKind::FdDoubleGreater => {
                let fd_num = parse_fd_prefix(&redir_tok.text);
                (Some(fd_num), RedirectOp::DoubleGreater)
            }
            TokenKind::FdDup => {
                let fd_num = parse_fd_prefix(&redir_tok.text);
                (Some(fd_num), RedirectOp::FdDup)
            }
            _ => (None, RedirectOp::Greater),
        };

        // Parse the target word
        let target = if self.at_word() {
            self.parse_word()
        } else {
            // Missing target — produce empty word
            Word {
                parts: vec![],
                span: self.span(),
            }
        };

        Redirect {
            fd,
            op,
            target,
            span: redir_tok.span,
        }
    }

    // ── Compound commands ──────────────────────────────────────

    fn parse_subshell(&mut self) -> Command {
        self.expect(TokenKind::LeftParen);
        self.skip_newlines();
        let body = self.parse_compound_body(&[TokenKind::RightParen]);
        self.expect(TokenKind::RightParen);
        let redirects = self.parse_trailing_redirects();
        Command::Subshell(Subshell { body, redirects })
    }

    fn parse_brace_group(&mut self) -> Command {
        self.expect(TokenKind::LeftBrace);
        self.skip_newlines();
        let body = self.parse_compound_body(&[TokenKind::RightBrace]);
        self.expect(TokenKind::RightBrace);
        let redirects = self.parse_trailing_redirects();
        Command::BraceGroup(BraceGroup { body, redirects })
    }

    fn parse_if(&mut self) -> Command {
        self.expect(TokenKind::If);
        self.skip_newlines();
        let condition = self.parse_compound_body(&[TokenKind::Then]);
        self.expect(TokenKind::Then);
        self.skip_newlines();
        let then_body = self.parse_compound_body(&[TokenKind::Elif, TokenKind::Else, TokenKind::Fi]);

        let mut elifs = Vec::new();
        while self.eat(TokenKind::Elif) {
            self.skip_newlines();
            let elif_cond = self.parse_compound_body(&[TokenKind::Then]);
            self.expect(TokenKind::Then);
            self.skip_newlines();
            let elif_body = self.parse_compound_body(&[TokenKind::Elif, TokenKind::Else, TokenKind::Fi]);
            elifs.push((elif_cond, elif_body));
        }

        let else_body = if self.eat(TokenKind::Else) {
            self.skip_newlines();
            Some(self.parse_compound_body(&[TokenKind::Fi]))
        } else {
            None
        };

        self.expect(TokenKind::Fi);
        let redirects = self.parse_trailing_redirects();
        Command::If(Box::new(IfClause {
            condition,
            then_body,
            elifs,
            else_body,
            redirects,
        }))
    }

    fn parse_for(&mut self) -> Command {
        self.expect(TokenKind::For);
        let var = self.advance().text.clone();

        let words = if self.eat(TokenKind::In) {
            let mut ws = Vec::new();
            while self.at_word() {
                ws.push(self.parse_word());
            }
            // Consume separator
            let _ = self.eat(TokenKind::Semi) || self.eat(TokenKind::Newline);
            Some(ws)
        } else {
            let _ = self.eat(TokenKind::Semi) || self.eat(TokenKind::Newline);
            None
        };

        self.skip_newlines();
        self.expect(TokenKind::Do);
        self.skip_newlines();
        let body = self.parse_compound_body(&[TokenKind::Done]);
        self.expect(TokenKind::Done);
        let redirects = self.parse_trailing_redirects();
        Command::For(Box::new(ForClause { var, words, body, redirects }))
    }

    fn parse_while(&mut self) -> Command {
        self.expect(TokenKind::While);
        self.skip_newlines();
        let condition = self.parse_compound_body(&[TokenKind::Do]);
        self.expect(TokenKind::Do);
        self.skip_newlines();
        let body = self.parse_compound_body(&[TokenKind::Done]);
        self.expect(TokenKind::Done);
        let redirects = self.parse_trailing_redirects();
        Command::While(Box::new(WhileClause { condition, body, redirects }))
    }

    fn parse_until(&mut self) -> Command {
        self.expect(TokenKind::Until);
        self.skip_newlines();
        let condition = self.parse_compound_body(&[TokenKind::Do]);
        self.expect(TokenKind::Do);
        self.skip_newlines();
        let body = self.parse_compound_body(&[TokenKind::Done]);
        self.expect(TokenKind::Done);
        let redirects = self.parse_trailing_redirects();
        Command::Until(Box::new(UntilClause { condition, body, redirects }))
    }

    fn parse_case(&mut self) -> Command {
        self.expect(TokenKind::Case);
        let word = self.parse_word();
        self.skip_newlines();
        self.expect(TokenKind::In);
        self.skip_newlines();

        let mut items = Vec::new();
        while !self.at(TokenKind::Esac) && !self.at_eof() {
            // Optional leading (
            self.eat(TokenKind::LeftParen);

            // Parse patterns: pat1 | pat2 )
            let mut patterns = Vec::new();
            if self.at_word() {
                patterns.push(self.parse_word());
                while self.eat(TokenKind::Pipe) {
                    if self.at_word() {
                        patterns.push(self.parse_word());
                    }
                }
            }
            self.expect(TokenKind::RightParen);
            self.skip_newlines();

            // Parse body until ;; or ;& or ;| or esac
            let body = self.parse_compound_body(&[
                TokenKind::DoubleSemi,
                TokenKind::SemiAnd,
                TokenKind::SemiPipe,
                TokenKind::Esac,
            ]);

            let terminator = if self.eat(TokenKind::SemiAnd) {
                CaseTerminator::SemiAnd
            } else if self.eat(TokenKind::SemiPipe) {
                CaseTerminator::SemiPipe
            } else {
                self.eat(TokenKind::DoubleSemi);
                CaseTerminator::DoubleSemi
            };
            self.skip_newlines();

            if !patterns.is_empty() {
                items.push(CaseItem { patterns, body, terminator });
            }
        }

        self.expect(TokenKind::Esac);
        let redirects = self.parse_trailing_redirects();
        Command::Case(Box::new(CaseClause { word, items, redirects }))
    }

    fn parse_select(&mut self) -> Command {
        self.expect(TokenKind::Select);
        let var = self.advance().text.clone();

        let words = if self.eat(TokenKind::In) {
            let mut ws = Vec::new();
            while self.at_word() {
                ws.push(self.parse_word());
            }
            let _ = self.eat(TokenKind::Semi) || self.eat(TokenKind::Newline);
            Some(ws)
        } else {
            let _ = self.eat(TokenKind::Semi) || self.eat(TokenKind::Newline);
            None
        };

        self.skip_newlines();
        self.expect(TokenKind::Do);
        self.skip_newlines();
        let body = self.parse_compound_body(&[TokenKind::Done]);
        self.expect(TokenKind::Done);
        let redirects = self.parse_trailing_redirects();
        Command::Select(Box::new(SelectClause { var, words, body, redirects }))
    }

    fn parse_function_def(&mut self) -> Command {
        self.expect(TokenKind::Function);
        let name = self.advance().text.clone();
        // Optional ()
        if self.eat(TokenKind::LeftParen) {
            self.eat(TokenKind::RightParen);
        }
        self.skip_newlines();
        let body = self.parse_command();
        let redirects = self.parse_trailing_redirects();
        Command::FunctionDef(Box::new(FunctionDef { name, body, redirects }))
    }

    fn parse_function_def_short(&mut self) -> Command {
        // name () { ... }
        let name = self.advance().text.clone();
        self.expect(TokenKind::LeftParen);
        self.expect(TokenKind::RightParen);
        self.skip_newlines();
        let body = self.parse_command();
        let redirects = self.parse_trailing_redirects();
        Command::FunctionDef(Box::new(FunctionDef { name, body, redirects }))
    }

    fn parse_time(&mut self) -> Command {
        self.expect(TokenKind::Time);
        let pipeline = self.parse_pipeline();
        Command::Time(Box::new(TimeClause { pipeline }))
    }

    fn parse_coproc(&mut self) -> Command {
        self.expect(TokenKind::Coproc);
        let name = if self.kind() == TokenKind::Word
            && !self.tokens.get(self.pos + 1)
                .is_some_and(|t| t.kind == TokenKind::LeftParen || t.kind == TokenKind::LeftBrace)
        {
            None
        } else {
            Some(self.advance().text.clone())
        };
        let command = self.parse_command();
        Command::Coproc(Box::new(Coproc { name, command }))
    }

    // ── Compound body helper ───────────────────────────────────

    /// Whether the current position matches any of the stop tokens (including word-keyword fallback).
    fn at_any(&self, kinds: &[TokenKind]) -> bool {
        kinds.iter().any(|k| self.at(*k))
    }

    /// Parse a sequence of complete commands until one of the stop tokens.
    fn parse_compound_body(&mut self, stop: &[TokenKind]) -> Vec<CompleteCommand> {
        let mut commands = Vec::new();
        loop {
            self.skip_newlines();
            if self.at_eof() || self.at_any(stop) {
                break;
            }
            if self.at_command_start() {
                commands.push(self.parse_complete_command());
            }
            // Consume separators
            if !self.eat(TokenKind::Semi) && !self.eat(TokenKind::Newline) {
                if self.at_eof() || self.at_any(stop) {
                    break;
                }
            }
        }
        commands
    }

    fn parse_trailing_redirects(&mut self) -> Vec<Redirect> {
        let mut redirects = Vec::new();
        while self.at_redirect() {
            redirects.push(self.parse_redirect());
        }
        redirects
    }
}

// ── Helper functions ───────────────────────────────────────────

fn strip_quotes(text: &str, quote: char) -> CompactString {
    let s = text.strip_prefix(quote).unwrap_or(text);
    let s = s.strip_suffix(quote).unwrap_or(s);
    CompactString::from(s)
}

/// Parse double-quoted content into word parts.
/// Handles $VAR and ${VAR} inside double quotes.
fn parse_double_quoted_parts(content: &str) -> Vec<WordPart> {
    let mut parts = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    let mut literal_start = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            // Flush literal before $
            if i > literal_start {
                parts.push(WordPart::Literal(CompactString::from(&content[literal_start..i])));
            }

            if bytes[i + 1] == b'{' {
                // ${VAR} — find closing }
                let start = i + 2;
                let mut end = start;
                let mut depth = 1u32;
                while end < bytes.len() && depth > 0 {
                    if bytes[end] == b'{' {
                        depth += 1;
                    } else if bytes[end] == b'}' {
                        depth -= 1;
                    }
                    if depth > 0 {
                        end += 1;
                    }
                }
                let param = &content[start..end];
                parts.push(WordPart::DollarBrace {
                    param: CompactString::from(param),
                    operator: None,
                    arg: None,
                });
                i = end + 1;
                literal_start = i;
            } else if bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_' {
                // $VAR
                let start = i + 1;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                let name = &content[start..end];
                parts.push(WordPart::DollarVar(CompactString::from(name)));
                i = end;
                literal_start = i;
            } else if bytes[i + 1] == b'(' {
                // $(cmd) inside double quotes — simplified: treat as literal for now
                parts.push(WordPart::Literal(CompactString::from("$(")));
                i += 2;
                literal_start = i;
            } else {
                // Special vars: $?, $!, $$, $#, $*, $@, $0-$9
                let special = bytes[i + 1];
                if matches!(special, b'?' | b'!' | b'$' | b'#' | b'*' | b'@' | b'0'..=b'9') {
                    parts.push(WordPart::DollarVar(CompactString::from(
                        &content[i + 1..i + 2],
                    )));
                    i += 2;
                    literal_start = i;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'\\' && i + 1 < bytes.len() {
            // Escaped character in double quotes
            i += 2;
        } else {
            i += 1;
        }
    }

    // Flush remaining literal
    if literal_start < bytes.len() {
        parts.push(WordPart::Literal(CompactString::from(&content[literal_start..])));
    }

    if parts.is_empty() {
        parts.push(WordPart::Literal(CompactString::default()));
    }

    parts
}

fn parse_fd_prefix(text: &str) -> u32 {
    text.bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0u32, |acc, b| acc * 10 + u32::from(b - b'0'))
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenize(input: &str) -> Vec<Token> {
        let mut lexer = frost_lexer::Lexer::new(input.as_bytes());
        let mut tokens = Vec::new();
        loop {
            let tok = lexer.next_token();
            let eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if eof { break; }
        }
        tokens
    }

    fn parse(input: &str) -> Program {
        let tokens = tokenize(input);
        Parser::new(&tokens).parse()
    }

    fn first_simple(program: &Program) -> &SimpleCommand {
        match &program.commands[0].list.first.commands[0] {
            Command::Simple(s) => s,
            other => panic!("expected Simple, got {other:?}"),
        }
    }

    #[test]
    fn parse_simple_command() {
        let p = parse("echo hello world");
        assert_eq!(p.commands.len(), 1);
        let cmd = first_simple(&p);
        assert_eq!(cmd.words.len(), 3);
    }

    #[test]
    fn parse_empty_program() {
        let p = parse("");
        assert_eq!(p.commands.len(), 0);
    }

    #[test]
    fn parse_newlines_only() {
        let p = parse("\n\n\n");
        assert_eq!(p.commands.len(), 0);
    }

    #[test]
    fn parse_semicolons() {
        let p = parse("echo a; echo b; echo c");
        assert_eq!(p.commands.len(), 3);
    }

    #[test]
    fn parse_pipe() {
        let p = parse("cat file | grep pattern | wc -l");
        let pipeline = &p.commands[0].list.first;
        assert_eq!(pipeline.commands.len(), 3);
    }

    #[test]
    fn parse_and_or_list() {
        let p = parse("test -f file && cat file || echo missing");
        let list = &p.commands[0].list;
        assert_eq!(list.rest.len(), 2);
        assert_eq!(list.rest[0].0, ListOp::And);
        assert_eq!(list.rest[1].0, ListOp::Or);
    }

    #[test]
    fn parse_background() {
        let p = parse("sleep 10 &");
        assert!(p.commands[0].is_async);
    }

    #[test]
    fn parse_bang() {
        let p = parse("! false");
        assert!(p.commands[0].list.first.bang);
    }

    #[test]
    fn parse_redirect_output() {
        let p = parse("echo hello > file.txt");
        let cmd = first_simple(&p);
        assert_eq!(cmd.redirects.len(), 1);
        assert_eq!(cmd.redirects[0].op, RedirectOp::Greater);
    }

    #[test]
    fn parse_redirect_append() {
        let p = parse("echo hello >> file.txt");
        let cmd = first_simple(&p);
        assert_eq!(cmd.redirects[0].op, RedirectOp::DoubleGreater);
    }

    #[test]
    fn parse_redirect_input() {
        let p = parse("cat < input.txt");
        let cmd = first_simple(&p);
        assert_eq!(cmd.redirects[0].op, RedirectOp::Less);
    }

    #[test]
    fn parse_assignment() {
        let p = parse("FOO=bar");
        let cmd = first_simple(&p);
        assert_eq!(cmd.assignments.len(), 1);
        assert_eq!(cmd.assignments[0].name.as_str(), "FOO");
        assert_eq!(cmd.assignments[0].op, AssignOp::Assign);
    }

    #[test]
    fn parse_assignment_before_command() {
        let p = parse("FOO=bar echo hello");
        let cmd = first_simple(&p);
        assert_eq!(cmd.assignments.len(), 1);
        assert_eq!(cmd.words.len(), 2);
    }

    #[test]
    fn parse_single_quoted() {
        let p = parse("echo 'hello world'");
        let cmd = first_simple(&p);
        assert_eq!(cmd.words.len(), 2);
        match &cmd.words[1].parts[0] {
            WordPart::SingleQuoted(s) => assert_eq!(s.as_str(), "hello world"),
            other => panic!("expected SingleQuoted, got {other:?}"),
        }
    }

    #[test]
    fn parse_double_quoted_with_var() {
        let p = parse(r#"echo "hello $name""#);
        let cmd = first_simple(&p);
        assert_eq!(cmd.words.len(), 2);
        let parts = &cmd.words[1].parts;
        // Should contain at least a literal and a DollarVar
        assert!(parts.iter().any(|p| matches!(p, WordPart::DollarVar(n) if n.as_str() == "name")));
    }

    #[test]
    fn parse_dollar_var() {
        let p = parse("echo $HOME");
        let cmd = first_simple(&p);
        assert_eq!(cmd.words.len(), 2);
        match &cmd.words[1].parts[0] {
            WordPart::DollarVar(name) => assert_eq!(name.as_str(), "HOME"),
            other => panic!("expected DollarVar, got {other:?}"),
        }
    }

    #[test]
    fn parse_if_then_fi() {
        let p = parse("if true; then echo yes; fi");
        match &p.commands[0].list.first.commands[0] {
            Command::If(clause) => {
                assert_eq!(clause.condition.len(), 1);
                assert_eq!(clause.then_body.len(), 1);
                assert!(clause.else_body.is_none());
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn parse_if_else() {
        let p = parse("if false; then echo no; else echo yes; fi");
        match &p.commands[0].list.first.commands[0] {
            Command::If(clause) => {
                assert!(clause.else_body.is_some());
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn parse_if_elif() {
        let p = parse("if false; then echo 1; elif true; then echo 2; else echo 3; fi");
        match &p.commands[0].list.first.commands[0] {
            Command::If(clause) => {
                assert_eq!(clause.elifs.len(), 1);
                assert!(clause.else_body.is_some());
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn parse_for_loop() {
        let p = parse("for x in a b c; do echo $x; done");
        match &p.commands[0].list.first.commands[0] {
            Command::For(clause) => {
                assert_eq!(clause.var.as_str(), "x");
                assert_eq!(clause.words.as_ref().unwrap().len(), 3);
                assert_eq!(clause.body.len(), 1);
            }
            other => panic!("expected For, got {other:?}"),
        }
    }

    #[test]
    fn parse_while_loop() {
        let p = parse("while true; do echo loop; done");
        match &p.commands[0].list.first.commands[0] {
            Command::While(clause) => {
                assert_eq!(clause.condition.len(), 1);
                assert_eq!(clause.body.len(), 1);
            }
            other => panic!("expected While, got {other:?}"),
        }
    }

    #[test]
    fn parse_case() {
        let p = parse("case $x in\n  a) echo A ;;\n  b) echo B ;;\nesac");
        match &p.commands[0].list.first.commands[0] {
            Command::Case(clause) => {
                assert_eq!(clause.items.len(), 2);
            }
            other => panic!("expected Case, got {other:?}"),
        }
    }

    #[test]
    fn parse_subshell() {
        let p = parse("(echo hello)");
        assert!(matches!(
            &p.commands[0].list.first.commands[0],
            Command::Subshell(_)
        ));
    }

    #[test]
    fn parse_brace_group() {
        let p = parse("{ echo hello; }");
        assert!(matches!(
            &p.commands[0].list.first.commands[0],
            Command::BraceGroup(_)
        ));
    }

    #[test]
    fn parse_function_keyword() {
        let p = parse("function greet { echo hello; }");
        match &p.commands[0].list.first.commands[0] {
            Command::FunctionDef(f) => assert_eq!(f.name.as_str(), "greet"),
            other => panic!("expected FunctionDef, got {other:?}"),
        }
    }

    #[test]
    fn parse_function_parens() {
        let p = parse("greet() { echo hello; }");
        match &p.commands[0].list.first.commands[0] {
            Command::FunctionDef(f) => assert_eq!(f.name.as_str(), "greet"),
            other => panic!("expected FunctionDef, got {other:?}"),
        }
    }

    #[test]
    fn parse_tilde() {
        let p = parse("cd ~");
        let cmd = first_simple(&p);
        assert!(matches!(&cmd.words[1].parts[0], WordPart::Tilde(_)));
    }

    #[test]
    fn parse_glob_star() {
        let p = parse("ls *");
        let cmd = first_simple(&p);
        assert!(matches!(
            &cmd.words[1].parts[0],
            WordPart::Glob(GlobKind::Star)
        ));
    }

    #[test]
    fn parse_multiple_commands_newlines() {
        let p = parse("echo a\necho b\necho c\n");
        assert_eq!(p.commands.len(), 3);
    }

    #[test]
    fn parse_herestring() {
        let p = parse("cat <<< 'hello'");
        let cmd = first_simple(&p);
        assert_eq!(cmd.redirects[0].op, RedirectOp::TripleLess);
    }

    #[test]
    fn parse_time() {
        let p = parse("time ls -la");
        assert!(matches!(
            &p.commands[0].list.first.commands[0],
            Command::Time(_)
        ));
    }

    #[test]
    fn parse_multiline_if() {
        let p = parse("if true\nthen\n  echo yes\nfi");
        assert!(matches!(
            &p.commands[0].list.first.commands[0],
            Command::If(_)
        ));
    }
}
