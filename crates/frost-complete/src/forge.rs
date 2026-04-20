//! `frost-complete-forge` library surface — introspect CLIs, normalize
//! their completion surface into [`SubcmdSpec`] / [`FlagSpec`] /
//! [`PositSpec`], and emit as Lisp forms ready to drop into
//! `~/.frostrc.lisp`.
//!
//! Today supports:
//!
//! * **Fish completion files** — the de-facto machine-readable format.
//!   Fish's `complete -c <cmd> -s X -l long -d "desc"` lines parse
//!   cleanly to our spec types.
//! * **Lisp emitter** — given any (subcmds, flags, positionals)
//!   triple, produce canonical `(def*)` output. Stable ordering by
//!   path+name so re-generating a file is a no-op when nothing
//!   changed.
//!
//! Not yet implemented (future work):
//!
//! * **--help introspection** — `$TOOL --help` is free-form; heuristic
//!   parsers work ~70% of the time. Better to prefer the tool's
//!   machine-readable output (`--help=json` where available, fish
//!   completions otherwise).
//! * **Zsh compdef parsing** — zsh's `_arguments` mini-language is
//!   dense but regular; a parser would cover much of the long tail
//!   (`_git`, `_kubectl`) that doesn't ship fish completions.
//! * **JSON/YAML formats** — CLIs with structured help (`podman`,
//!   newer `aws` CLI) expose completable trees directly.

use frost_lisp::{FlagSpec, PositSpec, SubcmdSpec, ValueKind};

/// Errors raised by the forge.
#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("failed to parse fish completion line: {line:?}: {reason}")]
    FishParse { line: String, reason: String },
    #[error("failed to parse skim-tab yaml: {0}")]
    YamlParse(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type ForgeResult<T> = Result<T, ForgeError>;

/// Output of a normalization pass — the spec triple that
/// [`emit_lisp`] consumes.
#[derive(Debug, Default, Clone)]
pub struct ForgeOutput {
    pub subcmds: Vec<SubcmdSpec>,
    pub flags: Vec<FlagSpec>,
    pub positionals: Vec<PositSpec>,
}

impl ForgeOutput {
    /// Cheap bookkeeping: sort each vec by path+name so repeated
    /// generations over the same input produce byte-identical
    /// output, and dedup exact duplicates. Dedup collapses the
    /// common fish-file pattern where a single `-n 'guard'` line
    /// emits the same flag under multiple paths (we've already
    /// attributed each to its guard-derived path), and also the
    /// repeat-within-file case where the same flag is listed twice.
    pub fn sort(&mut self) {
        self.subcmds.sort_by(|a, b| {
            (a.path.as_str(), a.name.as_str()).cmp(&(b.path.as_str(), b.name.as_str()))
        });
        self.subcmds
            .dedup_by(|a, b| a.path == b.path && a.name == b.name);

        self.flags.sort_by(|a, b| {
            (a.path.as_str(), a.name.as_str()).cmp(&(b.path.as_str(), b.name.as_str()))
        });
        self.flags
            .dedup_by(|a, b| a.path == b.path && a.name == b.name);

        self.positionals
            .sort_by(|a, b| (a.path.as_str(), a.index).cmp(&(b.path.as_str(), b.index)));
        self.positionals
            .dedup_by(|a, b| a.path == b.path && a.index == b.index);
    }
}

// ─── Fish completion parser ──────────────────────────────────────────────

/// Parse the contents of a fish completion file (typically found in
/// `$FISH_COMPLETE_PATH` or `~/.config/fish/completions/`). Returns
/// the accumulated spec set — fish's `complete` command is line-
/// oriented, one directive per physical line, so we process lazily
/// and accumulate.
///
/// Fish grammar recap (the subset that maps to our specs):
///
/// ```fish
/// complete -c <cmd>                 # the command
///          [-s <char>]              # short flag -X
///          [-l <name>]              # long flag --name
///          [-a '<candidates>']      # argument candidates (space-separated)
///          [-d '<desc>']            # description
///          [-r | -x]                # takes an argument (r: requires, x: exclusive)
///          [-n '<guard>']           # condition (e.g. __fish_seen_subcommand_from X)
///          [-f]                     # no file completion
///          [-F]                     # force file completion
/// ```
///
/// We map:
///
/// * `-c C -s s -l long -d "desc"` with no `-a` → `(defflag :path "C" :name "--long" :description "desc")`.
///   When both `-s` and `-l` exist, we emit TWO FlagSpec entries (short + long)
///   so either is Tab-completable.
/// * `-c C -a 'sub1 sub2' -d "desc"` with a `__fish_seen_subcommand_from`
///   guard → subcommands under `C` (or under `C.parent` if the guard
///   names a parent).
/// * `-c C -a 'sub1 sub2'` without a subcommand guard at top level →
///   each candidate becomes `(defsubcmd :path "C" :name "subN")`.
/// * `-r` / `-x` → the preceding flag `takes: "string"`.
/// * `-F` / file-expecting commands → `takes: "file"`.
///
/// What we don't cover (yet):
///
/// * Nested `__fish_seen_subcommand_from` chains (A then B then C).
///   First-level only today.
/// * Dynamic argument providers (`-a '(some-func)'`) — those are
///   Fish-script calls and we treat the raw parenthesized text as a
///   single opaque candidate (usually skipped).
pub fn parse_fish(src: &str) -> ForgeResult<ForgeOutput> {
    let mut out = ForgeOutput::default();
    for (lineno, raw) in src.lines().enumerate() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with("complete") {
            continue;
        }
        let parsed = parse_fish_line(line).map_err(|reason| ForgeError::FishParse {
            line: format!("line {}: {}", lineno + 1, raw),
            reason,
        })?;
        merge_fish(parsed, &mut out);
    }
    out.sort();
    Ok(out)
}

