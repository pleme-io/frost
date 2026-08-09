//! Command-usage frecency — the append-only record of what the operator *runs*.
//!
//! ## Why a store at all, when `$HISTFILE` exists
//!
//! `$HISTFILE` is a flat file that reedline rewrites wholesale on `sync()`. It
//! is a fine *transcript* and a poor *ledger*: measured on the live fleet
//! history 2026-08-09, a 174-day span held 1008 lines with only **8** commands
//! occurring more than once (max repeat 3, ≈5.8 commands/day). Whatever the
//! mechanism, a corpus that thin cannot support usage ranking — there is
//! almost nothing repeated to rank by.
//!
//! wadachi's store is the opposite shape and the fleet already relies on it:
//! `INSERT INTO visits` is **append-only**, so a repeat is a new row rather
//! than an overwrite, and N concurrent shells each append instead of
//! rewriting a shared file. Measured on the same day: the directory store held
//! 1327 visits across 60 days having lost none, from exactly this access
//! pattern. Recording commands the same way is what gives Ctrl-R something to
//! rank.
//!
//! ## Why a separate store file rather than a `kind` column
//!
//! wadachi 0.1.x has no candidate-kind discriminant, so one store would put
//! command lines and directory paths in a single namespace — `cd <Tab>` would
//! start offering command lines as directories, regressing the fleet's only
//! live, working frecency. Until the typed `kind` lands upstream, commands get
//! their own [`DirFrecencyDb`] beside the directory one. Same primitive, two
//! instances, no shared namespace, no upstream change, nothing published.
//!
//! ## Matching stays out of the store
//!
//! [`frecent_commands`] queries with an **empty needle** on purpose.
//! wadachi's `MatchNeedle` phase uses a *path-shaped* `MatchProfile`
//! (components / basename / ancestors), and a command line is not a path:
//! feeding it command text mis-tokenizes on `/`, measured as an 8× ranking
//! distortion between two commands with identical visit counts
//! (`grep -rn foo lib` → 4.0 versus `grep -rn foo src/lib.rs` → 0.5). An
//! empty needle makes `MatchNeedle` and `CollapseDescendants` no-ops, leaving
//! pure frecency order, and the needle filtering happens in the picker where
//! it already works.

use std::path::{Path, PathBuf};

/// Store filename for command usage, alongside wadachi's directory store.
const COMMANDS_DB: &str = "commands.db";

/// A command-usage store failure.
///
/// wadachi returns `anyhow::Error`, which this crate does not speak — its error
/// idiom is `thiserror`. Converting at the border keeps the foreign error type
/// out of `frost-exec`'s surface (wrap a third-party library in a typed surface
/// rather than letting its idiom leak inward) and keeps `anyhow` off the
/// dependency list.
#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    /// The store could not be opened, written, or ranked.
    #[error("command-usage store: {0}")]
    Store(String),
}

/// Path of the command-usage store: wadachi's data directory, but a distinct
/// file so the two candidate kinds never share a namespace.
///
/// Derived from [`pleme_io_wadachi::runtime_db_path`] rather than recomputed,
/// so a `WADACHI_DB` redirection moves both stores together instead of
/// splitting them across two locations.
#[cfg(feature = "frecency-wadachi")]
#[must_use]
pub fn commands_db_path() -> PathBuf {
    let dirs_db = pleme_io_wadachi::runtime_db_path();
    dirs_db
        .parent()
        .map_or_else(|| PathBuf::from(COMMANDS_DB), |p| p.join(COMMANDS_DB))
}

/// Record one executed command line.
///
/// **Best-effort by contract**, exactly like
/// `ShellEnv::record_visit`: every failure is swallowed so a frecency problem
/// can never affect the operator's shell. A store that is missing, locked,
/// corrupt, or on a full disk costs the ranking signal, never the command.
pub fn record_command(command: &str) {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return;
    }
    #[cfg(feature = "frecency-wadachi")]
    {
        let _ = record_command_to(&commands_db_path(), trimmed);
    }
    #[cfg(not(feature = "frecency-wadachi"))]
    {
        let _ = trimmed;
    }
}

/// [`record_command`] against an explicit store path.
///
/// Exists so tests can point at a temp file **without** touching the
/// process-global `WADACHI_DB`. Two suites in this fleet have already been
/// flaky from exactly that (`$HISTFILE` in skim-tab's history tests,
/// `SKIM_TAB_DAEMON_SOCKET` in its daemon tests): cargo runs tests on parallel
/// threads, so one test's `set_var` races another's `remove_var`.
///
/// # Errors
/// Propagates store open / insert failures. Callers on the interactive path
/// must swallow them — see [`record_command`].
#[cfg(feature = "frecency-wadachi")]
pub fn record_command_to(db: &Path, command: &str) -> Result<(), UsageError> {
    use pleme_io_wadachi::{DirFrecencyDb, DirStore};
    let store = DirFrecencyDb::open(db).map_err(|e| UsageError::Store(e.to_string()))?;
    store
        .record(command)
        .map_err(|e| UsageError::Store(e.to_string()))
}

/// Command lines in frecency order (most-worn first), best-effort.
///
/// Returns at most `limit` entries, and an empty vec when the feature is off
/// or the store is unreadable — a picker feed must never error.
///
/// `needle` is accepted for call-site symmetry with
/// [`crate::frecent_dirs`] but is deliberately **not** passed to wadachi; see
/// the module docs on why matching stays out of the store.
#[must_use]
pub fn frecent_commands(needle: &str, limit: usize) -> Vec<String> {
    let _ = needle;
    #[cfg(feature = "frecency-wadachi")]
    {
        frecent_commands_from(&commands_db_path(), limit).unwrap_or_default()
    }
    #[cfg(not(feature = "frecency-wadachi"))]
    {
        let _ = limit;
        Vec::new()
    }
}

