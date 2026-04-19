//! Recursive descent parser for zsh-compatible shell grammar.
//!
//! Transforms a flat token stream from `frost-lexer` into an AST
//! defined in [`crate::ast`]. The parser is tolerant of errors —
//! it never panics and produces the best AST it can from any input.

use compact_str::CompactString;

use frost_lexer::{Span, Token, TokenKind};

use crate::ast::*;

/// Recursive descent parser.
pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    // ── Helpers ──────────────────────────────────────────────────────

    /// Peek at the current token without consuming it.
    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    /// Current token kind.
    fn peek_kind(&self) -> TokenKind {
        self.peek().kind
    }

    /// Peek N tokens ahead.
    fn peek_nth(&self, n: usize) -> TokenKind {
        self.tokens
            .get(self.pos + n)
            .map_or(TokenKind::Eof, |t| t.kind)
    }

    /// Whether we're at the given token kind.
    fn at(&self, kind: TokenKind) -> bool {
        self.peek_kind() == kind
    }

    /// Whether we're at EOF.
    fn at_eof(&self) -> bool {
        self.at(TokenKind::Eof)
    }

    /// Advance to the next token, returning the current one.
    fn advance(&mut self) -> &'a Token {
        let tok = &self.tokens[self.pos.min(self.tokens.len() - 1)];
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        tok
    }

    /// Consume the current token if it matches `kind`.
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Expect and consume the current token. No-op if it doesn't match.
    fn expect(&mut self, kind: TokenKind) -> bool {
        if self.eat(kind) {
            true
        } else {
            // Error recovery: don't advance — let caller decide.
            false
        }
    }

    /// Consume a keyword that might appear as either its dedicated TokenKind
    /// or as a plain Word with matching text (the lexer only recognizes
    /// reserved words in command position; in other positions they come
    /// through as Word).
    fn eat_keyword(&mut self, kind: TokenKind, text: &str) -> bool {
        if self.at(kind) || (self.at(TokenKind::Word) && self.peek().text == text) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Skip newlines and comments (linebreak in the grammar).
    fn skip_newlines(&mut self) {
        while matches!(self.peek_kind(), TokenKind::Newline | TokenKind::Comment) {
            self.advance();
        }
    }

    /// Parse a sub-program from tokens[start..self.pos], appending an Eof
    /// token so the sub-parser terminates properly.
    fn sub_parse(&self, start: usize) -> Program {
        let inner = &self.tokens[start..self.pos];
        if inner.is_empty() {
            return Program { commands: Vec::new() };
        }
        // Build a vec with the inner tokens plus an Eof sentinel.
        let mut sub_tokens: Vec<Token> = inner.to_vec();
        let last_end = sub_tokens.last().map(|t| t.span.end).unwrap_or(0);
        sub_tokens.push(Token {
            kind: TokenKind::Eof,
            text: CompactString::default(),
            span: Span::new(last_end, last_end),
        });
        let mut sub_parser = Parser::new(&sub_tokens);
        sub_parser.parse()
    }

    /// Eat separators (`;`, `\n`, comments) between commands.
    fn eat_separators(&mut self) {
        while matches!(
            self.peek_kind(),
            TokenKind::Semi | TokenKind::Newline | TokenKind::Comment
        ) {
            self.advance();
        }
    }

    /// Whether we're at a token that can start a word.
    fn at_word(&self) -> bool {
        is_word_start(self.peek_kind())
    }

    /// Whether we're at a redirect operator.
    fn at_redirect(&self) -> bool {
        is_redirect_op(self.peek_kind())
    }

    /// Whether we're at a keyword that ends a compound command body.
    fn at_compound_end(&self) -> bool {
        let kind = self.peek_kind();
        let text = self.peek().text.as_str();
        matches!(
            kind,
            TokenKind::Then
                | TokenKind::Elif
                | TokenKind::Else
                | TokenKind::Fi
                | TokenKind::Do
                | TokenKind::Done
                | TokenKind::Esac
                | TokenKind::RightBrace
                | TokenKind::RightParen
                | TokenKind::DoubleSemi
                | TokenKind::SemiAnd
                | TokenKind::SemiPipe
        ) || (kind == TokenKind::Word
            && matches!(
                text,
                "then" | "elif" | "else" | "fi" | "do" | "done" | "esac"
            ))
    }

    /// Whether we're at a token that can begin a new command.
    fn at_command_start(&self) -> bool {
        self.at_word()
            || self.at_redirect()
            || matches!(
                self.peek_kind(),
                TokenKind::If
                    | TokenKind::For
                    | TokenKind::While
                    | TokenKind::Until
                    | TokenKind::Case
                    | TokenKind::Select
                    | TokenKind::Repeat
                    | TokenKind::Function
                    | TokenKind::Time
                    | TokenKind::Coproc
                    | TokenKind::LeftParen
                    | TokenKind::LeftBrace
                    | TokenKind::Bang
            )
            || (self.peek_kind() == TokenKind::Word
                && matches!(
                    self.peek().text.as_str(),
                    "if" | "for" | "while" | "until" | "case" | "select" | "repeat"
                        | "function" | "time" | "coproc"
                ))
    }

    /// Check if the current position looks like `name() ...` (function def).
    fn is_function_def_ahead(&self) -> bool {
        self.peek_kind() == TokenKind::Word
            && self.peek_nth(1) == TokenKind::LeftParen
            && self.peek_nth(2) == TokenKind::RightParen
    }

    /// Check if the current position looks like an assignment: `NAME=...`
    /// (word immediately followed by `=` with no whitespace).
    fn is_assignment_ahead(&self) -> bool {
        if self.peek_kind() != TokenKind::Word {
            return false;
        }
        if !is_identifier(&self.peek().text) {
            return false;
        }
        // Next token must be Equals, adjacent to the word (no space).
        if self.peek_nth(1) != TokenKind::Equals {
            return false;
        }
        // Adjacency check: word ends where equals starts.
        if let Some(eq_tok) = self.tokens.get(self.pos + 1) {
            self.peek().span.end == eq_tok.span.start
        } else {
            false
        }
    }

    // ── Grammar Rules ───────────────────────────────────────────────

    /// program → newline_list? complete_command_list? EOF
    pub fn parse(&mut self) -> Program {
        let mut commands = Vec::new();
        self.skip_newlines();

        while !self.at_eof() {
            if let Some(cmd) = self.parse_complete_command() {
                commands.push(cmd);
            } else {
                break;
            }
            self.eat_separators();
        }

        Program { commands }
    }

    /// compound_list — used inside compound commands (more lenient
    /// with newlines as separators).
    fn parse_compound_list(&mut self) -> Vec<CompleteCommand> {
        let mut commands = Vec::new();
        self.skip_newlines();

        while !self.at_eof() && !self.at_compound_end() {
            if let Some(cmd) = self.parse_complete_command() {
                commands.push(cmd);
            } else {
                break;
            }
            self.eat_separators();
        }

        commands
    }

    /// complete_command → list [`&`]
    fn parse_complete_command(&mut self) -> Option<CompleteCommand> {
        self.skip_newlines();
        if self.at_eof() || self.at_compound_end() {
            return None;
        }
        if !self.at_command_start() {
            return None;
        }

        let list = self.parse_list();
        let is_async = self.eat(TokenKind::Ampersand) || self.eat(TokenKind::Disown);

        Some(CompleteCommand { list, is_async })
    }

    /// list → pipeline ((`&&` | `||`) newline_list pipeline)*
    fn parse_list(&mut self) -> List {
        let first = self.parse_pipeline();
        let mut rest = Vec::new();

        loop {
            let op = match self.peek_kind() {
                TokenKind::AndAnd => ListOp::And,
                TokenKind::OrOr => ListOp::Or,
                _ => break,
            };
            self.advance();
            self.skip_newlines();
            let pipeline = self.parse_pipeline();
            rest.push((op, pipeline));
        }

        List { first, rest }
    }

    /// pipeline → [`!`] command (`|` newline_list command)*
    fn parse_pipeline(&mut self) -> Pipeline {
        let bang = self.eat(TokenKind::Bang);
        let first = self.parse_command();
        let mut commands = vec![first];
        let mut pipe_stderr = Vec::new();

        loop {
            match self.peek_kind() {
                TokenKind::Pipe => {
                    self.advance();
                    pipe_stderr.push(false);
                }
                TokenKind::PipeAmpersand => {
                    self.advance();
                    pipe_stderr.push(true);
                }
                _ => break,
            }
            self.skip_newlines();
            commands.push(self.parse_command());
        }

        Pipeline {
            bang,
            commands,
            pipe_stderr,
        }
    }

    /// command → compound_command | function_def | simple_command
    fn parse_command(&mut self) -> Command {
        // Check for compound commands by token kind or word text.
        let kind = self.peek_kind();
        let text = self.peek().text.as_str();

        match kind {
            TokenKind::If => return Command::If(Box::new(self.parse_if())),
            TokenKind::For => return self.parse_for_or_arith_for(),
            TokenKind::While => return Command::While(Box::new(self.parse_while())),
            TokenKind::Until => return Command::Until(Box::new(self.parse_until())),
            TokenKind::Case => return Command::Case(Box::new(self.parse_case())),
            TokenKind::Select => return Command::Select(Box::new(self.parse_select())),
            TokenKind::Repeat => return Command::Repeat(Box::new(self.parse_repeat())),
            TokenKind::LeftParen => {
                // Check if this is (( ... )) arithmetic command.
                if self.pos + 1 < self.tokens.len()
                    && self.tokens[self.pos + 1].kind == TokenKind::LeftParen
                    && self.tokens[self.pos + 1].span.start == self.tokens[self.pos].span.end
                {
                    return self.parse_arith_command();
                }
                return Command::Subshell(self.parse_subshell());
            }
            TokenKind::LeftBrace => {
                let bg = self.parse_brace_group();
                // Check for `{ ... } always { ... }` construct.
                if self.eat_keyword(TokenKind::Word, "always") {
                    let always_bg = self.parse_brace_group();
                    return Command::Always(Box::new(AlwaysClause {
                        try_body: bg.body,
                        always_body: always_bg.body,
                        redirects: always_bg.redirects,
                    }));
                }
                return Command::BraceGroup(bg);
            }
            TokenKind::Function => {
                return Command::FunctionDef(Box::new(self.parse_function_keyword()));
            }
            TokenKind::Time => return Command::Time(Box::new(self.parse_time())),
            TokenKind::Coproc => return Command::Coproc(Box::new(self.parse_coproc())),
            TokenKind::DoubleLeftBracket | TokenKind::CondStart => {
                return Command::Simple(self.parse_cond_command());
            }
            _ => {}
        }

        // Words that look like compound-command keywords but came through
        // as Word (lexer didn't recognise them in non-command position).
        if kind == TokenKind::Word {
            match text {
                "if" => return Command::If(Box::new(self.parse_if())),
                "for" => return self.parse_for_or_arith_for(),
                "while" => return Command::While(Box::new(self.parse_while())),
                "until" => return Command::Until(Box::new(self.parse_until())),
                "case" => return Command::Case(Box::new(self.parse_case())),
                "select" => return Command::Select(Box::new(self.parse_select())),
                "repeat" => return Command::Repeat(Box::new(self.parse_repeat())),
                "function" => {
                    return Command::FunctionDef(Box::new(self.parse_function_keyword()));
                }
                "time" => return Command::Time(Box::new(self.parse_time())),
                "coproc" => return Command::Coproc(Box::new(self.parse_coproc())),
                "[[" => return Command::Simple(self.parse_cond_command()),
                _ => {}
            }
        }

        // Check for `name() body` function definition.
        if self.is_function_def_ahead() {
            return Command::FunctionDef(Box::new(self.parse_function_shorthand()));
        }

        Command::Simple(self.parse_simple_command())
    }

    // ── Simple commands ─────────────────────────────────────────────

    /// simple_command → assignment* (word | redirect)*
    fn parse_simple_command(&mut self) -> SimpleCommand {
        let mut assignments = Vec::new();
        let mut words = Vec::new();
        let mut redirects = Vec::new();

        // Leading assignments (before any non-assignment word).
        while self.is_assignment_ahead() {
            assignments.push(self.parse_assignment());
        }

        // Words and redirects (interleaved).
        loop {
            if self.at_redirect() {
                redirects.push(self.parse_redirect());
            } else if self.at_word() || self.at(TokenKind::Equals) {
                words.push(self.parse_word());
            } else {
                break;
            }
        }

        SimpleCommand {
            assignments,
            words,
            redirects,
        }
    }

    /// Parse a variable assignment: `NAME=value` or `NAME+=value`.
    fn parse_assignment(&mut self) -> Assignment {
        let name_tok = self.advance(); // Word (identifier)
        let name = name_tok.text.clone();
        let span_start = name_tok.span.start;

        // Consume `=`.
        self.advance();
        let op = AssignOp::Assign;

        // Value is optional (bare `FOO=` is valid).
        let eq_end = self.tokens[self.pos - 1].span.end;
        let value = if self.pos < self.tokens.len()
            && self.tokens[self.pos].span.start == eq_end
            && (is_word_start(self.peek_kind()) || self.at(TokenKind::Equals))
        {
            Some(self.parse_word())
        } else {
            None
        };

        let span_end = self.peek().span.start;
        Assignment {
            name,
            op,
            value,
            span: Span::new(span_start, span_end),
        }
    }

    // ── Words ───────────────────────────────────────────────────────

    /// Parse a word, joining adjacent tokens into a single Word node.
    fn parse_word(&mut self) -> Word {
        let start_span = self.peek().span;
        let mut parts = Vec::new();

        // Parse the first word part.
        parts.push(self.parse_word_part());

        // Continue if the next token is directly adjacent (no whitespace).
        loop {
            if self.pos >= self.tokens.len() || self.pos == 0 {
                break;
            }
            let prev_end = self.tokens[self.pos - 1]
                .span
                .end;
            let next_start = self.tokens[self.pos].span.start;

            if prev_end != next_start {
                break; // whitespace gap — separate word
            }
            if !is_word_start(self.peek_kind()) && !self.at(TokenKind::Equals) {
                break; // not a word-like token
            }

            parts.push(self.parse_word_part());
        }

        Word {
            parts,
            span: start_span,
        }
    }

    /// Parse a single word part from the current token.
    fn parse_word_part(&mut self) -> WordPart {
        match self.peek_kind() {
            TokenKind::Dollar => {
                let dollar = self.advance();
                // Check if next token is adjacent and is a variable name or special char.
                if self.pos < self.tokens.len()
                    && self.tokens[self.pos].span.start == dollar.span.end
                {
                    let next = &self.tokens[self.pos];
                    match next.kind {
                        TokenKind::Word | TokenKind::Number => {
                            let name_tok = self.advance();
                            WordPart::DollarVar(name_tok.text.clone())
                        }
                        // Special variables: $?, $#, $@, $*, $!, $-, $$
                        TokenKind::Question => {
                            self.advance();
                            WordPart::DollarVar(CompactString::new("?"))
                        }
                        TokenKind::Bang => {
                            self.advance();
                            WordPart::DollarVar(CompactString::new("!"))
                        }
                        TokenKind::Dollar => {
                            self.advance();
                            WordPart::DollarVar(CompactString::new("$"))
                        }
                        TokenKind::Star => {
                            self.advance();
                            WordPart::DollarVar(CompactString::new("*"))
                        }
                        TokenKind::At => {
                            self.advance();
                            WordPart::DollarVar(CompactString::new("@"))
                        }
                        _ => {
                            // Check text for # and - which may be in Word tokens
                            if next.text.as_str() == "#" || next.text.as_str() == "-" {
                                let tok = self.advance();
                                WordPart::DollarVar(tok.text.clone())
                            } else {
                                WordPart::Literal(CompactString::new("$"))
                            }
                        }
                    }
                } else {
                    WordPart::Literal(CompactString::new("$"))
                }
            }

            TokenKind::DollarBrace => {
                self.advance(); // consume ${
                let mut raw = CompactString::default();
                // Consume tokens until }.
                while !self.at(TokenKind::RightBrace) && !self.at_eof() {
                    let tok = self.advance();
                    raw.push_str(&tok.text);
                }
                self.eat(TokenKind::RightBrace);
                // Handle ${#param} for string length.
                if raw.starts_with('#') && raw.len() > 1 {
                    let inner = &raw[1..];
                    // ${#param} — string length
                    WordPart::DollarBrace {
                        param: CompactString::new(inner),
                        operator: Some(CompactString::new("length")),
                        arg: None,
                    }
                } else if let Some(op_pos) = find_param_operator(&raw) {
                    let param = CompactString::new(&raw[..op_pos]);
                    let (op, arg_start) = extract_operator(&raw[op_pos..]);
                    let arg_str = &raw[op_pos + arg_start..];
                    WordPart::DollarBrace {
                        param,
                        operator: Some(CompactString::new(op)),
                        arg: if arg_str.is_empty() {
                            None
                        } else {
                            Some(Box::new(Word {
                                parts: vec![WordPart::Literal(CompactString::new(arg_str))],
                                span: Span::new(0, 0),
                            }))
                        },
                    }
                } else {
                    WordPart::DollarVar(CompactString::new(&raw))
                }
            }

            TokenKind::DollarParen => {
                self.advance(); // consume $(
                // Find the matching ) — track nesting depth.
                let start = self.pos;
                let mut depth: u32 = 1;
                while self.pos < self.tokens.len() {
                    match self.peek_kind() {
                        TokenKind::LeftParen | TokenKind::DollarParen => {
                            depth += 1;
                            self.advance();
                        }
                        TokenKind::RightParen => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            self.advance();
                        }
                        TokenKind::Eof => break,
                        _ => {
                            self.advance();
                        }
                    }
                }
                // Parse the inner tokens as a sub-program.
                // Must append an Eof token so the sub-parser terminates.
                let program = self.sub_parse(start);
                self.eat(TokenKind::RightParen);
                WordPart::CommandSub(Box::new(program))
            }

            TokenKind::DollarDoubleParen => {
                self.advance(); // consume $((
                let mut expr = String::new();
                // Consume until `)` `)`.
                loop {
                    if self.at_eof() {
                        break;
                    }
                    if self.at(TokenKind::RightParen)
                        && self.peek_nth(1) == TokenKind::RightParen
                    {
                        self.advance(); // first )
                        self.advance(); // second )
                        break;
                    }
                    expr.push_str(&self.advance().text);
                }
                WordPart::ArithSub(CompactString::new(&expr))
            }

            TokenKind::Backtick => {
                self.advance(); // opening `
                let start = self.pos;
                while !self.at(TokenKind::Backtick) && !self.at_eof() {
                    self.advance();
                }
                let program = self.sub_parse(start);
                self.eat(TokenKind::Backtick);
                WordPart::CommandSub(Box::new(program))
            }

            TokenKind::SingleQuoted => {
                let tok = self.advance();
                WordPart::SingleQuoted(strip_quotes(&tok.text, '\''))
            }

            TokenKind::DoubleQuoted => {
                let tok = self.advance();
                let inner = strip_quotes(&tok.text, '"');
                let parts = parse_dq_interior(&inner);
                WordPart::DoubleQuoted(parts)
            }

            TokenKind::DollarSingleQuoted => {
                let tok = self.advance();
                let s = &tok.text;
                let inner = if s.len() >= 3 {
                    CompactString::new(&s[2..s.len() - 1])
                } else {
                    CompactString::default()
                };
                WordPart::SingleQuoted(inner)
            }

            TokenKind::Tilde => {
                self.advance();
                WordPart::Tilde(CompactString::default())
            }

            TokenKind::Star => {
                self.advance();
                WordPart::Glob(GlobKind::Star)
            }

            TokenKind::Question => {
                self.advance();
                WordPart::Glob(GlobKind::Question)
            }

            TokenKind::At => {
                self.advance();
                WordPart::Glob(GlobKind::At)
            }

            TokenKind::Equals => {
                self.advance();
                WordPart::Literal(CompactString::new("="))
            }

            TokenKind::Bang => {
                self.advance();
                WordPart::Literal(CompactString::new("!"))
            }

            _ => {
                let tok = self.advance();
                WordPart::Literal(tok.text.clone())
            }
        }
    }

    // ── Redirections ────────────────────────────────────────────────

    /// Parse a redirect operator and its target word.
    fn parse_redirect(&mut self) -> Redirect {
        let tok = self.advance();
        let span = tok.span;

        let (fd, op) = match tok.kind {
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
                let fd_num = tok.text.trim_end_matches('>').parse().ok();
                (fd_num, RedirectOp::Greater)
            }
            TokenKind::FdLess => {
                let fd_num = tok.text.trim_end_matches('<').parse().ok();
                (fd_num, RedirectOp::Less)
            }
            TokenKind::FdDoubleGreater => {
                let fd_num = tok.text.trim_end_matches(">>").parse().ok();
                (fd_num, RedirectOp::DoubleGreater)
            }
            TokenKind::FdDup => {
                let fd_num = tok.text.split(">&").next().and_then(|s| s.parse().ok());
                (fd_num, RedirectOp::FdDup)
            }
            _ => (None, RedirectOp::Greater), // shouldn't happen
        };

        let target = if self.at_word() {
            self.parse_word()
        } else {
            // Missing redirect target.
            Word {
                parts: vec![],
                span: self.peek().span,
            }
        };

        Redirect {
            fd,
            op,
            target,
            span,
        }
    }

    /// Collect trailing redirects after a compound command.
    fn parse_trailing_redirects(&mut self) -> Vec<Redirect> {
        let mut redirects = Vec::new();
        while self.at_redirect() {
            redirects.push(self.parse_redirect());
        }
        redirects
    }

    // ── Compound commands ───────────────────────────────────────────

    /// if_clause → `if` compound_list `then` compound_list
    ///             (`elif` compound_list `then` compound_list)*
    ///             [`else` compound_list] `fi`
    fn parse_if(&mut self) -> IfClause {
        self.advance(); // consume `if` (keyword or word)
        let condition = self.parse_compound_list();
        self.eat_keyword(TokenKind::Then, "then");
        let then_body = self.parse_compound_list();

        let mut elifs = Vec::new();
        while self.eat_keyword(TokenKind::Elif, "elif") {
            let elif_cond = self.parse_compound_list();
            self.eat_keyword(TokenKind::Then, "then");
            let elif_body = self.parse_compound_list();
            elifs.push((elif_cond, elif_body));
        }

        let else_body = if self.eat_keyword(TokenKind::Else, "else") {
            Some(self.parse_compound_list())
        } else {
            None
        };

        self.eat_keyword(TokenKind::Fi, "fi");
        let redirects = self.parse_trailing_redirects();

        IfClause {
            condition,
            then_body,
            elifs,
            else_body,
            redirects,
        }
    }

    /// Dispatch between `for var in ...` and `for (( ... ))`.
    fn parse_for_or_arith_for(&mut self) -> Command {
        // After `for`, check if the next tokens are `((` (adjacent parens).
        let after_for = self.pos + 1;
        let is_arith = after_for + 1 < self.tokens.len()
            && self.tokens[after_for].kind == TokenKind::LeftParen
            && self.tokens[after_for + 1].kind == TokenKind::LeftParen
            && self.tokens[after_for + 1].span.start == self.tokens[after_for].span.end;

        if is_arith {
            Command::ArithFor(Box::new(self.parse_arith_for()))
        } else {
            Command::For(Box::new(self.parse_for()))
        }
    }

    /// arith_for → `for` `((` init `;` cond `;` step `))` separator `do` compound_list `done`
    fn parse_arith_for(&mut self) -> ArithForClause {
        self.advance(); // consume `for`
        self.advance(); // first (
        self.advance(); // second (

        // Collect tokens into three expressions separated by `;`.
        // We track paren depth to handle nested parens within expressions.
        let mut parts: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut depth: u32 = 0;

        loop {
            if self.at_eof() {
                break;
            }
            let kind = self.peek_kind();
            match kind {
                TokenKind::LeftParen => {
                    depth += 1;
                    current.push('(');
                    self.advance();
                }
                TokenKind::RightParen => {
                    if depth == 0 {
                        // End of (( ... )) — push remaining expression
                        parts.push(std::mem::take(&mut current));
                        self.advance(); // first )
                        if self.peek_kind() == TokenKind::RightParen {
                            self.advance(); // second )
                        }
                        break;
                    }
                    depth -= 1;
                    current.push(')');
                    self.advance();
                }
                TokenKind::Semi => {
                    if depth == 0 {
                        // Separator between init/cond/step
                        parts.push(std::mem::take(&mut current));
                        self.advance();
                    } else {
                        current.push(';');
                        self.advance();
                    }
                }
                _ => {
                    let tok = self.advance();
                    // Add spacing between tokens, but not between adjacent
                    // operator characters (same logic as parse_arith_command).
                    if !current.is_empty() {
                        let last = current.as_bytes()[current.len() - 1];
                        let first = tok.text.as_bytes().first().copied().unwrap_or(0);
                        let is_op = |c: u8| {
                            matches!(
                                c,
                                b'=' | b'!'
                                    | b'<'
                                    | b'>'
                                    | b'+'
                                    | b'-'
                                    | b'*'
                                    | b'/'
                                    | b'%'
                                    | b'&'
                                    | b'|'
                                    | b'^'
                                    | b'~'
                            )
                        };
                        if !(is_op(last) && is_op(first)) && !current.ends_with(' ') {
                            current.push(' ');
                        }
                    }
                    current.push_str(&tok.text);
                }
            }
        }

        // Ensure we have exactly 3 parts (init, condition, step).
        while parts.len() < 3 {
            parts.push(String::new());
        }

        // Eat optional separator (`;` or newline) between `))` and `do`.
        if !self.eat(TokenKind::Semi) {
            self.skip_newlines();
        }
        self.skip_newlines();

        self.eat_keyword(TokenKind::Do, "do");
        let body = self.parse_compound_list();
        self.eat_keyword(TokenKind::Done, "done");
        let redirects = self.parse_trailing_redirects();

        ArithForClause {
            init: CompactString::new(parts[0].trim()),
            condition: CompactString::new(parts[1].trim()),
            step: CompactString::new(parts[2].trim()),
            body,
            redirects,
        }
    }

    /// for_clause → `for` name [`in` word*] separator `do` compound_list `done`
    fn parse_for(&mut self) -> ForClause {
        self.advance(); // consume `for`
        let var_tok = self.advance();
        let var = var_tok.text.clone();
        self.skip_newlines();

        let words = if self.eat_keyword(TokenKind::In, "in") {
            let mut words = Vec::new();
            while self.at_word() {
                words.push(self.parse_word());
            }
            Some(words)
        } else {
            None
        };

        // Eat separator between word list and `do`.
        if !self.eat(TokenKind::Semi) {
            self.skip_newlines();
        }
        self.skip_newlines();

        self.eat_keyword(TokenKind::Do, "do");
        let body = self.parse_compound_list();
        self.eat_keyword(TokenKind::Done, "done");
        let redirects = self.parse_trailing_redirects();

        ForClause {
            var,
            words,
            body,
            redirects,
        }
    }

    /// while_clause → `while` compound_list `do` compound_list `done`
    fn parse_while(&mut self) -> WhileClause {
        self.advance(); // consume `while`
        let condition = self.parse_compound_list();
        self.eat_keyword(TokenKind::Do, "do");
        let body = self.parse_compound_list();
        self.eat_keyword(TokenKind::Done, "done");
        let redirects = self.parse_trailing_redirects();

        WhileClause {
            condition,
            body,
            redirects,
        }
    }

    /// until_clause → `until` compound_list `do` compound_list `done`
    fn parse_until(&mut self) -> UntilClause {
        self.advance(); // consume `until`
        let condition = self.parse_compound_list();
        self.eat_keyword(TokenKind::Do, "do");
        let body = self.parse_compound_list();
        self.eat_keyword(TokenKind::Done, "done");
        let redirects = self.parse_trailing_redirects();

        UntilClause {
            condition,
            body,
            redirects,
        }
    }

    /// case_clause → `case` word newline_list `in` newline_list case_item* `esac`
    fn parse_case(&mut self) -> CaseClause {
        self.advance(); // consume `case`
        let word = self.parse_word();
        self.skip_newlines();
        self.eat_keyword(TokenKind::In, "in");
        self.skip_newlines();

        let mut items = Vec::new();
        while !self.eat_keyword(TokenKind::Esac, "esac") && !self.at_eof() {
            items.push(self.parse_case_item());
            self.skip_newlines();
        }

        let redirects = self.parse_trailing_redirects();

        CaseClause {
            word,
            items,
            redirects,
        }
    }

    /// case_item → [`(`] pattern (`|` pattern)* `)` compound_list? (`;;`|`;&`|`;|`)
    fn parse_case_item(&mut self) -> CaseItem {
        self.eat(TokenKind::LeftParen); // optional leading (

        let mut patterns = vec![self.parse_word()];
        while self.eat(TokenKind::Pipe) {
            patterns.push(self.parse_word());
        }

        self.expect(TokenKind::RightParen);
        self.skip_newlines();

        let body = self.parse_compound_list();

        let terminator = match self.peek_kind() {
            TokenKind::DoubleSemi => {
                self.advance();
                CaseTerminator::DoubleSemi
            }
            TokenKind::SemiAnd => {
                self.advance();
                CaseTerminator::SemiAnd
            }
            TokenKind::SemiPipe => {
                self.advance();
                CaseTerminator::SemiPipe
            }
            _ => CaseTerminator::DoubleSemi,
        };

        CaseItem {
            patterns,
            body,
            terminator,
        }
    }

    /// select_clause → `select` name [`in` word*] separator `do` compound_list `done`
    fn parse_select(&mut self) -> SelectClause {
        self.advance(); // consume `select`
        let var_tok = self.advance();
        let var = var_tok.text.clone();
        self.skip_newlines();

        let words = if self.eat_keyword(TokenKind::In, "in") {
            let mut words = Vec::new();
            while self.at_word() {
                words.push(self.parse_word());
            }
            Some(words)
        } else {
            None
        };

        if !self.eat(TokenKind::Semi) {
            self.skip_newlines();
        }
        self.skip_newlines();

        self.eat_keyword(TokenKind::Do, "do");
        let body = self.parse_compound_list();
        self.eat_keyword(TokenKind::Done, "done");
        let redirects = self.parse_trailing_redirects();

        SelectClause {
            var,
            words,
            body,
            redirects,
        }
    }

    /// repeat_clause → `repeat` word separator `do` compound_list `done`
    fn parse_repeat(&mut self) -> RepeatClause {
        self.advance(); // consume `repeat`
        let count = self.parse_word();
        self.eat_separators();

        self.eat_keyword(TokenKind::Do, "do");
        let body = self.parse_compound_list();
        self.eat_keyword(TokenKind::Done, "done");
        let redirects = self.parse_trailing_redirects();

        RepeatClause {
            count,
            body,
            redirects,
        }
    }

    /// cond_command → `[[` ... `]]` — parse as a simple command.
    fn parse_cond_command(&mut self) -> SimpleCommand {
        let start_span = self.peek().span;
        self.advance(); // consume [[

        let mut words = vec![Word {
            parts: vec![WordPart::Literal(CompactString::new("[["))],
            span: start_span,
        }];

        // Consume tokens until ]] or EOF.
        loop {
            if self.at_eof() {
                break;
            }
            let kind = self.peek_kind();
            if kind == TokenKind::DoubleRightBracket || kind == TokenKind::CondEnd {
                let tok = self.advance();
                words.push(Word {
                    parts: vec![WordPart::Literal(CompactString::new("]]"))],
                    span: tok.span,
                });
                break;
            }
            // Check for word text "]]"
            if self.peek().text.as_str() == "]]" {
                let tok = self.advance();
                words.push(Word {
                    parts: vec![WordPart::Literal(CompactString::new("]]"))],
                    span: tok.span,
                });
                break;
            }
            words.push(self.parse_word());
        }

        let redirects = self.parse_trailing_redirects();

        SimpleCommand {
            assignments: Vec::new(),
            words,
            redirects,
        }
    }

    /// arith_command → `((` ... `))` — parse as a simple command.
    fn parse_arith_command(&mut self) -> Command {
        let start_span = self.peek().span;
        self.advance(); // first (
        self.advance(); // second (

        // Collect the expression text until ))
        let mut expr = String::new();
        let mut depth: u32 = 0;
        loop {
            if self.at_eof() {
                break;
            }
            let kind = self.peek_kind();
            match kind {
                TokenKind::LeftParen => {
                    depth += 1;
                    expr.push('(');
                    self.advance();
                }
                TokenKind::RightParen => {
                    if depth == 0 {
                        self.advance(); // first )
                        // Expect second )
                        if self.peek_kind() == TokenKind::RightParen {
                            self.advance();
                        }
                        break;
                    }
                    depth -= 1;
                    expr.push(')');
                    self.advance();
                }
                _ => {
                    let tok = self.advance();
                    // Add a space between tokens, BUT not between adjacent
                    // operator characters that form compound operators like
                    // ==, !=, <=, >=, +=, -=, *=, /=, %=, &&, ||, ++, --, <<, >>
                    if !expr.is_empty() {
                        let last = expr.as_bytes()[expr.len() - 1];
                        let first = tok.text.as_bytes().first().copied().unwrap_or(0);
                        let is_op = |c: u8| matches!(c, b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'~');
                        if !(is_op(last) && is_op(first)) && !expr.ends_with(' ') {
                            expr.push(' ');
                        }
                    }
                    expr.push_str(&tok.text);
                }
            }
        }

        // Create a simple command: (( expr ))
        let words = vec![
            Word {
                parts: vec![WordPart::Literal(CompactString::new("(("))],
                span: start_span,
            },
            Word {
                parts: vec![WordPart::Literal(CompactString::new(&expr))],
                span: start_span,
            },
            Word {
                parts: vec![WordPart::Literal(CompactString::new("))"))],
                span: start_span,
            },
        ];

        Command::Simple(SimpleCommand {
            assignments: Vec::new(),
            words,
            redirects: Vec::new(),
        })
    }

    /// subshell → `(` compound_list `)`
    fn parse_subshell(&mut self) -> Subshell {
        self.expect(TokenKind::LeftParen);
        let body = self.parse_compound_list();
        self.expect(TokenKind::RightParen);
        let redirects = self.parse_trailing_redirects();
        Subshell { body, redirects }
    }

    /// brace_group → `{` compound_list `}`
    fn parse_brace_group(&mut self) -> BraceGroup {
        self.expect(TokenKind::LeftBrace);
        let body = self.parse_compound_list();
        self.expect(TokenKind::RightBrace);
        let redirects = self.parse_trailing_redirects();
        BraceGroup { body, redirects }
    }

    /// function_def → `function` name [`(` `)`] newline_list command
    fn parse_function_keyword(&mut self) -> FunctionDef {
        self.advance(); // consume `function`
        let name_tok = self.advance();
        let name = name_tok.text.clone();

        // Optional ( )
        if self.eat(TokenKind::LeftParen) {
            self.expect(TokenKind::RightParen);
        }
        self.skip_newlines();

        let body = self.parse_command();
        let redirects = self.parse_trailing_redirects();

        FunctionDef {
            name,
            body,
            redirects,
        }
    }

    /// function_def → name `(` `)` newline_list command
    fn parse_function_shorthand(&mut self) -> FunctionDef {
        let name_tok = self.advance();
        let name = name_tok.text.clone();
        self.expect(TokenKind::LeftParen);
        self.expect(TokenKind::RightParen);
        self.skip_newlines();

        let body = self.parse_command();
        let redirects = self.parse_trailing_redirects();

        FunctionDef {
            name,
            body,
            redirects,
        }
    }

    /// time_clause → `time` pipeline
    fn parse_time(&mut self) -> TimeClause {
        self.advance(); // consume `time`
        let pipeline = self.parse_pipeline();
        TimeClause { pipeline }
    }

    /// coproc → `coproc` [name] command
    fn parse_coproc(&mut self) -> Coproc {
        self.advance(); // consume `coproc`

        let name = if self.peek_kind() == TokenKind::Word
            && is_identifier(&self.peek().text)
            && !matches!(
                self.peek_nth(1),
                TokenKind::Eof
                    | TokenKind::Semi
                    | TokenKind::Newline
                    | TokenKind::Pipe
                    | TokenKind::AndAnd
                    | TokenKind::OrOr
                    | TokenKind::Ampersand
            )
        {
            let name_tok = self.advance();
            Some(name_tok.text.clone())
        } else {
            None
        };

        let command = self.parse_command();
        Coproc { name, command }
    }
}