/// Decoded tokens from one `complete …` line.
#[derive(Debug, Default)]
struct FishLine {
    cmd: Option<String>,
    short: Option<String>,
    long: Option<String>,
    description: Option<String>,
    arguments: Option<String>, // raw `-a` payload
    guard: Option<String>,     // raw `-n` payload
    requires_arg: bool,        // -r or -x
    no_files: bool,            // -f
    force_files: bool,         // -F
}

fn parse_fish_line(line: &str) -> Result<FishLine, String> {
    // fish's `complete` accepts single-letter switches; we walk
    // token-by-token honoring single+double quote strings.
    let tokens = tokenize_shell(line)?;
    if tokens.is_empty() || tokens[0] != "complete" {
        return Err("expected `complete` at start".into());
    }

    let mut out = FishLine::default();
    let mut i = 1;
    while i < tokens.len() {
        let t = &tokens[i];
        let take =
            |out_field: &mut Option<String>, label: &str, i: &mut usize| -> Result<(), String> {
                *i += 1;
                if *i >= tokens.len() {
                    return Err(format!("{label} expects a value"));
                }
                *out_field = Some(tokens[*i].clone());
                Ok(())
            };
        match t.as_str() {
            "-c" | "--command" | "-p" | "--path" => take(&mut out.cmd, "-c", &mut i)?,
            "-s" | "--short-option" => take(&mut out.short, "-s", &mut i)?,
            "-l" | "--long-option" | "--old-option" | "-o" => take(&mut out.long, "-l", &mut i)?,
            "-d" | "--description" => take(&mut out.description, "-d", &mut i)?,
            "-a" | "--arguments" => take(&mut out.arguments, "-a", &mut i)?,
            "-n" | "--condition" => take(&mut out.guard, "-n", &mut i)?,
            "-r" | "--require-parameter" | "-x" | "--exclusive" => out.requires_arg = true,
            "-f" | "--no-files" => out.no_files = true,
            "-F" | "--force-files" => out.force_files = true,
            // Ignore unknown short flags + long flags we haven't mapped.
            _ => {}
        }
        i += 1;
    }
    Ok(out)
}

fn merge_fish(line: FishLine, out: &mut ForgeOutput) {
    let Some(cmd) = line.cmd.as_deref() else {
        return;
    };

    // Determine the target path. If the line has a `__fish_seen_subcommand_from X`
    // guard, the path is `cmd.X`. Otherwise it's the raw command.
    let path = match line.guard.as_deref() {
        Some(g) => {
            if let Some(sub) = seen_subcommand(g) {
                format!("{cmd}.{sub}")
            } else {
                cmd.to_string()
            }
        }
        None => cmd.to_string(),
    };

    // Flags: emit a FlagSpec for each of -s, -l that was given. Fish
    // pairs short + long into one `complete` directive; we split into
    // separate specs so either is discoverable.
    let takes = if line.requires_arg {
        Some(if line.force_files {
            "file".to_string()
        } else {
            "string".to_string()
        })
    } else if line.force_files {
        Some("file".to_string())
    } else {
        None
    };

    let mut had_flag = false;
    if let Some(s) = line.short.as_deref() {
        out.flags.push(FlagSpec {
            path: path.clone(),
            name: format!("-{}", s),
            takes: takes.clone(),
            description: line.description.clone(),
        });
        had_flag = true;
    }
    if let Some(l) = line.long.as_deref() {
        out.flags.push(FlagSpec {
            path: path.clone(),
            name: format!("--{}", l),
            takes: takes.clone(),
            description: line.description.clone(),
        });
        had_flag = true;
    }

    // Arguments / subcommands: if -a is present AND no flag was
    // emitted on this line, treat the payload as subcommands (or, if
    // this is clearly a value list, as a choice positional — we
    // default to subcommands, which is what most CLIs register).
    if !had_flag {
        if let Some(args) = line.arguments.as_deref() {
            // Fish often uses `(some-function)` for dynamic producers —
            // ignore those.
            if !args.contains('(') {
                for candidate in args.split_ascii_whitespace() {
                    out.subcmds.push(SubcmdSpec {
                        path: path.clone(),
                        name: candidate.to_string(),
                        description: line.description.clone(),
                    });
                }
            }
        }
    }
}

