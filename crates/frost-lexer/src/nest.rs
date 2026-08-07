//! Byte-level nesting scanner — the one place that knows how far a quoted
//! or bracketed shell construct reaches.
//!
//! Two consumers share it, which is the point: the lexer needs it to find
//! where a double-quoted token *really* ends (`"$(printf %s "x")"` closes at
//! the last `"`, not the third one), and the parser needs it to find the
//! extent of a `$(…)` / `${…}` / `$((…))` inside content the lexer already
//! handed it. Before this existed each side hand-rolled a `{`/`}` depth
//! counter that ignored quoting, and the two disagreed.
//!
//! ## Termination
//!
//! [`matching_close`] runs one `while` loop whose index `i` increases by at
//! least 1 on **every** arm, and whose bound is `src.len()`. There is no arm
//! that can leave `i` unchanged, so the scan cannot spin. Nesting is tracked
//! on an explicit `Vec` rather than by recursion, so deep input costs heap,
//! never stack.

/// What kind of run the scanner is currently inside.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Nest {
    /// `"…"` — expansions are live, `\` escapes.
    DQuote,
    /// `(…)` — a command-substitution or subshell body; a full command
    /// context, so single quotes, double quotes and `$'…'` all nest inside.
    Paren,
    /// `{…}` — a `${…}` parameter expansion body.
    Brace,
    /// `` `…` `` — legacy command substitution; `\` escapes.
    Backtick,
}

/// Given that `src[open]` is one of `"`, `(`, `{` or `` ` ``, return the byte
/// index **one past** its matching closer.
///
/// Returns `src.len()` when the construct is unterminated — callers treat an
/// unterminated quote the way the shell does (consume to end of input) rather
/// than erroring.
///
/// Returns `open` unchanged when `src[open]` is not an opener, so a caller
/// that mis-identifies a byte gets a no-op instead of a runaway scan.
#[must_use]
pub fn matching_close(src: &[u8], open: usize) -> usize {
    let Some(&opener) = src.get(open) else {
        return open;
    };
    let first = match opener {
        b'"' => Nest::DQuote,
        b'(' => Nest::Paren,
        b'{' => Nest::Brace,
        b'`' => Nest::Backtick,
        _ => return open,
    };

    let mut stack = vec![first];
    let mut i = open + 1;

    while i < src.len() {
        let Some(&top) = stack.last() else { break };
        let b = src[i];
        let next = src.get(i + 1).copied();

        // `\` escapes the following byte everywhere except inside a plain
        // single-quoted run, which `skip_single_quoted` handles on its own.
        if b == b'\\' {
            i += 2;
            continue;
        }

        match top {
            Nest::DQuote => match (b, next) {
                (b'"', _) => {
                    stack.pop();
                    i += 1;
                }
                (b'$', Some(b'(')) => {
                    stack.push(Nest::Paren);
                    i += 2;
                }
                (b'$', Some(b'{')) => {
                    stack.push(Nest::Brace);
                    i += 2;
                }
                (b'`', _) => {
                    stack.push(Nest::Backtick);
                    i += 1;
                }
                _ => i += 1,
            },
            Nest::Paren | Nest::Brace => match (b, next) {
                (b'$', Some(b'\'')) => i = skip_ansi_c_quoted(src, i + 1),
                (b'$', Some(b'(')) => {
                    stack.push(Nest::Paren);
                    i += 2;
                }
                (b'$', Some(b'{')) => {
                    stack.push(Nest::Brace);
                    i += 2;
                }
                (b'\'', _) => i = skip_single_quoted(src, i),
                (b'"', _) => {
                    stack.push(Nest::DQuote);
                    i += 1;
                }
                (b'`', _) => {
                    stack.push(Nest::Backtick);
                    i += 1;
                }
                (b'(', _) => {
                    stack.push(Nest::Paren);
                    i += 1;
                }
                (b'{', _) => {
                    stack.push(Nest::Brace);
                    i += 1;
                }
                (b')', _) if top == Nest::Paren => {
                    stack.pop();
                    i += 1;
                }
                (b'}', _) if top == Nest::Brace => {
                    stack.pop();
                    i += 1;
                }
                // A `)` inside `${…}` (or `}` inside `(…)`) is ordinary text.
                _ => i += 1,
            },
            Nest::Backtick => {
                if b == b'`' {
                    stack.pop();
                }
                i += 1;
            }
        }

        if stack.is_empty() {
            return i.min(src.len());
        }
    }

    src.len()
}

/// `src[open]` is `'`. Return the index one past the closing `'`.
/// A single-quoted run has no escapes — that is the whole point of it.
fn skip_single_quoted(src: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    while i < src.len() {
        if src[i] == b'\'' {
            return i + 1;
        }
        i += 1;
    }
    src.len()
}

/// `src[open]` is the `'` of a `$'…'` run. Return the index one past the
/// closing `'`. Unlike a plain single-quoted run, `\'` does **not** close it.
fn skip_ansi_c_quoted(src: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    while i < src.len() {
        match src[i] {
            b'\\' => i += 2,
            b'\'' => return i + 1,
            _ => i += 1,
        }
    }
    src.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(src: &str, open: usize) -> &str {
        &src[..matching_close(src.as_bytes(), open)]
    }

    #[test]
    fn plain_double_quote() {
        assert_eq!(close("\"abc\" rest", 0), "\"abc\"");
    }

    #[test]
    fn double_quote_holding_cmdsub_with_inner_quotes() {
        // The row that used to wedge the parser.
        let s = "\"$(printf %s \"export D=1\")\" tail";
        assert_eq!(close(s, 0), "\"$(printf %s \"export D=1\")\"");
    }

    #[test]
    fn nested_command_substitutions() {
        let s = "\"$(echo \"$(echo deep)\")\"x";
        assert_eq!(close(s, 0), "\"$(echo \"$(echo deep)\")\"");
    }

    #[test]
    fn single_quotes_inside_cmdsub_hide_parens() {
        let s = "$(echo 'a)b')z";
        assert_eq!(close(s, 1), "$(echo 'a)b')");
    }

    #[test]
    fn ansi_c_quotes_inside_cmdsub_hide_parens() {
        let s = "$(echo $'a\\')b')z";
        // `$'a\')b'` is one ANSI-C run: the `\'` does not close it.
        assert_eq!(close(s, 1), "$(echo $'a\\')b')");
    }

    #[test]
    fn escaped_quote_does_not_close() {
        assert_eq!(close("\"a\\\"b\"c", 0), "\"a\\\"b\"");
    }

    #[test]
    fn arith_double_parens() {
        assert_eq!(close("$(( (1+2) * 3 ))!", 1), "$(( (1+2) * 3 ))");
    }

    #[test]
    fn brace_group_with_quoted_brace() {
        assert_eq!(close("${a:-'}'}tail", 1), "${a:-'}'}");
    }

    #[test]
    fn backtick_run() {
        assert_eq!(close("\"`echo \"x\"`\"z", 0), "\"`echo \"x\"`\"");
    }

    #[test]
    fn unterminated_returns_end() {
        let s = "\"abc";
        assert_eq!(matching_close(s.as_bytes(), 0), s.len());
    }

    #[test]
    fn non_opener_is_a_no_op() {
        assert_eq!(matching_close(b"abc", 1), 1);
        assert_eq!(matching_close(b"", 0), 0);
    }

    #[test]
    fn trailing_backslash_terminates() {
        // `i += 2` can step past the end; the loop bound must still hold.
        assert_eq!(matching_close(b"\"ab\\", 0), 4);
    }
}
