//! Zsh completion system — compsys bridge and completion widget engine.
//!
//! Today this crate implements the minimum useful set for interactive
//! frost use:
//!
//! * **Command completion** at position 0 of a command: matches against
//!   shell builtins + every executable reachable via `$PATH`.
//! * **Typed per-command argument completion** from rc-authored
//!   `(defsubcmd)` / `(defflag)` / `(defposit)` specs (the rich tree),
//!   plus engine-native positional kinds for path-taking builtins —
//!   `cd` / `pushd` complete directories only, the typed equivalent of
//!   zsh's `_cd` / `_directories` compdef wiring.
//! * **Filename completion** everywhere else: expands the partial word
//!   against the filesystem, honoring `~` expansion.
//!
//! The entry point is [`FrostCompleter`], which implements
//! [`reedline::Completer`] and can be plugged into `ZleEngine` via
//! [`frost_zle::ZleEngine::with_completer`].
//!
//! Not yet covered (tracked upstream):
//!
//! * Consuming zsh-native compsys specs at runtime (`_arguments`,
//!   `compdef`) — forge-side conversion only, see [`parse_zsh_compdef`].
//! * Completion from aliases / functions / named parameters.
//! * Menu-select completion widgets and `zstyle` configuration.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use reedline::{Completer, Span, Suggestion};

mod forge;
mod tree;
pub use forge::{
    ForgeError, ForgeOutput, emit_lisp, parse_fish, parse_skim_yaml, parse_zsh_compdef,
};
pub use tree::{CompletionNode, CompletionTree, FlagNode, PositNode};
// Re-export the Lisp-side spec types so consumers don't need a direct
// frost-lisp dep for the common "wire specs into the completer" path.
pub use frost_lisp::{FlagSpec, PositSpec, SubcmdSpec, ValueKind};

/// The frost completion engine.
///
/// Construction is cheap; `complete` is called on every Tab press so
/// filesystem access stays on-demand and scoped to the directory the user
/// is currently referencing.
pub struct FrostCompleter {
    /// Shell builtins to suggest at command position.
    builtins: Vec<String>,
    /// Per-command argument completions, keyed by the first word of the
    /// current command. Populated from `(defcompletion :command … :args …)`
    /// forms in the user's rc (see `frost-lisp::ApplySummary::completion_map`).
    arg_completions: HashMap<String, Vec<String>>,
    /// Per-command description for the command itself (shown on Tab when
    /// the user is still typing the command name). Populated from
    /// `(defcompletion :description …)`.
    command_descriptions: HashMap<String, String>,
    /// Rich completion tree built from `(defsubcmd …)` / `(defflag …)`
    /// / `(defposit …)` forms. Consulted first; a miss falls through
    /// to the flat `arg_completions` path above.
    tree: CompletionTree,
}

impl FrostCompleter {
    pub fn new(builtins: impl IntoIterator<Item = String>) -> Self {
        Self {
            builtins: builtins.into_iter().collect(),
            arg_completions: HashMap::new(),
            command_descriptions: HashMap::new(),
            tree: CompletionTree::default(),
        }
    }

    /// Install the rich completion tree assembled from `(defsubcmd)`,
    /// `(defflag)`, `(defposit)` specs. Consulted first at argument
    /// position; falls through to flat `arg_completions` on miss.
    pub fn with_completion_tree(mut self, tree: CompletionTree) -> Self {
        self.tree = tree;
        self
    }

    /// Convenience: build + install a tree from raw spec vectors.
    pub fn with_rich_completions(
        self,
        subcmds: &[SubcmdSpec],
        flags: &[FlagSpec],
        positionals: &[PositSpec],
    ) -> Self {
        let tree = CompletionTree::build(subcmds, flags, positionals);
        self.with_completion_tree(tree)
    }

    /// Construct a default completer with a small built-in set — enough
    /// for the completer to be useful even if the caller hasn't plumbed
    /// through the real `BuiltinRegistry`.
    pub fn with_default_builtins() -> Self {
        Self::new(default_builtin_list().iter().map(|s| (*s).to_string()))
    }

