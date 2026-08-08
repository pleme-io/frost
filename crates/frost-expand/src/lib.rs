//! Zsh-compatible word expansion engine.
//!
//! Expansion pipeline order:
//!   1. Tilde expansion (`~` → `$HOME`)
//!   2. Parameter expansion (`$var`, `${var}`, `${var:-default}`, …)
//!   3. Command substitution (`$(cmd)`)
//!   4. Arithmetic expansion (`$((expr))`)
//!   5. Quote removal
//!   6. (Later phases: brace expansion, glob expansion, IFS word splitting)
//!
//! Returns `Vec<String>` because a single word can expand into multiple
//! words (e.g. `$@`, unquoted array, glob).

use compact_str::CompactString;
use frost_parser::ast::{GlobKind, Program, Word, WordPart};
use indexmap::IndexMap;

// ── Trait for the expansion environment ─────────────────────────────

/// Trait that the expansion engine uses to access shell state.
///
/// This is intentionally similar to `ShellEnvironment` but adds
/// methods for typed values and command substitution capture.
pub trait ExpandEnv {
    /// Look up a variable's string value.
    fn get_var(&self, name: &str) -> Option<&str>;
    /// Look up a variable's typed value (returns owned to avoid
    /// cross-crate lifetime issues).
    fn get_var_value(&self, name: &str) -> Option<ExpandValue>;
    /// Get the exit status of the last command.
    fn exit_status(&self) -> i32;
    /// Get the shell's PID.
    fn pid(&self) -> u32;
    /// Get the positional parameters ($1, $2, …).
    fn positional_params(&self) -> &[String];
    /// Execute a command substitution and capture its stdout.
    fn capture_command_sub(&self, program: &Program) -> String;
    /// Evaluate an arithmetic expression and return its result.
    fn eval_arithmetic(&self, expr: &str) -> i64;
    /// Get a random number 0-32767 (for $RANDOM).
    fn random(&self) -> u32 {
        0
    }
    /// Get seconds since shell start (for $SECONDS).
    fn seconds_elapsed(&self) -> u64 {
        0
    }
    /// Last argument of the previous command (for `$_`). Default empty.
    fn last_arg(&self) -> &str {
        ""
    }
    /// Enabled shell options rendered as single-char flags (for `$-`,
    /// e.g. `i` interactive, `m` monitor). Default empty.
    fn option_flags(&self) -> String {
        String::new()
    }
    /// The current `$IFS` value, used by the `${=name}` word-splitting
    /// flag. Defaults to the POSIX space/tab/newline set.
    fn ifs(&self) -> String {
        " \t\n".to_string()
    }
    /// Side-effect hook for `${name:=word}` / `${name=word}`: assign the
    /// resolved default back into the environment. The default is a no-op
    /// (read-only environments — e.g. the test mock — simply ignore the
    /// writeback). The executor's bridge records the assignment and applies
    /// it once expansion finishes (the trait itself stays `&self`).
    fn assign_var(&self, _name: &str, _value: &str) {}
    /// Side-effect hook for `${name:?word}` / `${name?word}`: when `name`
    /// is unset/null, record that the current command must abort with the
    /// (already-expanded) message. The default is a no-op. The executor's
    /// bridge records the request and surfaces it as a non-zero exit after
    /// expansion — matching zsh, which aborts a non-interactive shell.
    fn request_abort(&self, _name: &str, _msg: &str) {}
}

/// Typed variable values visible to the expansion engine.
#[derive(Debug, Clone, PartialEq)]
pub enum ExpandValue {
    Scalar(String),
    Integer(i64),
    Float(f64),
    Array(Vec<String>),
    Associative(IndexMap<String, String>),
}

impl ExpandValue {
    /// Scalar string representation.
    pub fn to_scalar(&self) -> String {
        match self {
            Self::Scalar(s) => s.clone(),
            Self::Integer(n) => n.to_string(),
            Self::Float(f) => format!("{f:.10}"),
            Self::Array(a) => a.join(" "),
            Self::Associative(m) => m.values().cloned().collect::<Vec<_>>().join(" "),
        }
    }
}

// ── Public API ──────────────────────────────────────────────────────

/// Expand a single `Word` AST node into one or more strings.
///
/// In double-quoted context, the result is always a single string.
/// Unquoted arrays and `$@` produce multiple words.
pub fn expand_word(word: &Word, env: &dyn ExpandEnv) -> Vec<String> {
    expand_word_inner(word, env, false)
}

/// [`expand_word`] in a context that performs FIELD SPLITTING — the argv of a
/// command, a `for` list, an array literal.
///
/// The distinction is not cosmetic: `x=$(echo a b)` assigns `a b` while
/// `set -- $(echo a b)` sets two positional parameters, and both go through
/// this same engine. Assignment and other single-value contexts keep calling
/// [`expand_word`].
pub fn expand_word_fields(word: &Word, env: &dyn ExpandEnv) -> Vec<String> {
    expand_word_inner(word, env, true)
}

fn expand_word_inner(word: &Word, env: &dyn ExpandEnv, split_fields: bool) -> Vec<String> {
    let mut ctx = ExpandCtx {
        env,
        in_double_quote: false,
        split_fields,
    };
    let parts = ctx.expand_word_parts(&word.parts);
    if parts.is_empty() && !split_fields {
        // A single-value context always has a value, even if it is empty:
        // `x=$unset` assigns the empty string. A field-splitting context
        // does not — it gets zero fields, and the word disappears.
        vec![String::new()]
    } else {
        parts
    }
}

/// Expand a list of `Word` AST nodes, concatenating each word's
/// expansion. Returns the flat list suitable for `argv`.
pub fn expand_words(words: &[Word], env: &dyn ExpandEnv) -> Vec<String> {
    let mut result = Vec::new();
    for word in words {
        result.extend(expand_word(word, env));
    }
    result
}

/// Expand a single word to exactly one string (for assignments, etc.).
pub fn expand_word_to_string(word: &Word, env: &dyn ExpandEnv) -> String {
    let parts = expand_word(word, env);
    parts.join("")
}

// ── Internal expansion context ──────────────────────────────────────

struct ExpandCtx<'a> {
    env: &'a dyn ExpandEnv,
    in_double_quote: bool,
    /// Whether this expansion feeds a context that splits into fields
    /// (argv, a `for` list, an array literal) rather than one value
    /// (an assignment, a `case` word, a `[[ … ]]` operand).
    split_fields: bool,
}

impl<'a> ExpandCtx<'a> {
    fn expand_word_parts(&mut self, parts: &[WordPart]) -> Vec<String> {
        // Build up segments. Each segment is either a single string
        // or multiple strings (from array/@ expansion).
        let mut segments: Vec<Vec<String>> = Vec::new();

        for part in parts {
            let expanded = self.expand_part(part);
            segments.push(expanded);
        }

        // A word made ENTIRELY of expansions that produced zero fields
        // produces zero words — `for w in ${=x}` with `x=""` iterates not
        // once, and `"$@"` with no positional parameters contributes
        // nothing. `combine_segments` cannot express that: it seeds its
        // accumulator with one empty string and `continue`s past an empty
        // segment, so the word came back as a single empty word. Callers in
        // a single-value context re-inflate this to `[""]`
        // (`expand_word_inner`), so only field-splitting contexts see the
        // difference.
        if !segments.is_empty() && segments.iter().all(Vec::is_empty) {
            return Vec::new();
        }

        // Combine segments: if all segments are single-element, concatenate
        // into one string. If any segment has multiple elements (array/@),
        // produce multiple words via cross-product with surrounding text.
        Self::combine_segments(segments)
    }

    fn combine_segments(segments: Vec<Vec<String>>) -> Vec<String> {
        if segments.is_empty() {
            return vec![String::new()];
        }

        let mut result = vec![String::new()];

        for segment in segments {
            if segment.is_empty() {
                continue;
            }
            if segment.len() == 1 {
                // Single element: append to all current results
                for r in &mut result {
                    r.push_str(&segment[0]);
                }
            } else {
                // Multiple elements: the last current result gets the
                // first element appended, then middle elements become
                // new standalone results, and the last element starts
                // a new result that subsequent segments append to.
                let mut new_result = Vec::new();
                let prefix = result.last().cloned().unwrap_or_default();
                // Drop the last element from result (will be replaced)
                if !result.is_empty() {
                    result.pop();
                }
                // First segment element appends to prefix
                let first = format!("{prefix}{}", segment[0]);
                new_result.push(first);
                // Middle elements are standalone
                for elem in &segment[1..segment.len() - 1] {
                    new_result.push(elem.clone());
                }
                // Last element becomes a new "open" string
                if segment.len() > 1 {
                    new_result.push(segment.last().unwrap().clone());
                }
                result.extend(new_result);
            }
        }

        result
    }

