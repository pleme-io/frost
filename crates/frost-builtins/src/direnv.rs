//! `__frost_direnv_apply` — apply direnv's environment delta natively.
//!
//! ## Why a builtin instead of the shell one-liner it replaces
//!
//! The direnv integration used to install this as its `chpwd` hook:
//!
//! ```sh
//! eval "$(direnv export bash 2>/dev/null)"
//! ```
//!
//! Three things are wrong with that, in increasing order of seriousness.
//!
//! **It is shell in a place the fleet's NO SHELL rule says it should not be.**
//! A hook body is a string the shell re-parses on every `cd`.
//!
//! **It asks a zsh-semantics shell to evaluate BASH-format text.** `direnv
//! export bash` emits every assignment as `export VAR=$'…'`, ANSI-C quoting,
//! which frost did not implement until 2026-08-07. The consequence was not a
//! visible error: the `eval` failed on every line, so `.envrc` variables never
//! applied AT ALL, `DIRENV_DIR` never persisted, and because direnv's own
//! state variables are what tell it a directory is already loaded, it did a
//! COLD reload on every single `cd` — 150 ms inside a tree whose `.envrc` runs
//! real work, against a 5 ms warm path. A shell integration that silently does
//! nothing while costing 150 ms per directory change is the exact failure mode
//! that untyped text-passing produces.
//!
//! **`eval` of a subprocess's stdout is unbounded authority.** Whatever direnv
//! prints, the shell executes. The JSON path can only ever set or unset
//! variables, because that is the only thing this builtin knows how to do.
//!
//! ## What it does
//!
//! Runs `direnv export json`, which emits a flat JSON object mapping variable
//! names to values, with `null` meaning "unset this". That is the whole of the
//! semantics `eval` was being used to get. Verified against direnv 2.37.1:
//! a 6-key object (`DIRENV_DIFF`, `DIRENV_DIR`, `DIRENV_FILE`,
//! `DIRENV_WARN_TIMEOUT`, `DIRENV_WATCHES`, `PATH`).
//!
//! ## Tier
//!
//! This is the first concrete `Act` of the typed shell cycle designed in
//! `docs/MEGURI.md` — `ApplyEnvDelta { provider: DirenvJson }`. The vocabulary
//! itself (the closed `Act`/`Fact` enums, the derived spawn budget) is still
//! DESIGN; this is one provider implemented natively, not the algebra. Do not
//! read it as meguri having landed.
//!
//! It is also NOT a latency fix. `direnv export` is a subprocess either way —
//! what goes away is the `eval`, the bash-format dependency, and the shell
//! string. Since `$'…'` now works, the old path is correct too; this makes it
//! correct by construction rather than by the lexer having caught up.

use crate::{Builtin, ShellEnvironment};
use std::process::Command;

pub struct DirenvApply;

impl Builtin for DirenvApply {
    fn name(&self) -> &str {
        "__frost_direnv_apply"
    }

    fn execute(&self, _args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let out = match Command::new("direnv").arg("export").arg("json").output() {
            Ok(o) => o,
            // direnv absent is the normal case on a host that does not use it.
            // The hook fires on every `cd`, so this must be silent — the old
            // shell form spelled that `2>/dev/null`.
            Err(_) => return 0,
        };

        // direnv exits non-zero for a blocked/denied .envrc and prints its own
        // diagnostic to stderr, which is already on the terminal. Nothing to
        // apply, and not this builtin's place to editorialise.
        if !out.status.success() {
            return 0;
        }

        // Empty stdout means "no change" — direnv's answer when the directory
        // has no .envrc and none was previously loaded.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return 0;
        }

        match parse_flat_json_object(trimmed) {
            Some(entries) => {
                for (key, value) in entries {
                    match value {
                        // EXPORT, not merely set. The shell form this replaces
                        // was `export VAR=…`, and the export is load-bearing
                        // rather than cosmetic: direnv decides what delta to
                        // emit by reading its own DIRENV_DIR / DIRENV_DIFF /
                        // DIRENV_WATCHES out of the CHILD's environment. A
                        // variable that is set but not exported is invisible to
                        // that child, so the next `direnv export` believes it
                        // is a cold start in a directory with no .envrc and
                        // emits nothing — which means the leaving-a-directory
                        // unsets never arrive and every `.envrc` variable leaks
                        // out of the tree it belongs to.
                        //
                        // Caught end-to-end, not by the unit tests above: the
                        // JSON parsed perfectly and the apply looked correct,
                        // and `PROVE_ENVRC` still survived a `cd` out of its
                        // directory.
                        Some(v) => {
                            env.set_var(&key, &v);
                            env.export_var(&key);
                        }
                        None => env.unset_var(&key),
                    }
                }
                0
            }
            // Unparseable output is a real fault — direnv changed its format,
            // or something else answered. Say so once rather than applying a
            // partial delta, which would leave the environment in a state
            // neither direnv nor the shell believes in.
            None => {
                eprintln!("frost: direnv export json: unparseable output, environment unchanged");
                1
            }
        }
    }
}