/// Parse a fish `-n` guard like `"__fish_seen_subcommand_from status"`
/// and return the subcommand name. Accepts either of:
/// * `__fish_seen_subcommand_from <name>`
/// * `__fish_git_using_command <name>` (a common git-plugin shape)
fn seen_subcommand(guard: &str) -> Option<String> {
    let g = guard.trim();
    for prefix in [
        "__fish_seen_subcommand_from ",
        "__fish_git_using_command ",
        "__fish_use_subcommand ",
    ] {
        if let Some(rest) = g.strip_prefix(prefix) {
            // Take only the first word (some guards list several alternatives).
            let word = rest
                .split(|c: char| c.is_whitespace() || c == ';' || c == ')')
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| c == '"' || c == '\'' || c == '(');
            if !word.is_empty() {
                return Some(word.to_string());
            }
        }
    }
    None
}

/// Minimal shell tokenizer for `complete` lines: respects single- and
/// double-quoted strings, splits on whitespace. Doesn't handle `\\`
/// escapes outside quotes — fish completion files rarely need them.
fn tokenize_shell(line: &str) -> Result<Vec<String>, String> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        match bytes[i] {
            b'\'' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err("unterminated single quote".into());
                }
                out.push(String::from_utf8_lossy(&bytes[start..i]).to_string());
                i += 1;
            }
            b'"' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err("unterminated double quote".into());
                }
                out.push(String::from_utf8_lossy(&bytes[start..i]).to_string());
                i += 1;
            }
            _ => {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                out.push(String::from_utf8_lossy(&bytes[start..i]).to_string());
            }
        }
    }
    Ok(out)
}

// ─── Lisp emitter ─────────────────────────────────────────────────────────

/// Render `output` as a sequence of `(def*)` forms, one per line.
/// Output is deterministic — `output.sort()` before calling this for
/// byte-stable results across runs.
pub fn emit_lisp(output: &ForgeOutput) -> String {
    let mut s = String::new();
    if !output.subcmds.is_empty() {
        s.push_str(";; subcommands (auto-generated)\n");
        for c in &output.subcmds {
            s.push_str(&format!(
                "(defsubcmd :path {} :name {}{})\n",
                lisp_str(&c.path),
                lisp_str(&c.name),
                opt_field("description", c.description.as_deref()),
            ));
        }
        s.push('\n');
    }
    if !output.flags.is_empty() {
        s.push_str(";; flags (auto-generated)\n");
        for f in &output.flags {
            s.push_str(&format!(
                "(defflag :path {} :name {}{}{})\n",
                lisp_str(&f.path),
                lisp_str(&f.name),
                opt_field("takes", f.takes.as_deref()),
                opt_field("description", f.description.as_deref()),
            ));
        }
        s.push('\n');
    }
    if !output.positionals.is_empty() {
        s.push_str(";; positionals (auto-generated)\n");
        for p in &output.positionals {
            s.push_str(&format!(
                "(defposit :path {} :index {}{}{})\n",
                lisp_str(&p.path),
                p.index,
                opt_field("takes", p.takes.as_deref()),
                opt_field("description", p.description.as_deref()),
            ));
        }
    }
    s
}

fn opt_field(name: &str, val: Option<&str>) -> String {
    match val {
        Some(v) if !v.is_empty() => format!(" :{} {}", name, lisp_str(v)),
        _ => String::new(),
    }
}

/// Format a string literal for Lisp — always double-quoted, backslash-
/// escape interior `"` and `\`. Matches `tatara-lisp`'s reader.
fn lisp_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ─── zsh compdef parser ───────────────────────────────────────────────────