    fn expand_part(&mut self, part: &WordPart) -> Vec<String> {
        match part {
            WordPart::Literal(s) => vec![s.to_string()],
            WordPart::SingleQuoted(s) => vec![s.to_string()],
            // `$'…'` — decode the escapes; the result is one quoted field,
            // never split and never re-globbed.
            WordPart::AnsiCQuoted(raw) => vec![expand_ansi_c(raw)],
            WordPart::DoubleQuoted(inner) => {
                let saved = self.in_double_quote;
                self.in_double_quote = true;
                let parts = self.expand_word_parts(inner);
                self.in_double_quote = saved;
                // "$@" can produce multiple words even inside double quotes;
                // for everything else the result is a single word.
                parts
            }
            WordPart::DollarVar(name) => self.expand_dollar_var(name),
            WordPart::DollarBrace {
                param,
                operator,
                arg,
            } => self.expand_dollar_brace(param, operator.as_deref(), arg.as_deref()),
            WordPart::CommandSub(program) => {
                let output = self.env.capture_command_sub(program);
                // Trim trailing newlines (POSIX/zsh behavior)
                let trimmed = output.trim_end_matches('\n');
                // zsh splits an UNQUOTED command substitution on `$IFS` even
                // with SH_WORD_SPLIT off — that option governs *parameter*
                // expansion only, which is why `$var` deliberately stays one
                // word here. Splitting is suppressed inside double quotes and
                // in a scalar-assignment context (`x=$(echo a b)` keeps the
                // space), which is what `split_fields` carries.
                if self.split_fields && !self.in_double_quote {
                    split_on_ifs(trimmed, &self.env.ifs())
                } else {
                    vec![trimmed.to_string()]
                }
            }
            WordPart::ArithSub(expr) => {
                let result = self.env.eval_arithmetic(expr);
                vec![result.to_string()]
            }
            WordPart::Tilde(user) => {
                if user.is_empty() {
                    if let Some(home) = self.env.get_var("HOME") {
                        vec![home.to_string()]
                    } else {
                        vec!["~".to_string()]
                    }
                } else {
                    vec![format!("~{user}")]
                }
            }
            WordPart::Glob(kind) => {
                // Glob chars pass through expansion — glob matching happens
                // after expansion (Phase 5). Pass through as literal.
                let ch = match kind {
                    GlobKind::Star => "*",
                    GlobKind::Question => "?",
                    GlobKind::At => "@",
                };
                vec![ch.to_string()]
            }
            WordPart::ParamExp(pe) => self.expand_param_exp(pe),
            WordPart::BraceExp(_) => {
                // Brace expansion is handled at a higher level before param expansion.
                // If we reach here, pass through as literal.
                vec![String::new()]
            }
            WordPart::ProcessSub { .. } => {
                // Process substitution is handled at exec level.
                vec![String::new()]
            }
            WordPart::ExtGlob { op, pattern } => {
                // Extended glob passes through expansion like regular globs.
                let prefix = match op {
                    frost_parser::ast::ExtGlobOp::Star => "*(",
                    frost_parser::ast::ExtGlobOp::Plus => "+(",
                    frost_parser::ast::ExtGlobOp::Question => "?(",
                    frost_parser::ast::ExtGlobOp::At => "@(",
                    frost_parser::ast::ExtGlobOp::Not => "!(",
                };
                vec![format!("{prefix}{pattern})")]
            }
        }
    }

    /// Expand `$name` — special params and ordinary variables.
    fn expand_dollar_var(&self, name: &CompactString) -> Vec<String> {
        match name.as_str() {
            "?" => vec![self.env.exit_status().to_string()],
            "$" => vec![self.env.pid().to_string()],
            "!" => vec![String::new()], // last background PID (TODO)
            "#" => vec![self.env.positional_params().len().to_string()],
            "*" => {
                // "$*" → all params joined by space (single word)
                // $* outside quotes → same in zsh (no word splitting by default)
                vec![self.env.positional_params().join(" ")]
            }
            "@" => {
                if self.in_double_quote {
                    // "$@" → each param as a separate word
                    let params = self.env.positional_params();
                    if params.is_empty() {
                        vec![]
                    } else {
                        params.to_vec()
                    }
                } else {
                    // $@ outside quotes in zsh → each as separate word
                    let params = self.env.positional_params();
                    if params.is_empty() {
                        vec![]
                    } else {
                        params.to_vec()
                    }
                }
            }
            "0" => vec!["frost".to_string()],
            "-" => vec![self.env.option_flags()], // current option flags
            "_" => vec![self.env.last_arg().to_string()], // last arg of previous command
            "RANDOM" if self.env.get_var("RANDOM").is_none() => {
                vec![self.env.random().to_string()]
            }
            "SECONDS" if self.env.get_var("SECONDS").is_none() => {
                vec![self.env.seconds_elapsed().to_string()]
            }
            "LINENO" if self.env.get_var("LINENO").is_none() => {
                vec!["0".to_string()] // TODO: track line numbers
            }
            "ZSH_VERSION" if self.env.get_var("ZSH_VERSION").is_none() => {
                vec!["5.9".to_string()] // Report as zsh 5.9 for compatibility
            }
            "EPOCHSECONDS" if self.env.get_var("EPOCHSECONDS").is_none() => {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                vec![secs.to_string()]
            }
            "EPOCHREALTIME" if self.env.get_var("EPOCHREALTIME").is_none() => {
                let dur = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                vec![format!("{}.{:09}", dur.as_secs(), dur.subsec_nanos())]
            }
            "SHLVL" if self.env.get_var("SHLVL").is_none() => {
                vec!["1".to_string()]
            }
            "COLUMNS" if self.env.get_var("COLUMNS").is_none() => {
                vec!["80".to_string()]
            }
            "LINES" if self.env.get_var("LINES").is_none() => {
                vec!["24".to_string()]
            }
            n if n.len() == 1 && n.as_bytes()[0].is_ascii_digit() => {
                let idx = (n.as_bytes()[0] - b'1') as usize;
                let val = self
                    .env
                    .positional_params()
                    .get(idx)
                    .cloned()
                    .unwrap_or_default();
                vec![val]
            }
            _ => {
                // Ordinary variable lookup — check for typed value first
                if let Some(value) = self.env.get_var_value(name) {
                    match &value {
                        ExpandValue::Array(arr) if !self.in_double_quote => {
                            if arr.is_empty() {
                                vec![]
                            } else {
                                arr.clone()
                            }
                        }
                        _ => vec![value.to_scalar()],
                    }
                } else if let Some(s) = self.env.get_var(name) {
                    // Fall back to string lookup
                    vec![s.to_string()]
                } else {
                    // Unset variable → empty string
                    vec![String::new()]
                }
            }
        }
    }

