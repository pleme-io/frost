//! Boot-path integration harness — drives every `frostmourne/lisp/*.lisp`
//! file through the canonical `shigoto::InProcessScheduler` so a hang in
//! any single rc-load form surfaces as a typed Job stuck in
//! `JobPhase::Running` past its per-Job timeout, not as "user sees an
//! empty screen and we don't know which form deadlocked."
//!
//! Spec backdrop: `theory/SHIGOTO.md` §III.1–III.6 + ★★ Shigoto
//! directive (org-level `pleme-io/CLAUDE.md`). Any work graph with ≥3
//! typed steps, retry/budget concerns, and observability requirements
//! expresses as a `shigoto::Dag` of `RecordingJob` impls — the
//! canonical model is `pleme-io/tend/src/jobs/status_repo.rs` (the
//! sequential per-repo classifier that this harness mirrors).
//!
//! ## What this proves
//!
//! For the current bundled-rc deadlock: the offending file lands in
//! `Failed { attempts: 1 }` (timeout dispatched via the FSM's
//! `Signal::Timeout`) and the test fails listing its filename. The
//! surrounding files that DID complete land in `Succeeded` so the
//! failure is localised to the breakage rather than aggregated as
//! "boot hung."
//!
//! ## How to run
//!
//! The bundled frostmourne checkout has to be adjacent on the
//! filesystem (`~/code/github/pleme-io/frostmourne/lisp/*.lisp`).
//! Nix builds vendor frostmourne into `/nix/store/...` at frost build
//! time so the path-relative discovery used here is intentionally
//! local-only. The test is `#[ignore]` by default so `cargo test`
//! stays green on hermetic builds:
//!
//! ```bash
//! cargo test --test boot_harness -- --ignored --nocapture
//! ```
//!
//! ## Sample passing output
//!
//! ```text
//! boot_harness::all_frostmourne_rc_files_apply_within_timeout ... ok
//!   discovered 22 rc files under .../frostmourne/lisp
//!   running InProcessScheduler tick loop (budget=1, per-job timeout=1000ms)
//!   tick 1: 22 transitions  (Pending→Ready, Ready→Running, Running→Succeeded)
//!   tick 2: 0 transitions   (steady state)
//!   all 22 BootStepJob instances reached JobPhase::Succeeded
//! ```
//!
//! ## Sample failing output (the bundled-rc deadlock case)
//!
//! ```text
//! boot_harness::all_frostmourne_rc_files_apply_within_timeout ... FAILED
//!
//! failed: 1 of 22 BootStepJob instances did not reach Succeeded:
//!   - 01-blzsh-parity.lisp  →  Failed { attempts: 1 } (timeout @ 1s)
//!
//! every other rc file completed cleanly, so the deadlock is
//! localised to the form(s) declared in 01-blzsh-parity.lisp —
//! grep that file for the most recent change.
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use frost_exec::ShellEnv;

use shigoto_budget::{BudgetSpec, BudgetTree};
use shigoto_dag::Dag;
use shigoto_emit::{NullEmitter, TransitionEmitter};
use shigoto_retry::RetryPolicy;
use shigoto_scheduler::{InProcessScheduler, Scheduler};
use shigoto_types::{
    Job, JobId, JobKindId, JobPhase, JobScope, JobSubject, OutputSink, RecordingJob,
};

/// Typed Output for `BootStepJob`. `frost_lisp::ApplySummary` is the
/// natural payload but doesn't impl `Clone`, which `RecordingJob`
/// requires so the blanket `Job::execute` can hand the value to a
/// `Sink` across an await boundary. We project the `ApplySummary`
/// into the three counters the harness actually asserts on (aliases,
/// hooks, binds) — every one a `usize`, trivially `Clone + Send + Sync`.
/// The full `ApplySummary` stays inside `execute_body`; the projection
/// is what crosses the typed sink boundary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct BootStepCounters {
    aliases: usize,
    hooks: usize,
    binds: usize,
}

// ─── BootStepJob ─────────────────────────────────────────────────────
//
// Mirrors the wrapper pattern from `tend/src/jobs/status_repo.rs` —
// a pure-data struct holding everything `execute_body` needs, plus a
// `RecordingJob` impl that defers the synchronous `frost_lisp::apply_source`
// call to `tokio::task::spawn_blocking` so the scheduler's async
// runtime stays responsive.
//
// One BootStepJob == one .lisp file == one rc-load form pass. The
// scheduler's per-Job timeout is what catches the hang case: if
// `apply_source` deadlocks inside a tatara-lisp form (the current
// bundled-rc bug), `tokio::time::timeout` dispatches `Signal::Timeout`
// and the Job transitions Running → Failed { attempts: 1 } — we then
// match against `JobPhase` for the offending file name.

