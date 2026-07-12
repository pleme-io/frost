//! L0 persona PTY harness — the REAL frost binary under a typed fake terminal.
//!
//! M0 of the terminal integration-test plan, now espelho-native: the typed
//! host surface (`espelho::{AnswerPolicy, TerminalPersona, VtQuery,
//! VtAnswer}`) owns the query catalog, the rolling wire scan, the answer
//! wires, and the policy algebra; this file keeps only the PTY plumbing.
//! A `TerminalPersona` owns the master side of a real PTY and answers (or
//! refuses to answer) frost's VT queries per its typed `AnswerPolicy`,
//! while a timed script injects keystrokes. Each test asserts *liveness
//! invariants*, not mechanisms — they hold regardless of how
//! reedline/crossterm evolve.
//!
//! Deliberately NOT espelho's `apply()` interpreter: its `busy_wait`
//! drains-and-discards env reads during answer latency, which would drop
//! exactly the inter-query traffic (seki repaint between CPR query and
//! answer) the E5 rows exist to exercise. The local poll loop sleeps
//! through latency instead, so the rolling scan never loses bytes —
//! equal-strength assertions, typed surface upstream. (Seam noted for
//! espelho: a latency model that keeps reading without discarding.)
//!
//! Incidents codified (2026-06-10):
//! - E2: a mute terminal (never answers `ESC[6n`) fatally killed the shell at
//!   ~2.1s ("frost: read error: … cursor position …"). → `mute_dsr_never_fatal`.
//! - E5/S2: with a PROMPTLY-ANSWERING terminal, the first accept-line (Enter)
//!   triggered a CPR answer-loss race in the reedline/crossterm event path and
//!   the shell died ~2s later. The REPL's bounded CPR retry recovers by
//!   re-querying on a quiet event queue. → `post_accept_line_survives_and_executes`.
//! - E1-harness fragility: a per-chunk (non-rolling) `ESC[6n` scan misses
//!   queries split across reads. → `split_escapes_roundtrip` answers with the
//!   reply itself fragmented, proving frost tolerates fragmented answers.
//! - Host-died-mid-session: a host that answers, then goes permanently mute
//!   (mado's attach thread exiting, a daemon restart) must degrade to the
//!   mute contract, not kill the guest. → `answer_then_mute_midsession_survives`.
//! - fd-exhaustion / kernel-exit-teardown stall (2026-07-12): the SIGTERM
//!   regression test (`interactive_session_terminates_on_sigterm_with_no_trap`)
//!   appeared to hang under this file's default parallel execution. Real,
//!   evidenced findings, none of them a defect in this file's fork/exec/
//!   signal code or in frost's production signal handling: (1) the
//!   workstation it was authored on was independently carrying 62
//!   orphaned PRE-FIX frost processes (the very bug the test guards
//!   against) that had driven the kernel's system-wide file table to its
//!   ceiling — cleared, unrelated to the test's own logic; (2) live
//!   `ps -o state` sampling through reproduced stalls caught the SIGTERM'd
//!   child sitting in macOS's `Es` ("trying to exit") state at 0% CPU for
//!   a long time (120s+ observed) before the kernel posted its exit
//!   status, correlated with a `taskgated-helper` process spinning up — a
//!   kernel-level delay finalizing an externally-signaled process's exit,
//!   NOT bounded by any timeout tried. Precisely characterized by
//!   invocation shape: `cargo test -p frost --test persona_pty` (this
//!   file alone) was 100% reliable across 15+ runs in every configuration;
//!   `cargo test --workspace` (60+ prior frost subprocess spawns from
//!   other test files) reproduced the stall 4/4 attempts. See
//!   `interactive_session_terminates_on_sigterm_with_no_trap`'s doc
//!   comment for the full account, the bounded watchdog shipped as
//!   honest defense-in-depth (not a claimed fix), and the recommended
//!   invocation for a trustworthy signal.
//!
//! The no-freeze-under-mute invariant went LIVE with the pleme-io reedline
//! fork (CPR as optimization, never a liveness dependency): under Mute the
//! painter falls back to a safe row, the prompt paints, and commands round-
//! trip. Residual (#[ignore]d): fd0-TARGETING builtin redirects cycle fd 0 and
//! kill the crossterm/mio kqueue registration — an INPUT-liveness class (no
//! keystrokes at all, CPR irrelevant) needing an event-source reset upstream.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use espelho::{AnswerPolicy, TerminalPersona, VtQuery};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};