    /// Expand `${param}`, `${param:-default}`, `${#param}`, etc.
    fn expand_dollar_brace(
        &mut self,
        param: &CompactString,
        operator: Option<&str>,
        arg: Option<&Word>,
    ) -> Vec<String> {
        // If no operator was parsed by the parser, try to extract one from the param string.
        // This handles the case where the parser delivers "var:-default" as the param.
        if operator.is_none() && arg.is_none() {
            return self.expand_dollar_brace_raw(param);
        }

        let val = self.env.get_var(param).unwrap_or("").to_string();

        match operator {
            None => {
                // ${param} — simple lookup
                vec![val]
            }
            Some("#") => {
                // ${#param} — length
                vec![val.len().to_string()]
            }
            Some(":-") => {
                // ${param:-word} — use default if empty/unset
                if val.is_empty() {
                    if let Some(a) = arg {
                        expand_word(a, self.env)
                    } else {
                        vec![String::new()]
                    }
                } else {
                    vec![val]
                }
            }
            Some("-") => {
                // ${param-word} — use default if unset (but not if empty)
                if self.env.get_var(param).is_none() {
                    if let Some(a) = arg {
                        expand_word(a, self.env)
                    } else {
                        vec![String::new()]
                    }
                } else {
                    vec![val]
                }
            }
            Some(":+") => {
                // ${param:+word} — use alternative if non-empty
                if !val.is_empty() {
                    if let Some(a) = arg {
                        expand_word(a, self.env)
                    } else {
                        vec![String::new()]
                    }
                } else {
                    vec![String::new()]
                }
            }
            Some("+") => {
                // ${param+word} — use alternative if set
                if self.env.get_var(param).is_some() {
                    if let Some(a) = arg {
                        expand_word(a, self.env)
                    } else {
                        vec![String::new()]
                    }
                } else {
                    vec![String::new()]
                }
            }
            Some(":?") | Some("?") => {
                // ${param:?word} — error if empty/unset
                let check_empty = operator == Some(":?");
                if self.env.get_var(param).is_none() || (check_empty && val.is_empty()) {
                    let msg = match arg {
                        Some(a) => expand_word_to_string(a, self.env),
                        None => "parameter not set".to_string(),
                    };
                    self.env.request_abort(param, &msg);
                    vec![String::new()]
                } else {
                    vec![val]
                }
            }
            Some(":=") | Some("=") => {
                // ${param:=word} — assign expanded default if empty/unset
                let check_empty = operator == Some(":=");
                if self.env.get_var(param).is_none() || (check_empty && val.is_empty()) {
                    let default = arg
                        .map(|a| expand_word_to_string(a, self.env))
                        .unwrap_or_default();
                    self.env.assign_var(param, &default);
                    vec![default]
                } else {
                    vec![val]
                }
            }
            Some("##") => {
                // ${param##pattern} — remove longest prefix match
                if let Some(a) = arg {
                    let pattern = expand_word_to_string(a, self.env);
                    vec![strip_glob_prefix(&val, &pattern, true)]
                } else {
                    vec![val]
                }
            }
            Some("#_") => {
                // ${param#pattern} — remove shortest prefix match
                if let Some(a) = arg {
                    let pattern = expand_word_to_string(a, self.env);
                    vec![strip_glob_prefix(&val, &pattern, false)]
                } else {
                    vec![val]
                }
            }
            Some("%%") => {
                // ${param%%pattern} — remove longest suffix match
                if let Some(a) = arg {
                    let pattern = expand_word_to_string(a, self.env);
                    vec![strip_glob_suffix(&val, &pattern, true)]
                } else {
                    vec![val]
                }
            }
            Some("%") => {
                // ${param%pattern} — remove shortest suffix match
                if let Some(a) = arg {
                    let pattern = expand_word_to_string(a, self.env);
                    vec![strip_glob_suffix(&val, &pattern, false)]
                } else {
                    vec![val]
                }
            }
            Some("/") | Some("//") | Some(":/") => {
                // ${param/pattern/replacement} or ${param//pattern/replacement}
                // The arg contains pattern/replacement joined
                let global = operator == Some("//");
                if let Some(a) = arg {
                    let arg_str = expand_word_to_string(a, self.env);
                    // Split on first unescaped /
                    let (pat, rep) = if let Some(pos) = arg_str.find('/') {
                        (&arg_str[..pos], &arg_str[pos + 1..])
                    } else {
                        (arg_str.as_str(), "")
                    };
                    vec![glob_replace(&val, pat, rep, global)]
                } else {
                    vec![val]
                }
            }
            _ => {
                // Unknown operator: just return the value
                vec![val]
            }
        }
    }