    /// Replace the rc-authored per-command argument completion map.
    /// Merged with filesystem suggestions at argument position, so a
    /// command with declared args still allows filename completion for
    /// anything not in the list. Exception: commands with a typed
    /// builtin positional kind (`cd`, `pushd`, `mkdir`, `rmdir`) merge
    /// declared args with *directories only* — plain files are never
    /// offered for those.
    pub fn with_arg_completions(mut self, map: HashMap<String, Vec<String>>) -> Self {
        self.arg_completions = map;
        self
    }

    /// Install per-command descriptions (shown on Tab at command position).
    pub fn with_descriptions(mut self, map: HashMap<String, String>) -> Self {
        self.command_descriptions = map;
        self
    }
}

impl Completer for FrostCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let ctx = current_word(line, pos);
        let span = Span {
            start: ctx.word_start,
            end: pos,
        };

        // Command-name position: list builtins + aliases + functions +
        // PATH executables matching the prefix, with per-command
        // descriptions from `(defcompletion …)`.
        if ctx.is_command_position && !ctx.word.contains('/') {
            let cands = command_candidates(&self.builtins, &ctx.word);
            return cands
                .into_iter()
                .map(|value| Suggestion {
                    description: self.command_descriptions.get(&value).cloned(),
                    span,
                    append_whitespace: true,
                    style: None,
                    extra: None,
                    value,
                })
                .collect();
        }

        // Identify the command the cursor's argument belongs to — the
        // first word of the current *logical command segment*, not of
        // the whole buffer (`cd /tmp && cat <Tab>` completes for
        // `cat`, never `cd`).
        let segment_start = logical_segment_start(line, ctx.word_start);
        let segment = &line[segment_start..ctx.word_start];
        let cmd_name = first_word(segment);

        // Argument position — try the rich tree first for description-
        // bearing candidates. Falls through to flat args + filesystem.
        if let Some(cmd_name) = cmd_name
            && let Some((mut tree_sugs, active_kind)) =
                self.tree_suggestions(segment, &ctx, cmd_name, span)
        {
            // Fold in filesystem completion so path-like arguments stay
            // easy to type after a known subcommand — unless the active
            // spec's value kind already walked the filesystem (file/dir
            // kinds): those candidates were enumerated above with the
            // kind's own filter, so a generic fold would re-add the very
            // entries the kind excludes (files after a dirs-only `dir`
            // kind) and duplicate the rest.
            let kind_owns_fs = active_kind
                .as_ref()
                .is_some_and(ValueKind::completes_from_fs);
            if !kind_owns_fs {
                tree_sugs.extend(
                    filename_candidates(&ctx.word)
                        .into_iter()
                        .map(|value| Suggestion {
                            description: None,
                            append_whitespace: !value.ends_with('/'),
                            style: None,
                            extra: None,
                            span,
                            value,
                        }),
                );
            }
            return tree_sugs;
        }

        // Typed positional kinds for path-taking builtins — `cd` and
        // friends complete directories only, with no rc spec needed.
        // rc-authored flat args (e.g. bookmark names) still merge in;
        // an rc completion tree entry for the command wins outright
        // (consulted above).
        if let Some(cmd_name) = cmd_name
            && let Some(kind) = builtin_positional_kind(cmd_name)
            && !ctx.word.starts_with('-')
        {
            let mut out: Vec<Suggestion> = Vec::new();
            if let Some(args) = self.arg_completions.get(cmd_name) {
                out.extend(args.iter().filter(|a| a.starts_with(&ctx.word)).map(
                    |value| Suggestion {
                        description: None,
                        append_whitespace: !value.ends_with('/'),
                        style: None,
                        extra: None,
                        span,
                        value: value.clone(),
                    },
                ));
            }
            out.extend(value_kind_candidates(&kind, &ctx.word, span));
            return out;
        }

        // Flat-args fallback (legacy `(defcompletion :args …)` path).
        let mut out: Vec<String> = Vec::new();
        if let Some(cmd_name) = cmd_name {
            if let Some(args) = self.arg_completions.get(cmd_name) {
                out.extend(args.iter().filter(|a| a.starts_with(&ctx.word)).cloned());
            }
        }
        out.extend(filename_candidates(&ctx.word));
        out.into_iter()
            .map(|value| Suggestion {
                description: None,
                append_whitespace: !value.ends_with('/'),
                style: None,
                extra: None,
                span,
                value,
            })
            .collect()
    }
}