/// Parse a zsh completion script (as found in `_git`, `_kubectl`,
/// `_ssh`, etc.) and emit frost-complete specs. Targets the common
/// shape:
///
/// ```zsh
/// #compdef git
/// _git() {
///   _arguments -C \
///     '-C[change working directory]:directory:_files -/' \
///     '--version[show version]' \
///     '--help[show help]' \
///     '1: :_git_commands' \
///     '*::arg:->args'
/// }
/// ```
///
/// What we extract:
///
/// * `#compdef <name> [, <alias>, …]` lines → the target command
///   name(s). Every alias gets a parallel branch, so `_kubectl` with
///   `#compdef kubectl kubecolor` emits FlagSpec/PositSpec under both.
/// * Each argument spec string (the `'...'` quoted chunks passed to
///   `_arguments`) → one or more FlagSpec / PositSpec entries.
///
/// What we don't cover in this first pass:
///
/// * `case $state` subcommand state machines. Zsh uses `$state`
///   dispatching to group flags per subcommand; parsing that
///   requires running a partial zsh interpreter. v1 treats every
///   flag as living at the top level — good enough for flat CLIs,
///   and still far better than nothing for subcommanded ones
///   (the user sees SOME flags for every subcommand rather than
///   none at all).
/// * Helper-function dispatchers like `:_files`, `:_git_commands`.
///   We map `_files` / `_directories` / `_path_files` to file/dir
///   value kinds; unknown helpers become string-typed.
/// * `(group1 group2 ...)` exclusion. We ignore the group name and
///   still emit the flag.
///
/// Every failure point is non-fatal: unparseable lines are dropped
/// with a debug note, because real-world `_git` runs 8000+ lines of
/// zsh that includes inline functions we shouldn't try to parse.
pub fn parse_zsh_compdef(src: &str) -> ForgeResult<ForgeOutput> {
    let mut out = ForgeOutput::default();
    let Some(commands) = find_compdef_commands(src) else {
        // No `#compdef` marker — skip, return empty. Some completion
        // files are helpers called by the main compdef and don't bind
        // to a command themselves.
        return Ok(out);
    };
    let spec_strings = extract_arguments_specs(src);
    for spec in &spec_strings {
        let parsed = match parse_arguments_spec(spec) {
            Some(p) => p,
            None => continue,
        };
        for cmd in &commands {
            emit_parsed_for_command(cmd, &parsed, &mut out);
        }
    }
    out.sort();
    Ok(out)
}

/// First-line `#compdef foo bar,baz` → `["foo", "bar", "baz"]`.
/// Zsh accepts both space and comma separators. `=cmd` aliases are
/// treated as additional names, stripping the `=`.
fn find_compdef_commands(src: &str) -> Option<Vec<String>> {
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("#compdef ") {
            let names: Vec<String> = rest
                .split(|c: char| c == ',' || c.is_whitespace())
                .map(|s| s.trim().trim_start_matches('='))
                .filter(|s| !s.is_empty() && !s.starts_with('-'))
                .map(String::from)
                .collect();
            if !names.is_empty() {
                return Some(names);
            }
        }
    }
    None
}

/// Find every single- or double-quoted string passed to `_arguments`.
/// Zsh line-continuation with `\` is honored — multi-line _arguments
/// invocations produce one flat concatenated view here.
///
/// We deliberately over-collect: any quoted string that looks like an
/// `_arguments` spec (starts with `-`, `*`, or digit after optional
/// paren-group) is returned. Non-spec strings get filtered out by
/// [`parse_arguments_spec`] returning None.
fn extract_arguments_specs(src: &str) -> Vec<String> {
    // Collapse `\<newline>` continuations. Zsh uses them heavily.
    let joined = src.replace("\\\n", " ");
    let bytes = joined.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                let start = i + 1;
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
                if start < i {
                    let s = &joined[start..i];
                    if looks_like_arg_spec(s) {
                        out.push(s.to_string());
                    }
                }
                i += 1;
            }
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        break;
                    }
                    i += 1;
                }
                if start < i {
                    let s = &joined[start..i];
                    if looks_like_arg_spec(s) {
                        out.push(s.to_string());
                    }
                }
                i += 1;
            }
            // Fast-skip line comments (# outside a string).
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// Heuristic: does this quoted string look like an `_arguments` spec?
/// _arguments specs start with one of:
///   -flag | --long | *:…  | 1:…  | (a|b):…
/// plus an optional `(group)` prefix for mutual-exclusion sets.
fn looks_like_arg_spec(s: &str) -> bool {
    let mut rest = s;
    // Strip leading `(group)` → continue checking the remainder.
    if let Some(stripped) = strip_group_prefix(rest) {
        rest = stripped;
    }
    // Leading `!` means "hidden" in _arguments. Ignore.
    rest = rest.trim_start_matches('!');
    let Some(c) = rest.chars().next() else {
        return false;
    };
    c == '-' || c == '*' || c.is_ascii_digit() || c == ':'
}

fn strip_group_prefix(s: &str) -> Option<&str> {
    if !s.starts_with('(') {
        return None;
    }
    // Scan forward to matching `)` — `_arguments` groups don't nest
    // parens, so a flat scan is sufficient.
    let bytes = s.as_bytes();
    let mut i = 1;
    while i < bytes.len() && bytes[i] != b')' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    // Trim any whitespace after the `)` and return the tail.
    let tail = &s[i + 1..];
    Some(tail.trim_start())
}