    /// Parse and expand a raw `${...}` string where the parser didn't split
    /// operator/arg. Handles: `${#name}`, `${name:-word}`, `${name/pat/rep}`, etc.
    fn expand_dollar_brace_raw(&mut self, raw: &str) -> Vec<String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return vec![String::new()];
        }

        // ${=name…} — the `shwordsplit` flag. Expand the inner form normally,
        // then split every field on `$IFS` — *even inside double quotes*,
        // which is the entire reason the flag exists. Empty IFS-runs collapse
        // (zsh: `"a  b"` → two words) and an empty value yields zero words.
        if let Some(inner) = raw.strip_prefix('=') {
            let fields = self.expand_dollar_brace_raw(inner);
            let ifs = self.env.ifs();
            let mut out = Vec::new();
            for field in fields {
                out.extend(split_on_ifs(&field, &ifs));
            }
            return out;
        }

        // ${#name} — length (string length or array element count)
        if let Some(name) = raw.strip_prefix('#') {
            if !name.is_empty() && !name.contains(':') && !name.contains('-') {
                // Check if it's an array — return element count
                if let Some(value) = self.env.get_var_value(name) {
                    return match &value {
                        ExpandValue::Array(arr) => vec![arr.len().to_string()],
                        ExpandValue::Associative(m) => vec![m.len().to_string()],
                        _ => vec![value.to_scalar().len().to_string()],
                    };
                }
                let val = self.resolve_param(name);
                return vec![val.len().to_string()];
            }
        }

        // ${+name} — existence test
        if let Some(name) = raw.strip_prefix('+') {
            if !name.is_empty() {
                return vec![
                    if self.env.get_var(name).is_some() {
                        "1"
                    } else {
                        "0"
                    }
                    .to_string(),
                ];
            }
        }

        // Find the parameter name (stops at operator chars)
        let name_end = raw
            .find(|c: char| matches!(c, ':' | '-' | '+' | '=' | '?' | '#' | '%' | '/' | '^' | ','))
            .unwrap_or(raw.len());

        // Handle subscript: name[sub]
        // Look for brackets anywhere in the raw string (not just before name_end)
        let (name, subscript) = if let Some(bracket_pos) = raw[..name_end].find('[') {
            let close = raw[bracket_pos..]
                .find(']')
                .map(|p| bracket_pos + p + 1)
                .unwrap_or(name_end);
            (&raw[..bracket_pos], Some(&raw[bracket_pos + 1..close - 1]))
        } else {
            (&raw[..name_end], None)
        };

        // Adjust name_end to skip past the subscript brackets
        let effective_name_end = if subscript.is_some() {
            raw[name_end..]
                .find(']')
                .map(|p| name_end + p + 1)
                .or(Some(name_end))
                .unwrap_or(name_end)
                .max(name_end)
        } else {
            name_end
        };
        // Recalculate: look for operator chars after the subscript
        let name_end = if subscript.is_some() {
            let after_bracket = raw.find(']').map(|p| p + 1).unwrap_or(name_end);
            after_bracket
        } else {
            name_end
        };

        // Resolve value with subscript
        let val = if let Some(sub) = subscript {
            self.resolve_param_subscript(name, sub)
        } else {
            self.resolve_param(name)
        };

        if name_end >= raw.len() {
            return vec![val];
        }

        let rest = &raw[name_end..];
        // If rest starts with operator chars, use the resolved val
        // Otherwise fall through to operator handling
        let _ = effective_name_end; // used above

        // Try to match operators in order of specificity. Pattern operands
        // (`#`/`##`/`%`/`%%`/`/`/`//`) are themselves expanded (recursively)
        // before matching, and matched through `frost_glob::match_pattern`
        // so `*`, `?`, `[…]` glob metacharacters work — not just literals.
        // ${name##pattern} — remove longest matching prefix
        if let Some(pat) = rest.strip_prefix("##") {
            return vec![strip_glob_prefix(&val, &self.expand_inline_word(pat), true)];
        }
        // ${name#pattern} — remove shortest matching prefix
        if let Some(pat) = rest.strip_prefix('#') {
            return vec![strip_glob_prefix(
                &val,
                &self.expand_inline_word(pat),
                false,
            )];
        }
        // ${name%%pattern} — remove longest matching suffix
        if let Some(pat) = rest.strip_prefix("%%") {
            return vec![strip_glob_suffix(&val, &self.expand_inline_word(pat), true)];
        }
        // ${name%pattern} — remove shortest matching suffix
        if let Some(pat) = rest.strip_prefix('%') {
            return vec![strip_glob_suffix(
                &val,
                &self.expand_inline_word(pat),
                false,
            )];
        }
        // ${name//pattern/replacement} — replace all glob matches
        if let Some(rest) = rest.strip_prefix("//") {
            let (pat, rep) = split_first_slash(rest);
            let pat = self.expand_inline_word(pat);
            let rep = self.expand_inline_word(rep);
            return vec![glob_replace(&val, &pat, &rep, true)];
        }
        // ${name/pattern/replacement} — replace first glob match. Supports the
        // anchored zsh forms `/#pat/rep` (anchor at start) and `/%pat/rep`
        // (anchor at end).
        if let Some(rest) = rest.strip_prefix('/') {
            let (pat_raw, rep_raw) = split_first_slash(rest);
            let rep = self.expand_inline_word(rep_raw);
            if let Some(pat) = pat_raw.strip_prefix('#') {
                let pat = self.expand_inline_word(pat);
                return vec![glob_replace_anchored(&val, &pat, &rep, SubAnchorRaw::Start)];
            }
            if let Some(pat) = pat_raw.strip_prefix('%') {
                let pat = self.expand_inline_word(pat);
                return vec![glob_replace_anchored(&val, &pat, &rep, SubAnchorRaw::End)];
            }
            let pat = self.expand_inline_word(pat_raw);
            return vec![glob_replace(&val, &pat, &rep, false)];
        }
        // ${name:-word}
        if let Some(word) = rest.strip_prefix(":-") {
            return if val.is_empty() {
                vec![self.expand_inline_word(word)]
            } else {
                vec![val]
            };
        }
        // ${name-word}
        if let Some(word) = rest.strip_prefix('-') {
            return if self.env.get_var(name).is_none() {
                vec![self.expand_inline_word(word)]
            } else {
                vec![val]
            };
        }
        // ${name:+word}
        if let Some(word) = rest.strip_prefix(":+") {
            return if !val.is_empty() {
                vec![self.expand_inline_word(word)]
            } else {
                vec![String::new()]
            };
        }
        // ${name+word}
        if let Some(word) = rest.strip_prefix('+') {
            return if self.env.get_var(name).is_some() {
                vec![self.expand_inline_word(word)]
            } else {
                vec![String::new()]
            };
        }
        // ${name:=word} — assign the expanded default back into `name`
        if let Some(word) = rest.strip_prefix(":=") {
            return if val.is_empty() {
                let default = self.expand_inline_word(word);
                self.env.assign_var(name, &default);
                vec![default]
            } else {
                vec![val]
            };
        }
        // ${name=word} — assign default back into `name` if unset (only)
        if let Some(word) = rest.strip_prefix('=') {
            return if self.env.get_var(name).is_none() {
                let default = self.expand_inline_word(word);
                self.env.assign_var(name, &default);
                vec![default]
            } else {
                vec![val]
            };
        }
        // ${name:?word} — abort with the expanded message if empty/unset
        if let Some(word) = rest.strip_prefix(":?") {
            if val.is_empty() {
                let msg = self.error_message(word);
                self.env.request_abort(name, &msg);
                return vec![String::new()];
            }
            return vec![val];
        }
        // ${name?word} — abort if unset (but not merely empty)
        if let Some(word) = rest.strip_prefix('?') {
            if self.env.get_var(name).is_none() {
                let msg = self.error_message(word);
                self.env.request_abort(name, &msg);
                return vec![String::new()];
            }
            return vec![val];
        }
        // ${name:offset:length} — substring
        if rest.starts_with(':') {
            let colon_rest = &rest[1..];
            let parts: Vec<&str> = colon_rest.splitn(2, ':').collect();
            let offset: i64 = parts[0].trim().parse().unwrap_or(0);
            let offset = if offset < 0 {
                (val.len() as i64 + offset).max(0) as usize
            } else {
                offset as usize
            };
            if parts.len() == 2 {
                let length: usize = parts[1].trim().parse().unwrap_or(val.len());
                let end = (offset + length).min(val.len());
                return vec![val.get(offset..end).unwrap_or("").to_string()];
            }
            return vec![val.get(offset..).unwrap_or("").to_string()];
        }
        // ${name^} ${name^^} ${name,} ${name,,} — case modification
        if rest == "^^" {
            return vec![val.to_uppercase()];
        }
        if rest == "^" {
            let mut chars = val.chars();
            return vec![match chars.next() {
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }];
        }
        if rest == ",," {
            return vec![val.to_lowercase()];
        }
        if rest == "," {
            let mut chars = val.chars();
            return vec![match chars.next() {
                Some(c) => c.to_lowercase().to_string() + chars.as_str(),
                None => String::new(),
            }];
        }

        // Unknown operator — return the value
        vec![val]
    }

    /// Expand a structured `ParamExpansion` node.
    fn expand_param_exp(&mut self, pe: &frost_parser::ast::ParamExpansion) -> Vec<String> {
        use frost_parser::ast::{CaseOp, ParamModifier, SubAnchor, Subscript};

        // Step 1: Resolve the base value
        let name = pe.name.as_str();

        // Handle ${+name} — is-set test
        if pe.is_set_test {
            return vec![
                if self.env.get_var(name).is_some() {
                    "1"
                } else {
                    "0"
                }
                .to_string(),
            ];
        }

        // Handle ${#name} — length
        if pe.length && pe.modifier.is_none() {
            let val = self.resolve_param(name);
            // For arrays, length is element count
            if let Some(value) = self.env.get_var_value(name) {
                match &value {
                    ExpandValue::Array(arr) => return vec![arr.len().to_string()],
                    ExpandValue::Associative(m) => return vec![m.len().to_string()],
                    _ => return vec![val.len().to_string()],
                }
            }
            return vec![val.len().to_string()];
        }

        // Resolve the base value, handling subscripts
        let val = if let Some(ref sub) = pe.subscript {
            match sub {
                Subscript::All | Subscript::Star => {
                    if let Some(value) = self.env.get_var_value(name) {
                        match &value {
                            ExpandValue::Array(arr) => {
                                if matches!(sub, Subscript::All) && self.in_double_quote {
                                    return arr.clone();
                                }
                                return vec![arr.join(" ")];
                            }
                            ExpandValue::Associative(m) => {
                                let vals: Vec<String> = m.values().cloned().collect();
                                if matches!(sub, Subscript::All) && self.in_double_quote {
                                    return vals;
                                }
                                return vec![vals.join(" ")];
                            }
                            _ => value.to_scalar(),
                        }
                    } else {
                        self.resolve_param(name)
                    }
                }
                Subscript::Index(idx_str) => {
                    if let Some(value) = self.env.get_var_value(name) {
                        match &value {
                            ExpandValue::Array(arr) => {
                                let idx: i64 = idx_str.parse().unwrap_or(0);
                                // zsh: 1-indexed, negative from end
                                let real_idx = if idx < 0 {
                                    (arr.len() as i64 + idx) as usize
                                } else if idx > 0 {
                                    (idx - 1) as usize
                                } else {
                                    0
                                };
                                arr.get(real_idx).cloned().unwrap_or_default()
                            }
                            ExpandValue::Associative(m) => {
                                m.get(idx_str.as_str()).cloned().unwrap_or_default()
                            }
                            _ => value.to_scalar(),
                        }
                    } else {
                        self.resolve_param(name)
                    }
                }
                Subscript::Pattern { .. } => self.resolve_param(name),
            }
        } else {
            self.resolve_param(name)
        };

        // If no modifier, return the value (apply flags later)
        let result = if let Some(ref modifier) = pe.modifier {
            match modifier {
                ParamModifier::Default { colon, word } => {
                    let empty_or_unset = if *colon {
                        val.is_empty()
                    } else {
                        self.env.get_var(name).is_none()
                    };
                    if empty_or_unset {
                        expand_word(word, self.env).join("")
                    } else {
                        val
                    }
                }
                ParamModifier::Assign { colon, word } => {
                    let empty_or_unset = if *colon {
                        val.is_empty()
                    } else {
                        self.env.get_var(name).is_none()
                    };
                    if empty_or_unset {
                        let default = expand_word(word, self.env).join("");
                        self.env.assign_var(name, &default);
                        default
                    } else {
                        val
                    }
                }
                ParamModifier::Alternative { colon, word } => {
                    let has_value = if *colon {
                        !val.is_empty()
                    } else {
                        self.env.get_var(name).is_some()
                    };
                    if has_value {
                        expand_word(word, self.env).join("")
                    } else {
                        String::new()
                    }
                }
                ParamModifier::Error { colon, word } => {
                    let empty_or_unset = if *colon {
                        val.is_empty()
                    } else {
                        self.env.get_var(name).is_none()
                    };
                    if empty_or_unset {
                        let msg = expand_word(word, self.env).join("");
                        let msg = if msg.is_empty() {
                            "parameter not set".to_string()
                        } else {
                            msg
                        };
                        self.env.request_abort(name, &msg);
                        String::new()
                    } else {
                        val
                    }
                }
                ParamModifier::TrimPrefix { longest, pattern } => {
                    let pat = expand_word_to_string(pattern, self.env);
                    strip_glob_prefix(&val, &pat, *longest)
                }
                ParamModifier::TrimSuffix { longest, pattern } => {
                    let pat = expand_word_to_string(pattern, self.env);
                    strip_glob_suffix(&val, &pat, *longest)
                }
                ParamModifier::Substitute {
                    anchor,
                    pattern,
                    replacement,
                } => {
                    let pat = expand_word_to_string(pattern, self.env);
                    let rep = replacement
                        .as_ref()
                        .map(|w| expand_word_to_string(w, self.env))
                        .unwrap_or_default();
                    match anchor {
                        SubAnchor::All => glob_replace(&val, &pat, &rep, true),
                        SubAnchor::First => glob_replace(&val, &pat, &rep, false),
                        SubAnchor::Start => {
                            glob_replace_anchored(&val, &pat, &rep, SubAnchorRaw::Start)
                        }
                        SubAnchor::End => {
                            glob_replace_anchored(&val, &pat, &rep, SubAnchorRaw::End)
                        }
                    }
                }
                ParamModifier::Substring { offset, length } => {
                    let off_str = expand_word_to_string(offset, self.env);
                    let off: i64 = off_str.trim().parse().unwrap_or(0);
                    let off = if off < 0 {
                        (val.len() as i64 + off).max(0) as usize
                    } else {
                        off as usize
                    };
                    if let Some(len_word) = length {
                        let len_str = expand_word_to_string(len_word, self.env);
                        let len: usize = len_str.trim().parse().unwrap_or(val.len());
                        let end = (off + len).min(val.len());
                        val.get(off..end).unwrap_or("").to_string()
                    } else {
                        val.get(off..).unwrap_or("").to_string()
                    }
                }
                ParamModifier::Case(case_op) => match case_op {
                    CaseOp::UpperAll => val.to_uppercase(),
                    CaseOp::UpperFirst => {
                        let mut chars = val.chars();
                        match chars.next() {
                            Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                            None => String::new(),
                        }
                    }
                    CaseOp::LowerAll => val.to_lowercase(),
                    CaseOp::LowerFirst => {
                        let mut chars = val.chars();
                        match chars.next() {
                            Some(c) => c.to_lowercase().to_string() + chars.as_str(),
                            None => String::new(),
                        }
                    }
                },
            }
        } else {
            val
        };

        // Apply flags (simplified — tier 1)
        let mut result = result;
        for flag in &pe.flags {
            result = match flag {
                frost_parser::ast::ParamFlag::Lower => result.to_lowercase(),
                frost_parser::ast::ParamFlag::Upper => result.to_uppercase(),
                frost_parser::ast::ParamFlag::Capitalize => {
                    let mut chars = result.chars();
                    match chars.next() {
                        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                        None => String::new(),
                    }
                }
                _ => result, // Other flags: TODO
            };
        }

        vec![result]
    }

    /// Resolve a parameter with a subscript: `${name[sub]}`.
    fn resolve_param_subscript(&self, name: &str, sub: &str) -> String {
        if let Some(value) = self.env.get_var_value(name) {
            match &value {
                ExpandValue::Array(arr) => {
                    match sub {
                        "@" | "*" => arr.join(" "),
                        _ => {
                            if let Ok(idx) = sub.parse::<i64>() {
                                // zsh: 1-indexed, negative from end
                                let real_idx = if idx < 0 {
                                    (arr.len() as i64 + idx) as usize
                                } else if idx > 0 {
                                    (idx - 1) as usize
                                } else {
                                    0
                                };
                                arr.get(real_idx).cloned().unwrap_or_default()
                            } else {
                                // Non-numeric subscript, try as-is
                                arr.join(" ")
                            }
                        }
                    }
                }
                ExpandValue::Associative(m) => match sub {
                    "@" | "*" => m.values().cloned().collect::<Vec<_>>().join(" "),
                    _ => m.get(sub).cloned().unwrap_or_default(),
                },
                _ => value.to_scalar(),
            }
        } else {
            self.resolve_param(name)
        }
    }

    /// Resolve a parameter name to its string value (handles special params).
    fn resolve_param(&self, name: &str) -> String {
        match name {
            "?" => self.env.exit_status().to_string(),
            "$" => self.env.pid().to_string(),
            "#" => self.env.positional_params().len().to_string(),
            "*" => self.env.positional_params().join(" "),
            "@" => self.env.positional_params().join(" "),
            "0" => "frost".to_string(),
            "-" => self.env.option_flags(),
            "_" => self.env.last_arg().to_string(),
            "RANDOM" if self.env.get_var("RANDOM").is_none() => self.env.random().to_string(),
            "SECONDS" if self.env.get_var("SECONDS").is_none() => {
                self.env.seconds_elapsed().to_string()
            }
            n if n.len() == 1 && n.as_bytes()[0].is_ascii_digit() => {
                let idx = (n.as_bytes()[0] - b'1') as usize;
                self.env
                    .positional_params()
                    .get(idx)
                    .cloned()
                    .unwrap_or_default()
            }
            _ => self.env.get_var(name).unwrap_or("").to_string(),
        }
    }

    /// Expand the *word* argument of an operator like `${var:-word}`.
    ///
    /// The word is itself subject to the full expansion pipeline — zsh
    /// recursively expands `$other`, `${nested}`, `$(cmd)`, `$((expr))` and
    /// quoting inside the default/alternative/assign/error word, while
    /// preserving every literal byte (including runs of whitespace) verbatim.
    ///
    /// We scan the raw word, copy literal runs unchanged, and for each
    /// embedded expansion (`$…`, `${…}`, `$(…)`, `$((…))`, `` `…` ``) hand
    /// the substring back to the real lexer + parser + expander. That keeps a
    /// single expansion path (the parser owns all `$`-form parsing) and never
    /// re-implements either a second parser or a second expander.
    fn expand_inline_word(&self, s: &str) -> String {
        // Fast path: nothing to expand (no `$`/backtick) — the word is its
        // own literal value, whitespace and all.
        if !s.contains(['$', '`']) {
            return s.to_string();
        }

        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        let mut lit_start = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'$' || b == b'`' {
                let end = expansion_extent(s, i);
                if end > i {
                    // Flush the literal run before this expansion (UTF-8 safe:
                    // we only ever split on ASCII `$`/backtick boundaries).
                    out.push_str(&s[lit_start..i]);
                    out.push_str(&self.expand_segment(&s[i..end]));
                    i = end;
                    lit_start = end;
                    continue;
                }
            }
            i += 1;
        }
        out.push_str(&s[lit_start..]);
        out
    }

    /// Build the (expanded) message for a `${name:?word}` abort. zsh uses
    /// the standard "parameter not set" text when the word is empty.
    fn error_message(&self, word: &str) -> String {
        if word.is_empty() {
            "parameter not set".to_string()
        } else {
            self.expand_inline_word(word)
        }
    }

    /// Expand a single self-contained expansion segment (`$x`, `${…}`,
    /// `$(…)`, `$((…))`, `` `…` ``) through the real lexer + parser + engine.
    fn expand_segment(&self, seg: &str) -> String {
        let tokens = tokenize_str(seg);
        let mut parser = frost_parser::Parser::new(&tokens);
        let word = parser.parse_word_for_expansion();
        let mut ctx = ExpandCtx {
            env: self.env,
            // Inherit quoting so a command sub inside a double-quoted default
            // doesn't word-split.
            in_double_quote: self.in_double_quote,
            // The result is `join("")`-ed back into one string, so splitting
            // here could only lose the separators. Never split.
            split_fields: false,
        };
        ctx.expand_word_parts(&word.parts).join("")
    }
}

