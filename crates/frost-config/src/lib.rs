//! `frost-config` — shikumi-backed typed configuration for frost.
//!
//! Sister crate to mado's `MadoConfig` + tear's `TearConfig`. Every
//! pleme-io tool now exposes the same configuration shape:
//!
//! * **Discovery**: XDG (`~/.config/frost/frost.yaml`) with env
//!   override (`FROST_CONFIG=...`).
//! * **Schema**: typed Rust struct + serde, `deny_unknown_fields`
//!   for catch-typos-at-parse-time.
//! * **Backwards-compat**: every newly-added field carries a
//!   `#[serde(default)]` so pre-existing operator YAML continues
//!   to parse.
//! * **Hot-reload**: `shikumi::ConfigStore::load_and_watch`
//!   reloads on file change, signals subscribers via the standard
//!   shikumi callback.
//! * **Env overrides**: any key reachable via the `FROST_`-prefixed
//!   nested env path (`FROST_PROMPT_TEMPLATE=...`,
//!   `FROST_HISTORY_FILE=...`) wins over YAML at load time.
//!
//! The schema is intentionally COMPREHENSIVE — it covers every
//! axis frostmourne's tatara-lisp preset reaches today, so the
//! lisp can compile to this YAML without losing fidelity. New
//! features add fields here first, then the lisp authoring layer
//! gains the corresponding `defform`.
//!
//! ## Why shikumi
//!
//! Tear, mado, frost, and frostmourne are the four pillars of the
//! pleme-io operator UX. Operators who set up one expect the same
//! pattern in the others: same config root, same hot-reload
//! semantics, same env-override grammar. Shikumi is the typed
//! primitive that makes "same" mechanical instead of disciplined.

#![forbid(unsafe_code)]
#![doc(html_root_url = "https://docs.rs/frost-config/0.1.0")]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("shikumi error: {0}")]
    Shikumi(#[from] shikumi::ShikumiError),
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Top-level frost configuration. Every field defaults so an
/// empty `frost.yaml` is a valid, ready-to-run setup; operators
/// can override only what they care about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrostConfig {
    /// Shell options (parallels zsh's `setopt`). Each entry is
    /// `OPTION_NAME` → bool. Unrecognised names are rejected at
    /// parse time via `deny_unknown_fields` on the per-option map
    /// — handled by frost-options at apply time.
    #[serde(default)]
    pub options: BTreeMap<String, bool>,

    /// Aliases — `alias_name` → expansion string. Same semantics
    /// as zsh's `alias` builtin. Multi-word values use shell
    /// quoting; the YAML parser handles that for free.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,

    /// Environment variables applied at shell startup. Keys are
    /// upper-case by convention (`EDITOR`, `PAGER`); values are
    /// strings (numbers are quoted in YAML).
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Prompt configuration — the visible per-line decoration.
    #[serde(default)]
    pub prompt: PromptConfig,

    /// History configuration — file path, size limits, dedup rules.
    #[serde(default)]
    pub history: HistoryConfig,

    /// Keybindings — `chord` → `widget_name`. Operators bind
    /// `'^A'` → `'beginning-of-line'` etc. The map preserves
    /// insertion order so duplicate-key warnings surface
    /// deterministically.
    #[serde(default)]
    pub keybindings: BTreeMap<String, String>,

    /// Completion configuration — opt-in completion sources.
    #[serde(default)]
    pub completion: CompletionConfig,

    /// `frostmourne` preset opt-in. When `true`, the default
    /// frostmourne tatara-lisp preset is applied BEFORE the
    /// operator YAML — operator YAML still wins on conflict, but
    /// they get the curated baseline for free. Default `false`
    /// so `frost` stays minimal and `frostmourne` is the
    /// batteries-included entry point.
    #[serde(default)]
    pub frostmourne_preset: bool,

    /// Hot-reload behavior — debounce ms for the notify watcher.
    /// Operators saving twice in a row shouldn't get two reload
    /// events; coalesce within the window.
    #[serde(default = "default_reload_debounce")]
    pub reload_debounce_ms: u64,
}

