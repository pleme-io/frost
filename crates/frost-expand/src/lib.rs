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
    let mut ctx = ExpandCtx {
        env,
        in_double_quote: false,
    };
    let parts = ctx.expand_word_parts(&word.parts);
    if parts.is_empty() {
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
                vec![trimmed.to_string()]
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
            "-" => vec![String::new()], // current option flags (TODO)
            "_" => vec![String::new()], // last argument of previous command (TODO)
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
                    let msg = if let Some(a) = arg {
                        expand_word_to_string(a, self.env)
                    } else {
                        "parameter not set".to_string()
                    };
                    eprintln!("frost: {param}: {msg}");
                    vec![String::new()]
                } else {
                    vec![val]
                }
            }
            Some(":=") | Some("=") => {
                // ${param:=word} — assign default if empty/unset
                let check_empty = operator == Some(":=");
                if self.env.get_var(param).is_none() || (check_empty && val.is_empty()) {
                    if let Some(a) = arg {
                        let default = expand_word_to_string(a, self.env);
                        // Note: we can't actually assign through the ExpandEnv trait
                        // (it's read-only). The executor handles := assignment.
                        vec![default]
                    } else {
                        vec![String::new()]
                    }
                } else {
                    vec![val]
                }
            }
            Some("##") => {
                // ${param##pattern} — remove longest prefix match
                if let Some(a) = arg {
                    let pattern = expand_word_to_string(a, self.env);
                    vec![trim_prefix(&val, &pattern, true)]
                } else {
                    vec![val]
                }
            }
            Some("#_") => {
                // ${param#pattern} — remove shortest prefix match
                if let Some(a) = arg {
                    let pattern = expand_word_to_string(a, self.env);
                    vec![trim_prefix(&val, &pattern, false)]
                } else {
                    vec![val]
                }
            }
            Some("%%") => {
                // ${param%%pattern} — remove longest suffix match
                if let Some(a) = arg {
                    let pattern = expand_word_to_string(a, self.env);
                    vec![trim_suffix(&val, &pattern, true)]
                } else {
                    vec![val]
                }
            }
            Some("%") => {
                // ${param%pattern} — remove shortest suffix match
                if let Some(a) = arg {
                    let pattern = expand_word_to_string(a, self.env);
                    vec![trim_suffix(&val, &pattern, false)]
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
                    if global {
                        vec![val.replace(pat, rep)]
                    } else {
                        vec![val.replacen(pat, rep, 1)]
                    }
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

        // Try to match operators in order of specificity
        // ${name##pattern}
        if let Some(pat) = rest.strip_prefix("##") {
            return vec![trim_prefix(&val, pat, true)];
        }
        // ${name#pattern} — note: # is already consumed by prefix check above for ${#name},
        // but here name_end would be at the # in the operator position
        if let Some(pat) = rest.strip_prefix('#') {
            return vec![trim_prefix(&val, pat, false)];
        }
        // ${name%%pattern}
        if let Some(pat) = rest.strip_prefix("%%") {
            return vec![trim_suffix(&val, pat, true)];
        }
        // ${name%pattern}
        if let Some(pat) = rest.strip_prefix('%') {
            return vec![trim_suffix(&val, pat, false)];
        }
        // ${name//pattern/replacement}
        if let Some(rest) = rest.strip_prefix("//") {
            let (pat, rep) = split_first_slash(rest);
            return vec![val.replace(pat, rep)];
        }
        // ${name/pattern/replacement}
        if let Some(rest) = rest.strip_prefix('/') {
            let (pat, rep) = split_first_slash(rest);
            return vec![val.replacen(pat, rep, 1)];
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
        // ${name:=word}
        if let Some(word) = rest.strip_prefix(":=") {
            return if val.is_empty() {
                vec![self.expand_inline_word(word)]
            } else {
                vec![val]
            };
        }
        // ${name=word}
        if let Some(word) = rest.strip_prefix('=') {
            return if self.env.get_var(name).is_none() {
                vec![self.expand_inline_word(word)]
            } else {
                vec![val]
            };
        }
        // ${name:?word}
        if let Some(word) = rest.strip_prefix(":?") {
            if val.is_empty() {
                let msg = if word.is_empty() {
                    "parameter not set"
                } else {
                    word
                };
                eprintln!("frost: {name}: {msg}");
                return vec![String::new()];
            }
            return vec![val];
        }
        // ${name?word}
        if let Some(word) = rest.strip_prefix('?') {
            if self.env.get_var(name).is_none() {
                let msg = if word.is_empty() {
                    "parameter not set"
                } else {
                    word
                };
                eprintln!("frost: {name}: {msg}");
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
                        expand_word(word, self.env).join("")
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
                        eprintln!("frost: {name}: {msg}");
                        String::new()
                    } else {
                        val
                    }
                }
                ParamModifier::TrimPrefix { longest, pattern } => {
                    let pat = expand_word_to_string(pattern, self.env);
                    trim_prefix(&val, &pat, *longest)
                }
                ParamModifier::TrimSuffix { longest, pattern } => {
                    let pat = expand_word_to_string(pattern, self.env);
                    trim_suffix(&val, &pat, *longest)
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
                        SubAnchor::All => val.replace(&pat, &rep),
                        SubAnchor::First => val.replacen(&pat, &rep, 1),
                        SubAnchor::Start => {
                            if val.starts_with(&pat) {
                                format!("{rep}{}", &val[pat.len()..])
                            } else {
                                val
                            }
                        }
                        SubAnchor::End => {
                            if val.ends_with(&pat) {
                                format!("{}{rep}", &val[..val.len() - pat.len()])
                            } else {
                                val
                            }
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

    /// Expand an inline word string (from operator arguments like ${var:-word}).
    fn expand_inline_word(&self, s: &str) -> String {
        // Handle simple variable references in the word
        if s.starts_with('$') {
            let var_name = &s[1..];
            return self.resolve_param(var_name);
        }
        s.to_string()
    }
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

/// Remove a glob pattern from the beginning of `s`.
fn trim_prefix(s: &str, pattern: &str, longest: bool) -> String {
    if pattern == "*" {
        return if longest {
            String::new()
        } else {
            s.to_string()
        };
    }

    // Convert simple shell glob to prefix-matching
    if let Some(suffix) = pattern.strip_prefix('*') {
        // Pattern is *SUFFIX — find SUFFIX in s
        if longest {
            // Find last occurrence
            if let Some(pos) = s.rfind(suffix) {
                return s[pos + suffix.len()..].to_string();
            }
        } else {
            // Find first occurrence
            if let Some(pos) = s.find(suffix) {
                return s[pos + suffix.len()..].to_string();
            }
        }
        return s.to_string();
    }

    // Simple literal prefix
    if s.starts_with(pattern) {
        s[pattern.len()..].to_string()
    } else {
        s.to_string()
    }
}

/// Remove a glob pattern from the end of `s`.
fn trim_suffix(s: &str, pattern: &str, longest: bool) -> String {
    if pattern == "*" {
        return if longest {
            String::new()
        } else {
            s.to_string()
        };
    }

    if let Some(prefix) = pattern.strip_suffix('*') {
        // Pattern is PREFIX* — find PREFIX in s
        if longest {
            if let Some(pos) = s.find(prefix) {
                return s[..pos].to_string();
            }
        } else {
            if let Some(pos) = s.rfind(prefix) {
                return s[..pos].to_string();
            }
        }
        return s.to_string();
    }

    // Simple literal suffix
    if s.ends_with(pattern) {
        s[..s.len() - pattern.len()].to_string()
    } else {
        s.to_string()
    }
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
                    // Octal \0NNN
                    let mut val = 0u32;
                    for _ in 0..3 {
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
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
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
    fn trim_prefix_literal() {
        assert_eq!(trim_prefix("hello_world", "hello_", false), "world");
    }

    #[test]
    fn trim_suffix_literal() {
        assert_eq!(trim_suffix("hello_world", "_world", false), "hello");
    }

    #[test]
    fn trim_prefix_star() {
        assert_eq!(trim_prefix("a/b/c", "*/", false), "b/c");
        assert_eq!(trim_prefix("a/b/c", "*/", true), "c");
    }

    #[test]
    fn trim_suffix_star() {
        assert_eq!(trim_suffix("a/b/c", "/*", false), "a/b");
        assert_eq!(trim_suffix("a/b/c", "/*", true), "a");
    }
}