/// Lex a raw string into a token stream — `frost_lexer::tokenize_str`, the
/// one guarded drain-to-Eof loop. Kept as a local alias only so call sites
/// read unchanged.
fn tokenize_str(input: &str) -> Vec<frost_lexer::Token> {
    frost_lexer::tokenize_str(input)
}

/// Given that `s[start]` is `$` or `` ` ``, return the byte index one past the
/// end of the single expansion that begins there. Returns `start` itself if
/// `start` is a bare `$`/backtick that introduces no expansion (so the caller
/// treats it as a literal).
fn expansion_extent(s: &str, start: usize) -> usize {
    let bytes = s.as_bytes();
    if bytes[start] == b'`' {
        // `cmd` — to the matching backtick (or end of string).
        let mut j = start + 1;
        while j < bytes.len() && bytes[j] != b'`' {
            j += 1;
        }
        return (j + 1).min(bytes.len());
    }
    // bytes[start] == b'$'
    let next = bytes.get(start + 1).copied();
    match next {
        Some(b'{') => balanced(bytes, start + 1, b'{', b'}'),
        Some(b'(') => {
            // `$((expr))` arithmetic or `$(cmd)` command sub — both close on
            // the balanced paren run.
            balanced(bytes, start + 1, b'(', b')')
        }
        // `$name` — an identifier or a single special parameter.
        Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
            let mut j = start + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            j
        }
        Some(c)
            if matches!(c, b'?' | b'$' | b'!' | b'#' | b'*' | b'@' | b'-')
                || c.is_ascii_digit() =>
        {
            start + 2
        }
        // Bare `$` (e.g. `$` at end, or `$ `) — not an expansion.
        _ => start,
    }
}

