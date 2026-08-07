//! meguri (巡り) — the typed shell cycle.
//!
//! *Meguri*, "a going-around": the round the shell makes once per command —
//! accept the line, stamp, execute, gather what happened, paint the next
//! prompt. This module is that round expressed as **data an integration
//! declares** rather than **shell text the shell re-parses**.
//!
//! Full design, Gate-0 list and tier ledger: `docs/MEGURI.md`.
//!
//! ## The one idea
//!
//! A beat does not get a *body*. It gets a closed list of [`Act`]s.
//!
//! Every defect found in the 2026-08-07 shell-latency work was a consequence
//! of beats carrying shell strings, not of the logic anyone wrote — a gap in
//! the evaluator degrades into a plausible wrong answer instead of a refusal:
//!
//! - `[ -n "$(git status --porcelain)" ]` tested a non-empty *literal*, so git
//!   never ran and every repo read as dirty;
//! - `eval "$(direnv export bash)"` failed on every line for months because the
//!   shell did not implement `$'…'`, so `.envrc` never applied at all;
//! - `date +%s` forked twice a prompt to compute a duration the shell already
//!   knew to the nanosecond, at one-second resolution.
//!
//! ## What the types forbid
//!
//! [`Fact`] has **no `Shell` variant**, and [`Act::Publish`] takes a `Fact`.
//! There is therefore no expressible path from "publish a variable" to "run a
//! command" — the first defect class above is not guarded against, it is
//! unconstructible. A value that genuinely needs a subprocess is not a Fact;
//! it is an [`Act::Spawn`], and spawning is counted ([`Beat::spawn_cost`]).
//!
//! [`Fact::DurationMs`] is the only duration in the vocabulary. There is no
//! `DurationSecs` to confuse it with, so the unit travels in the type rather
//! than in a variable's name.
//!
//! ## Tier, honestly
//!
//! This is the **border**, and the border is real: the enums are closed, the
//! budget is derived, and the tests below are green. It is **not** yet the
//! whole cycle — `IntegrationRecipe`'s `*_body: Option<&str>` fields still
//! exist and still carry shell for the integrations that have not been
//! converted, and `(defhook :body "…")` remains available to operators. So:
//! the *algebra* is shipped and the *migration* is partial. Do not read this
//! module's existence as "hooks no longer carry shell".

use std::collections::BTreeSet;

/// A beat of the cycle. Closed — frost fires exactly these three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Beat {
    /// Before a command runs.
    Preexec,
    /// After a command finishes, before the prompt is painted.
    Precmd,
    /// After the working directory changes.
    Chpwd,
}

impl Beat {
    /// Every beat. The bijection with [`Beat::hook_fn`] is test-enforced, so a
    /// fourth beat cannot be added without giving it a hook name.
    pub const ALL: &'static [Beat] = &[Beat::Preexec, Beat::Precmd, Beat::Chpwd];

    /// The shell function frost invokes for this beat.
    #[must_use]
    pub fn hook_fn(self) -> &'static str {
        match self {
            Beat::Preexec => "__frost_hook_preexec",
            Beat::Precmd => "__frost_hook_precmd",
            Beat::Chpwd => "__frost_hook_chpwd",
        }
    }
}

/// A value frost can answer from its own state, with **no subprocess**.
///
/// The absence of a `Shell(String)` variant is the point, not an oversight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fact {
    /// `$?` of the command that just finished — captured by frost *before* the
    /// beat's first act runs, so an author can never observe a clobbered one.
    ExitStatus,
    /// Wall time of the last command, in **milliseconds**. The only duration in
    /// the vocabulary; the unit is in the type.
    DurationMs,
    /// Current working directory.
    Cwd,
    /// Previous working directory.
    OldCwd,
    /// Number of active jobs.
    JobCount,
    /// The shell's own pid.
    ShellPid,
}

impl Fact {
    pub const ALL: &'static [Fact] = &[
        Fact::ExitStatus,
        Fact::DurationMs,
        Fact::Cwd,
        Fact::OldCwd,
        Fact::JobCount,
        Fact::ShellPid,
    ];

    /// Stable name, used in catalog output and diagnostics.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Fact::ExitStatus => "exit-status",
            Fact::DurationMs => "duration-ms",
            Fact::Cwd => "cwd",
            Fact::OldCwd => "old-cwd",
            Fact::JobCount => "job-count",
            Fact::ShellPid => "shell-pid",
        }
    }
}

/// A native producer of an environment delta — no shell, no `eval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvDeltaProvider {
    /// `direnv export json`, applied by the `__frost_direnv_apply` builtin.
    /// A flat `string -> string | null` map where null means unset.
    DirenvJson,
}