impl FrostCompleter {
    /// Return rich completions from the tree if the command is
    /// known. Consumes the partial-line context; each suggestion
    /// carries the spec's description so reedline's menu shows it.
    ///
    /// The second tuple element is the [`ValueKind`] that drove the
    /// candidates for the *current word* (a flag's value or a
    /// positional), when one applied — `complete` uses it to decide
    /// whether the generic filesystem fold-in is still warranted.
    ///
    /// `segment` is the current logical command's text up to (but not
    /// including) the current word — see `logical_segment_start`.
    fn tree_suggestions(
        &self,
        segment: &str,
        ctx: &WordContext<'_>,
        cmd_name: &str,
        span: Span,
    ) -> Option<(Vec<Suggestion>, Option<ValueKind>)> {
        if !self.tree.knows(cmd_name) {
            return None;
        }

        // Everything in the segment past the command name is a
        // potential subcommand token.
        let mut path_parts: Vec<&str> = segment.split_whitespace().collect();
        // Drop the command name itself from the walk — the tree's
        // top-level lookup is keyed by it.
        let _ = path_parts.drain(..1);

        // Walk the tree, consuming subcommand tokens from the left.
        // Stop at the first token that doesn't match a known
        // subcommand; remaining tokens are either flags or positionals
        // we don't descend into.
        let mut current = self.tree.walk(cmd_name)?;
        let mut positional_index: u32 = 1;
        let mut last_flag_takes_value = false;
        let mut last_flag_kind: Option<ValueKind> = None;

        for token in &path_parts {
            // Starts with `-` → flag token; remember if it takes a value.
            if token.starts_with('-') {
                if let Some(flag) = current.flags.get(*token) {
                    last_flag_takes_value = flag.takes.is_some();
                    last_flag_kind = flag.takes.clone();
                } else {
                    last_flag_takes_value = false;
                    last_flag_kind = None;
                }
                continue;
            }
            // If the previous token was a flag that takes a value,
            // this token is that value — consumed, not a subcommand.
            if last_flag_takes_value {
                last_flag_takes_value = false;
                last_flag_kind = None;
                continue;
            }
            // Subcommand token — descend if known.
            if let Some((_, child)) = current.subcommands.get(*token) {
                current = child;
                positional_index = 1; // reset positional counter on subcommand descent
            } else {
                // Unknown token — treat as a positional; advance index.
                positional_index += 1;
            }
        }

        let mut out: Vec<Suggestion> = Vec::new();

        // Case A — the previous token was a flag taking a value: offer
        // that value kind's candidates.
        if last_flag_takes_value {
            if let Some(kind) = last_flag_kind {
                out.extend(value_kind_candidates(&kind, &ctx.word, span));
                return Some((out, Some(kind)));
            }
            return Some((out, None));
        }

        // Case B — the current word starts with `-`: offer flags.
        if ctx.word.starts_with('-') {
            for (name, flag) in &current.flags {
                if name.starts_with(&ctx.word) {
                    out.push(Suggestion {
                        value: name.clone(),
                        description: flag.description.clone(),
                        style: None,
                        extra: None,
                        span,
                        append_whitespace: flag.takes.is_none(),
                    });
                }
            }
            return Some((out, None));
        }

        // Case C — at a subcommand / positional position. Offer
        // subcommands first (with descriptions), then positionals at
        // the current index.
        for (name, (desc, _)) in &current.subcommands {
            if name.starts_with(&ctx.word) {
                out.push(Suggestion {
                    value: name.clone(),
                    description: desc.clone(),
                    style: None,
                    extra: None,
                    span,
                    append_whitespace: true,
                });
            }
        }
        let mut active_kind = None;
        if let Some(posit) = current.positionals.get(&positional_index) {
            out.extend(value_kind_candidates(&posit.takes, &ctx.word, span));
            active_kind = Some(posit.takes.clone());
        }

        Some((out, active_kind))
    }
}