/// Return the byte index one past the matching closer for the opener at
/// `bytes[open]`, scanning a balanced `open`/`close` run. Falls back to the
/// end of the input on an unbalanced opener.
fn balanced(bytes: &[u8], open: usize, open_ch: u8, close_ch: u8) -> usize {
    let mut depth = 0i32;
    let mut j = open;
    while j < bytes.len() {
        let b = bytes[j];
        if b == open_ch {
            depth += 1;
        } else if b == close_ch {
            depth -= 1;
            if depth == 0 {
                return j + 1;
            }
        }
        j += 1;
    }
    bytes.len()
}

/// Split a string on the first unescaped `/`.
fn split_first_slash(s: &str) -> (&str, &str) {
    if let Some(pos) = s.find('/') {
        (&s[..pos], &s[pos + 1..])
    } else {
        (s, "")
    }
}

// ── Pattern trimming helpers ────────────────────────────────────────
//
// All four pattern-op primitives route through `frost_glob::match_pattern`
// — the one glob engine the shell already ships — so `*`, `?`, `[…]`,
// `[!…]`, `[a-z]` and escapes behave identically here and in filename
// globbing. We pass `dot_glob: true` so the filename-only leading-dot rule
// (which has no meaning for parameter values) is disabled and `*` spans
// every character (including `/`), matching zsh's `${path##*/}` semantics.

/// Glob options for parameter pattern-ops: a pure glob match with the
/// filename leading-dot guard switched off.
fn pattern_op_opts() -> frost_glob::GlobOptions {
    frost_glob::GlobOptions {
        dot_glob: true,
        case_insensitive: false,
    }
}

/// Char-boundary cut points of `s`, ascending (`0..=s.len()`).
fn cut_points(s: &str) -> Vec<usize> {
    let mut pts = vec![0];
    pts.extend(s.char_indices().skip(1).map(|(i, _)| i));
    pts.push(s.len());
    pts
}

/// `${s#pat}` / `${s##pat}` — remove the shortest (or longest) prefix of `s`
/// that matches the glob `pattern`. Returns `s` unchanged when nothing
/// matches.
fn strip_glob_prefix(s: &str, pattern: &str, longest: bool) -> String {
    let opts = pattern_op_opts();
    let cuts = cut_points(s);
    // Shortest scans short→long and takes the first match; longest scans
    // long→short and takes the first match.
    let order: Box<dyn Iterator<Item = &usize>> = if longest {
        Box::new(cuts.iter().rev())
    } else {
        Box::new(cuts.iter())
    };
    for &cut in order {
        if frost_glob::match_pattern(pattern, &s[..cut], &opts) {
            return s[cut..].to_string();
        }
    }
    s.to_string()
}

/// `${s%pat}` / `${s%%pat}` — remove the shortest (or longest) suffix of `s`
/// that matches the glob `pattern`.
fn strip_glob_suffix(s: &str, pattern: &str, longest: bool) -> String {
    let opts = pattern_op_opts();
    let cuts = cut_points(s);
    // Shortest suffix = highest cut point first; longest suffix = lowest.
    let order: Box<dyn Iterator<Item = &usize>> = if longest {
        Box::new(cuts.iter())
    } else {
        Box::new(cuts.iter().rev())
    };
    for &cut in order {
        if frost_glob::match_pattern(pattern, &s[cut..], &opts) {
            return s[..cut].to_string();
        }
    }
    s.to_string()
}

/// `${s/pat/rep}` (first) / `${s//pat/rep}` (all) — replace glob matches.
///
/// zsh matches leftmost-greedy: at each position the *longest* match wins,
/// and scanning resumes after the replaced span (or advances one char on a
/// zero-width or no match). An empty pattern matches nothing (zsh leaves the
/// value unchanged) to avoid an infinite expansion.
fn glob_replace(s: &str, pattern: &str, rep: &str, global: bool) -> String {
    if pattern.is_empty() {
        return s.to_string();
    }
    let opts = pattern_op_opts();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    let mut replaced = false;
    while pos < s.len() {
        if !(global || !replaced) {
            break;
        }
        // Longest match anchored at `pos`.
        let mut matched_end = None;
        let tail = &s[pos..];
        let mut ends: Vec<usize> = cut_points(tail).into_iter().map(|c| pos + c).collect();
        ends.retain(|&e| e > pos); // non-empty spans only
        for &end in ends.iter().rev() {
            if frost_glob::match_pattern(pattern, &s[pos..end], &opts) {
                matched_end = Some(end);
                break;
            }
        }
        if let Some(end) = matched_end {
            out.push_str(rep);
            pos = end;
            replaced = true;
        } else {
            // Copy one char and advance.
            let ch_len = s[pos..].chars().next().map_or(1, |c| c.len_utf8());
            out.push_str(&s[pos..pos + ch_len]);
            pos += ch_len;
        }
    }
    out.push_str(&s[pos..]);
    out
}

/// Anchor for the zsh `${s/#pat/rep}` (start) / `${s/%pat/rep}` (end) forms.
#[derive(Clone, Copy)]
enum SubAnchorRaw {
    Start,
    End,
}

/// `${s/#pat/rep}` / `${s/%pat/rep}` — replace a single match anchored at the
/// start or end of `s` (longest match at the anchor).
fn glob_replace_anchored(s: &str, pattern: &str, rep: &str, anchor: SubAnchorRaw) -> String {
    let opts = pattern_op_opts();
    let cuts = cut_points(s);
    match anchor {
        SubAnchorRaw::Start => {
            // Longest prefix match.
            for &cut in cuts.iter().rev() {
                if frost_glob::match_pattern(pattern, &s[..cut], &opts) {
                    let mut out = String::with_capacity(rep.len() + s.len() - cut);
                    out.push_str(rep);
                    out.push_str(&s[cut..]);
                    return out;
                }
            }
        }
        SubAnchorRaw::End => {
            // Longest suffix match.
            for &cut in cuts.iter() {
                if frost_glob::match_pattern(pattern, &s[cut..], &opts) {
                    let mut out = String::with_capacity(cut + rep.len());
                    out.push_str(&s[..cut]);
                    out.push_str(rep);
                    return out;
                }
            }
        }
    }
    s.to_string()
}

/// Split a string on `$IFS` characters, collapsing runs of IFS whitespace and
/// dropping leading/trailing empties (zsh `shwordsplit` semantics).
fn split_on_ifs(s: &str, ifs: &str) -> Vec<String> {
    if ifs.is_empty() {
        return if s.is_empty() {
            Vec::new()
        } else {
            vec![s.to_string()]
        };
    }
    let ifs_set: Vec<char> = ifs.chars().collect();
    s.split(|c| ifs_set.contains(&c))
        .filter(|f| !f.is_empty())
        .map(|f| f.to_string())
        .collect()
}

// ── ANSI-C quoting ($'...') helper ──────────────────────────────────