/// Parsed shape of one `_arguments` spec string. We express the full
/// `_arguments` surface conservatively — anything we don't understand
/// falls through as `Other` and is skipped downstream.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedArg {
    /// `-f[desc]` or `--long[desc]` with optional value.
    Flag {
        name: String,
        takes: Option<ValueKind>,
        description: Option<String>,
    },
    /// `1: :action` / `*:arg:action` — positional at index `n` (or
    /// `u32::MAX` for `*`, meaning "rest").
    Positional {
        index: u32,
        takes: Option<ValueKind>,
        description: Option<String>,
    },
    /// We parsed the shape but don't know what to do — drop.
    Other,
}

fn parse_arguments_spec(raw: &str) -> Option<ParsedArg> {
    let mut s = raw;
    if let Some(stripped) = strip_group_prefix(s) {
        s = stripped;
    }
    s = s.trim_start_matches('!');

    // Is this a flag (starts with `-`) or positional (`*` / digit / `:`)?
    if s.starts_with('-') {
        return parse_flag_spec(s);
    }
    if s.starts_with('*')
        || s.starts_with(':')
        || s.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return parse_positional_spec(s);
    }
    Some(ParsedArg::Other)
}

/// Parse a flag spec: optional `-name` possibly with `+`, optional
/// `[description]`, optional `:label:action` for value types.
fn parse_flag_spec(s: &str) -> Option<ParsedArg> {
    // `-f+` / `-f=` means "takes an arg"; we'll pick up the value
    // type from the `:label:action` later.
    let (name_part, rest) = split_flag_name(s);
    let name = name_part
        .trim_end_matches('+')
        .trim_end_matches('=')
        .to_string();
    if name.is_empty() || name == "-" {
        return Some(ParsedArg::Other);
    }

    // Description in brackets: `[description]`.
    let mut description: Option<String> = None;
    let mut after_desc = rest;
    if let Some(desc_rest) = rest.strip_prefix('[') {
        if let Some(close) = desc_rest.find(']') {
            description = Some(desc_rest[..close].to_string());
            after_desc = &desc_rest[close + 1..];
        }
    }

    // Value action: `:label:action` — possibly multiple colon groups.
    // First colon-group's action names the value type.
    let takes = parse_value_action(after_desc);

    Some(ParsedArg::Flag {
        name,
        takes,
        description,
    })
}

/// Split `-f+foo` into ("-f+", "foo"). Split `--help[desc]` into
/// ("--help", "[desc]"). A bare `-abc` without `+`/`=` is ambiguous
/// in zsh (cluster of short flags) — treat as literal `-abc`.
fn split_flag_name(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'[' || c == b':' {
            return (&s[..i], &s[i..]);
        }
        if c == b'+' || c == b'=' {
            return (&s[..=i], &s[i + 1..]);
        }
        i += 1;
    }
    (s, "")
}

/// Parse a positional spec: `N: :action`, `*:label:action`, `1:file:_files`.
fn parse_positional_spec(s: &str) -> Option<ParsedArg> {
    // Index: `*` = u32::MAX sentinel for "rest"; digits parse to N; `:` means "next" (skip).
    let (index, rest) = match s.chars().next()? {
        '*' => (u32::MAX, &s[1..]),
        ':' => (1, s),
        c if c.is_ascii_digit() => {
            let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            let n: u32 = s[..end].parse().ok()?;
            (n.max(1), &s[end..])
        }
        _ => return None,
    };

    // `::` or ':' — skip leading separators.
    let after_index = rest.trim_start_matches(':');
    // Next colon-delimited label is the description-ish placeholder;
    // zsh spec is `label:action`. We extract a description from
    // `[desc]` if it appears, otherwise use the label.
    let (label_or_desc, after_label) = match after_index.find(':') {
        Some(idx) => (&after_index[..idx], &after_index[idx + 1..]),
        None => (after_index, ""),
    };
    let trimmed_label = label_or_desc.trim();
    let description =
        if !trimmed_label.is_empty() && trimmed_label != "arg" && !trimmed_label.starts_with('_') {
            Some(trimmed_label.to_string())
        } else {
            None
        };
    let takes = parse_value_action(after_label);
    Some(ParsedArg::Positional {
        index,
        takes,
        description,
    })
}

