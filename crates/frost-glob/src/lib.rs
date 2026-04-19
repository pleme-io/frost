//! Zsh-compatible globbing with extended globs and qualifiers.
//!
//! Scope of this crate:
//!
//! * `match_pattern` — test a single filename against a glob pattern (no path
//!   separators). Handles `*`, `?`, `[abc]`, `[!abc]`, `[a-z]`, and escapes.
//! * `expand_pattern` — walk the filesystem, expanding `pattern` relative to
//!   `cwd`. Supports path-separated patterns (e.g. `src/**/*.rs`) and the
//!   recursive `**` segment.
//!
//! Behavior intentionally mirrors zsh's default rules:
//!
//! * A leading `.` in a filename is **not** matched by `*`, `?`, or `[...]`
//!   unless the pattern segment itself starts with `.` or [`GlobOptions::dot_glob`]
//!   is true (zsh's `GLOB_DOTS` option).
//! * `/` is a hard separator — never matched by `*` or `?`.
//! * `**` matches any number of directory components (including zero).
//! * When no entry matches, `expand_pattern` returns an empty `Vec` and lets
//!   the caller decide the policy (zsh NOMATCH / NULL_GLOB / default behavior
//!   depends on options).
//!
//! Extended globs (`+(…)`, `*(…)`, `@(…)`, `?(…)`, `!(…)`) and zsh path
//! qualifiers (`*(.)`, `*(N)`, …) are not yet implemented; callers should
//! fall back to the raw pattern when these are present.

use std::path::{Path, PathBuf};