/// Enumerate candidates for a [`ValueKind`] filtered by `prefix`.
/// File / dir kinds walk the filesystem; choice kinds enumerate the
/// fixed set; string/integer return empty (no way to enumerate).
fn value_kind_candidates(kind: &ValueKind, prefix: &str, span: Span) -> Vec<Suggestion> {
    match kind {
        ValueKind::Choice(choices) => choices
            .iter()
            .filter(|c| c.starts_with(prefix))
            .cloned()
            .map(|value| Suggestion {
                description: None,
                append_whitespace: true,
                style: None,
                extra: None,
                span,
                value,
            })
            .collect(),
        // Filesystem kinds walk the directory under the prefix. File
        // kinds include directories too — descending into a
        // subdirectory is part of typing a file path; dir kinds keep
        // directories only (`cd`'s contract).
        k if k.completes_from_fs() => filename_candidates(prefix)
            .into_iter()
            .filter(|c| !k.directories_only() || c.ends_with('/'))
            .map(|value| Suggestion {
                description: None,
                append_whitespace: !value.ends_with('/'),
                style: None,
                extra: None,
                span,
                value,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Positional [`ValueKind`]s frost knows natively for path-taking
/// builtins — the typed equivalent of zsh's `_cd` / `_directories`
/// compdef wiring, shipped in the engine so `cd <Tab>` is dirs-only
/// out of the box. An rc-authored completion tree entry for the same
/// command overrides this (the tree is consulted first in `complete`).
fn builtin_positional_kind(cmd: &str) -> Option<ValueKind> {
    match cmd {
        "cd" | "pushd" => Some(ValueKind::Dir),
        "mkdir" | "rmdir" => Some(ValueKind::Dirs),
        _ => None,
    }
}

/// Very small set of commonly-used builtins. `frost-complete` does not
/// depend on `frost-builtins` (to avoid a circular dep chain), so the
/// caller should normally construct `FrostCompleter::new(real_builtins)`
/// with the full registry.
pub fn default_builtin_list() -> &'static [&'static str] {
    &[
        "alias",
        "bg",
        "bindkey",
        "break",
        "builtin",
        "case",
        "cd",
        "command",
        "continue",
        "declare",
        "dirs",
        "disable",
        "do",
        "done",
        "echo",
        "elif",
        "else",
        "enable",
        "esac",
        "eval",
        "exec",
        "exit",
        "export",
        "false",
        "fc",
        "fg",
        "fi",
        "for",
        "function",
        "getopts",
        "hash",
        "help",
        "history",
        "if",
        "in",
        "integer",
        "jobs",
        "kill",
        "let",
        "local",
        "popd",
        "printf",
        "pushd",
        "pwd",
        "read",
        "readonly",
        "return",
        "select",
        "set",
        "setopt",
        "shift",
        "source",
        "suspend",
        "test",
        "then",
        "time",
        "times",
        "trap",
        "true",
        "type",
        "typeset",
        "ulimit",
        "umask",
        "unalias",
        "unfunction",
        "unhash",
        "unset",
        "unsetopt",
        "until",
        "wait",
        "whence",
        "which",
        "while",
        "zmodload",
        "zstyle",
    ]
}

/// Per-call context derived from the raw readline buffer.
#[derive(Debug, PartialEq, Eq)]
struct WordContext<'a> {
    /// The text of the partial word under the cursor.
    word: String,
    /// Byte offset where the partial word starts in `line`.
    word_start: usize,
    /// True iff the partial word is at command position (first word of
    /// the current command — i.e. nothing but whitespace precedes it on
    /// the "logical line" after the last `;`, `|`, `&`, `&&`, or `||`).
    is_command_position: bool,
    _phantom: std::marker::PhantomData<&'a ()>,
}

/// First word of `line` (everything up to the first whitespace),
/// or None if the line is empty. Used to identify which command we're
/// completing arguments for — crude but matches zsh's default.
fn first_word(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let end = trimmed
        .find(|c: char| c.is_whitespace())
        .unwrap_or(trimmed.len());
    if end == 0 {
        None
    } else {
        Some(&trimmed[..end])
    }
}

/// Byte offset where the *current logical command* begins — the
/// position just past the last unquoted `;`, `|`, `&`, newline, `(`
/// or `{` before `before`. Mirrors the separator set `current_word`
/// uses for command-position detection, with the same quote-state
/// tracking, so `cd /tmp && cat <Tab>` attributes the argument to
/// `cat`, not to the buffer's first word.
fn logical_segment_start(line: &str, before: usize) -> usize {
    let bytes = line.as_bytes();
    let end = before.min(bytes.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut seg = 0;
    let mut i = 0;
    while i < end {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
        } else if in_double {
            if b == b'"' {
                in_double = false;
            }
        } else {
            match b {
                b'\'' => in_single = true,
                b'"' => in_double = true,
                b';' | b'|' | b'&' | b'\n' | b'(' | b'{' => seg = i + 1,
                _ => {}
            }
        }
        i += 1;
    }
    seg
}

fn current_word(line: &str, pos: usize) -> WordContext<'_> {
    // Find the start of the current word by scanning from BOL to the cursor
    // while tracking quote state. Word breaks (whitespace + `|;&<>()`) only
    // split OUTSIDE quotes; an opening quote starts the word *after* the
    // quote char — so completing inside `cat "my dir/<TAB>` extracts
    // `my dir/` (the space inside the quote does not split the word, and the
    // quote char itself is excluded from the partial). zsh/readline behave
    // the same. (Backslash-escaped breaks are not yet handled.)
    let bytes = line.as_bytes();
    let end = pos.min(bytes.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut start = 0;
    let mut i = 0;
    while i < end {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
        } else if in_double {
            if b == b'"' {
                in_double = false;
            }
        } else {
            match b {
                b'\'' => {
                    in_single = true;
                    start = i + 1;
                }
                b'"' => {
                    in_double = true;
                    start = i + 1;
                }
                b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b'<' | b'>' | b'(' | b')' => {
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }

    // Command position: scan backwards from the word start, skipping
    // whitespace. If we hit BOL or a command separator before any other
    // character, this word is in command position. (An opening quote
    // immediately before `start` is not a separator, so completing inside a
    // quote is never command position — correct.)
    let mut j = start;
    while j > 0 && matches!(bytes[j - 1], b' ' | b'\t') {
        j -= 1;
    }
    let is_command_position =
        j == 0 || matches!(bytes[j - 1], b';' | b'|' | b'&' | b'\n' | b'(' | b'{');

    WordContext {
        word: line[start..end].to_string(),
        word_start: start,
        is_command_position,
        _phantom: std::marker::PhantomData,
    }
}

fn command_candidates(builtins: &[String], partial: &str) -> Vec<String> {
    let mut out: BTreeSet<String> = builtins
        .iter()
        .filter(|b| b.starts_with(partial))
        .cloned()
        .collect();

    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':').filter(|p| !p.is_empty()) {
            let d = Path::new(dir);
            let Ok(entries) = std::fs::read_dir(d) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if !name_str.starts_with(partial) {
                    continue;
                }
                // Executable-bit check — cheap best-effort; if the
                // filesystem won't tell us, include it anyway.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = entry.metadata() {
                        if !meta.is_file() {
                            continue;
                        }
                        if meta.permissions().mode() & 0o111 == 0 {
                            continue;
                        }
                    }
                }
                out.insert(name_str.into_owned());
            }
        }
    }

    out.into_iter().collect()
}

