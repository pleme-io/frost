//! Pratt-parser arithmetic evaluation engine for `$(( … ))` and `(( … ))`.
//!
//! Supports the full C operator set plus zsh extensions:
//!   - Binary: `+` `-` `*` `/` `%` `**` `<<` `>>` `&` `|` `^`
//!   - Comparison: `==` `!=` `<` `>` `<=` `>=`
//!   - Logical: `&&` `||` `!`
//!   - Ternary: `? :`
//!   - Assignment: `=` `+=` `-=` `*=` `/=` `%=` `<<=` `>>=` `&=` `|=` `^=`
//!   - Prefix: `++` `--` `+` `-` `~` `!`
//!   - Postfix: `++` `--`
//!   - Base literals: `0x`, `0o`, `0b`, `N#value`
//!   - Variable deref: bare `x` reads var, `x=5` assigns
//!   - Grouping: `( expr )`

use crate::env::ShellEnv;

/// Evaluate an arithmetic expression string and return the result.
pub fn eval_arithmetic(expr: &str, env: &ShellEnv) -> i64 {
    let expr = expr.trim();
    if expr.is_empty() {
        return 0;
    }
    let tokens = tokenize(expr);
    let mut parser = ArithParser::new(&tokens, env);
    parser.parse_expr(0)
}

/// Evaluate an arithmetic expression, with ability to assign to variables.
pub fn eval_arithmetic_mut(expr: &str, env: &mut ShellEnv) -> i64 {
    let expr = expr.trim();
    if expr.is_empty() {
        return 0;
    }
    let tokens = tokenize(expr);
    let mut parser = ArithParser::new(&tokens, env);
    let result = parser.parse_expr(0);
    // Apply deferred assignments
    for (name, value) in parser.deferred_assigns.clone() {
        env.set_var(&name, &value.to_string());
    }
    result
}

// ── Token types ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(i64),
    Ident(String),
    // Operators
    Plus, Minus, Star, Slash, Percent, Power,
    Amp, Pipe, Caret, Tilde,
    Shl, Shr,
    Eq, Ne, Lt, Gt, Le, Ge,
    And, Or, Not,
    Assign, PlusAssign, MinusAssign, StarAssign, SlashAssign, PercentAssign,
    ShlAssign, ShrAssign, AmpAssign, PipeAssign, CaretAssign,
    Inc, Dec,
    Question, Colon,
    LParen, RParen,
    Comma,
    Eof,
}

// ── Tokenizer ───────────────────────────────────────────────────────

fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\n' | b'\r' => { i += 1; }
            b'0'..=b'9' => {
                let start = i;
                // Check for base prefixes
                if bytes[i] == b'0' && i + 1 < bytes.len() {
                    match bytes[i + 1] {
                        b'x' | b'X' => {
                            i += 2;
                            let hex_start = i;
                            while i < bytes.len() && bytes[i].is_ascii_hexdigit() { i += 1; }
                            let hex = &input[hex_start..i];
                            tokens.push(Token::Num(i64::from_str_radix(hex, 16).unwrap_or(0)));
                            continue;
                        }
                        b'o' | b'O' => {
                            i += 2;
                            let oct_start = i;
                            while i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) { i += 1; }
                            let oct = &input[oct_start..i];
                            tokens.push(Token::Num(i64::from_str_radix(oct, 8).unwrap_or(0)));
                            continue;
                        }
                        b'b' | b'B' => {
                            i += 2;
                            let bin_start = i;
                            while i < bytes.len() && (bytes[i] == b'0' || bytes[i] == b'1') { i += 1; }
                            let bin = &input[bin_start..i];
                            tokens.push(Token::Num(i64::from_str_radix(bin, 2).unwrap_or(0)));
                            continue;
                        }
                        _ => {}
                    }
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
                // Check for N#value (base N)
                if i < bytes.len() && bytes[i] == b'#' {
                    let base: u32 = input[start..i].parse().unwrap_or(10);
                    i += 1; // skip #
                    let val_start = i;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric()) { i += 1; }
                    let val = &input[val_start..i];
                    tokens.push(Token::Num(i64::from_str_radix(val, base).unwrap_or(0)));
                } else {
                    let num: i64 = input[start..i].parse().unwrap_or(0);
                    tokens.push(Token::Num(num));
                }
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') { i += 1; }
                tokens.push(Token::Ident(input[start..i].to_string()));
            }
            b'+' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'+' {
                    tokens.push(Token::Inc); i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::PlusAssign); i += 2;
                } else {
                    tokens.push(Token::Plus); i += 1;
                }
            }
            b'-' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'-' {
                    tokens.push(Token::Dec); i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::MinusAssign); i += 2;
                } else {
                    tokens.push(Token::Minus); i += 1;
                }
            }
            b'*' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    tokens.push(Token::Power); i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::StarAssign); i += 2;
                } else {
                    tokens.push(Token::Star); i += 1;
                }
            }
            b'/' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::SlashAssign); i += 2;
                } else {
                    tokens.push(Token::Slash); i += 1;
                }
            }
            b'%' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::PercentAssign); i += 2;
                } else {
                    tokens.push(Token::Percent); i += 1;
                }
            }
            b'<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'<' {
                    if i + 2 < bytes.len() && bytes[i + 2] == b'=' {
                        tokens.push(Token::ShlAssign); i += 3;
                    } else {
                        tokens.push(Token::Shl); i += 2;
                    }
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::Le); i += 2;
                } else {
                    tokens.push(Token::Lt); i += 1;
                }
            }
            b'>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    if i + 2 < bytes.len() && bytes[i + 2] == b'=' {
                        tokens.push(Token::ShrAssign); i += 3;
                    } else {
                        tokens.push(Token::Shr); i += 2;
                    }
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::Ge); i += 2;
                } else {
                    tokens.push(Token::Gt); i += 1;
                }
            }
            b'&' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                    tokens.push(Token::And); i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::AmpAssign); i += 2;
                } else {
                    tokens.push(Token::Amp); i += 1;
                }
            }
            b'|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    tokens.push(Token::Or); i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::PipeAssign); i += 2;
                } else {
                    tokens.push(Token::Pipe); i += 1;
                }
            }
            b'^' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::CaretAssign); i += 2;
                } else {
                    tokens.push(Token::Caret); i += 1;
                }
            }
            b'=' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::Eq); i += 2;
                } else {
                    tokens.push(Token::Assign); i += 1;
                }
            }
            b'!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Token::Ne); i += 2;
                } else {
                    tokens.push(Token::Not); i += 1;
                }
            }
            b'~' => { tokens.push(Token::Tilde); i += 1; }
            b'?' => { tokens.push(Token::Question); i += 1; }
            b':' => { tokens.push(Token::Colon); i += 1; }
            b'(' => { tokens.push(Token::LParen); i += 1; }
            b')' => { tokens.push(Token::RParen); i += 1; }
            b',' => { tokens.push(Token::Comma); i += 1; }
            _ => { i += 1; } // skip unknown
        }
    }

    tokens.push(Token::Eof);
    tokens
}

// ── Operator precedence (Pratt parser binding powers) ───────────────

/// Returns (left binding power, right binding power) for infix operators.
fn infix_bp(tok: &Token) -> Option<(u8, u8)> {
    Some(match tok {
        Token::Comma => (1, 2),
        Token::Assign | Token::PlusAssign | Token::MinusAssign |
        Token::StarAssign | Token::SlashAssign | Token::PercentAssign |
        Token::ShlAssign | Token::ShrAssign | Token::AmpAssign |
        Token::PipeAssign | Token::CaretAssign => (3, 2), // right-assoc
        Token::Question => (5, 4), // ternary
        Token::Or => (7, 8),
        Token::And => (9, 10),
        Token::Pipe => (11, 12),
        Token::Caret => (13, 14),
        Token::Amp => (15, 16),
        Token::Eq | Token::Ne => (17, 18),
        Token::Lt | Token::Gt | Token::Le | Token::Ge => (19, 20),
        Token::Shl | Token::Shr => (21, 22),
        Token::Plus | Token::Minus => (23, 24),
        Token::Star | Token::Slash | Token::Percent => (25, 26),
        Token::Power => (28, 27), // right-assoc
        _ => return None,
    })
}

fn prefix_bp(tok: &Token) -> Option<u8> {
    Some(match tok {
        Token::Plus | Token::Minus | Token::Not | Token::Tilde => 29,
        Token::Inc | Token::Dec => 29,
        _ => return None,
    })
}