/// Options that change glob semantics.
#[derive(Debug, Clone, Copy, Default)]
pub struct GlobOptions {
    /// Match filenames beginning with `.` without requiring the pattern to
    /// start with `.`. Corresponds to zsh `GLOB_DOTS`.
    pub dot_glob: bool,
    /// Match case-insensitively (zsh `NO_CASE_GLOB` / `globsubst`).
    pub case_insensitive: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum GlobError {
    #[error("invalid character class in pattern: {0}")]
    InvalidClass(String),
    #[error("io error walking filesystem: {0}")]
    Io(#[from] std::io::Error),
}

/// Returns true if `name` matches `pattern`. `name` must not contain `/`.
///
/// Quoted/escaped metacharacters in `pattern` are handled: `\*` matches a
/// literal `*`, and so on.
pub fn match_pattern(pattern: &str, name: &str, opts: &GlobOptions) -> bool {
    // Respect the leading-dot rule: the first char of the filename can only
    // match `.` if the pattern explicitly starts with `.` (or dot_glob is on).
    if !opts.dot_glob && name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    match_inner(pattern, name, opts.case_insensitive)
}

/// Expand a glob pattern relative to `cwd`. Returns a sorted list of
/// matching paths. Paths are returned *relative to `cwd`* if `pattern` is
/// relative, or absolute if `pattern` is absolute.
///
/// Components of `pattern` are matched per-segment. `**` consumes any number
/// of intermediate directories.
pub fn expand_pattern(pattern: &str, cwd: &Path, opts: &GlobOptions) -> Result<Vec<PathBuf>, GlobError> {
    let absolute = pattern.starts_with('/');
    let (root, rest) = split_root(pattern, cwd);
    let segments: Vec<&str> = split_segments(rest);
    if segments.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    walk(&root, &segments, 0, opts, &mut out, &root, absolute)?;
    out.sort();
    out.dedup();
    Ok(out)
}

// ─── internals ───────────────────────────────────────────────────────────

/// Split `pattern` into (walk_root, remaining_pattern).
/// For an absolute pattern (`/foo/*`), the root is `/`; for a relative one,
/// the root is `cwd`. The returned `remaining_pattern` never has a leading `/`.
fn split_root<'a>(pattern: &'a str, cwd: &Path) -> (PathBuf, &'a str) {
    if let Some(rest) = pattern.strip_prefix('/') {
        (PathBuf::from("/"), rest)
    } else {
        (cwd.to_path_buf(), pattern)
    }
}

fn split_segments(rest: &str) -> Vec<&str> {
    rest.split('/').filter(|s| !s.is_empty()).collect()
}

/// Recursive depth-first walk.
///
/// `dir` is the current filesystem directory we're matching inside.
/// `segments[idx..]` is the remaining pattern.
/// `walk_root` is the original root so we can produce paths relative to it.
fn walk(
    dir: &Path,
    segments: &[&str],
    idx: usize,
    opts: &GlobOptions,
    out: &mut Vec<PathBuf>,
    walk_root: &Path,
    absolute: bool,
) -> Result<(), GlobError> {
    if idx >= segments.len() {
        return Ok(());
    }
    let seg = segments[idx];
    let is_last = idx + 1 == segments.len();

    if seg == "**" {
        // `**` matches zero or more directories. Record "zero case" by
        // skipping the `**` segment and continuing at the same dir.
        walk(dir, segments, idx + 1, opts, out, walk_root, absolute)?;
        // Then descend into every subdirectory and re-try `**` there.
        for entry in read_sorted_dir(dir)? {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !opts.dot_glob && name_str.starts_with('.') {
                continue;
            }
            let child = dir.join(&name);
            if child.is_dir() {
                walk(&child, segments, idx, opts, out, walk_root, absolute)?;
            }
        }
        return Ok(());
    }

    for entry in read_sorted_dir(dir)? {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !match_pattern(seg, &name_str, opts) {
            continue;
        }
        let child = dir.join(&name);
        if is_last {
            out.push(rel_to(&child, walk_root, absolute));
        } else if child.is_dir() {
            walk(&child, segments, idx + 1, opts, out, walk_root, absolute)?;
        }
    }
    Ok(())
}

fn read_sorted_dir(dir: &Path) -> Result<Vec<std::fs::DirEntry>, GlobError> {
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(it) => it.filter_map(|e| e.ok()).collect(),
        Err(_) => return Ok(Vec::new()), // non-existent or unreadable → no matches
    };
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

fn rel_to(path: &Path, root: &Path, absolute: bool) -> PathBuf {
    if absolute {
        path.to_path_buf()
    } else {
        path.strip_prefix(root).unwrap_or(path).to_path_buf()
    }
}

/// Core pattern matcher. `name` is a single path component (no `/`).
fn match_inner(pattern: &str, name: &str, case_insensitive: bool) -> bool {
    // Iterative NFA-ish matcher with backtracking on `*`. Avoids regex
    // compilation for the common case.
    let p = pattern.as_bytes();
    let n = name.as_bytes();
    let case_cmp = |a: u8, b: u8| -> bool {
        if case_insensitive { a.eq_ignore_ascii_case(&b) } else { a == b }
    };

    // Indexes and the last backtrack point if we hit a `*`.
    let mut pi = 0usize;
    let mut ni = 0usize;
    let mut star_p: Option<usize> = None;
    let mut star_n: usize = 0;

    while ni < n.len() {
        let pc = p.get(pi).copied();
        match pc {
            Some(b'*') => {
                star_p = Some(pi);
                star_n = ni;
                pi += 1;
            }
            Some(b'?') => {
                pi += 1;
                ni += 1;
            }
            Some(b'[') => {
                match match_class(&p[pi..], n[ni], case_insensitive) {
                    Some(consumed) => {
                        pi += consumed;
                        ni += 1;
                    }
                    None => {
                        // Class failed — try to backtrack to the last `*`.
                        if let Some(sp) = star_p {
                            star_n += 1;
                            pi = sp + 1;
                            ni = star_n;
                        } else {
                            return false;
                        }
                    }
                }
            }
            Some(b'\\') => {
                // Escaped literal
                if pi + 1 >= p.len() { return false; }
                if case_cmp(p[pi + 1], n[ni]) {
                    pi += 2;
                    ni += 1;
                } else if let Some(sp) = star_p {
                    star_n += 1;
                    pi = sp + 1;
                    ni = star_n;
                } else {
                    return false;
                }
            }
            Some(c) => {
                if case_cmp(c, n[ni]) {
                    pi += 1;
                    ni += 1;
                } else if let Some(sp) = star_p {
                    star_n += 1;
                    pi = sp + 1;
                    ni = star_n;
                } else {
                    return false;
                }
            }
            None => {
                if let Some(sp) = star_p {
                    star_n += 1;
                    pi = sp + 1;
                    ni = star_n;
                } else {
                    return false;
                }
            }
        }
    }
    // Trailing `*`s in pattern must also match.
    while p.get(pi) == Some(&b'*') { pi += 1; }
    pi == p.len()
}

/// Match a `[...]` character class starting at `p[0] == '['`. Returns the
/// number of pattern bytes consumed if `c` is in the class, else None.
fn match_class(p: &[u8], c: u8, case_insensitive: bool) -> Option<usize> {
    // `p[0]` is '['
    let mut i = 1usize;
    let negate = p.get(i) == Some(&b'!') || p.get(i) == Some(&b'^');
    if negate { i += 1; }
    let mut matched = false;
    let mut first = true;
    while i < p.len() {
        let b = p[i];
        // Allow ']' as first class char.
        if b == b']' && !first {
            return if matched ^ negate { Some(i + 1) } else { None };
        }
        // Range a-z
        if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
            let lo = p[i];
            let hi = p[i + 2];
            let (cl, ll, hl) = if case_insensitive {
                (c.to_ascii_lowercase(), lo.to_ascii_lowercase(), hi.to_ascii_lowercase())
            } else {
                (c, lo, hi)
            };
            if cl >= ll && cl <= hl { matched = true; }
            i += 3;
        } else {
            let eq = if case_insensitive {
                b.eq_ignore_ascii_case(&c)
            } else {
                b == c
            };
            if eq { matched = true; }
            i += 1;
        }
        first = false;
    }
    None // Unterminated class: not a match
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> GlobOptions { GlobOptions::default() }