// ── Free functions ──────────────────────────────────────────────────

/// Whether a token kind can begin a word or be part of one.
fn is_word_start(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Word
            | TokenKind::SingleQuoted
            | TokenKind::DoubleQuoted
            | TokenKind::DollarSingleQuoted
            | TokenKind::Number
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
    )
}

/// Whether a token kind is a redirect operator.
fn is_redirect_op(kind: TokenKind) -> bool {
    matches!(
        kind,
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

/// Whether a string is a valid shell identifier (`[a-zA-Z_][a-zA-Z0-9_]*`).
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Strip matching quote characters from the start and end of a string.
fn strip_quotes(s: &str, quote: char) -> CompactString {
    if s.starts_with(quote) && s.ends_with(quote) && s.len() >= 2 {
        CompactString::new(&s[1..s.len() - 1])
    } else {
        CompactString::new(s)
    }
}

/// Parse the interior of a double-quoted string, extracting `$VAR`,
/// `${param}`, `$(cmd)`, `$((expr))`, and backtick command substitutions.
fn parse_dq_interior(s: &str) -> Vec<WordPart> {
    let mut parts = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut literal_start = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            // Escaped char — include it literally.
            i += 2;
            continue;
        }

        if bytes[i] == b'$' {
            // Flush accumulated literal.
            if i > literal_start {
                parts.push(WordPart::Literal(CompactString::new(&s[literal_start..i])));
            }

            if i + 1 >= bytes.len() {
                // Bare `$` at end.
                parts.push(WordPart::Literal(CompactString::new("$")));
                i += 1;
                literal_start = i;
                continue;
            }

            match bytes[i + 1] {
                b'{' => {
                    // ${...}
                    let start = i + 2;
                    let mut depth = 1;
                    let mut j = start;
                    while j < bytes.len() && depth > 0 {
                        if bytes[j] == b'{' {
                            depth += 1;
                        } else if bytes[j] == b'}' {
                            depth -= 1;
                        }
                        if depth > 0 {
                            j += 1;
                        }
                    }
                    let inner = &s[start..j];
                    // Parse operator if present.
                    if let Some(op_pos) = find_param_operator(inner) {
                        let param = CompactString::new(&inner[..op_pos]);
                        let (op, arg_start) = extract_operator(&inner[op_pos..]);
                        let arg_str = &inner[op_pos + arg_start..];
                        parts.push(WordPart::DollarBrace {
                            param,
                            operator: Some(CompactString::new(op)),
                            arg: if arg_str.is_empty() {
                                None
                            } else {
                                Some(Box::new(Word {
                                    parts: vec![WordPart::Literal(CompactString::new(arg_str))],
                                    span: Span::new(0, 0),
                                }))
                            },
                        });
                    } else {
                        parts.push(WordPart::DollarVar(CompactString::new(inner)));
                    }
                    i = j + 1;
                    literal_start = i;
                }
                b'(' => {
                    if i + 2 < bytes.len() && bytes[i + 2] == b'(' {
                        // $((...)) arithmetic substitution.
                        let start = i + 3;
                        let mut j = start;
                        while j + 1 < bytes.len() && !(bytes[j] == b')' && bytes[j + 1] == b')') {
                            j += 1;
                        }
                        let expr = &s[start..j];
                        parts.push(WordPart::ArithSub(CompactString::new(expr)));
                        i = j + 2;
                    } else {
                        // $(...) command substitution.
                        let start = i + 2;
                        let mut depth = 1;
                        let mut j = start;
                        while j < bytes.len() && depth > 0 {
                            if bytes[j] == b'(' {
                                depth += 1;
                            } else if bytes[j] == b')' {
                                depth -= 1;
                            }
                            if depth > 0 {
                                j += 1;
                            }
                        }
                        let inner = &s[start..j];
                        // Parse the inner command as a sub-program.
                        let tokens = frost_lexer::lexer::tokenize(inner.as_bytes());
                        let mut sub_parser = Parser::new(&tokens);
                        let program = sub_parser.parse();
                        parts.push(WordPart::CommandSub(Box::new(program)));
                        i = j + 1;
                    }
                    literal_start = i;
                }
                c if c.is_ascii_alphanumeric() || c == b'_' || c == b'?' || c == b'#'
                    || c == b'@' || c == b'*' || c == b'!' || c == b'-' || c == b'$' => {
                    // $VAR or special var ($?, $$, $#, $@, $*, $!, $-)
                    if matches!(c, b'?' | b'#' | b'@' | b'*' | b'!' | b'-' | b'$') {
                        parts.push(WordPart::DollarVar(CompactString::new(
                            &s[i + 1..i + 2],
                        )));
                        i += 2;
                    } else {
                        let start = i + 1;
                        let mut j = start;
                        while j < bytes.len()
                            && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_')
                        {
                            j += 1;
                        }
                        parts.push(WordPart::DollarVar(CompactString::new(&s[start..j])));
                        i = j;
                    }
                    literal_start = i;
                }
                _ => {
                    // Bare `$` followed by something we don't recognize.
                    parts.push(WordPart::Literal(CompactString::new("$")));
                    i += 1;
                    literal_start = i;
                }
            }
        } else if bytes[i] == b'`' {
            // Backtick command substitution.
            if i > literal_start {
                parts.push(WordPart::Literal(CompactString::new(&s[literal_start..i])));
            }
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'`' {
                if bytes[j] == b'\\' {
                    j += 1; // skip escaped char
                }
                j += 1;
            }
            let inner = &s[start..j];
            let tokens = frost_lexer::lexer::tokenize(inner.as_bytes());
            let mut sub_parser = Parser::new(&tokens);
            let program = sub_parser.parse();
            parts.push(WordPart::CommandSub(Box::new(program)));
            i = j + 1;
            literal_start = i;
        } else {
            i += 1;
        }
    }

    // Flush remaining literal.
    if literal_start < bytes.len() {
        parts.push(WordPart::Literal(CompactString::new(&s[literal_start..])));
    }

    if parts.is_empty() {
        parts.push(WordPart::Literal(CompactString::default()));
    }

    parts
}