/// Expand ANSI-C escape sequences in a `$'...'` string.
pub fn expand_ansi_c(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('a') => out.push('\x07'),
                Some('b') => out.push('\x08'),
                Some('e') | Some('E') => out.push('\x1b'),
                Some('f') => out.push('\x0c'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('v') => out.push('\x0b'),
                Some('\\') => out.push('\\'),
                Some('\'') => out.push('\''),
                Some('"') => out.push('"'),
                Some('0') => {
                    // Octal. zsh reads at most THREE octal digits in total,
                    // the leading `0` included — so `$'\0101'` is `\010`
                    // (backspace) followed by a literal `1`, not `\0101`
                    // (`A`). Reading three digits AFTER the `0` made frost
                    // print `A` (measured against zsh --no-rcs 5.9,
                    // 2026-08-07). Two more, not three.
                    let mut val = 0u32;
                    for _ in 0..2 {
                        if let Some(&d) = chars.peek() {
                            if ('0'..='7').contains(&d) {
                                val = val * 8 + (d as u32 - '0' as u32);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                    out.push(char::from_u32(val).unwrap_or('\0'));
                }
                Some('x') => {
                    // Hex \xNN
                    let mut val = 0u32;
                    for _ in 0..2 {
                        if let Some(&d) = chars.peek() {
                            if d.is_ascii_hexdigit() {
                                val = val * 16 + d.to_digit(16).unwrap_or(0);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                    out.push(char::from_u32(val).unwrap_or('\0'));
                }
                Some('u') => {
                    // Unicode \uNNNN
                    let mut val = 0u32;
                    for _ in 0..4 {
                        if let Some(&d) = chars.peek() {
                            if d.is_ascii_hexdigit() {
                                val = val * 16 + d.to_digit(16).unwrap_or(0);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                    out.push(char::from_u32(val).unwrap_or('\u{FFFD}'));
                }
                Some('U') => {
                    // Unicode \UNNNNNNNN
                    let mut val = 0u32;
                    for _ in 0..8 {
                        if let Some(&d) = chars.peek() {
                            if d.is_ascii_hexdigit() {
                                val = val * 16 + d.to_digit(16).unwrap_or(0);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                    out.push(char::from_u32(val).unwrap_or('\u{FFFD}'));
                }
                Some(d) if ('1'..='7').contains(&d) => {
                    // Octal \NNN (without leading 0)
                    let mut val = d as u32 - '0' as u32;
                    for _ in 0..2 {
                        if let Some(&next) = chars.peek() {
                            if ('0'..='7').contains(&next) {
                                val = val * 8 + (next as u32 - '0' as u32);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                    out.push(char::from_u32(val).unwrap_or('\0'));
                }
                // An unrecognized escape: zsh DROPS the backslash
                // (`$'a\qb'` → `aqb`), bash KEEPS it (`a\qb`). frost targets
                // zsh, so the backslash goes. Verified against
                // `zsh --no-rcs` 5.9, 2026-08-07.
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Brace expansion ─────────────────────────────────────────────────

/// Expand brace expressions in a string.
///
/// Handles `{a,b,c}` → `["a", "b", "c"]` and `{1..5}` → `["1","2","3","4","5"]`.
/// Nested braces and multiple brace groups in one string are supported.
pub fn expand_braces(input: &str) -> Vec<String> {
    // Find the first top-level brace group
    let Some((prefix, alternatives, suffix)) = find_brace_group(input) else {
        return vec![input.to_string()];
    };

    let mut results = Vec::new();
    for alt in &alternatives {
        let expanded_suffix = expand_braces(suffix);
        for s in expanded_suffix {
            // Recursively expand braces in prefix+alt+suffix
            let candidate = format!("{prefix}{alt}{s}");
            results.extend(expand_braces(&candidate));
        }
    }

    if results.is_empty() {
        vec![input.to_string()]
    } else {
        results
    }
}

/// Find the first balanced `{...}` group, returning (prefix, alternatives, suffix).
fn find_brace_group(s: &str) -> Option<(&str, Vec<String>, &str)> {
    let bytes = s.as_bytes();
    let mut depth = 0;
    let mut open_pos = None;

    for (i, &b) in bytes.iter().enumerate() {
        // Skip escaped characters
        if i > 0 && bytes[i - 1] == b'\\' {
            continue;
        }
        match b {
            b'{' => {
                if depth == 0 {
                    open_pos = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = open_pos {
                        let prefix = &s[..start];
                        let inner = &s[start + 1..i];
                        let suffix = &s[i + 1..];

                        // Try to parse as range: {start..end} or {start..end..step}
                        if let Some(range) = parse_brace_range(inner) {
                            return Some((prefix, range, suffix));
                        }

                        // Parse as comma-separated list
                        let alternatives = split_brace_alternatives(inner);
                        if alternatives.len() > 1 {
                            return Some((prefix, alternatives, suffix));
                        }
                        // Single alternative = not a brace expansion
                        return None;
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// Split brace content by top-level commas (respecting nested braces).
fn split_brace_alternatives(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0;

    for ch in s.chars() {
        match ch {
            '{' => {
                depth += 1;
                current.push(ch);
            }
            '}' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    parts.push(current);
    parts
}

/// Parse `{start..end}` or `{start..end..step}` range expressions.
fn parse_brace_range(inner: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = inner.splitn(3, "..").collect();
    if parts.len() < 2 {
        return None;
    }

    let start_str = parts[0].trim();
    let end_str = parts[1].trim();
    let step_str = parts.get(2).map(|s| s.trim());

    // Try numeric range
    if let (Ok(start), Ok(end)) = (start_str.parse::<i64>(), end_str.parse::<i64>()) {
        let step: i64 = step_str.and_then(|s| s.parse().ok()).unwrap_or(1).max(1);

        // Detect zero-padding
        let pad_width = if start_str.starts_with('0') && start_str.len() > 1 {
            start_str.len()
        } else if end_str.starts_with('0') && end_str.len() > 1 {
            end_str.len()
        } else {
            0
        };

        let mut result = Vec::new();
        if start <= end {
            let mut i = start;
            while i <= end {
                if pad_width > 0 {
                    result.push(format!("{i:0>width$}", width = pad_width));
                } else {
                    result.push(i.to_string());
                }
                i += step;
            }
        } else {
            let mut i = start;
            while i >= end {
                if pad_width > 0 {
                    result.push(format!("{i:0>width$}", width = pad_width));
                } else {
                    result.push(i.to_string());
                }
                i -= step;
            }
        }
        return Some(result);
    }

    // Try character range
    if start_str.len() == 1 && end_str.len() == 1 {
        let start_ch = start_str.chars().next()?;
        let end_ch = end_str.chars().next()?;
        if start_ch.is_ascii_alphabetic() && end_ch.is_ascii_alphabetic() {
            let step: u32 = step_str.and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
            let mut result = Vec::new();
            let (s, e) = (start_ch as u32, end_ch as u32);
            if s <= e {
                let mut i = s;
                while i <= e {
                    if let Some(c) = char::from_u32(i) {
                        result.push(c.to_string());
                    }
                    i += step;
                }
            } else {
                let mut i = s;
                while i >= e {
                    if let Some(c) = char::from_u32(i) {
                        result.push(c.to_string());
                    }
                    if i < step {
                        break;
                    }
                    i -= step;
                }
            }
            return Some(result);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use frost_lexer::Span;
    use frost_parser::ast::{Program, Word, WordPart};
    use pretty_assertions::assert_eq;

    // Minimal ExpandEnv for testing
    struct TestEnv {
        vars: std::collections::HashMap<String, String>,
        params: Vec<String>,
        exit_status: i32,
    }

    impl TestEnv {
        fn new() -> Self {
            Self {
                vars: std::collections::HashMap::new(),
                params: Vec::new(),
                exit_status: 0,
            }
        }
    }

    impl ExpandEnv for TestEnv {
        fn get_var(&self, name: &str) -> Option<&str> {
            self.vars.get(name).map(|s| s.as_str())
        }
        fn get_var_value(&self, _name: &str) -> Option<ExpandValue> {
            None // simplified: no typed values in test
        }
        fn exit_status(&self) -> i32 {
            self.exit_status
        }
        fn pid(&self) -> u32 {
            12345
        }
        fn positional_params(&self) -> &[String] {
            &self.params
        }
        fn capture_command_sub(&self, _program: &Program) -> String {
            String::new()
        }
        fn eval_arithmetic(&self, expr: &str) -> i64 {
            expr.trim().parse().unwrap_or(0)
        }
    }

    fn mk_word(parts: Vec<WordPart>) -> Word {
        Word {
            parts,
            span: Span::new(0, 1),
        }
    }

    #[test]
    fn expand_literal() {
        let env = TestEnv::new();
        let word = mk_word(vec![WordPart::Literal("hello".into())]);
        assert_eq!(expand_word(&word, &env), vec!["hello"]);
    }

    #[test]
    fn expand_single_quoted() {
        let env = TestEnv::new();
        let word = mk_word(vec![WordPart::SingleQuoted("$FOO".into())]);
        assert_eq!(expand_word(&word, &env), vec!["$FOO"]);
    }

    #[test]
    fn expand_dollar_var() {
        let mut env = TestEnv::new();
        env.vars.insert("FOO".into(), "bar".into());
        let word = mk_word(vec![WordPart::DollarVar("FOO".into())]);
        assert_eq!(expand_word(&word, &env), vec!["bar"]);
    }

    #[test]
    fn expand_dollar_question() {
        let mut env = TestEnv::new();
        env.exit_status = 42;
        let word = mk_word(vec![WordPart::DollarVar("?".into())]);
        assert_eq!(expand_word(&word, &env), vec!["42"]);
    }

    #[test]
    fn expand_dollar_hash() {
        let mut env = TestEnv::new();
        env.params = vec!["a".into(), "b".into()];
        let word = mk_word(vec![WordPart::DollarVar("#".into())]);
        assert_eq!(expand_word(&word, &env), vec!["2"]);
    }

    #[test]
    fn expand_dollar_at_in_double_quotes() {
        let mut env = TestEnv::new();
        env.params = vec!["a".into(), "b".into(), "c".into()];
        let word = mk_word(vec![WordPart::DoubleQuoted(vec![WordPart::DollarVar(
            "@".into(),
        )])]);
        // "$@" produces each param as a separate word
        assert_eq!(expand_word(&word, &env), vec!["a", "b", "c"]);
    }

    #[test]
    fn expand_dollar_star() {
        let mut env = TestEnv::new();
        env.params = vec!["a".into(), "b".into()];
        let word = mk_word(vec![WordPart::DollarVar("*".into())]);
        assert_eq!(expand_word(&word, &env), vec!["a b"]);
    }

    #[test]
    fn expand_tilde() {
        let mut env = TestEnv::new();
        env.vars.insert("HOME".into(), "/home/user".into());
        let word = mk_word(vec![WordPart::Tilde("".into())]);
        assert_eq!(expand_word(&word, &env), vec!["/home/user"]);
    }

    #[test]
    fn expand_double_quoted_concat() {
        let mut env = TestEnv::new();
        env.vars.insert("X".into(), "world".into());
        let word = mk_word(vec![WordPart::DoubleQuoted(vec![
            WordPart::Literal("hello ".into()),
            WordPart::DollarVar("X".into()),
        ])]);
        assert_eq!(expand_word(&word, &env), vec!["hello world"]);
    }

    #[test]
    fn expand_default_value() {
        let env = TestEnv::new();
        let word = mk_word(vec![WordPart::DollarBrace {
            param: "UNSET".into(),
            operator: Some(":-".into()),
            arg: Some(Box::new(mk_word(vec![WordPart::Literal("default".into())]))),
        }]);
        assert_eq!(expand_word(&word, &env), vec!["default"]);
    }

    #[test]
    fn expand_alternative_value() {
        let mut env = TestEnv::new();
        env.vars.insert("SET".into(), "yes".into());
        let word = mk_word(vec![WordPart::DollarBrace {
            param: "SET".into(),
            operator: Some(":+".into()),
            arg: Some(Box::new(mk_word(vec![WordPart::Literal("alt".into())]))),
        }]);
        assert_eq!(expand_word(&word, &env), vec!["alt"]);
    }

    #[test]
    fn expand_string_length() {
        let mut env = TestEnv::new();
        env.vars.insert("X".into(), "hello".into());
        let word = mk_word(vec![WordPart::DollarBrace {
            param: "X".into(),
            operator: Some("#".into()),
            arg: None,
        }]);
        assert_eq!(expand_word(&word, &env), vec!["5"]);
    }

    #[test]
    fn expand_arithmetic() {
        let env = TestEnv::new();
        let word = mk_word(vec![WordPart::ArithSub("42".into())]);
        assert_eq!(expand_word(&word, &env), vec!["42"]);
    }

    #[test]
    fn strip_glob_prefix_literal() {
        assert_eq!(strip_glob_prefix("hello_world", "hello_", false), "world");
    }

    #[test]
    fn strip_glob_suffix_literal() {
        assert_eq!(strip_glob_suffix("hello_world", "_world", false), "hello");
    }

    #[test]
    fn strip_glob_prefix_star() {
        // `*` spans `/` for parameter pattern-ops (unlike filename globbing).
        assert_eq!(strip_glob_prefix("a/b/c", "*/", false), "b/c");
        assert_eq!(strip_glob_prefix("a/b/c", "*/", true), "c");
    }

    #[test]
    fn strip_glob_suffix_star() {
        assert_eq!(strip_glob_suffix("a/b/c", "/*", false), "a/b");
        assert_eq!(strip_glob_suffix("a/b/c", "/*", true), "a");
    }

    #[test]
    fn strip_glob_prefix_class() {
        // Character class in the pattern (was a literal-only path before).
        assert_eq!(strip_glob_prefix("abc123", "[a-c]", false), "bc123");
    }

    #[test]
    fn strip_glob_suffix_class() {
        assert_eq!(strip_glob_suffix("abc123", "[0-9]", false), "abc12");
    }

    #[test]
    fn glob_replace_class_all() {
        assert_eq!(glob_replace("a1b2c3", "[0-9]", "_", true), "a_b_c_");
    }

    #[test]
    fn glob_replace_first_only() {
        assert_eq!(glob_replace("aXbXc", "X", "-", false), "a-bXc");
    }

    #[test]
    fn glob_replace_anchored_start_end() {
        assert_eq!(
            glob_replace_anchored("abcabc", "abc", "X", SubAnchorRaw::Start),
            "Xabc"
        );
        assert_eq!(
            glob_replace_anchored("abcabc", "abc", "Y", SubAnchorRaw::End),
            "abcY"
        );
    }

    #[test]
    fn split_on_ifs_collapses_runs() {
        assert_eq!(split_on_ifs("a  b\tc", " \t\n"), vec!["a", "b", "c"]);
        assert_eq!(split_on_ifs("", " \t\n"), Vec::<String>::new());
    }

    // ── $'…' ANSI-C quoting (2026-08-07) ─────────────────────────────

    /// An env whose command substitutions return a fixed string, so the
    /// field-splitting tests below can exercise the real `CommandSub` arm.
    struct FixedSubEnv(&'static str);

    impl ExpandEnv for FixedSubEnv {
        fn get_var(&self, _name: &str) -> Option<&str> {
            None
        }
        fn get_var_value(&self, _name: &str) -> Option<ExpandValue> {
            None
        }
        fn exit_status(&self) -> i32 {
            0
        }
        fn pid(&self) -> u32 {
            1
        }
        fn positional_params(&self) -> &[String] {
            &[]
        }
        fn capture_command_sub(&self, _program: &Program) -> String {
            self.0.to_string()
        }
        fn eval_arithmetic(&self, _expr: &str) -> i64 {
            0
        }
    }

    fn cmd_sub_word() -> Word {
        mk_word(vec![WordPart::CommandSub(Box::new(Program {
            commands: vec![],
            syntax_errors: vec![],
        }))])
    }

    #[test]
    fn ansi_c_part_decodes_its_escapes() {
        let env = TestEnv::new();
        let w = mk_word(vec![WordPart::AnsiCQuoted(r"a\nb\tc".into())]);
        assert_eq!(expand_word(&w, &env), vec!["a\nb\tc".to_string()]);
    }

    #[test]
    fn ansi_c_covers_every_escape_form() {
        assert_eq!(expand_ansi_c(r"\a\b\f\v\r\n\t"), "\x07\x08\x0c\x0b\r\n\t");
        assert_eq!(expand_ansi_c(r#"\\\'\""#), "\\'\"");
        assert_eq!(expand_ansi_c(r"\e|\E"), "\x1b|\x1b");
        assert_eq!(expand_ansi_c(r"\x41\x42"), "AB");
        assert_eq!(expand_ansi_c(r"\101\102"), "AB");
        assert_eq!(expand_ansi_c(r"\u0041"), "A");
        assert_eq!(expand_ansi_c(r"\U00000041"), "A");
        assert_eq!(expand_ansi_c(r"a\0b"), "a\0b");
        // zsh DROPS the backslash of an unknown escape; bash keeps it.
        // frost follows zsh (verified against `zsh --no-rcs` 5.9).
        assert_eq!(expand_ansi_c(r"a\qb"), "aqb");
    }

    // ── Field splitting of unquoted $(…) (2026-08-07) ─────────────────

    #[test]
    fn unquoted_command_substitution_splits_in_a_field_context() {
        let env = FixedSubEnv("a b c\n");
        assert_eq!(
            expand_word_fields(&cmd_sub_word(), &env),
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            "zsh splits `$(…)` even with SH_WORD_SPLIT off"
        );
    }

    #[test]
    fn unquoted_command_substitution_does_not_split_in_a_value_context() {
        let env = FixedSubEnv("a b c\n");
        assert_eq!(
            expand_word(&cmd_sub_word(), &env),
            vec!["a b c".to_string()],
            "`x=$(echo a b c)` keeps one value"
        );
    }

    #[test]
    fn quoted_command_substitution_never_splits() {
        let env = FixedSubEnv("a b c\n");
        let w = mk_word(vec![WordPart::DoubleQuoted(vec![WordPart::CommandSub(
            Box::new(Program {
                commands: vec![],
                syntax_errors: vec![],
            }),
        )])]);
        assert_eq!(expand_word_fields(&w, &env), vec!["a b c".to_string()]);
    }

    #[test]
    fn a_word_of_only_zero_field_expansions_produces_no_words() {
        // `for w in ${=x}` with `x=""` must iterate zero times. The
        // single-value entry point still yields one empty string.
        let env = TestEnv::new();
        let w = mk_word(vec![WordPart::DollarVar("@".into())]); // no params
        assert_eq!(expand_word_fields(&w, &env), Vec::<String>::new());
        assert_eq!(expand_word(&w, &env), vec![String::new()]);
    }
}
