# meguri (巡り) — the typed shell cycle

**Status: BORDER SHIPPED, MIGRATION PARTIAL (2026-08-07).** Grounded in a measured latency + differential-parity
recon of the shipped frostmourne stack on ryn. Nothing in this document is
implemented yet; every tier claim below is a claim about the *destination*, and the
ledger in §5 says so per row. A named follow-up is not a shipped follow-up.

*Meguri* (巡り, "a going-around") — the round the shell makes once per command:
accept the line → stamp → execute → gather what happened → paint the next prompt.
The gloss is meant to let a reader guess the job without the doc: it is the
**circuit**, not the shell and not the prompt. Family: motion/path, alongside
`wadachi` (轍, the wheel-rut the shell wears into your directories) and `seki`
(関, the gate the prompt is rendered at). Japanese, because this is foundational
substrate rather than a place or a flow (NAMING.md L1). Collision-checked across
`~/code/github/pleme-io`: zero hits.

---

> **Update, same day.** The algebra in §3 is now real code:
> `crates/frost-lisp/src/meguri.rs` (closed `Beat`/`Fact`/`Act`/`BeatSpec`,
> derived `spawn_cost()`, 8 tests) plus `integration::beats()` as the
> migration seam. direnv is the one converted beat. **The migration is
> partial and stays honestly labelled:** `IntegrationRecipe`'s
> `*_body: Option<&str>` fields still carry shell for unconverted
> integrations, and `(defhook :body "…")` is still available to operators —
> so §5's ledger rows describing the destination remain the destination, not
> a description of today.

## 1. The problem, stated as measurements

Every beat of frost's cycle is today a **shell string**. `IntegrationRecipe`
(`crates/frost-lisp/src/integration.rs`) carries `precmd_body`, `preexec_body`,
`chpwd_body` as `Option<&str>`, and `install_body_as_function`
(`crates/frost-lisp/src/lib.rs:1255`) parses that text into a shell function that is
re-executed every cycle. `(defhook :body "…")` is the same thing with the operator
holding the pen.

That single decision is the root of every defect found in the recon:

| observed | root |
|---|---|
| `git status --porcelain` in the dirty-marker check **never ran**, and the marker was pinned on for every repo | the body is shell, and frost does not expand `$(…)` inside double quotes — so `[ -n "$(git status --porcelain)" ]` tested a non-empty *literal*. No error, no diagnostic. |
| `FROST_CMD_DURATION_MS` was always a multiple of 1000, so `05-notify.lisp`'s threshold could not see a sub-second command | the body computed time by forking `date +%s`, which frost's own clock already knew to the nanosecond |
| `FROST_GIT_BRANCH` cost a `git branch` fork every prompt and had no reader anywhere in the fleet | a string coupling between a body that writes and a prompt that reads, checked by nobody |
| `.envrc` variables never applied at all, and every `cd` inside the org tree cost a 150 ms cold direnv reload instead of the 5 ms warm path | `chpwd_body: eval "$(direnv export bash …)"` — bash-format text eval'd by a zsh-semantics shell that does not implement `$'…'`, which is the quoting direnv emits for every assignment |
| `$?` is read *after* the hook body has started, so a failed command could render a success prompt | the body observes `$?`, and `$?` is a mutable global that anything can clobber |

Note the shape they share. None is a bug in the *logic* anyone wrote. Each is a
consequence of expressing cycle logic as **untyped text evaluated later**, where a
gap in the evaluator degrades into a plausible wrong answer instead of a refusal.

## 2. Gate 0 — the illegal states this domain admits

1. A beat computes, by forking, a fact the shell already holds.
2. A beat is free shell text, so an expander gap yields a wrong value rather than an error.
3. The exit status is observed after something has already reset it.
4. A duration crosses a boundary in a unit its name does not match.
5. A directory change applies foreign shell text through `eval`.
6. A beat publishes a variable nothing reads, or a consumer reads one nothing publishes.
7. The number of subprocesses per cycle drifts upward with nobody counting.
8. A beat or integration names an event/tool the shell does not implement.

## 3. The vocabulary

The move is to stop giving a beat a *body* and start giving it a closed list of
**acts**. The shell side becomes passthrough: it carries no logic at all.

```lisp
;; specs/meguri.lisp — the destination form
(defmeguri
  :beat  precmd
  :acts  ((publish FROST_CMD_DURATION_MS duration-ms)
          (publish SEKI_CMD_DURATION_MS  duration-ms)
          (publish FROST_LAST_EXIT       exit-status)
          (render  seki)))

(defmeguri
  :beat  chpwd
  :acts  ((apply-env-delta direnv-json)))
```