/// `_files` → `file`, `_directories` → `dir`, `_path_files -/` → `dir`,
/// `(json yaml text)` → `choice:json,yaml,text`. Unknown actions
/// fall back to `String`.
///
/// zsh spec shape: `:label:action`. Callers pass the whole
/// post-description tail (may include leading `:`), so we strip a
/// leading colon, peel off the label (everything up to the next
/// colon), then parse what remains as the action.
fn parse_value_action(s: &str) -> Option<ValueKind> {
    let s = s.trim().trim_start_matches(':');
    if s.is_empty() {
        return None;
    }
    // Spec is `label:action` — split once. If no colon, the whole
    // string is already the action.
    let action = match s.find(':') {
        Some(idx) => s[idx + 1..].trim(),
        None => s,
    };
    if action.is_empty() {
        return None;
    }
    // Strip leading `->state` — state dispatchers are subcommand
    // hand-off we don't follow here.
    if action.starts_with("->") {
        return None;
    }
    let s = action;
    // `(a b c)` choice-literal.
    if let Some(rest) = s.strip_prefix('(') {
        if let Some(close) = rest.find(')') {
            let choices: Vec<String> = rest[..close]
                .split(|c: char| c.is_whitespace() || c == '|')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            if !choices.is_empty() {
                return Some(ValueKind::Choice(choices));
            }
        }
    }
    // Helper-function dispatchers.
    for (prefix, kind) in &[
        ("_files", ValueKind::File),
        ("_path_files", ValueKind::File),
        ("_directories", ValueKind::Dir),
        ("_cd", ValueKind::Dir),
    ] {
        if s.starts_with(*prefix) {
            // `_files -/` or `_path_files -/` restricts to directories.
            if s.contains("-/") {
                return Some(ValueKind::Dir);
            }
            return Some(kind.clone());
        }
    }
    // Label-only (`:msg`) with nothing after — string.
    Some(ValueKind::String)
}

/// Fold one ParsedArg into the output under `command`.
fn emit_parsed_for_command(command: &str, arg: &ParsedArg, out: &mut ForgeOutput) {
    match arg {
        ParsedArg::Flag {
            name,
            takes,
            description,
        } => {
            out.flags.push(FlagSpec {
                path: command.to_string(),
                name: name.clone(),
                takes: takes.as_ref().map(|k| value_kind_to_str(k)),
                description: description.clone(),
            });
        }
        ParsedArg::Positional {
            index,
            takes,
            description,
        } => {
            // Clamp `*` (u32::MAX) to a large but sentinel-ish slot
            // so BTreeMap<u32> ordering in the tree stays sane.
            let idx = if *index == u32::MAX { 999 } else { *index };
            out.positionals.push(PositSpec {
                path: command.to_string(),
                index: idx,
                takes: takes.as_ref().map(|k| value_kind_to_str(k)),
                description: description.clone(),
            });
        }
        ParsedArg::Other => {}
    }
}

/// Inverse of [`ValueKind::parse`] — used by the compdef emitter so
/// its output round-trips through the Lisp reader.
fn value_kind_to_str(k: &ValueKind) -> String {
    match k {
        ValueKind::String => "string".into(),
        ValueKind::Integer => "integer".into(),
        ValueKind::File => "file".into(),
        ValueKind::Files => "files".into(),
        ValueKind::Dir => "dir".into(),
        ValueKind::Dirs => "dirs".into(),
        ValueKind::Choice(choices) => {
            let joined = choices.join(",");
            format!("choice:{joined}")
        }
    }
}

// ─── skim-tab YAML parser ─────────────────────────────────────────────────

use serde::Deserialize;
use std::collections::BTreeMap;

/// Top-level document shape for `pleme-io/skim-tab/specs/*.yaml`.
///
/// ```yaml
/// commands: [kubectl, kubecolor, k]
/// icon: "⎈ "
/// subcommands:
///   get:
///     description: "Display resources"
///     glyph: "◈"
///     subcommands:             # optional — nested
///       ...
/// ```
///
/// `commands` lists aliases the spec applies to (kubectl, kubecolor,
/// and `k` all get the same tree). `icon` is cosmetic — skim-tab
/// shows it in the picker header; we ignore it here. Each subcommand
/// recursively may itself declare `subcommands`.
#[derive(Deserialize)]
struct SkimSpec {
    #[serde(default)]
    commands: Vec<String>,
    #[serde(default)]
    subcommands: BTreeMap<String, SkimSub>,
}

#[derive(Deserialize)]
struct SkimSub {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    subcommands: BTreeMap<String, SkimSub>,
}

