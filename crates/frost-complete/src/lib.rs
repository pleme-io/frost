//! Zsh completion system — compsys bridge and completion widget engine.
//!
//! Today this crate implements the minimum useful set for interactive
//! frost use:
//!
//! * **Command completion** at position 0 of a command: matches against
//!   shell builtins + every executable reachable via `$PATH`.
//! * **Filename completion** everywhere else: expands the partial word
//!   against the filesystem, honoring `~` expansion.
//!
//! The entry point is [`FrostCompleter`], which implements
//! [`reedline::Completer`] and can be plugged into `ZleEngine` via
//! [`frost_zle::ZleEngine::with_completer`].
//!
//! Not yet covered (tracked upstream):
//!
//! * Per-command argument completion (compsys `_arguments` specs, zsh
//!   `compdef` definitions).
//! * Completion from aliases / functions / named parameters.
//! * Menu-select completion widgets and `zstyle` configuration.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use reedline::{Completer, Span, Suggestion};

mod tree;
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
        Self::new(
            default_builtin_list()
                .iter()
                .map(|s| (*s).to_string()),
        )
    }

    /// Replace the rc-authored per-command argument completion map.
    /// Merged with filesystem suggestions at argument position, so a
    /// command with declared args still allows filename completion for
    /// anything not in the list.
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
        let span = Span { start: ctx.word_start, end: pos };

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

        // Argument position — try the rich tree first for description-
        // bearing candidates. Falls through to flat args + filesystem.
        if let Some(cmd_name) = first_word(line)
            && let Some(mut tree_sugs) = self.tree_suggestions(line, &ctx, cmd_name, span)
        {
            // Still fold in filesystem completion so path-like arguments
            // remain easy to type even after a known subcommand.
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
            return tree_sugs;
        }

        // Flat-args fallback (legacy `(defcompletion :args …)` path).
        let mut out: Vec<String> = Vec::new();
        if let Some(cmd_name) = first_word(line) {
            if let Some(args) = self.arg_completions.get(cmd_name) {
                out.extend(
                    args.iter()
                        .filter(|a| a.starts_with(&ctx.word))
                        .cloned(),
                );
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
    fn tree_suggestions(
        &self,
        line: &str,
        ctx: &WordContext<'_>,
        cmd_name: &str,
        span: Span,
    ) -> Option<Vec<Suggestion>> {
        if !self.tree.knows(cmd_name) {
            return None;
        }

        // Split the line up to (but not including) the current word.
        // Everything before `word_start` that's past the command name
        // is a potential subcommand token.
        let prefix_line = &line[..ctx.word_start];
        let mut path_parts: Vec<&str> = prefix_line.split_whitespace().collect();
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
            }
            return Some(out);
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
            return Some(out);
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
        if let Some(posit) = current.positionals.get(&positional_index) {
            out.extend(value_kind_candidates(&posit.takes, &ctx.word, span));
        }

        Some(out)
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
        ValueKind::File | ValueKind::Files => filename_candidates(prefix)
            .into_iter()
            .filter(|c| !c.ends_with('/') || kind.directories_only())
            .map(|value| Suggestion {
                description: None,
                append_whitespace: !value.ends_with('/'),
                style: None,
                extra: None,
                span,
                value,
            })
            .collect(),
        ValueKind::Dir | ValueKind::Dirs => filename_candidates(prefix)
            .into_iter()
            .filter(|c| c.ends_with('/'))
            .map(|value| Suggestion {
                description: None,
                append_whitespace: false,
                style: None,
                extra: None,
                span,
                value,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Very small set of commonly-used builtins. `frost-complete` does not
/// depend on `frost-builtins` (to avoid a circular dep chain), so the
/// caller should normally construct `FrostCompleter::new(real_builtins)`
/// with the full registry.
pub fn default_builtin_list() -> &'static [&'static str] {
    &[
        "alias", "bg", "bindkey", "break", "builtin", "case", "cd", "command",
        "continue", "declare", "dirs", "disable", "do", "done", "echo", "elif",
        "else", "enable", "esac", "eval", "exec", "exit", "export", "false",
        "fc", "fg", "fi", "for", "function", "getopts", "hash", "help",
        "history", "if", "in", "integer", "jobs", "kill", "let", "local",
        "popd", "printf", "pushd", "pwd", "read", "readonly", "return",
        "select", "set", "setopt", "shift", "source", "suspend", "test",
        "then", "time", "times", "trap", "true", "type", "typeset", "ulimit",
        "umask", "unalias", "unfunction", "unhash", "unset", "unsetopt",
        "until", "wait", "whence", "which", "while", "zmodload", "zstyle",
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
    let end = trimmed.find(|c: char| c.is_whitespace()).unwrap_or(trimmed.len());
    if end == 0 { None } else { Some(&trimmed[..end]) }
}

fn current_word(line: &str, pos: usize) -> WordContext<'_> {
    // Find the start of the current word: walk backwards until whitespace
    // or a shell word break. Keep it simple — treat `|;&<>()` and whitespace
    // as breaks. Doesn't honor quotes yet; close enough for a first pass.
    let bytes = line.as_bytes();
    let end = pos.min(bytes.len());
    let mut start = end;
    while start > 0 {
        let b = bytes[start - 1];
        if matches!(b, b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b'<' | b'>' | b'(' | b')') {
            break;
        }
        start -= 1;
    }

    // Command position: scan backwards from word_start, skipping whitespace.
    // If we hit BOL or a command separator (`;` `|` `&` `&&` `||`) before
    // any non-separator character, we are at command position.
    let mut i = start;
    while i > 0 {
        let b = bytes[i - 1];
        if matches!(b, b' ' | b'\t') {
            i -= 1;
            continue;
        }
        break;
    }
    let is_command_position = i == 0
        || matches!(bytes[i - 1], b';' | b'|' | b'&' | b'\n' | b'(' | b'{');

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
            let Ok(entries) = std::fs::read_dir(d) else { continue };
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
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
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
        assert_eq!(split_dir_and_prefix("src/li"), ("src/".to_string(), "li".to_string()));
        assert_eq!(split_dir_and_prefix("file"), (String::new(), "file".to_string()));
        assert_eq!(split_dir_and_prefix("/etc/"), ("/etc/".to_string(), String::new()));
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
}