    #[test]
    fn star_matches_any_chars() {
        assert!(match_pattern("*.txt", "foo.txt", &opts()));
        assert!(match_pattern("*.txt", ".txt", &GlobOptions { dot_glob: true, ..opts() }));
        assert!(match_pattern("foo*", "foobar", &opts()));
        assert!(!match_pattern("*.txt", "foo.md", &opts()));
    }

    #[test]
    fn question_matches_single_char() {
        assert!(match_pattern("?", "a", &opts()));
        assert!(!match_pattern("?", "ab", &opts()));
        assert!(match_pattern("f?o", "foo", &opts()));
    }

    #[test]
    fn leading_dot_is_hidden_by_default() {
        assert!(!match_pattern("*", ".hidden", &opts()));
        assert!(match_pattern(".*", ".hidden", &opts()));
        assert!(match_pattern("*", ".hidden", &GlobOptions { dot_glob: true, ..opts() }));
    }

    #[test]
    fn character_classes() {
        assert!(match_pattern("[abc]", "b", &opts()));
        assert!(!match_pattern("[abc]", "d", &opts()));
        assert!(match_pattern("[a-z]", "m", &opts()));
        assert!(!match_pattern("[a-z]", "M", &opts()));
        assert!(match_pattern("[!a-z]", "M", &opts()));
        assert!(match_pattern("[^a-z]", "M", &opts()));
    }

    #[test]
    fn case_insensitive_matching() {
        let o = GlobOptions { case_insensitive: true, ..opts() };
        assert!(match_pattern("FOO", "foo", &o));
        assert!(match_pattern("[A-Z]", "m", &o));
    }

    #[test]
    fn escaped_metachars_are_literal() {
        assert!(match_pattern(r"foo\*", "foo*", &opts()));
        assert!(!match_pattern(r"foo\*", "foobar", &opts()));
    }

    #[test]
    fn filesystem_expansion_relative() {
        let tmp = std::env::temp_dir().join(format!("frost-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        for name in ["a.txt", "b.txt", "c.md", ".hidden"] {
            std::fs::write(tmp.join(name), "").unwrap();
        }
        let out = expand_pattern("*.txt", &tmp, &opts()).unwrap();
        assert_eq!(out, vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")]);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn filesystem_expansion_with_subdir() {
        let tmp = std::env::temp_dir().join(format!("frost-glob-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("src/lib.rs"), "").unwrap();
        std::fs::write(tmp.join("src/main.rs"), "").unwrap();
        std::fs::write(tmp.join("src/README.md"), "").unwrap();
        let out = expand_pattern("src/*.rs", &tmp, &opts()).unwrap();
        assert_eq!(out, vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")]);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn recursive_globstar() {
        let tmp = std::env::temp_dir().join(format!("frost-glob-star-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("a/b/c")).unwrap();
        std::fs::write(tmp.join("top.rs"), "").unwrap();
        std::fs::write(tmp.join("a/one.rs"), "").unwrap();
        std::fs::write(tmp.join("a/b/two.rs"), "").unwrap();
        std::fs::write(tmp.join("a/b/c/three.rs"), "").unwrap();
        let mut out = expand_pattern("**/*.rs", &tmp, &opts()).unwrap();
        out.sort();
        assert_eq!(out, vec![
            PathBuf::from("a/b/c/three.rs"),
            PathBuf::from("a/b/two.rs"),
            PathBuf::from("a/one.rs"),
            PathBuf::from("top.rs"),
        ]);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn no_match_returns_empty() {
        let tmp = std::env::temp_dir().join(format!("frost-glob-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = expand_pattern("*.rs", &tmp, &opts()).unwrap();
        assert!(out.is_empty());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