```rust
/// A beat of the cycle. Closed — frost fires exactly these three.
pub enum Beat { Preexec, Precmd, Chpwd }

/// A fact frost can answer from its own state, with NO subprocess.
///
/// There is deliberately no `Fact::Shell(String)`. A value that genuinely
/// requires a subprocess is not a Fact — it is an `Act::Spawn`, and spawning
/// is counted (§4). That absence is the whole seal on Gate-0 states 1 and 2:
/// there is no path from "publish a variable" to "run a command".
pub enum Fact {
    ExitStatus,   // i32  — captured before the first act of the beat runs
    DurationMs,   // u64  — monotonic, from the Instant frost already stamps
    Cwd, OldCwd,  // PathBuf
    JobCount,     // usize
    ShellPid,     // u32
}

/// What a beat may do. Closed, and free text is not a member.
pub enum Act {
    Publish { var: EnvVarName, fact: Fact },
    ApplyEnvDelta { provider: EnvDeltaProvider },
    Render { renderer: PromptRenderer },
    Spawn { cmd: SpawnSpec },   // argv, never a shell line
}

/// Native producers of an environment delta. No shell, no eval.
pub enum EnvDeltaProvider {
    /// `direnv export json` → parsed as JSON → applied as a typed delta.
    /// A JSON null means "unset this variable", which is the whole of the
    /// semantics `eval` was being used to get.
    DirenvJson,
}

pub enum PromptRenderer {
    Linked(LinkedRenderer),  // in-process
    Spawned(SpawnSpec),      // counted against the budget
}
```

`Fact::DurationMs` is the only duration in the vocabulary. There is no
`DurationSecs` to confuse it with, so Gate-0 state 4 has no way to arise — the unit
travels in the type, not in the variable's name.

`Fact::ExitStatus` is captured by frost **before** the beat's first act. An author
never writes `$?`, so there is no moment at which they can observe a clobbered one.
(This is not hypothetical: writing the interim shell version of this hook, a `:`
null-command placed above the capture reset `$?` to 0 — the class biting its author
inside the same hour it was documented.)

## 4. The spawn budget is derived, never declared

```rust
impl Meguri {
    /// Counts `Act::Spawn` plus `Render{ Spawned }`. Derived, so a declared
    /// budget cannot disagree with the acts it is supposed to bound — the
    /// `readOnly`-derived-option shape from the nix repo's fleet module,
    /// applied to a Rust border.
    pub fn spawn_cost(&self) -> u8 { … }
}
```

The point is not to forbid spawning — `seki` is a spawn today and will stay one
until the linked renderer is measured to be worth its coupling. The point is that
the count is a **property of the declaration**, visible in a catalog, rather than an
emergent fact nobody can see. Per-prompt subprocess count went 0 → 5 in this stack
without a single commit noting it.

## 5. Tier ledger — graded at the destination, honestly

<!-- tier-ledger -->

| bad state | how the vocabulary corners it | tier |
|---|---|---|
| a beat forks to compute a fact the shell holds | `Fact` has no `Shell` variant and `Publish` takes a `Fact` — no expressible path from publishing to executing | truly-unrep |
| a beat is free shell text | `Act` carries no body; the only text is `SpawnSpec`, which is argv | truly-unrep |
| `$?` observed after being clobbered | captured by frost before act 0; the author never names `$?` | truly-unrep |
| a duration in the wrong unit | `Fact::DurationMs` is the only duration; unit is in the type | truly-unrep |
| direnv applied by `eval` of foreign shell | `EnvDeltaProvider::DirenvJson` — typed JSON delta | truly-unrep |
| an unknown beat or integration name | closed enums; the loader `Err`s at the parse boundary | parse-time-rejected |
| per-cycle spawn drift | `spawn_cost()` derived from the acts, asserted against a ceiling | parse-time-rejected |
| a published var nothing reads / a consumer reading an unpublished var | a catalog cross-check test over declared publishers vs declared consumers — CI, not a type: the consumer may live in another repo (seki reads `SEKI_CMD_DURATION_MS`), and a cross-repo string coupling has no compile-time home here | only-mitigated (C2 — external observation; the check sees only consumers that declare themselves) |

Two rows deliberately do **not** claim more than they earn. The publish/consume
cross-check is the honest floor because the coupling genuinely crosses a repo
boundary — `seki-modules/src/cmd_duration.rs:44` reads an env var name, and no type
in frost can reach it. Naming that as `only-mitigated (C2)` rather than dressing it
as a seal is the difference between a vocabulary and a claim.

## 6. What this does NOT do

- It does not make seki fast. It makes seki's cost **visible and declared**; whether
  the renderer becomes linked is a separate, measured decision.
- It does not remove the `Spawn` escape hatch. A shell that cannot run a program is
  not a shell. It removes *unaccounted* spawning.
- It does not fix frost's expander gaps (`$(…)`/`$((…))` inside double quotes,
  `$'…'`, adjacent expansions in a bare assignment word). Those are real zsh-parity
  bugs and are being fixed on their own merit — meguri only means the shell's *own*
  cycle stops depending on them.