/// Parse a FLAT JSON object of `string -> string | null`.
///
/// Hand-written rather than pulling a JSON crate into `frost-builtins`,
/// because the input shape is fixed and tiny and this crate has no JSON
/// dependency today. It deliberately REFUSES anything that is not that shape
/// (nested objects, arrays, numbers, booleans) by returning `None`, so a
/// direnv that starts emitting a richer document fails loudly instead of being
/// silently half-applied.
fn parse_flat_json_object(src: &str) -> Option<Vec<(String, Option<String>)>> {
    let bytes = src.as_bytes();
    let mut i = 0usize;

    let skip_ws = |i: &mut usize| {
        while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
            *i += 1;
        }
    };

    skip_ws(&mut i);
    if i >= bytes.len() || bytes[i] != b'{' {
        return None;
    }
    i += 1;

    let mut out = Vec::new();
    loop {
        skip_ws(&mut i);
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'}' {
            return Some(out);
        }

        let key = parse_json_string(bytes, &mut i)?;
        skip_ws(&mut i);
        if i >= bytes.len() || bytes[i] != b':' {
            return None;
        }
        i += 1;
        skip_ws(&mut i);
        if i >= bytes.len() {
            return None;
        }

        let value = if bytes[i] == b'"' {
            Some(parse_json_string(bytes, &mut i)?)
        } else if bytes[i..].starts_with(b"null") {
            i += 4;
            None
        } else {
            // A number, a bool, a nested object or an array. Not our shape.
            return None;
        };
        out.push((key, value));

        skip_ws(&mut i);
        if i < bytes.len() && bytes[i] == b',' {
            i += 1;
            continue;
        }
    }
}

/// Parse one JSON string literal starting at `bytes[*i] == '"'`.
///
/// Handles the escapes direnv actually emits. `\u` is decoded for the BMP;
/// a surrogate pair is refused rather than mangled, because a half-decoded
/// path is worse than a loud failure.
fn parse_json_string(bytes: &[u8], i: &mut usize) -> Option<String> {
    if *i >= bytes.len() || bytes[*i] != b'"' {
        return None;
    }
    *i += 1;
    let mut s = String::new();
    while *i < bytes.len() {
        match bytes[*i] {
            b'"' => {
                *i += 1;
                return Some(s);
            }
            b'\\' => {
                *i += 1;
                let esc = *bytes.get(*i)?;
                *i += 1;
                match esc {
                    b'"' => s.push('"'),
                    b'\\' => s.push('\\'),
                    b'/' => s.push('/'),
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'r' => s.push('\r'),
                    b'b' => s.push('\u{8}'),
                    b'f' => s.push('\u{c}'),
                    b'u' => {
                        let hex = bytes.get(*i..*i + 4)?;
                        *i += 4;
                        let cp = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
                        s.push(char::from_u32(cp)?);
                    }
                    _ => return None,
                }
            }
            _ => {
                // Copy the whole UTF-8 sequence, not a byte — a multi-byte
                // path would otherwise be cut mid-character.
                let rest = std::str::from_utf8(&bytes[*i..]).ok()?;
                let ch = rest.chars().next()?;
                s.push(ch);
                *i += ch.len_utf8();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shape_direnv_actually_emits() {
        let src = r#"{"DIRENV_DIR":"-/tmp/x","PATH":"/a:/b","STALE":null}"#;
        let got = parse_flat_json_object(src).expect("must parse");
        assert_eq!(
            got,
            vec![
                ("DIRENV_DIR".to_string(), Some("-/tmp/x".to_string())),
                ("PATH".to_string(), Some("/a:/b".to_string())),
                ("STALE".to_string(), None),
            ]
        );
    }

    #[test]
    fn null_means_unset_not_empty_string() {
        // The distinction the whole delta rests on: direnv uses null to REMOVE
        // a variable when leaving a directory. Treating it as "" would leave a
        // stale empty var behind, which reads as "set but blank" to every
        // consumer.
        let got = parse_flat_json_object(r#"{"GONE":null,"KEPT":""}"#).unwrap();
        assert_eq!(got[0].1, None, "null must be an unset");
        assert_eq!(got[1].1, Some(String::new()), "\"\" must stay a set-to-empty");
    }

    #[test]
    fn escapes_and_multibyte_survive() {
        let got = parse_flat_json_object(r#"{"A":"a\nb","B":"q\"q","C":"\u00e9","D":"日本"}"#)
            .unwrap();
        assert_eq!(got[0].1, Some("a\nb".to_string()));
        assert_eq!(got[1].1, Some("q\"q".to_string()));
        assert_eq!(got[2].1, Some("é".to_string()));
        assert_eq!(got[3].1, Some("日本".to_string()));
    }

    #[test]
    fn empty_object_is_valid_and_yields_no_changes() {
        assert_eq!(parse_flat_json_object("{}"), Some(vec![]));
    }

    #[test]
    fn a_richer_document_is_refused_not_half_applied() {
        // The point of refusing: a partial apply leaves the environment in a
        // state neither direnv nor the shell believes in.
        for bad in [
            r#"{"a":{"nested":1}}"#,
            r#"{"a":[1,2]}"#,
            r#"{"a":1}"#,
            r#"{"a":true}"#,
            r#"["not","an","object"]"#,
            r#"{"unterminated":"#,
            r#"{"#,
            "",
        ] {
            assert_eq!(parse_flat_json_object(bad), None, "must refuse: {bad}");
        }
    }
}