/// The harness persona: espelho policy + the cursor position the old
/// hand-rolled harness always reported (`ESC[25;1R`) — identical wire.
fn persona(policy: AnswerPolicy) -> TerminalPersona {
    TerminalPersona {
        policy,
        report_row: 25,
        report_col: 1,
    }
}

/// A timed keystroke injection.
struct Send {
    at: Duration,
    bytes: &'static [u8],
}

/// Everything observed from one driven session.
struct Outcome {
    transcript: Vec<u8>,
    cpr_queries: usize,
    read_error: bool,
    alive_at_end: bool,
}

/// Drive the real `frost` binary on a real PTY under `persona` for `total`,
/// injecting `script` keystrokes at their offsets. `rc` (when set) becomes
/// the child's FROSTRC so personas can exercise rc-declared behavior
/// hermetically. Returns the observation; never panics on session mechanics
/// (assertions live in the tests).
fn drive(
    persona: TerminalPersona,
    script: &[Send],
    total: Duration,
    rc: Option<&std::path::Path>,
) -> Option<Outcome> {
    use nix::pty::ForkptyResult;
    use std::ffi::CString;

    let home = tempfile::tempdir().expect("tempdir");
    let exe = CString::new(env!("CARGO_BIN_EXE_frost")).unwrap();
    let argv = [exe.clone()];
    let mut envp: Vec<CString> = vec![
        CString::new("TERM=xterm-256color").unwrap(),
        CString::new(format!("HOME={}", home.path().display())).unwrap(),
        CString::new("PATH=/usr/bin:/bin").unwrap(),
    ];
    if let Some(rc) = rc {
        envp.push(CString::new(format!("FROSTRC={}", rc.display())).unwrap());
    }

    // SAFETY: the child performs only async-signal-safe operations between
    // fork and exec (execve / _exit), per fork-in-threaded-process rules.
    let fork = unsafe { nix::pty::forkpty(None, None) };
    let (child, master) = match fork {
        Ok(ForkptyResult::Parent { child, master }) => (child, master),
        Ok(ForkptyResult::Child) => {
            let _ = nix::unistd::execve(&exe, &argv, &envp);
            unsafe { libc::_exit(127) };
        }
        Err(e) => {
            eprintln!("SKIP persona_pty: forkpty unavailable in this environment: {e}");
            return None;
        }
    };

    // Non-blocking master so the poll loop owns all timing.
    nix::fcntl::fcntl(
        master.as_raw_fd(),
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .expect("set O_NONBLOCK on pty master");
    let mut master = std::fs::File::from(master);

    let mut transcript: Vec<u8> = Vec::new();
    let mut scan = 0usize; // rolling VT-query scan cursor (never per-chunk)
    let mut queries_seen = 0u32; // all kinds — drives AnswerThenMute's cutover
    let mut cpr_queries = 0usize;
    let mut sent = vec![false; script.len()];
    let mut alive = true;
    let start = Instant::now();

    while start.elapsed() < total {
        let mut buf = [0u8; 4096];
        match master.read(&mut buf) {
            Ok(0) => {
                alive = false;
                break;
            }
            Ok(n) => transcript.extend_from_slice(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => {
                alive = false;
                break;
            }
        }
        // Rolling scan: answer every complete VT query found so far, per
        // the persona's typed policy (espelho owns catalog + wires).
        while let Some((query, end)) = VtQuery::scan(&transcript, scan) {
            scan = end;
            queries_seen += 1;
            if query == VtQuery::CursorPosition {
                cpr_queries += 1;
            }
            if let Some(answer) = persona.answer_for(query, queries_seen) {
                let wire = answer.wire();
                match persona.policy {
                    AnswerPolicy::SplitReply { gap } => {
                        let mid = wire.len() / 2;
                        let _ = master.write_all(&wire[..mid]);
                        std::thread::sleep(gap);
                        let _ = master.write_all(&wire[mid..]);
                    }
                    _ => {
                        std::thread::sleep(persona.latency());
                        let _ = master.write_all(&wire);
                    }
                }
            }
        }
        for (i, s) in script.iter().enumerate() {
            if !sent[i] && start.elapsed() >= s.at {
                let _ = master.write_all(s.bytes);
                sent[i] = true;
            }
        }
        if let Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) =
            waitpid(child, Some(WaitPidFlag::WNOHANG))
        {
            alive = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    if alive {
        let _ = kill(child, Signal::SIGKILL);
        let _ = waitpid(child, None);
    }
    let read_error = find(&transcript, b"read error").is_some();
    Some(Outcome {
        transcript,
        cpr_queries,
        read_error,
        alive_at_end: alive,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Healthy terminal, idle shell: stays alive, no read errors. (Baseline.)
#[test]
fn answers_dsr_idle_stays_alive() {
    let Some(o) = drive(
        persona(AnswerPolicy::Answer {
            // A REAL terminal answers with latency (mado's full
            // engate→VT→send_keys loop measured ~tens of ms) — and the
            // 2026-06-10 answer-loss race only triggers when the answer
            // lands AFTER reedline's filtered read window. latency=0
            // would test an unrealistically instant terminal and miss it.
            latency: Duration::from_millis(50),
        }),
        &[],
        Duration::from_secs(2),
        None,
    ) else {
        return;
    };
    assert!(
        o.alive_at_end && !o.read_error,
        "idle shell under a healthy terminal must stay alive: alive={} read_error={} cpr={}",
        o.alive_at_end,
        o.read_error,
        o.cpr_queries
    );
}

/// THE 2026-06-10 regression invariant: accept-line (Enter) must never kill
/// the shell, and a subsequent command must execute. Old frost died ~2s after
/// Enter under the full frostmourne distribution (seki prompt repaint traffic
/// between the CPR query and its answer triggers the reedline/crossterm
/// answer-loss race; 50ms answer latency reproduced it 5/5).
///
/// TIER-HONEST: bare frost's lighter prompt does not reproduce the race, so
/// on pre-retry frost this row passes — its teeth for the E5 class arrive at
/// M1 when the persona drives the full frostmourne rc (boot_harness-style
/// local discovery). Today this row guards the bare-frost invariant; the
/// fatal-CPR class is covered with proven teeth by `mute_dsr_never_fatal`
/// (fails on pre-retry frost).
#[test]
fn post_accept_line_survives_and_executes() {
    let script = [
        Send {
            at: Duration::from_millis(800),
            bytes: b"\r",
        },
        // Past the worst-case single CPR-timeout window (~2s) so the
        // command lands after recovery, not during it.
        Send {
            at: Duration::from_millis(3300),
            bytes: b"echo MARKER_E5\r",
        },
    ];
    let Some(o) = drive(
        persona(AnswerPolicy::Answer {
            // 50ms reproduced the answer-loss death 100% of the time on
            // pre-retry frost (5/5 sessions, 2026-06-10 evidence runs).
            latency: Duration::from_millis(50),
        }),
        &script,
        Duration::from_secs(6),
        None,
    ) else {
        return;
    };
    // The echoed input contains the literal; require the OUTPUT line too
    // (the marker appearing at least twice: echo-back + execution).
    let ran = count(&o.transcript, b"MARKER_E5") >= 2;
    assert!(
        o.alive_at_end && ran,
        "shell must survive accept-line and execute afterwards: alive={} ran={} read_error={} cpr={} tail={:?}",
        o.alive_at_end,
        ran,
        o.read_error,
        o.cpr_queries,
        String::from_utf8_lossy(&o.transcript[o.transcript.len().saturating_sub(200)..])
    );
}

/// E2/E4 codified, full strength: a terminal that never answers CPR must not
/// kill OR freeze the shell — the prompt proceeds (reedline-fork painter
/// fallback) and a command still round-trips. Old frost died at ~2.1s;
/// retry-only frost survived but ate input; the fork executes.
#[test]
fn mute_dsr_never_fatal() {
    let script = [Send {
        at: Duration::from_millis(2500),
        bytes: b"echo MARKER_MUTE\r",
    }];
    // Wide window: under Mute every remaining tolerant cursor::position()
    // site still blocks ~2s before its fallback, so the round-trip lands
    // ~6s in. Slow-but-usable is the contract for a hostile terminal.
    let Some(o) = drive(
        persona(AnswerPolicy::Mute),
        &script,
        Duration::from_secs(12),
        None,
    ) else {
        return;
    };
    let ran = count(&o.transcript, b"MARKER_MUTE") >= 2;
    assert!(
        o.alive_at_end && ran && !o.read_error,
        "mute-DSR terminal must be fully usable (no-freeze): alive={} ran={} read_error={} cpr={}",
        o.alive_at_end,
        ran,
        o.read_error,
        o.cpr_queries
    );
}

/// The "host died mid-session" class (mado's attach thread exiting, a
/// daemon restart): a persona that answers the first 2 queries then goes
/// permanently mute. The shell starts life under a healthy terminal, loses
/// it MID-SESSION, and must degrade to the mute contract — alive, prompt
/// proceeds via the painter fallback, and a command still round-trips
/// after the recovery window. Espelho's `AnswerThenMute` policy is the
/// typed surface for exactly this cutover.
#[test]
fn answer_then_mute_midsession_survives_and_executes() {
    let script = [
        // Accept-line while the host is (still) answering — repaint
        // traffic burns through the answered budget so the mute cutover
        // lands mid-session, not at boot.
        Send {
            at: Duration::from_millis(800),
            bytes: b"\r",
        },
        // Past the worst-case CPR-timeout window after the cutover so the
        // command lands in the degraded-but-usable regime.
        Send {
            at: Duration::from_millis(4000),
            bytes: b"echo MARKER_ATM\r",
        },
    ];
    // Same wide window as the mute row: post-cutover cursor::position()
    // sites block ~2s each before their fallback.
    let Some(o) = drive(
        persona(AnswerPolicy::AnswerThenMute {
            n: 2,
            latency: Duration::from_millis(20),
        }),
        &script,
        Duration::from_secs(12),
        None,
    ) else {
        return;
    };
    let ran = count(&o.transcript, b"MARKER_ATM") >= 2;
    assert!(
        o.alive_at_end && ran && !o.read_error,
        "host death mid-session must degrade to the mute contract: alive={} ran={} read_error={} cpr={}",
        o.alive_at_end,
        ran,
        o.read_error,
        o.cpr_queries
    );
}

/// Terminals may fragment an escape reply across writes; frost must
/// reassemble and proceed (command round-trip proves the whole loop).
#[test]
fn split_escapes_roundtrip() {
    let script = [Send {
        at: Duration::from_millis(800),
        bytes: b"echo MARKER_SPLIT\r",
    }];
    let Some(o) = drive(
        persona(AnswerPolicy::SplitReply {
            gap: Duration::from_millis(10),
        }),
        &script,
        Duration::from_secs(5),
        None,
    ) else {
        return;
    };
    let ran = count(&o.transcript, b"MARKER_SPLIT") >= 2;
    assert!(
        o.alive_at_end && ran,
        "fragmented CPR replies must still be consumed: alive={} ran={} cpr={}",
        o.alive_at_end,
        ran,
        o.cpr_queries
    );
}

/// 2026-06-10 part 2 — the defnotify freeze: a `(defnotify …)` rc form
/// synthesizes a precmd hook whose body runs through the command engine
/// before EVERY prompt. Root cause: the in-process builtin-redirect
/// save/restore cycled fd 0 through dup2 even for `2>/dev/null`, and the
/// momentary close deleted crossterm/mio's kqueue registration for the tty
/// — every later CPR read timed out (alive-but-frozen, any DSR latency).
/// Fixed by `redirect::touched_std_fds` (back up only the touched fds).
#[test]
fn notify_hook_must_not_break_cpr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rc = dir.path().join("rc.lisp");
    std::fs::write(
        &rc,
        "(defnotify :threshold-ms 30000 :title \"t\" :message \"m\")\n",
    )
    .unwrap();
    let script = [
        Send {
            at: Duration::from_millis(1200),
            bytes: b"\r",
        },
        Send {
            at: Duration::from_millis(3600),
            bytes: b"echo MARKER_NOTIFY\r",
        },
    ];
    let Some(o) = drive(
        persona(AnswerPolicy::Answer {
            latency: Duration::from_millis(0),
        }),
        &script,
        Duration::from_secs(7),
        Some(&rc),
    ) else {
        return;
    };
    let ran = count(&o.transcript, b"MARKER_NOTIFY") >= 2;
    assert!(
        o.alive_at_end && ran,
        "defnotify precmd must not wedge the post-accept CPR: alive={} ran={} cpr={}",
        o.alive_at_end,
        ran,
        o.cpr_queries
    );
}

/// Minimal repro of the kqueue-deletion wedge: a BUILTIN with a non-fd0
/// redirect must not affect the next prompt (externals fork, so only the
/// in-process path is at risk). Failed before `touched_std_fds`.
#[test]
fn builtin_redirect_must_not_wedge_next_prompt() {
    let script = [
        Send {
            at: Duration::from_millis(1200),
            bytes: b"true 2>/dev/null\r",
        },
        Send {
            at: Duration::from_millis(3600),
            bytes: b"echo MARKER_REDIR\r",
        },
    ];
    let Some(o) = drive(
        persona(AnswerPolicy::Answer {
            latency: Duration::from_millis(20),
        }),
        &script,
        Duration::from_secs(7),
        None,
    ) else {
        return;
    };
    let ran = count(&o.transcript, b"MARKER_REDIR") >= 2;
    assert!(
        o.alive_at_end && ran,
        "builtin 2>/dev/null must not wedge the next prompt: alive={} ran={} cpr={}",
        o.alive_at_end,
        ran,
        o.cpr_queries
    );
}

/// CLOSED 2026-06-10: fd0-targeting builtin redirects cycle fd 0, which
/// used to delete crossterm/mio's kqueue registration (input-deaf forever).
/// The reedline fork now enables crossterm's `use-dev-tty` poll(2) source —
/// pollfds are rebuilt per read, so there is no persistent registration to
/// lose when an fd instance cycles. This row is the proof.
#[test]
fn builtin_stdin_redirect_must_not_wedge_next_prompt() {
    let script = [
        Send {
            at: Duration::from_millis(1200),
            bytes: b"true </dev/null\r",
        },
        Send {
            at: Duration::from_millis(3600),
            bytes: b"echo MARKER_FD0\r",
        },
    ];
    let Some(o) = drive(
        persona(AnswerPolicy::Answer {
            latency: Duration::from_millis(20),
        }),
        &script,
        Duration::from_secs(7),
        None,
    ) else {
        return;
    };
    let ran = count(&o.transcript, b"MARKER_FD0") >= 2;
    assert!(
        o.alive_at_end && ran,
        "builtin </dev/null wedges the next prompt (fd0 cycled): alive={} ran={} cpr={}",
        o.alive_at_end,
        ran,
        o.cpr_queries
    );
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while let Some(p) = find(&haystack[i..], needle) {
        n += 1;
        i += p + needle.len();
    }
    n
}

/// E5-faithful row: the FULL frostmourne rc (seki prompt, hooks, notify,
/// bindings — the repaint traffic that made the answer-loss race and the
/// kqueue wedge reproducible) driven under a realistic-latency persona.
/// Local-only discovery (boot_harness pattern): builds the rc aggregate
/// from the adjacent frostmourne checkout's lisp/*.lisp in lexical order;
/// SKIPs when the checkout is absent (hermetic builds).
#[test]
fn frostmourne_rc_post_accept_survives_and_executes() {
    let home = std::env::var("HOME").unwrap_or_default();
    let lisp_dir = std::path::PathBuf::from(home).join("code/github/pleme-io/frostmourne/lisp");
    if !lisp_dir.is_dir() {
        eprintln!("SKIP frostmourne_rc row: no local frostmourne checkout at {}", lisp_dir.display());
        return;
    }
    let mut files: Vec<_> = std::fs::read_dir(&lisp_dir)
        .expect("read lisp dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "lisp"))
        .collect();
    files.sort();
    let mut rc = String::new();
    for f in &files {
        rc.push_str(&std::fs::read_to_string(f).expect("read rc file"));
        rc.push('\n');
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let rc_path = dir.path().join("rc.lisp");
    std::fs::write(&rc_path, rc).unwrap();

    let script = [
        Send {
            at: Duration::from_millis(2000),
            bytes: b"\r",
        },
        Send {
            at: Duration::from_millis(5000),
            bytes: b"echo MARKER_FM_RC\r",
        },
    ];
    let Some(o) = drive(
        persona(AnswerPolicy::Answer {
            // The latency that reproduced the answer-loss death 5/5 on
            // pre-fix frost under this exact rc.
            latency: Duration::from_millis(50),
        }),
        &script,
        Duration::from_secs(12),
        Some(&rc_path),
    ) else {
        return;
    };
    let ran = count(&o.transcript, b"MARKER_FM_RC") >= 2;
    assert!(
        o.alive_at_end && ran && !o.read_error,
        "full frostmourne rc must survive accept-line and execute: alive={} ran={} read_error={} cpr={}",
        o.alive_at_end,
        ran,
        o.read_error,
        o.cpr_queries
    );
}

/// Regression test for the 2026-07-10 fd-exhaustion incident: an
/// interactive frost session (a real PTY, `install_signal_traps()`'s
/// actual invocation path — the `-c`/script paths never call it, so
/// they were never affected) with no rc-authored `(deftrap :signal
/// TERM ...)` must still terminate on SIGTERM. Before the fix,
/// `check_pending_traps` recorded the signal and did nothing further
/// when no trap function existed — 66 orphaned frost processes
/// accumulated this way over one long session, ignored plain `pkill`,
/// and required SIGKILL, each holding ~25,000 fds (a fleet-wide
/// file-descriptor exhaustion incident). Minimal spawn — no persona/
/// VT-query machinery needed, just: interactive session up, SIGTERM,
/// bounded wait for exit.
///
/// 2026-07-12 investigation (was `#[ignore]`d as "hangs under parallel
/// execution, root cause not yet isolated"). Multiple real findings,
/// none of them a defect in this test's fork/exec/signal-handling logic
/// nor in frost's production fix:
///
/// 1. The workstation this was verified on (`ryn`) was independently
///    carrying 62 ORPHANED frost processes at the time — confirmed
///    `PPID == 1`, every one still running the PRE-FIX binary this very
///    test guards against — that had driven the kernel's system-wide
///    file table to its ceiling (`sysctl kern.num_files` at 16,776,780
///    of a 16,777,216 `kern.maxfiles`, with `bash`/`cargo`/`ld` all
///    failing outright on a literal `ENFILE`). Clearing those 62 orphans
///    was necessary before anything in this file would even build. The
///    orphans exist *because* the deployed system binary predates the
///    2026-07-10 fix (`5214254`); a `nix run .#rebuild` on `ryn` retires
///    that legacy accumulation.
///
/// 2. Independent of (1) — the real, still-open finding. Live
///    `ps -o state` sampling through multiple reproduced stalls caught
///    the SIGTERM'd child sitting in macOS's `Es` ("trying to exit")
///    state at 0% CPU for a long time — the parent's non-blocking
///    `waitpid` is correct and simply hasn't been told by the kernel
///    that the child is done yet; this is a kernel-level exit-teardown
///    delay, not a userspace bug. A `taskgated-helper` process was
///    observed spinning up at the same moment. A targeted warm-up (pay
///    the exact fork+exec+SIGTERM+wait cycle once, synchronously, up
///    front) was tried and did NOT eliminate it, so it isn't a simple
///    per-binary validation cache.
///
///    Precisely characterized by invocation shape, not just "after a
///    rebuild": `cargo test -p frost --test persona_pty` (this file
///    alone, the way the fix's own regression-test instructions name)
///    was **100% reliable** across 15+ runs in every configuration tried
///    — isolated, `--test-threads=1`, the full 10-test default-parallel
///    file, doubled to 20 concurrent forkpty sessions across two
///    processes, immediately after a fresh rebuild, repeatedly. `cargo
///    test --workspace` (which runs `integration.rs`, `frostmourne_rc.rs`,
///    and `param_expansion.rs` — collectively 60+ frost subprocess
///    spawns — immediately before this file) reproduced the stall on
///    every attempt (4/4), with the child observed still in `Es` past
///    120s wall-clock before being force-killed. The most plausible
///    mechanism: those 60+ prior frost spawns queue enough signature-
///    validation work (each one an exec of the same not-yet-fully-
///    trusted debug binary) that by the time this test's own
///    externally-signaled child needs the kernel to finalize its exit,
///    the backlog hasn't drained — and, per the isolated-run evidence
///    above, no timeout this file can pick is guaranteed to outwait it.
///
/// Given (2) is real, evidenced, but NOT closed — no timeout tried here
/// (including 120s) reliably bounds the `--workspace` case, so this is
/// reported honestly rather than papered over with a bigger number —
/// this test ships un-ignored (the direct, targeted invocation is 100%
/// reliable, and the invariant it guards is real and worth covering)
/// wrapped in a bounded watchdog on a background thread, so a stall
/// fails THIS test alone with a diagnostic instead of wedging the whole
/// binary or CI job forever. Practical guidance: run this test via
/// `cargo test -p frost --test persona_pty` (with or without a name
/// filter) when you need a trustworthy signal; a `cargo test --workspace`
/// run tripping this one test specifically is a known, tracked,
/// open environment issue on heavily-loaded macOS hosts — check that
/// before treating it as a regression. Follow-up worth doing later:
/// `dtrace`/`log stream`-level tracing of `amfid`/`taskgated` during a
/// live `--workspace` repro to pin the exact kernel-side queue.
#[test]
fn interactive_session_terminates_on_sigterm_with_no_trap() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(sigterm_no_trap_probe());
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => panic!("{msg}"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
            "interactive_session_terminates_on_sigterm_with_no_trap: probe \
             thread did not complete within 30s (normal runtime is well \
             under 5s). This is a KNOWN, tracked macOS kernel-exit-teardown \
             stall (see this test's doc comment) most reliably triggered by \
             `cargo test --workspace` -- rerun via `cargo test -p frost \
             --test persona_pty` alone, which was 100% reliable in this \
             investigation, before treating this as a real regression."
        ),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!(
            "interactive_session_terminates_on_sigterm_with_no_trap: probe \
             thread panicked without sending a result"
        ),
    }
}

/// The probe body proper, run on a background thread by the `#[test]`
/// above so a stall anywhere in this path fails loudly via the
/// watchdog's `recv_timeout` instead of hanging the whole test binary.
/// Returns `Ok(())` for both "exited as expected" and "forkpty
/// unavailable, skipped" (matching `drive`'s SKIP convention above);
/// `Err(String)` carries a diagnostic for the outer test to panic with.
fn sigterm_no_trap_probe() -> Result<(), String> {
    use nix::pty::ForkptyResult;
    use std::ffi::CString;

    let home = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let exe = CString::new(env!("CARGO_BIN_EXE_frost")).unwrap();
    let argv = [exe.clone()];
    let envp: Vec<CString> = vec![
        CString::new("TERM=xterm-256color").unwrap(),
        CString::new(format!("HOME={}", home.path().display())).unwrap(),
        CString::new("PATH=/usr/bin:/bin").unwrap(),
        // No FROSTRC -- deliberately no user trap of any kind.
    ];

    // SAFETY: same fork-in-threaded-process contract as `drive` above --
    // only async-signal-safe operations between fork and exec.
    let fork = unsafe { nix::pty::forkpty(None, None) };
    let (child, master) = match fork {
        Ok(ForkptyResult::Parent { child, master }) => (child, master),
        Ok(ForkptyResult::Child) => {
            let _ = nix::unistd::execve(&exe, &argv, &envp);
            unsafe { libc::_exit(127) };
        }
        Err(e) => {
            eprintln!(
                "SKIP interactive_session_terminates_on_sigterm_with_no_trap: \
                 forkpty unavailable: {e}"
            );
            return Ok(());
        }
    };
    // Keep the pty master open for the whole session, matching `drive`
    // above — dropping it early hangs up the slave's controlling
    // terminal (a real incidental SIGHUP to the child, which is *also*
    // untrapped/DEFAULT_TERMINATES) and would confound this test's
    // SIGTERM-specific assertion with an unrelated signal race.
    let _master = master;

    // Give the interactive REPL time to reach install_signal_traps().
    std::thread::sleep(Duration::from_millis(500));

    kill(child, Signal::SIGTERM)
        .map_err(|e| format!("kill(SIGTERM) syscall itself should succeed: {e}"))?;

    // 20s (vs. the original 5s): wide enough to absorb the kernel-exit-
    // teardown stall documented above in the common case it's brief,
    // while still bounded so a GENUINE regression (signal truly
    // swallowed forever) fails this test rather than hanging it forever.
    // Not a claimed guarantee -- see the doc comment above: NO bound
    // tried, up to 120s, reliably covered the `--workspace` case.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if Instant::now() >= deadline {
                    let _ = kill(child, Signal::SIGKILL);
                    let _ = waitpid(child, None);
                    return Err(
                        "interactive frost did not exit within 20s of SIGTERM -- \
                         either the swallowed-signal regression is back, or \
                         this is the known macOS kernel-exit-teardown stall \
                         (see this test's doc comment) exceeding even a \
                         generous bound"
                            .to_string(),
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(_status) => return Ok(()), // exited, one way or another -- that's the invariant
            Err(e) => return Err(format!("waitpid failed: {e}")),
        }
    }
}