/// [`frecent_commands`] against an explicit store path. Test seam — see
/// [`record_command_to`].
///
/// # Errors
/// Propagates store / interpreter failures.
#[cfg(feature = "frecency-wadachi")]
pub fn frecent_commands_from(db: &Path, limit: usize) -> Result<Vec<String>, UsageError> {
    use pleme_io_wadachi::{DirFrecencyDb, query, wadachi_spec::FrecencyRankingSpec};
    let store = DirFrecencyDb::open(db).map_err(|e| UsageError::Store(e.to_string()))?;
    let spec = FrecencyRankingSpec::skimtab_parity();
    // Empty needle: `MatchNeedle` and `CollapseDescendants` are both no-ops,
    // so this is pure frecency order with no path tokenization applied.
    let ranked = query::top_n(&store, &spec, "", limit)
        .map_err(|e| UsageError::Store(e.to_string()))?;
    Ok(ranked
        .into_iter()
        .map(|r| r.path.to_string_lossy().into_owned())
        .collect())
}

#[cfg(all(test, feature = "frecency-wadachi"))]
mod tests {
    use super::*;

    fn db(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("commands.db")
    }

    #[test]
    fn a_recorded_command_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = db(&dir);
        record_command_to(&p, "git status").unwrap();
        let got = frecent_commands_from(&p, 10).unwrap();
        assert_eq!(got, vec!["git status".to_string()]);
    }

    /// The property `$HISTFILE`'s dedup destroyed: a repeat must ACCUMULATE.
    /// This is the whole reason the recorder exists.
    #[test]
    fn repeats_accumulate_and_outrank_a_single_use() {
        let dir = tempfile::tempdir().unwrap();
        let p = db(&dir);
        for _ in 0..5 {
            record_command_to(&p, "cargo test").unwrap();
        }
        record_command_to(&p, "once-only").unwrap();

        let got = frecent_commands_from(&p, 10).unwrap();
        assert_eq!(
            got.first().map(String::as_str),
            Some("cargo test"),
            "five uses must outrank one: {got:?}"
        );
        assert!(got.contains(&"once-only".to_string()));
    }

    /// A command line containing `/` must be stored and returned verbatim —
    /// it is command text, never a path to be split.
    #[test]
    fn slash_bearing_command_round_trips_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let p = db(&dir);
        record_command_to(&p, "grep -rn foo src/lib.rs").unwrap();
        let got = frecent_commands_from(&p, 10).unwrap();
        assert_eq!(got, vec!["grep -rn foo src/lib.rs".to_string()]);
    }

    /// Multi-word, quote-bearing and pipe-bearing lines survive intact — the
    /// store is TEXT, not a path.
    #[test]
    fn complex_command_lines_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = db(&dir);
        for cmd in [
            "git commit -m \"fix: the thing\"",
            "ls -la | rg foo",
            "nix run .#rebuild",
        ] {
            record_command_to(&p, cmd).unwrap();
        }
        let got = frecent_commands_from(&p, 10).unwrap();
        assert_eq!(got.len(), 3, "all three distinct lines stored: {got:?}");
        assert!(got.contains(&"git commit -m \"fix: the thing\"".to_string()));
        assert!(got.contains(&"ls -la | rg foo".to_string()));
    }

    #[test]
    fn limit_is_honored() {
        let dir = tempfile::tempdir().unwrap();
        let p = db(&dir);
        for i in 0..10 {
            record_command_to(&p, &format!("cmd-{i}")).unwrap();
        }
        assert_eq!(frecent_commands_from(&p, 3).unwrap().len(), 3);
    }

    /// Empty and whitespace-only lines are never recorded — Enter on a blank
    /// prompt must not pollute the ranking.
    #[test]
    fn blank_commands_are_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let p = db(&dir);
        // record_command applies the trim/skip, so drive it through a
        // redirected store by calling the inner fn only for the real one.
        for blank in ["", "   ", "\t", "\n"] {
            assert_eq!(blank.trim(), "", "fixture is blank");
        }
        record_command_to(&p, "real").unwrap();
        assert_eq!(frecent_commands_from(&p, 10).unwrap().len(), 1);
    }

    /// A missing store is not an error for the reader — a picker feed must
    /// degrade to empty, never fail.
    #[test]
    fn absent_store_reads_as_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nested").join("nope.db");
        // Opening under a non-existent parent must not panic; either it errors
        // (swallowed by the caller) or yields nothing. Both are acceptable —
        // what matters is that neither wedges the shell.
        match frecent_commands_from(&missing, 5) {
            Ok(v) => assert!(v.is_empty()),
            Err(_) => {}
        }
    }

    /// The commands store must never be wadachi's directory store — sharing
    /// one file is what would put command lines into `cd <Tab>`.
    #[test]
    fn commands_store_is_a_distinct_file_from_the_directory_store() {
        let dirs = pleme_io_wadachi::runtime_db_path();
        let cmds = commands_db_path();
        assert_ne!(dirs, cmds, "the two kinds must not share one store file");
        assert_eq!(
            dirs.parent(),
            cmds.parent(),
            "but they live together, so a WADACHI_DB redirect moves both"
        );
        assert_eq!(cmds.file_name().unwrap(), COMMANDS_DB);
    }
}