/// Parse a skim-tab YAML spec file. For every entry in `commands`, a
/// parallel branch of the subcommand tree is emitted under that root
/// path — so `kubectl get` and `k get` share the same tree but each
/// is discoverable via its own top-level name.
pub fn parse_skim_yaml(src: &str) -> ForgeResult<ForgeOutput> {
    let spec: SkimSpec =
        serde_yaml_ng::from_str(src).map_err(|e| ForgeError::YamlParse(e.to_string()))?;
    let mut out = ForgeOutput::default();

    // If `commands:` is empty, fall back to `kubectl` → skip. (A yaml
    // without any command name can't be materialized into specs.)
    for root in &spec.commands {
        emit_sub_tree(root, &spec.subcommands, &mut out);
    }

    out.sort();
    Ok(out)
}

fn emit_sub_tree(parent_path: &str, subs: &BTreeMap<String, SkimSub>, out: &mut ForgeOutput) {
    for (name, sub) in subs {
        out.subcmds.push(SubcmdSpec {
            path: parent_path.to_string(),
            name: name.clone(),
            description: sub.description.clone(),
        });
        if !sub.subcommands.is_empty() {
            let child_path = format!("{parent_path}.{name}");
            emit_sub_tree(&child_path, &sub.subcommands, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_respects_quotes() {
        let toks =
            tokenize_shell(r#"complete -c git -d 'version control' -a "status log""#).unwrap();
        assert_eq!(
            toks,
            vec![
                "complete",
                "-c",
                "git",
                "-d",
                "version control",
                "-a",
                "status log"
            ]
        );
    }

    #[test]
    fn parse_simple_flag() {
        let out = parse_fish(r#"complete -c git -s v -l verbose -d "be loud""#).unwrap();
        assert_eq!(out.flags.len(), 2);
        let names: Vec<&str> = out.flags.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"-v"));
        assert!(names.contains(&"--verbose"));
        assert_eq!(out.flags[0].description.as_deref(), Some("be loud"));
    }

    #[test]
    fn parse_subcommand_via_seen_guard() {
        let out = parse_fish(
            r#"
            complete -c git -a 'status log commit' -d 'subcommands'
            complete -c git -n '__fish_seen_subcommand_from commit' -s m -d 'commit message' -r
            "#,
        )
        .unwrap();
        // 3 subcommands from the first line (all at path "git").
        assert_eq!(out.subcmds.len(), 3);
        assert!(out.subcmds.iter().all(|s| s.path == "git"));
        // Short `-m` flag under `git.commit` with takes = string (because `-r`).
        let m = out.flags.iter().find(|f| f.name == "-m").unwrap();
        assert_eq!(m.path, "git.commit");
        assert_eq!(m.takes.as_deref(), Some("string"));
    }

    #[test]
    fn emit_round_trips_subcmds_and_flags() {
        let mut out = ForgeOutput::default();
        out.subcmds.push(SubcmdSpec {
            path: "git".into(),
            name: "commit".into(),
            description: Some("record changes".into()),
        });
        out.flags.push(FlagSpec {
            path: "git.commit".into(),
            name: "-m".into(),
            takes: Some("string".into()),
            description: Some("commit message".into()),
        });
        out.positionals.push(PositSpec {
            path: "git.commit".into(),
            index: 1,
            takes: Some("files".into()),
            description: Some("paths".into()),
        });
        out.sort();
        let emitted = emit_lisp(&out);
        assert!(
            emitted.contains(
                r#"(defsubcmd :path "git" :name "commit" :description "record changes")"#
            )
        );
        assert!(emitted.contains(r#"(defflag :path "git.commit" :name "-m" :takes "string" :description "commit message")"#));
        assert!(emitted.contains(
            r#"(defposit :path "git.commit" :index 1 :takes "files" :description "paths")"#
        ));
    }

    #[test]
    fn lisp_str_escapes_quotes_and_backslashes() {
        assert_eq!(lisp_str(r#"hello "world""#), r#""hello \"world\"""#);
        assert_eq!(lisp_str(r"a\b"), r#""a\\b""#);
    }

    #[test]
    fn seen_subcommand_extracts_name() {
        assert_eq!(
            seen_subcommand("__fish_seen_subcommand_from commit"),
            Some("commit".into())
        );
        assert_eq!(
            seen_subcommand("__fish_git_using_command push"),
            Some("push".into())
        );
        assert_eq!(seen_subcommand("__fish_use_subcommand"), None);
    }

    #[test]
    fn parse_skim_yaml_single_command() {
        let yaml = r#"
commands: [nix]
icon: "❄ "
subcommands:
  build:
    description: "Build a derivation"
  flake:
    description: "Flake operations"
    subcommands:
      update:
        description: "Update flake inputs"
"#;
        let out = parse_skim_yaml(yaml).unwrap();
        // Two top-level + one nested = 3.
        assert_eq!(out.subcmds.len(), 3);
        let build = out.subcmds.iter().find(|s| s.name == "build").unwrap();
        assert_eq!(build.path, "nix");
        assert_eq!(build.description.as_deref(), Some("Build a derivation"));
        let update = out.subcmds.iter().find(|s| s.name == "update").unwrap();
        assert_eq!(update.path, "nix.flake");
    }

    #[test]
    fn parse_skim_yaml_multiple_command_aliases() {
        let yaml = r#"
commands: [kubectl, k]
subcommands:
  get:
    description: "Display resources"
"#;
        let out = parse_skim_yaml(yaml).unwrap();
        // Two aliases × one subcommand = 2 specs.
        assert_eq!(out.subcmds.len(), 2);
        let paths: Vec<&str> = out.subcmds.iter().map(|s| s.path.as_str()).collect();
        assert!(paths.contains(&"kubectl"));
        assert!(paths.contains(&"k"));
    }

    #[test]
    fn parse_zsh_extracts_compdef_command_name() {
        let z = r#"
#compdef git
_git() {
  _arguments
}
"#;
        assert_eq!(find_compdef_commands(z), Some(vec!["git".into()]));
    }

    #[test]
    fn parse_zsh_extracts_multiple_compdef_names() {
        let z = "#compdef git gitk, gh\n_git() {}";
        let names = find_compdef_commands(z).unwrap();
        assert_eq!(
            names,
            vec!["git".to_string(), "gitk".to_string(), "gh".to_string()]
        );
    }

    #[test]
    fn parse_zsh_flag_with_description_and_file_type() {
        let z = r#"
#compdef mytool
_mytool() {
  _arguments -C \
    '-C[change working directory]:directory:_files -/' \
    '--version[show version]' \
    '-h[show help]' \
    '1: :_files'
}
"#;
        let out = parse_zsh_compdef(z).unwrap();
        let c_flag = out.flags.iter().find(|f| f.name == "-C").unwrap();
        assert_eq!(c_flag.takes.as_deref(), Some("dir")); // `-/` → directory only
        assert_eq!(
            c_flag.description.as_deref(),
            Some("change working directory")
        );
        let v = out.flags.iter().find(|f| f.name == "--version").unwrap();
        assert_eq!(v.description.as_deref(), Some("show version"));
        assert_eq!(v.takes, None); // bool flag
        // Positional 1 → file action.
        assert_eq!(out.positionals.len(), 1);
        assert_eq!(out.positionals[0].takes.as_deref(), Some("file"));
    }

    #[test]
    fn parse_zsh_choice_literal_becomes_choice_kind() {
        let z = r#"
#compdef foo
_foo() {
  _arguments \
    '-f[format]:format:(json yaml text)'
}
"#;
        let out = parse_zsh_compdef(z).unwrap();
        let f = out.flags.iter().find(|f| f.name == "-f").unwrap();
        assert!(f.takes.as_deref().unwrap().starts_with("choice:"));
        let t = f.takes.as_deref().unwrap();
        assert!(t.contains("json"));
        assert!(t.contains("yaml"));
        assert!(t.contains("text"));
    }

    #[test]
    fn parse_zsh_group_prefix_is_stripped() {
        let z = r#"
#compdef foo
_foo() {
  _arguments \
    '(--verbose)--quiet[be quiet]'
}
"#;
        let out = parse_zsh_compdef(z).unwrap();
        let q = out.flags.iter().find(|f| f.name == "--quiet").unwrap();
        assert_eq!(q.description.as_deref(), Some("be quiet"));
    }

    #[test]
    fn parse_zsh_without_compdef_returns_empty() {
        let z = "_helper_function() { return 0; }";
        let out = parse_zsh_compdef(z).unwrap();
        assert_eq!(out.flags.len(), 0);
        assert_eq!(out.subcmds.len(), 0);
        assert_eq!(out.positionals.len(), 0);
    }

    #[test]
    fn full_round_trip_fish_to_lisp() {
        let fish = r#"
            complete -c kubectl -a 'get apply delete' -d 'subcommands'
            complete -c kubectl -n '__fish_seen_subcommand_from get' -s A -l all-namespaces -d 'list across ns'
            complete -c kubectl -n '__fish_seen_subcommand_from apply' -s f -l filename -r -d 'yaml file'
        "#;
        let out = parse_fish(fish).unwrap();
        let lisp = emit_lisp(&out);
        assert!(lisp.contains(r#"(defsubcmd :path "kubectl" :name "get""#));
        assert!(lisp.contains(r#"(defsubcmd :path "kubectl" :name "apply""#));
        assert!(lisp.contains(r#"(defflag :path "kubectl.get" :name "-A""#));
        assert!(lisp.contains(r#"(defflag :path "kubectl.get" :name "--all-namespaces""#));
        assert!(lisp.contains(r#"(defflag :path "kubectl.apply" :name "-f""#));
        assert!(lisp.contains(":takes \"string\""));
    }
}