impl Default for FrostConfig {
    fn default() -> Self {
        Self {
            options: BTreeMap::new(),
            aliases: BTreeMap::new(),
            env: BTreeMap::new(),
            prompt: PromptConfig::default(),
            history: HistoryConfig::default(),
            keybindings: BTreeMap::new(),
            completion: CompletionConfig::default(),
            frostmourne_preset: false,
            reload_debounce_ms: default_reload_debounce(),
        }
    }
}

fn default_reload_debounce() -> u64 {
    250
}

/// Prompt template + theme.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptConfig {
    /// Zsh-compatible `%`-escape template (`%~`, `%n`, `%m`,
    /// `%#`, `%(?...)`).
    #[serde(default = "default_prompt_template")]
    pub template: String,

    /// Right-side prompt (RPS1). Empty disables.
    #[serde(default)]
    pub right_template: String,

    /// Whether to substitute env vars / commands in the prompt
    /// (zsh's `promptsubst` option). Default true — most modern
    /// prompts depend on this.
    #[serde(default = "default_prompt_subst")]
    pub subst: bool,
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            template: default_prompt_template(),
            right_template: String::new(),
            subst: default_prompt_subst(),
        }
    }
}

fn default_prompt_template() -> String {
    "%n@%m %~ %# ".to_string()
}
fn default_prompt_subst() -> bool {
    true
}

/// History file + retention.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryConfig {
    /// Absolute path or `~`-prefixed shell-expanded path for the
    /// persistent history file. Default
    /// `~/.local/share/frost/history`.
    #[serde(default = "default_history_file")]
    pub file: String,

    /// Max number of entries kept in memory.
    #[serde(default = "default_history_size")]
    pub size: usize,

    /// Max number of entries persisted to disk on save. Usually
    /// equal to `size`; smaller saves shrink long shells.
    #[serde(default = "default_history_save")]
    pub save: usize,

    /// Drop duplicate-of-previous entries (zsh `histignoredups`).
    #[serde(default = "default_ignore_dups")]
    pub ignore_dups: bool,

    /// Drop entries whose first byte is whitespace (zsh
    /// `histignorespace`). Useful for typing secrets prefixed by
    /// a space.
    #[serde(default)]
    pub ignore_space: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            file: default_history_file(),
            size: default_history_size(),
            save: default_history_save(),
            ignore_dups: default_ignore_dups(),
            ignore_space: false,
        }
    }
}

fn default_history_file() -> String {
    "~/.local/share/frost/history".to_string()
}
/// Default in-memory history cap.
///
/// 10_000 was reedline's own default carried over unexamined, and it is
/// far below what this hardware wants: the fleet directive for the
/// mado → frostmourne stack is that command history **never silently
/// drops** — it fills disk before it exhausts itself. At ~80 bytes per
/// entry, 5M entries is ~400 MB of history, comfortably held by an
/// M4 Pro / 48 GB workstation and orders of magnitude past any human
/// session lifetime.
///
/// This is the FALLBACK, not the ceiling: a `(defhistory :size …)` in
/// the rc exports `HISTSIZE`, and `frost::interactive` prefers that
/// value (frostmourne authors 100_000_000). This number is what frost
/// uses when no rc said otherwise.
pub const DEFAULT_HISTORY_SIZE: usize = 5_000_000;

fn default_history_size() -> usize {
    DEFAULT_HISTORY_SIZE
}
fn default_history_save() -> usize {
    DEFAULT_HISTORY_SIZE
}
fn default_ignore_dups() -> bool {
    true
}

/// Completion subsystem configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionConfig {
    /// Master enable. Default true.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Case-insensitive match (zsh `_complete_ignore_case`).
    #[serde(default)]
    pub ignore_case: bool,
    /// Show menu after first Tab instead of expanding the
    /// longest common prefix.
    #[serde(default)]
    pub menu: bool,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            ignore_case: false,
            menu: false,
        }
    }
}

fn default_enabled() -> bool {
    true
}