/// Find the position of a parameter expansion operator in a ${...} expression.
fn find_param_operator(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    // Skip the parameter name (alphanumeric + _).
    let mut i = 0;
    // Handle special params: ?, #, @, *, !, -, $, and digits.
    if i < bytes.len() && matches!(bytes[i], b'?' | b'@' | b'*' | b'!' | b'-' | b'$') {
        i += 1;
    } else if i < bytes.len() && bytes[i] == b'#' {
        // Could be ${#param} (string length) — skip.
        return None;
    } else {
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
    }
    if i < bytes.len() && i > 0 {
        // Check if the char at position i is a valid operator start.
        let c = bytes[i];
        if matches!(c, b':' | b'#' | b'%' | b'-' | b'+' | b'=' | b'?' | b'/' | b',' | b'^') {
            Some(i)
        } else {
            None
        }
    } else {
        None
    }
}

/// Extract operator and arg start position from a parameter operator string.
fn extract_operator(s: &str) -> (&str, usize) {
    if s.starts_with(":-") { (":-", 2) }
    else if s.starts_with(":+") { (":+", 2) }
    else if s.starts_with(":=") { (":=", 2) }
    else if s.starts_with(":?") { (":?", 2) }
    else if s.starts_with("##") { ("##", 2) }
    else if s.starts_with("%%") { ("%%", 2) }
    else if s.starts_with('#') { ("#", 1) }
    else if s.starts_with('%') { ("%", 1) }
    else if s.starts_with('-') { ("-", 1) }
    else if s.starts_with('+') { ("+", 1) }
    else if s.starts_with('=') { ("=", 1) }
    else if s.starts_with('?') { ("?", 1) }
    else if s.starts_with("//") { ("//", 2) }
    else if s.starts_with('/') { ("/", 1) }
    else if s.starts_with(",,") { (",,", 2) }
    else if s.starts_with(',') { (",", 1) }
    else if s.starts_with("^^") { ("^^", 2) }
    else if s.starts_with('^') { ("^", 1) }
    else { ("", 0) }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use frost_lexer::lexer::tokenize;
    use pretty_assertions::assert_eq;

    fn parse_str(src: &str) -> Program {
        let tokens = tokenize(src.as_bytes());
        let mut parser = Parser::new(&tokens);
        parser.parse()
    }

    fn first_simple(program: &Program) -> &SimpleCommand {
        match &program.commands[0].list.first.commands[0] {
            Command::Simple(s) => s,
            other => panic!("expected Simple, got {other:?}"),
        }
    }

    fn word_text(w: &Word) -> String {
        w.parts
            .iter()
            .map(|p| match p {
                WordPart::Literal(s) | WordPart::SingleQuoted(s) => s.to_string(),
                WordPart::DoubleQuoted(parts) => parts
                    .iter()
                    .map(|p| match p {
                        WordPart::Literal(s) => s.to_string(),
                        _ => String::new(),
                    })
                    .collect(),
                WordPart::DollarVar(name) => format!("${name}"),
                _ => String::new(),
            })
            .collect()
    }

    // ── Simple commands ─────────────────────────────────────────

    #[test]
    fn parse_single_word() {
        let prog = parse_str("true");
        assert_eq!(prog.commands.len(), 1);
        let simple = first_simple(&prog);
        assert_eq!(simple.words.len(), 1);
        assert_eq!(word_text(&simple.words[0]), "true");
    }

    #[test]
    fn parse_two_words() {
        let prog = parse_str("echo hello");
        let simple = first_simple(&prog);
        assert_eq!(simple.words.len(), 2);
        assert_eq!(word_text(&simple.words[0]), "echo");
        assert_eq!(word_text(&simple.words[1]), "hello");
    }

    #[test]
    fn parse_three_words() {
        let prog = parse_str("echo hello world");
        let simple = first_simple(&prog);
        assert_eq!(simple.words.len(), 3);
        assert_eq!(word_text(&simple.words[2]), "world");
    }

    #[test]
    fn parse_single_quoted() {
        let prog = parse_str("echo 'hello world'");
        let simple = first_simple(&prog);
        assert_eq!(simple.words.len(), 2);
        assert!(matches!(&simple.words[1].parts[0], WordPart::SingleQuoted(s) if s == "hello world"));
    }

    #[test]
    fn parse_double_quoted() {
        let prog = parse_str(r#"echo "hello""#);
        let simple = first_simple(&prog);
        assert_eq!(simple.words.len(), 2);
        assert!(matches!(&simple.words[1].parts[0], WordPart::DoubleQuoted(_)));
    }

    // ── Pipelines ───────────────────────────────────────────────

    #[test]
    fn parse_pipeline() {
        let prog = parse_str("echo a | cat");
        let pipeline = &prog.commands[0].list.first;
        assert_eq!(pipeline.commands.len(), 2);
        assert!(!pipeline.bang);
    }

    #[test]
    fn parse_bang_pipeline() {
        let prog = parse_str("! false");
        let pipeline = &prog.commands[0].list.first;
        assert!(pipeline.bang);
        assert_eq!(pipeline.commands.len(), 1);
    }

    // ── Lists ───────────────────────────────────────────────────

    #[test]
    fn parse_and_list() {
        let prog = parse_str("true && echo yes");
        let list = &prog.commands[0].list;
        assert_eq!(list.rest.len(), 1);
        assert_eq!(list.rest[0].0, ListOp::And);
    }

    #[test]
    fn parse_or_list() {
        let prog = parse_str("false || echo fallback");
        let list = &prog.commands[0].list;
        assert_eq!(list.rest.len(), 1);
        assert_eq!(list.rest[0].0, ListOp::Or);
    }

    // ── Semicolons / Multiple commands ──────────────────────────

    #[test]
    fn parse_semicolon_separated() {
        let prog = parse_str("echo a; echo b");
        assert_eq!(prog.commands.len(), 2);
    }

    #[test]
    fn parse_newline_separated() {
        let prog = parse_str("echo a\necho b");
        assert_eq!(prog.commands.len(), 2);
    }

    // ── Assignments ─────────────────────────────────────────────

    #[test]
    fn parse_assignment() {
        let prog = parse_str("FOO=bar");
        let simple = first_simple(&prog);
        assert_eq!(simple.assignments.len(), 1);
        assert_eq!(simple.assignments[0].name, "FOO");
        assert!(simple.words.is_empty());
    }

    #[test]
    fn parse_assignment_with_command() {
        let prog = parse_str("FOO=bar echo hello");
        let simple = first_simple(&prog);
        assert_eq!(simple.assignments.len(), 1);
        assert_eq!(simple.words.len(), 2);
    }

    // ── Redirects ───────────────────────────────────────────────

    #[test]
    fn parse_output_redirect() {
        let prog = parse_str("echo hello > out.txt");
        let simple = first_simple(&prog);
        assert_eq!(simple.redirects.len(), 1);
        assert_eq!(simple.redirects[0].op, RedirectOp::Greater);
        assert_eq!(word_text(&simple.redirects[0].target), "out.txt");
    }

    // ── Compound commands ───────────────────────────────────────

    #[test]
    fn parse_if_then_fi() {
        let prog = parse_str("if true; then echo yes; fi");
        assert_eq!(prog.commands.len(), 1);
        match &prog.commands[0].list.first.commands[0] {
            Command::If(clause) => {
                assert!(!clause.condition.is_empty());
                assert!(!clause.then_body.is_empty());
                assert!(clause.else_body.is_none());
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn parse_if_else() {
        let prog = parse_str("if false; then echo no; else echo yes; fi");
        match &prog.commands[0].list.first.commands[0] {
            Command::If(clause) => {
                assert!(clause.else_body.is_some());
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn parse_for_loop() {
        let prog = parse_str("for x in a b c; do echo $x; done");
        match &prog.commands[0].list.first.commands[0] {
            Command::For(clause) => {
                assert_eq!(clause.var, "x");
                assert_eq!(clause.words.as_ref().unwrap().len(), 3);
                assert!(!clause.body.is_empty());
            }
            other => panic!("expected For, got {other:?}"),
        }
    }

    #[test]
    fn parse_arith_for_loop() {
        let prog = parse_str("for ((i=0; i<3; i++)); do echo $i; done");
        match &prog.commands[0].list.first.commands[0] {
            Command::ArithFor(clause) => {
                // The lexer tokenizes `=` separately, so the parser produces
                // spaced expressions like `i = 0`. The executor's
                // eval_arith_with_assignment handles this correctly.
                assert!(clause.init.contains("i"));
                assert!(clause.init.contains("0"));
                assert!(clause.condition.contains("i"));
                assert!(clause.condition.contains("3"));
                assert!(clause.step.contains("i"));
                assert!(clause.step.contains("++"));
                assert!(!clause.body.is_empty());
            }
            other => panic!("expected ArithFor, got {other:?}"),
        }
    }

    #[test]
    fn parse_arith_for_loop_multiline() {
        let prog = parse_str("for ((x=1; x<=5; x++))\ndo\n  echo $x\ndone");
        match &prog.commands[0].list.first.commands[0] {
            Command::ArithFor(clause) => {
                assert!(clause.init.contains("x"));
                assert!(clause.init.contains("1"));
                assert!(clause.condition.contains("x"));
                assert!(clause.condition.contains("5"));
                assert!(clause.step.contains("x"));
                assert!(clause.step.contains("++"));
                assert!(!clause.body.is_empty());
            }
            other => panic!("expected ArithFor, got {other:?}"),
        }
    }

    #[test]
    fn parse_while_loop() {
        let prog = parse_str("while true; do echo loop; done");
        match &prog.commands[0].list.first.commands[0] {
            Command::While(clause) => {
                assert!(!clause.condition.is_empty());
                assert!(!clause.body.is_empty());
            }
            other => panic!("expected While, got {other:?}"),
        }
    }

    #[test]
    fn parse_subshell() {
        let prog = parse_str("(echo sub)");
        match &prog.commands[0].list.first.commands[0] {
            Command::Subshell(sub) => {
                assert!(!sub.body.is_empty());
            }
            other => panic!("expected Subshell, got {other:?}"),
        }
    }

    #[test]
    fn parse_brace_group() {
        let prog = parse_str("{ echo group; }");
        match &prog.commands[0].list.first.commands[0] {
            Command::BraceGroup(bg) => {
                assert!(!bg.body.is_empty());
            }
            other => panic!("expected BraceGroup, got {other:?}"),
        }
    }

    #[test]
    fn parse_function_def() {
        let prog = parse_str("myfn() { echo hello; }");
        match &prog.commands[0].list.first.commands[0] {
            Command::FunctionDef(fd) => {
                assert_eq!(fd.name, "myfn");
            }
            other => panic!("expected FunctionDef, got {other:?}"),
        }
    }

    #[test]
    fn parse_function_keyword() {
        let prog = parse_str("function myfn { echo hello; }");
        match &prog.commands[0].list.first.commands[0] {
            Command::FunctionDef(fd) => {
                assert_eq!(fd.name, "myfn");
            }
            other => panic!("expected FunctionDef, got {other:?}"),
        }
    }

    // ── Async / background ──────────────────────────────────────

    #[test]
    fn parse_background() {
        let prog = parse_str("sleep 10 &");
        assert!(prog.commands[0].is_async);
    }

    // ── Empty / edge cases ──────────────────────────────────────

    #[test]
    fn parse_empty() {
        let prog = parse_str("");
        assert!(prog.commands.is_empty());
    }

    #[test]
    fn parse_only_newlines() {
        let prog = parse_str("\n\n\n");
        assert!(prog.commands.is_empty());
    }

    #[test]
    fn parse_comment_only() {
        let prog = parse_str("# this is a comment\n");
        assert!(prog.commands.is_empty());
    }
}