const BOOT_STEP_KIND: &str = "frost.boot-step";

#[derive(Clone)]
struct BootStepJob {
    name: &'static str,
    lisp_source: String,
}

impl std::fmt::Debug for BootStepJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootStepJob")
            .field("name", &self.name)
            .field("lisp_source_bytes", &self.lisp_source.len())
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
enum BootStepError {
    #[error("frost_lisp::apply_source failed for {file}: {source}")]
    Apply {
        file: &'static str,
        source: frost_lisp::LispError,
    },
    #[error("spawn_blocking join error for {file}: {source}")]
    Join {
        file: &'static str,
        source: tokio::task::JoinError,
    },
}

#[async_trait]
impl RecordingJob for BootStepJob {
    type Output = BootStepCounters;
    type Error = BootStepError;
    const KIND: &'static str = BOOT_STEP_KIND;

    fn scope(&self) -> JobScope {
        // Every boot step belongs to the synthetic "frostmourne" workspace.
        // Pinning a JobScope here mirrors tend's per-workspace scoping
        // and keeps the BudgetTree's by-scope dimension addressable.
        JobScope::Workspace("frostmourne".to_string())
    }

    fn subject(&self) -> JobSubject {
        // The .lisp filename is the canonical identifier; using
        // `JobSubject::Pinned` (free-form string) over `JobSubject::Path`
        // because the static `&'static str` survives intact through
        // serde without filesystem-path quirks (Path goes through PathBuf
        // which is platform-dependent in serialization).
        JobSubject::Pinned(self.name.to_string())
    }

    fn output_sink(&self) -> Option<&Arc<dyn OutputSink<Self::Output>>> {
        // No sink — the test asserts via the scheduler's `snapshot()`,
        // not via accumulated outputs. Keeping the typed surface
        // explicit so future consumers know where to thread one in.
        None
    }

    async fn execute_body(&self) -> Result<BootStepCounters, BootStepError> {
        let file = self.name;
        let source = self.lisp_source.clone();
        let summary = tokio::task::spawn_blocking(move || {
            let mut env = ShellEnv::new();
            frost_lisp::apply_source(&source, &mut env)
        })
        .await
        .map_err(|join| BootStepError::Join {
            file,
            source: join,
        })?
        .map_err(|err| BootStepError::Apply { file, source: err })?;
        Ok(BootStepCounters {
            aliases: summary.aliases,
            hooks: summary.hooks,
            binds: summary.binds,
        })
    }
}

// ─── Test scaffolding ────────────────────────────────────────────────

/// Resolve `../frostmourne/lisp/` relative to the frost crate root.
/// `CARGO_MANIFEST_DIR` is the package dir (`crates/frost/`); two
/// `..` hops climb to `frost/`, where `../frostmourne/` is the
/// sibling checkout.
fn frostmourne_lisp_dir() -> PathBuf {
    let manifest_dir = option_env!("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is always set by cargo at compile time");
    Path::new(manifest_dir)
        .join("..") // crates/
        .join("..") // frost/
        .join("..") // pleme-io/
        .join("frostmourne")
        .join("lisp")
}

/// Read every `*.lisp` file under `frostmourne/lisp/`, sorted by
/// filename so the boot order is deterministic (frostmourne's filenames
/// are intentionally numerically prefixed: `00-core.lisp`,
/// `01-blzsh-parity.lisp`, …).
fn discover_rc_files(dir: &Path) -> std::io::Result<Vec<(&'static str, PathBuf)>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "lisp"))
        .collect();
    entries.sort();

    // Leak the filename strings to satisfy the `&'static str` bound on
    // BootStepJob::name. Negligible test-only allocation; mirrors the
    // pattern other shigoto consumers use for static job-naming.
    Ok(entries
        .into_iter()
        .map(|p| {
            let leaked: &'static str = Box::leak(
                p.file_name()
                    .expect("path came from read_dir")
                    .to_string_lossy()
                    .into_owned()
                    .into_boxed_str(),
            );
            (leaked, p)
        })
        .collect())
}

/// Sequentially drive the scheduler to steady state. Capped at a
/// generous tick count (#files × 8) so a truly stuck Job can't loop
/// forever — at the cap we break and let the assertion phase surface
/// the offender. Mirrors the bounded-tick pattern from
/// `tend/src/jobs/pull_repo.rs`.
async fn tick_to_steady_state(
    scheduler: &InProcessScheduler,
    dag: &mut Dag,
    file_count: usize,
) -> Result<(), shigoto_scheduler::SchedulerError> {
    let max_ticks = (file_count * 8).max(16);
    for _ in 0..max_ticks {
        let receipt = scheduler.tick(dag).await?;
        if receipt.transitions_this_tick.is_empty() {
            return Ok(());
        }
    }
    Ok(())
}