/// Load + watch the operator's frost config. Returns the
/// initial parse plus a [`shikumi::ConfigStore`] that callers
/// keep alive for the lifetime of the shell — the store owns the
/// notify watcher and the `ArcSwap<FrostConfig>` snapshot.
///
/// Callers that want to react to live edits pass `on_reload`;
/// shikumi invokes it inside the notify watcher thread after a
/// successful re-parse. Callers that only want the snapshot at
/// startup pass [`load`] (no watcher).
pub fn load_and_watch<F>(
    path: &std::path::Path,
    on_reload: F,
) -> Result<(FrostConfig, shikumi::ConfigStore<FrostConfig>), ConfigError>
where
    F: Fn(&FrostConfig) + Send + Sync + 'static,
{
    let store = shikumi::ConfigStore::<FrostConfig>::load_and_watch(path, "FROST_", on_reload)?;
    let snap = (**store.get()).clone();
    Ok((snap, store))
}

/// Load the config once, no watcher. Cheaper for one-shot
/// invocations (`frost -c '...'`).
pub fn load(
    path: &std::path::Path,
) -> Result<(FrostConfig, shikumi::ConfigStore<FrostConfig>), ConfigError> {
    let store = shikumi::ConfigStore::<FrostConfig>::load(path, "FROST_")?;
    let snap = (**store.get()).clone();
    Ok((snap, store))
}

/// XDG-canonical default path for the operator's frost config.
/// Resolves at call time so env tweaks (`XDG_CONFIG_HOME`) take
/// effect.
///
/// Both env arms used to take their variable verbatim, so
/// `XDG_CONFIG_HOME=""` resolved the operator's config to
/// `frost/frost.yaml` — relative to whatever directory the shell
/// happened to start in — and `HOME=""` to `.config/frost/frost.yaml`.
/// Nothing failed; frost simply read (and shikumi's watcher watched) a
/// different file per cwd. okiba resolves the identical chain
/// (`$XDG_CONFIG_HOME/frost/frost.yaml`, else
/// `$HOME/.config/frost/frost.yaml`) with the one difference that a
/// relative or empty override is IGNORED rather than joined, so no
/// valid configuration moves.
pub fn default_config_path() -> std::path::PathBuf {
    config_path_in(&okiba::Okiba::for_app("frost"))
}