// ── Pratt parser ────────────────────────────────────────────────────

struct ArithParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    env: &'a ShellEnv,
    /// Deferred variable assignments (name, value).
    deferred_assigns: Vec<(String, i64)>,
}

impl<'a> ArithParser<'a> {
    fn new(tokens: &'a [Token], env: &'a ShellEnv) -> Self {
        Self { tokens, pos: 0, env, deferred_assigns: Vec::new() }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: &Token) {
        let tok = self.advance();
        debug_assert_eq!(&tok, expected, "expected {expected:?}, got {tok:?}");
    }

    /// Look up a variable's integer value.
    fn var_value(&self, name: &str) -> i64 {
        self.env.get_var(name)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    /// Main Pratt parsing entry point.
    fn parse_expr(&mut self, min_bp: u8) -> i64 {
        // Prefix / atom
        let mut lhs = match self.advance() {
            Token::Num(n) => n,
            Token::Ident(name) => {
                // Check for postfix ++ / --
                match self.peek() {
                    Token::Inc => {
                        self.advance();
                        let val = self.var_value(&name);
                        self.deferred_assigns.push((name, val + 1));
                        val // post-increment returns old value
                    }
                    Token::Dec => {
                        self.advance();
                        let val = self.var_value(&name);
                        self.deferred_assigns.push((name, val - 1));
                        val
                    }
                    _ => self.var_value(&name),
                }
            }
            Token::LParen => {
                let val = self.parse_expr(0);
                if self.peek() == &Token::RParen {
                    self.advance();
                }
                val
            }
            Token::Plus => {
                let bp = prefix_bp(&Token::Plus).unwrap();
                self.parse_expr(bp)
            }
            Token::Minus => {
                let bp = prefix_bp(&Token::Minus).unwrap();
                -self.parse_expr(bp)
            }
            Token::Not => {
                let bp = prefix_bp(&Token::Not).unwrap();
                let val = self.parse_expr(bp);
                if val == 0 { 1 } else { 0 }
            }
            Token::Tilde => {
                let bp = prefix_bp(&Token::Tilde).unwrap();
                let val = self.parse_expr(bp);
                !val
            }
            Token::Inc => {
                // Pre-increment: ++var
                if let Token::Ident(name) = self.advance() {
                    let val = self.var_value(&name) + 1;
                    self.deferred_assigns.push((name, val));
                    val
                } else {
                    0
                }
            }
            Token::Dec => {
                // Pre-decrement: --var
                if let Token::Ident(name) = self.advance() {
                    let val = self.var_value(&name) - 1;
                    self.deferred_assigns.push((name, val));
                    val
                } else {
                    0
                }
            }
            _ => 0,
        };

        // Infix
        loop {
            let op = self.peek().clone();
            if let Some((l_bp, r_bp)) = infix_bp(&op) {
                if l_bp < min_bp {
                    break;
                }
                self.advance();

                // Special handling for ternary
                if op == Token::Question {
                    let then_val = self.parse_expr(0);
                    if self.peek() == &Token::Colon {
                        self.advance();
                    }
                    let else_val = self.parse_expr(r_bp);
                    lhs = if lhs != 0 { then_val } else { else_val };
                    continue;
                }

                // Short-circuit for && and ||
                if op == Token::And {
                    if lhs == 0 {
                        // Don't evaluate RHS
                        let _ = self.parse_expr(r_bp);
                        lhs = 0;
                    } else {
                        let rhs = self.parse_expr(r_bp);
                        lhs = if rhs != 0 { 1 } else { 0 };
                    }
                    continue;
                }
                if op == Token::Or {
                    if lhs != 0 {
                        let _ = self.parse_expr(r_bp);
                        lhs = 1;
                    } else {
                        let rhs = self.parse_expr(r_bp);
                        lhs = if rhs != 0 { 1 } else { 0 };
                    }
                    continue;
                }

                // Assignment operators: LHS must be an identifier
                if matches!(op, Token::Assign | Token::PlusAssign | Token::MinusAssign |
                    Token::StarAssign | Token::SlashAssign | Token::PercentAssign |
                    Token::ShlAssign | Token::ShrAssign | Token::AmpAssign |
                    Token::PipeAssign | Token::CaretAssign)
                {
                    // Look back for the identifier name
                    // In a proper implementation we'd track lvalues;
                    // for now, peek at the previous token
                    let rhs = self.parse_expr(r_bp);
                    let val = match op {
                        Token::Assign => rhs,
                        Token::PlusAssign => lhs + rhs,
                        Token::MinusAssign => lhs - rhs,
                        Token::StarAssign => lhs * rhs,
                        Token::SlashAssign => if rhs != 0 { lhs / rhs } else { 0 },
                        Token::PercentAssign => if rhs != 0 { lhs % rhs } else { 0 },
                        Token::ShlAssign => lhs << (rhs & 63),
                        Token::ShrAssign => lhs >> (rhs & 63),
                        Token::AmpAssign => lhs & rhs,
                        Token::PipeAssign => lhs | rhs,
                        Token::CaretAssign => lhs ^ rhs,
                        _ => rhs,
                    };
                    // Find the variable name from the token stream
                    // We look backwards for the most recent Ident before the assignment
                    if let Some(name) = self.find_lvalue_name() {
                        self.deferred_assigns.push((name, val));
                    }
                    lhs = val;
                    continue;
                }

                let rhs = self.parse_expr(r_bp);

                lhs = match op {
                    Token::Plus => lhs.wrapping_add(rhs),
                    Token::Minus => lhs.wrapping_sub(rhs),
                    Token::Star => lhs.wrapping_mul(rhs),
                    Token::Slash => if rhs != 0 { lhs / rhs } else { 0 },
                    Token::Percent => if rhs != 0 { lhs % rhs } else { 0 },
                    Token::Power => pow(lhs, rhs),
                    Token::Shl => lhs.wrapping_shl((rhs & 63) as u32),
                    Token::Shr => lhs.wrapping_shr((rhs & 63) as u32),
                    Token::Amp => lhs & rhs,
                    Token::Pipe => lhs | rhs,
                    Token::Caret => lhs ^ rhs,
                    Token::Eq => if lhs == rhs { 1 } else { 0 },
                    Token::Ne => if lhs != rhs { 1 } else { 0 },
                    Token::Lt => if lhs < rhs { 1 } else { 0 },
                    Token::Gt => if lhs > rhs { 1 } else { 0 },
                    Token::Le => if lhs <= rhs { 1 } else { 0 },
                    Token::Ge => if lhs >= rhs { 1 } else { 0 },
                    Token::Comma => rhs,
                    _ => lhs,
                };
            } else {
                break;
            }
        }

        lhs
    }

    /// Try to find the variable name for assignment operators.
    fn find_lvalue_name(&self) -> Option<String> {
        // Walk backwards from current position to find the nearest Ident
        for i in (0..self.pos.saturating_sub(1)).rev() {
            if let Token::Ident(name) = &self.tokens[i] {
                return Some(name.clone());
            }
            // Stop at operators that would break the lvalue chain
            if matches!(&self.tokens[i],
                Token::Num(_) | Token::RParen | Token::Comma |
                Token::Plus | Token::Minus | Token::Star | Token::Slash)
            {
                break;
            }
        }
        None
    }
}

/// Integer exponentiation.
fn pow(base: i64, exp: i64) -> i64 {
    if exp < 0 {
        return 0; // Integer division: x ** -n = 0 for |x| > 1
    }
    let mut result: i64 = 1;
    let mut b = base;
    let mut e = exp as u64;
    while e > 0 {
        if e & 1 == 1 {
            result = result.wrapping_mul(b);
        }
        b = b.wrapping_mul(b);
        e >>= 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::ShellEnv;
    use pretty_assertions::assert_eq;

    fn eval(expr: &str) -> i64 {
        let env = ShellEnv::new();
        eval_arithmetic(expr, &env)
    }

    fn eval_with_var(expr: &str, name: &str, val: &str) -> i64 {
        let mut env = ShellEnv::new();
        env.set_var(name, val);
        eval_arithmetic(expr, &env)
    }

    #[test]
    fn basic_integers() {
        assert_eq!(eval("42"), 42);
        assert_eq!(eval("0"), 0);
        assert_eq!(eval("-5"), -5);
    }

    #[test]
    fn addition_subtraction() {
        assert_eq!(eval("3 + 4"), 7);
        assert_eq!(eval("10 - 3"), 7);
        assert_eq!(eval("1 + 2 + 3"), 6);
    }

    #[test]
    fn multiplication_division() {
        assert_eq!(eval("6 * 7"), 42);
        assert_eq!(eval("15 / 3"), 5);
        assert_eq!(eval("17 % 5"), 2);
    }

    #[test]
    fn precedence() {
        assert_eq!(eval("2 + 3 * 4"), 14);
        assert_eq!(eval("(2 + 3) * 4"), 20);
        assert_eq!(eval("2 * 3 + 4"), 10);
    }

    #[test]
    fn power() {
        assert_eq!(eval("2 ** 10"), 1024);
        assert_eq!(eval("3 ** 3"), 27);
        // Right-associative: 2 ** 3 ** 2 = 2 ** 9 = 512
        assert_eq!(eval("2 ** 3 ** 2"), 512);
    }

    #[test]
    fn comparison() {
        assert_eq!(eval("3 == 3"), 1);
        assert_eq!(eval("3 == 4"), 0);
        assert_eq!(eval("3 != 4"), 1);
        assert_eq!(eval("3 < 4"), 1);
        assert_eq!(eval("4 < 3"), 0);
        assert_eq!(eval("3 <= 3"), 1);
        assert_eq!(eval("3 > 2"), 1);
        assert_eq!(eval("3 >= 3"), 1);
    }

    #[test]
    fn logical() {
        assert_eq!(eval("1 && 1"), 1);
        assert_eq!(eval("1 && 0"), 0);
        assert_eq!(eval("0 || 1"), 1);
        assert_eq!(eval("0 || 0"), 0);
        assert_eq!(eval("!0"), 1);
        assert_eq!(eval("!1"), 0);
        assert_eq!(eval("!42"), 0);
    }

    #[test]
    fn bitwise() {
        assert_eq!(eval("5 & 3"), 1);
        assert_eq!(eval("5 | 3"), 7);
        assert_eq!(eval("5 ^ 3"), 6);
        assert_eq!(eval("~0"), -1);
        assert_eq!(eval("1 << 4"), 16);
        assert_eq!(eval("16 >> 2"), 4);
    }

    #[test]
    fn ternary() {
        assert_eq!(eval("1 ? 42 : 0"), 42);
        assert_eq!(eval("0 ? 42 : 99"), 99);
    }

    #[test]
    fn variable_lookup() {
        assert_eq!(eval_with_var("x + 1", "x", "5"), 6);
        assert_eq!(eval_with_var("x * y", "x", "3"), 0); // y unset = 0
    }

    #[test]
    fn base_literals() {
        assert_eq!(eval("0xFF"), 255);
        assert_eq!(eval("0o77"), 63);
        assert_eq!(eval("0b1010"), 10);
        assert_eq!(eval("16#FF"), 255);
        assert_eq!(eval("2#1010"), 10);
    }

    #[test]
    fn division_by_zero() {
        assert_eq!(eval("5 / 0"), 0);
        assert_eq!(eval("5 % 0"), 0);
    }

    #[test]
    fn assignment() {
        let mut env = ShellEnv::new();
        env.set_var("x", "0");
        let result = eval_arithmetic_mut("x = 42", &mut env);
        assert_eq!(result, 42);
        assert_eq!(env.get_var("x"), Some("42"));
    }

    #[test]
    fn compound_assignment() {
        let mut env = ShellEnv::new();
        env.set_var("x", "10");
        let result = eval_arithmetic_mut("x += 5", &mut env);
        assert_eq!(result, 15);
        assert_eq!(env.get_var("x"), Some("15"));
    }

    #[test]
    fn pre_increment() {
        let mut env = ShellEnv::new();
        env.set_var("x", "5");
        let result = eval_arithmetic_mut("++x", &mut env);
        assert_eq!(result, 6);
        assert_eq!(env.get_var("x"), Some("6"));
    }

    #[test]
    fn comma_operator() {
        assert_eq!(eval("1, 2, 3"), 3);
    }

    #[test]
    fn nested_parens() {
        assert_eq!(eval("((2 + 3) * (4 + 5))"), 45);
    }

    #[test]
    fn unary_plus_minus() {
        assert_eq!(eval("+5"), 5);
        assert_eq!(eval("-5"), -5);
        assert_eq!(eval("- -5"), 5);
    }
}