/// Bundled rc lives in nix-store at build time; this test only runs
/// locally with the frostmourne checkout adjacent on disk. Marked
/// `#[ignore]` so hermetic CI builds stay green. Run with:
///
/// ```bash
/// cargo test --test boot_harness -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires sibling frostmourne checkout; run with --ignored"]
async fn all_frostmourne_rc_files_apply_within_timeout() {
    let lisp_dir = frostmourne_lisp_dir();
    assert!(
        lisp_dir.is_dir(),
        "frostmourne lisp dir not found at {} — ensure the frostmourne \
         checkout is adjacent to frost (~/code/github/pleme-io/frostmourne/)",
        lisp_dir.display()
    );

    let files = discover_rc_files(&lisp_dir).expect("read_dir failed on frostmourne/lisp/");
    assert!(
        !files.is_empty(),
        "no *.lisp files found under {} — the harness has nothing to verify",
        lisp_dir.display()
    );

    eprintln!(
        "  discovered {} rc files under {}",
        files.len(),
        lisp_dir.display()
    );

    // ── Build one BootStepJob per file + register with the scheduler.
    //
    // BudgetSpec::max_concurrent(1) keeps boot strictly sequential —
    // this matches what `frost/src/main.rs` actually does at startup
    // (no concurrent rc-form application) so a hang in any single file
    // reproduces faithfully. Per-Job 1-second timeout via
    // shigoto-scheduler's `set_timeout` is the canonical Signal::Timeout
    // entry point.
    let emitter: Arc<dyn TransitionEmitter> = Arc::new(NullEmitter::new());
    let scheduler = InProcessScheduler::new("frostmourne-boot").with_emitter(emitter);

    let mut budget = BudgetTree::new();
    budget.global = Some(BudgetSpec::max_concurrent(1));
    scheduler.install_budget(budget).await;

    // RetryPolicy::NoRetry — a hang isn't transient, it's a bug in the
    // rc form. The first Failed transition routes directly to
    // Deadlettered; the test then matches against either
    // Failed { attempts: 1 } or Deadlettered for the offender.
    scheduler
        .register_retry_policy(JobKindId::new(BOOT_STEP_KIND), RetryPolicy::NoRetry)
        .await;

    let mut dag = Dag::new();
    let mut ids: Vec<(&'static str, JobId)> = Vec::with_capacity(files.len());

    for (name, path) in &files {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read_to_string {}: {e}", path.display()));
        let job = Arc::new(BootStepJob {
            name,
            lisp_source: source,
        });
        let id = <BootStepJob as Job>::id(&job);

        scheduler.register_job(job).await;
        scheduler
            .set_timeout(id.clone(), Duration::from_secs(1))
            .await;
        dag.ensure_node(id.clone());
        ids.push((name, id));
    }

    eprintln!(
        "  running InProcessScheduler tick loop (budget=1, per-job timeout=1000ms)"
    );

    tick_to_steady_state(&scheduler, &mut dag, files.len())
        .await
        .expect("scheduler tick failed");

    // ── Assert every BootStepJob reached JobPhase::Succeeded.
    //
    // Phase taxonomy at the assert point:
    //   Succeeded                — file loaded cleanly        (pass)
    //   Failed { .. }            — apply_source returned Err  (typed bug)
    //   Deadlettered             — NoRetry routed Failed here (typed bug)
    //   Running                  — never completed             (THE HANG)
    //   anything else            — illegal state               (regression)
    //
    // The hang case is the load-bearing one this harness exists for:
    // before the per-Job timeout existed, a deadlocked rc form left the
    // scheduler spinning. With it, Running gets converted to Failed and
    // surfaces here with a concrete filename.
    let snap = scheduler.snapshot(&dag).await;
    let mut offenders: Vec<(&'static str, JobPhase)> = Vec::new();
    for (name, id) in &ids {
        match snap.phases.get(id) {
            Some(JobPhase::Succeeded) => continue,
            Some(other) => offenders.push((name, other.clone())),
            None => offenders.push((name, JobPhase::Pending)),
        }
    }

    assert!(
        offenders.is_empty(),
        "failed: {} of {} BootStepJob instances did not reach Succeeded:\n{}",
        offenders.len(),
        ids.len(),
        offenders
            .iter()
            .map(|(name, phase)| format!("  - {name:<32}  →  {phase:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    eprintln!(
        "  all {} BootStepJob instances reached JobPhase::Succeeded",
        ids.len()
    );
}