/// The resolution above, over an explicit [`okiba::Okiba`] — the seam a
/// test drives, so the invariant is pinned without mutating
/// `std::env` (which races under parallel test execution).
fn config_path_in(base: &okiba::Okiba) -> std::path::PathBuf {
    // Err means no usable override AND no usable `$HOME`; keep the
    // pre-existing terminal literal rather than inventing a new home.
    base.try_path(okiba::Tier::Config, "frost.yaml")
        .unwrap_or_else(|_| std::path::PathBuf::from("frost.yaml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── defaults + round-trip ─────────────────────────────────────

    #[test]
    fn default_config_is_constructible_and_minimal() {
        let cfg = FrostConfig::default();
        assert!(cfg.options.is_empty());
        assert!(cfg.aliases.is_empty());
        assert!(cfg.env.is_empty());
        assert!(!cfg.frostmourne_preset);
        assert_eq!(cfg.reload_debounce_ms, 250);
    }

    #[test]
    fn empty_yaml_yields_default_config() {
        // Pre-existing operator with no YAML still gets a valid
        // config. Critical backwards-compat guarantee.
        let cfg: FrostConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(cfg, FrostConfig::default());
    }

    #[test]
    fn full_config_yaml_round_trips() {
        let mut cfg = FrostConfig::default();
        cfg.aliases.insert("ll".into(), "ls -la".into());
        cfg.aliases.insert("gst".into(), "git status".into());
        cfg.env.insert("EDITOR".into(), "vim".into());
        cfg.options.insert("EXTENDED_GLOB".into(), true);
        cfg.options.insert("BEEP".into(), false);
        cfg.prompt.template = "%~ ➜ ".into();
        cfg.prompt.right_template = "%T".into();
        cfg.history.file = "/tmp/frost.hist".into();
        cfg.history.size = 50_000;
        cfg.history.ignore_space = true;
        cfg.keybindings
            .insert("^A".into(), "beginning-of-line".into());
        cfg.completion.menu = true;
        cfg.frostmourne_preset = true;

        let y = serde_yaml::to_string(&cfg).unwrap();
        let back: FrostConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let y = "typo_field: 42\n";
        let err = serde_yaml::from_str::<FrostConfig>(y).unwrap_err();
        assert!(err.to_string().contains("typo_field"), "msg: {err}");
    }

    #[test]
    fn unknown_prompt_field_is_rejected() {
        let y = "prompt:\n  bogus: true\n";
        let err = serde_yaml::from_str::<FrostConfig>(y).unwrap_err();
        assert!(err.to_string().contains("bogus"), "msg: {err}");
    }

    #[test]
    fn unknown_history_field_is_rejected() {
        let y = "history:\n  bogus: 1\n";
        let err = serde_yaml::from_str::<FrostConfig>(y).unwrap_err();
        assert!(err.to_string().contains("bogus"), "msg: {err}");
    }

    // ── per-section defaults ──────────────────────────────────────

    #[test]
    fn prompt_defaults_are_sensible() {
        let p = PromptConfig::default();
        assert!(p.template.contains("%n"));
        assert!(p.template.contains("%m"));
        assert!(p.template.contains("%~"));
        assert!(p.template.contains("%#"));
        assert!(p.subst);
    }

    /// The cap is deliberately absurd — "fills disk before it drops a
    /// command" is the directive, not "matches zsh's conventional
    /// 10_000". Pinned so a future edit to one of the two default
    /// functions cannot silently desync them.
    #[test]
    fn history_defaults_are_effectively_unbounded() {
        let h = HistoryConfig::default();
        assert!(h.file.starts_with("~/"));
        assert_eq!(h.size, DEFAULT_HISTORY_SIZE);
        assert_eq!(h.save, DEFAULT_HISTORY_SIZE);
        assert_eq!(DEFAULT_HISTORY_SIZE, 5_000_000);
        assert!(h.ignore_dups);
        assert!(!h.ignore_space);
    }

    #[test]
    fn completion_defaults_are_minimal() {
        let c = CompletionConfig::default();
        assert!(c.enabled);
        assert!(!c.ignore_case);
        assert!(!c.menu);
    }

    // ── default_config_path ───────────────────────────────────────

    #[test]
    fn default_config_path_ends_in_frost_yaml() {
        // We can't mutate env in tests (forbid(unsafe_code)), so
        // assert just the structural invariant: the path always
        // ends in `frost/frost.yaml` regardless of which branch
        // resolves the prefix.
        let p = default_config_path();
        let s = p.to_string_lossy();
        assert!(
            s.ends_with("frost.yaml") || s.ends_with("frost/frost.yaml"),
            "{s}"
        );
    }

    // ── load + load_and_watch round-trip ──────────────────────────

    #[test]
    fn load_round_trips_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frost.yaml");
        let mut cfg = FrostConfig::default();
        cfg.aliases.insert("k".into(), "kubectl".into());
        cfg.options.insert("EXTENDED_GLOB".into(), true);
        std::fs::write(&path, serde_yaml::to_string(&cfg).unwrap()).unwrap();
        let (loaded, _store) = match load(&path) {
            Ok(p) => p,
            Err(e) => panic!("load failed: {e}"),
        };
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn load_treats_missing_file_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doesnotexist.yaml");
        let (loaded, _store) = match load(&path) {
            Ok(p) => p,
            Err(e) => panic!("load of missing path should not error: {e}"),
        };
        // shikumi treats a missing file as "use defaults" so a
        // fresh operator install just works — same contract as
        // mado / tear today.
        assert_eq!(loaded, FrostConfig::default());
    }

    #[test]
    fn malformed_yaml_returns_error_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frost.yaml");
        std::fs::write(&path, "this: is: not: yaml: ok\n  - extra").unwrap();
        let err = match load(&path) {
            Ok(_) => panic!("malformed yaml should error"),
            Err(e) => e,
        };
        // Don't depend on the exact wording — just that we surface
        // something useful and don't panic.
        let msg = format!("{err}");
        assert!(
            msg.contains("yaml")
                || msg.contains("parse")
                || msg.contains("error")
                || msg.contains("expected")
                || msg.contains("shikumi"),
            "msg: {msg}"
        );
    }

    // ── per-field round-trip (catches `#[serde(default)]` omissions) ──

    #[test]
    fn frostmourne_preset_round_trips() {
        let mut cfg = FrostConfig::default();
        cfg.frostmourne_preset = true;
        let y = serde_yaml::to_string(&cfg).unwrap();
        assert!(y.contains("frostmourne_preset: true"), "yaml: {y}");
        let back: FrostConfig = serde_yaml::from_str(&y).unwrap();
        assert!(back.frostmourne_preset);
    }

    #[test]
    fn reload_debounce_ms_round_trips() {
        let mut cfg = FrostConfig::default();
        cfg.reload_debounce_ms = 999;
        let y = serde_yaml::to_string(&cfg).unwrap();
        let back: FrostConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back.reload_debounce_ms, 999);
    }

    #[test]
    fn aliases_btreemap_preserves_keys_alphabetically_in_yaml() {
        let mut cfg = FrostConfig::default();
        cfg.aliases.insert("z_last".into(), "last".into());
        cfg.aliases.insert("a_first".into(), "first".into());
        let y = serde_yaml::to_string(&cfg).unwrap();
        // BTreeMap serialises in sort order — deterministic
        // diffs for operator-edited files.
        let a_pos = y.find("a_first").unwrap();
        let z_pos = y.find("z_last").unwrap();
        assert!(a_pos < z_pos, "yaml: {y}");
    }

    // ── default config path: no arm may yield a relative path ─────

    /// Drive the resolver over an explicit environment — never
    /// `std::env`, which is process-global and races under `cargo
    /// test`'s parallel threads.
    fn path_for(env: &[(&str, &str)]) -> std::path::PathBuf {
        let owned: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        config_path_in(&okiba::Okiba::from_env("frost", move |k| {
            owned
                .iter()
                .find(|(name, _)| name == k)
                .map(|(_, v)| v.clone())
        }))
    }

    #[test]
    fn xdg_config_home_resolves_the_config() {
        assert_eq!(
            path_for(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/op")]),
            std::path::PathBuf::from("/xdg/frost/frost.yaml"),
        );
    }

    #[test]
    fn home_resolves_the_config_when_xdg_is_unset() {
        assert_eq!(
            path_for(&[("HOME", "/home/op")]),
            std::path::PathBuf::from("/home/op/.config/frost/frost.yaml"),
        );
    }

    #[test]
    fn a_non_absolute_xdg_config_home_is_ignored_not_joined() {
        // The masked-branch class: the literal third arm made this read
        // as safe while the two env arms took their variable verbatim.
        // `XDG_CONFIG_HOME=""` used to yield `frost/frost.yaml`, i.e. a
        // different config file per working directory.
        for bad in ["", "rel/x", "./x", ".."] {
            assert_eq!(
                path_for(&[("XDG_CONFIG_HOME", bad), ("HOME", "/home/op")]),
                std::path::PathBuf::from("/home/op/.config/frost/frost.yaml"),
                "XDG_CONFIG_HOME={bad:?} must fall through, not resolve",
            );
        }
    }

    #[test]
    fn a_non_absolute_home_is_ignored_not_joined() {
        for bad in ["", "rel/x", "./x"] {
            let p = path_for(&[("HOME", bad)]);
            assert_eq!(
                p,
                std::path::PathBuf::from("frost.yaml"),
                "HOME={bad:?} must fall through to the terminal literal",
            );
        }
    }

    #[test]
    fn every_arm_with_a_usable_base_is_absolute() {
        for env in [
            vec![("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/op")],
            vec![("XDG_CONFIG_HOME", "rel"), ("HOME", "/home/op")],
            vec![("HOME", "/home/op")],
        ] {
            let p = path_for(&env);
            assert!(p.is_absolute(), "{env:?} yielded a relative {p:?}");
        }
    }
}