fn filename_candidates(partial: &str) -> Vec<String> {
    let (dir_part, file_prefix) = split_dir_and_prefix(partial);
    let expanded_dir = expand_tilde(&dir_part);

    let dir_path: PathBuf = if expanded_dir.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(&expanded_dir)
    };

    let mut out: BTreeSet<String> = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(&dir_path) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Hide dotfiles unless the user typed a leading `.`.
        if name_str.starts_with('.') && !file_prefix.starts_with('.') {
            continue;
        }
        if !name_str.starts_with(&file_prefix) {
            continue;
        }

        let mut rendered = String::new();
        // Preserve the directory prefix the user typed (including any `~`
        // — we don't replace that back after tilde expansion; reedline
        // will substitute `value` for the span, so the final buffer
        // contains the typed `~/...`).
        if !dir_part.is_empty() {
            rendered.push_str(&dir_part);
            if !dir_part.ends_with('/') {
                rendered.push('/');
            }
        }
        rendered.push_str(&name_str);

        // Append `/` for directories so the user can keep completing.
        // `DirEntry::file_type()` does NOT follow symlinks — resolve
        // through `metadata()` so a symlinked directory (e.g. `/tmp`
        // on macOS) still reads as a dir and survives dirs-only
        // filters like `cd`'s.
        let is_dir = entry.file_type().is_ok_and(|t| {
            t.is_dir()
                || (t.is_symlink()
                    && std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()))
        });
        if is_dir {
            rendered.push('/');
        }
        out.insert(rendered);
    }
    out.into_iter().collect()
}