impl EnvDeltaProvider {
    /// The builtin that applies this provider's delta.
    #[must_use]
    pub fn builtin(self) -> &'static str {
        match self {
            EnvDeltaProvider::DirenvJson => "__frost_direnv_apply",
        }
    }

    /// Applying a delta costs one child (the provider is a real program), but
    /// it is a *counted, declared* child rather than an `eval` of arbitrary
    /// text.
    #[must_use]
    pub fn spawns(self) -> u8 {
        match self {
            EnvDeltaProvider::DirenvJson => 1,
        }
    }
}

/// An argv — never a shell line. The escape hatch, and the only variant that
/// can cost an unaccounted process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub argv: Vec<String>,
}

/// What a beat may do. Closed, and free shell text is not a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Export `var` with the value of `fact`.
    Publish { var: String, fact: Fact },
    /// Apply an environment delta from a native provider.
    ApplyEnvDelta { provider: EnvDeltaProvider },
    /// Render the prompt by spawning a renderer.
    Render { cmd: SpawnSpec },
    /// Run a program. Argv, never a shell line.
    Spawn { cmd: SpawnSpec },
}

impl Act {
    /// How many child processes this act costs.
    #[must_use]
    pub fn spawn_cost(&self) -> u8 {
        match self {
            // The whole reason Fact exists: a publish cannot fork.
            Act::Publish { .. } => 0,
            Act::ApplyEnvDelta { provider } => provider.spawns(),
            Act::Render { .. } | Act::Spawn { .. } => 1,
        }
    }
}

/// One beat's declared acts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeatSpec {
    pub beat: Beat,
    pub acts: Vec<Act>,
}

impl BeatSpec {
    /// Total child processes this beat costs per fire.
    ///
    /// **Derived, never declared.** A budget an author writes down can
    /// disagree with the acts it is meant to bound; this one cannot. The
    /// per-prompt subprocess count in this stack went 0 → 5 without a single
    /// commit noting it, which is the failure this closes.
    #[must_use]
    pub fn spawn_cost(&self) -> u8 {
        self.acts.iter().map(Act::spawn_cost).sum()
    }

    /// Every variable this beat publishes.
    #[must_use]
    pub fn published_vars(&self) -> BTreeSet<&str> {
        self.acts
            .iter()
            .filter_map(|a| match a {
                Act::Publish { var, .. } => Some(var.as_str()),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_publish_cannot_fork() {
        // The load-bearing property. If `Fact` ever grows a variant that
        // carries a command, this assertion still passes but the TYPE has
        // changed — which is why the real seal is the enum's shape, and this
        // test only pins the arithmetic that shape buys.
        for f in Fact::ALL {
            let a = Act::Publish {
                var: "X".into(),
                fact: *f,
            };
            assert_eq!(a.spawn_cost(), 0, "publishing {} must not spawn", f.name());
        }
    }

    #[test]
    fn the_budget_is_derived_not_declared() {
        let b = BeatSpec {
            beat: Beat::Precmd,
            acts: vec![
                Act::Publish {
                    var: "FROST_CMD_DURATION_MS".into(),
                    fact: Fact::DurationMs,
                },
                Act::Publish {
                    var: "FROST_LAST_EXIT".into(),
                    fact: Fact::ExitStatus,
                },
                Act::Render {
                    cmd: SpawnSpec {
                        argv: vec!["seki".into(), "prompt".into()],
                    },
                },
            ],
        };
        // Two publishes are free; only the renderer costs a child.
        assert_eq!(b.spawn_cost(), 1);
        assert_eq!(
            b.published_vars().into_iter().collect::<Vec<_>>(),
            vec!["FROST_CMD_DURATION_MS", "FROST_LAST_EXIT"]
        );
    }

    #[test]
    fn the_direnv_chpwd_beat_costs_exactly_one_child() {
        // Regression pin on the shape that replaced
        // `eval "$(direnv export bash 2>/dev/null)"`: one counted child, and
        // no eval anywhere in the declaration.
        let b = BeatSpec {
            beat: Beat::Chpwd,
            acts: vec![Act::ApplyEnvDelta {
                provider: EnvDeltaProvider::DirenvJson,
            }],
        };
        assert_eq!(b.spawn_cost(), 1);
        assert_eq!(
            EnvDeltaProvider::DirenvJson.builtin(),
            "__frost_direnv_apply"
        );
    }

    #[test]
    fn beat_hook_names_are_a_bijection() {
        // CATALOG REFLECTION: a fourth beat cannot land without a hook name,
        // and two beats cannot share one.
        let names: BTreeSet<&str> = Beat::ALL.iter().map(|b| b.hook_fn()).collect();
        assert_eq!(names.len(), Beat::ALL.len(), "hook names must be distinct");
        for b in Beat::ALL {
            assert!(
                b.hook_fn().starts_with("__frost_hook_"),
                "{b:?} must map to a real hook function"
            );
        }
    }

    #[test]
    fn fact_names_are_distinct() {
        let names: BTreeSet<&str> = Fact::ALL.iter().map(|f| f.name()).collect();
        assert_eq!(names.len(), Fact::ALL.len());
    }
}