fn split_dir_and_prefix(partial: &str) -> (String, String) {
    match partial.rfind('/') {
        Some(idx) => (partial[..=idx].to_string(), partial[idx + 1..].to_string()),
        None => (String::new(), partial.to_string()),
    }
}

fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    } else if s == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
    }
    s.to_string()
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_at_end_of_line() {
        let ctx = current_word("echo hell", 9);
        assert_eq!(ctx.word, "hell");
        assert_eq!(ctx.word_start, 5);
        assert!(!ctx.is_command_position);
    }

    #[test]
    fn word_inside_double_quotes_keeps_spaces() {
        // Completing inside `cat "my dir/<TAB>` — the space does not split
        // the word, and the opening quote is excluded from the partial.
        let ctx = current_word("cat \"my dir/", 12);
        assert_eq!(ctx.word, "my dir/");
        assert_eq!(ctx.word_start, 5);
        assert!(!ctx.is_command_position);
    }

    #[test]
    fn word_inside_single_quotes_keeps_spaces() {
        let ctx = current_word("ls 'a b/c", 9);
        assert_eq!(ctx.word, "a b/c");
        assert_eq!(ctx.word_start, 4);
    }

    #[test]
    fn unquoted_space_still_breaks_the_word() {
        let ctx = current_word("cat my dir", 10);
        assert_eq!(ctx.word, "dir");
        assert_eq!(ctx.word_start, 7);
    }

    #[test]
    fn closed_quote_then_space_breaks_again() {
        // After a closed quote + space, a new word starts normally.
        let ctx = current_word("cp \"a b\" c", 10);
        assert_eq!(ctx.word, "c");
    }

    #[test]
    fn first_word_is_command_position() {
        let ctx = current_word("ech", 3);
        assert_eq!(ctx.word, "ech");
        assert!(ctx.is_command_position);
    }

    #[test]
    fn word_after_pipe_is_command_position() {
        let ctx = current_word("ls | gr", 7);
        assert_eq!(ctx.word, "gr");
        assert!(ctx.is_command_position);
    }

    #[test]
    fn split_dir_and_prefix_basic() {
        assert_eq!(
            split_dir_and_prefix("src/li"),
            ("src/".to_string(), "li".to_string())
        );
        assert_eq!(
            split_dir_and_prefix("file"),
            (String::new(), "file".to_string())
        );
        assert_eq!(
            split_dir_and_prefix("/etc/"),
            ("/etc/".to_string(), String::new())
        );
    }

    #[test]
    fn filename_completion_lists_matching_entries() {
        let tmp = std::env::temp_dir().join(format!("frost-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("alpha.txt"), "").unwrap();
        std::fs::write(tmp.join("alpaca.md"), "").unwrap();
        std::fs::write(tmp.join("beta.txt"), "").unwrap();
        std::fs::create_dir_all(tmp.join("alpha-dir")).unwrap();

        let partial = format!("{}/alp", tmp.display());
        let matches = filename_candidates(&partial);
        // Order is deterministic (BTreeSet).
        assert_eq!(matches.len(), 3);
        assert!(matches[0].ends_with("alpaca.md"));
        assert!(matches[1].ends_with("alpha-dir/"));
        assert!(matches[2].ends_with("alpha.txt"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn filename_completion_hides_dotfiles_unless_typed() {
        let tmp = std::env::temp_dir().join(format!("frost-complete-dot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(".hidden"), "").unwrap();
        std::fs::write(tmp.join("visible"), "").unwrap();

        let no_dot = filename_candidates(&format!("{}/", tmp.display()));
        assert_eq!(no_dot.len(), 1);
        assert!(no_dot[0].ends_with("visible"));

        let with_dot = filename_candidates(&format!("{}/.", tmp.display()));
        assert_eq!(with_dot.len(), 1);
        assert!(with_dot[0].ends_with(".hidden"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn command_candidates_filters_builtins_by_prefix() {
        let builtins = vec!["cd".to_string(), "echo".to_string(), "exit".to_string()];
        let out = command_candidates(&builtins, "ex");
        // May include any `ex…` executables from PATH — at minimum we
        // must see the matching builtins.
        assert!(out.contains(&"exit".to_string()));
        assert!(!out.contains(&"echo".to_string()));
    }

    /// Scratch dir with one file (`alpha.txt`) and one subdir (`sub/`).
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!("frost-complete-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("alpha.txt"), "").unwrap();
        tmp
    }

    #[test]
    fn cd_completes_directories_only() {
        let tmp = scratch_dir("cd");
        let mut completer = FrostCompleter::with_default_builtins();
        let line = format!("cd {}/", tmp.display());
        let out = completer.complete(&line, line.len());
        assert!(
            out.iter().any(|s| s.value.ends_with("sub/")),
            "cd must offer the subdirectory: {out:?}"
        );
        assert!(
            out.iter().all(|s| s.value.ends_with('/')),
            "cd must offer directories only, got: {out:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn cd_merges_rc_flat_args_with_directories() {
        let tmp = scratch_dir("cd-args");
        let mut map = HashMap::new();
        map.insert("cd".to_string(), vec!["sub".to_string()]);
        let mut completer = FrostCompleter::with_default_builtins().with_arg_completions(map);
        let line = format!("cd {}/", tmp.display());
        let out = completer.complete(&line, line.len());
        // The flat arg doesn't match the typed path prefix, but the
        // dirs-only filesystem walk still applies.
        assert!(out.iter().all(|s| s.value.ends_with('/')));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn cd_offers_matching_rc_flat_args_alongside_directories() {
        let mut map = HashMap::new();
        map.insert(
            "cd".to_string(),
            vec!["bookmark-work".to_string(), "bookmark-play/".to_string()],
        );
        let mut completer = FrostCompleter::with_default_builtins().with_arg_completions(map);
        let out = completer.complete("cd bookmark-", 12);
        let work = out
            .iter()
            .find(|s| s.value == "bookmark-work")
            .expect("rc flat arg must be offered for cd");
        assert!(work.append_whitespace);
        // Slash-terminated rc args keep the path open for descent —
        // same rule as every filesystem suggestion in this crate.
        let play = out
            .iter()
            .find(|s| s.value == "bookmark-play/")
            .expect("slash-terminated rc flat arg must be offered for cd");
        assert!(!play.append_whitespace);
    }

    #[cfg(unix)]
    #[test]
    fn cd_offers_symlinked_directories() {
        // `DirEntry::file_type()` does not follow symlinks; the dirs-
        // only filter must still see a symlink-to-dir as a directory
        // (on macOS `/tmp` itself is one — `cd /tm<Tab>` must work).
        let tmp = scratch_dir("cd-symlink");
        std::os::unix::fs::symlink(tmp.join("sub"), tmp.join("link")).unwrap();
        let mut completer = FrostCompleter::with_default_builtins();
        let line = format!("cd {}/li", tmp.display());
        let out = completer.complete(&line, line.len());
        assert!(
            out.iter().any(|s| s.value.ends_with("link/")),
            "symlinked dir must complete for cd: {out:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn second_command_after_cd_still_completes_files() {
        // `cd /tmp && cat <Tab>` belongs to `cat` — the dirs-only
        // builtin kind of the buffer's FIRST word must not leak into
        // later logical commands.
        let tmp = scratch_dir("cd-multi");
        let mut completer = FrostCompleter::with_default_builtins();
        for joiner in ["&&", ";", "|"] {
            let line = format!("cd /tmp {joiner} cat {}/", tmp.display());
            let out = completer.complete(&line, line.len());
            assert!(
                out.iter().any(|s| s.value.ends_with("alpha.txt")),
                "files must complete for the second command (joiner {joiner:?}): {out:?}"
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn logical_segment_start_tracks_separators_and_quotes() {
        assert_eq!(logical_segment_start("cat file", 8), 0);
        assert_eq!(logical_segment_start("cd /tmp && cat f", 16), 10);
        assert_eq!(logical_segment_start("ls; cd s", 8), 3);
        // Separators inside quotes don't start a new segment.
        assert_eq!(logical_segment_start("echo 'a;b' c", 12), 0);
        assert_eq!(logical_segment_start("echo \"a|b\" c", 12), 0);
    }

    #[test]
    fn builtin_positional_kinds_cover_path_taking_builtins() {
        assert_eq!(builtin_positional_kind("cd"), Some(ValueKind::Dir));
        assert_eq!(builtin_positional_kind("pushd"), Some(ValueKind::Dir));
        assert_eq!(builtin_positional_kind("mkdir"), Some(ValueKind::Dirs));
        assert_eq!(builtin_positional_kind("rmdir"), Some(ValueKind::Dirs));
        assert_eq!(builtin_positional_kind("echo"), None);
    }

    #[test]
    fn tree_dir_positional_does_not_fold_in_files() {
        let tmp = scratch_dir("tree-dir");
        let mut completer = FrostCompleter::with_default_builtins().with_rich_completions(
            &[],
            &[],
            &[frost_lisp::PositSpec {
                path: "go".into(),
                index: 1,
                takes: Some("dir".into()),
                description: None,
            }],
        );
        let line = format!("go {}/", tmp.display());
        let out = completer.complete(&line, line.len());
        assert!(
            out.iter().any(|s| s.value.ends_with("sub/")),
            "dir positional must offer the subdirectory: {out:?}"
        );
        assert!(
            out.iter().all(|s| s.value.ends_with('/')),
            "dir positional must not fold plain files back in: {out:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn file_kind_offers_directories_for_descent() {
        let tmp = scratch_dir("file-descent");
        let span = Span { start: 0, end: 0 };
        let prefix = format!("{}/", tmp.display());
        let out = value_kind_candidates(&ValueKind::File, &prefix, span);
        let values: Vec<&str> = out.iter().map(|s| s.value.as_str()).collect();
        assert!(values.iter().any(|v| v.ends_with("alpha.txt")));
        assert!(
            values.iter().any(|v| v.ends_with("sub/")),
            "file kind must include directories so the user can descend: {values:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}
