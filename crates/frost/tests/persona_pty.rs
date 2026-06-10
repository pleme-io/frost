//! L0 persona PTY harness — the REAL frost binary under a typed fake terminal.
//!
//! M0 of the terminal integration-test plan (espelho destination): a
//! `TerminalPersona` owns the master side of a real PTY and answers (or
//! refuses to answer) frost's VT queries per a typed `DsrPolicy`, while a
//! timed script injects keystrokes. Each test asserts *liveness invariants*,
//! not mechanisms — they hold regardless of how reedline/crossterm evolve.
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
//!
//! The no-freeze-under-mute invariant (typed keystrokes echoed during a CPR
//! wait) is deliberately NOT asserted yet — it is the M1 reedline-fork
//! deliverable (CPR as optimization, never a liveness dependency). Tier:
//! only-mitigated today; do not round up.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};

/// How the persona treats `ESC[6n` (DSR-6 / CPR) queries.
#[derive(Clone, Copy, Debug)]
enum DsrPolicy {
    /// Answer every query with `ESC[25;1R` after `latency`. A REAL terminal
    /// answers with latency (mado's full engate→VT→send_keys loop measured
    /// ~tens of ms) — and the 2026-06-10 answer-loss race only triggers when
    /// the answer lands AFTER reedline's filtered read window. latency=0
    /// would test an unrealistically instant terminal and miss the race.
    Answer { latency: Duration },
    /// Never answer — the hostile-terminal class (E2/E4).
    Mute,
    /// Answer, but fragment the reply across two writes with a gap —
    /// terminals are not obligated to write escape sequences atomically.
    SplitReply { gap: Duration },
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

/// Drive the real `frost` binary on a real PTY under `policy` for `total`,
/// injecting `script` keystrokes at their offsets. `rc` (when set) becomes
/// the child's FROSTRC so personas can exercise rc-declared behavior
/// hermetically. Returns the observation; never panics on session mechanics
/// (assertions live in the tests).
fn drive(
    policy: DsrPolicy,
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
    let mut scan = 0usize; // rolling ESC[6n scan cursor (never per-chunk)
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
        // Rolling scan: answer every complete ESC[6n found so far.
        while let Some(rel) = find(&transcript[scan..], b"\x1b[6n") {
            scan += rel + 4;
            cpr_queries += 1;
            match policy {
                DsrPolicy::Answer { latency } => {
                    std::thread::sleep(latency);
                    let _ = master.write_all(b"\x1b[25;1R");
                }
                DsrPolicy::Mute => {}
                DsrPolicy::SplitReply { gap } => {
                    let _ = master.write_all(b"\x1b[25;");
                    std::thread::sleep(gap);
                    let _ = master.write_all(b"1R");
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
        DsrPolicy::Answer {
            latency: Duration::from_millis(50),
        },
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
        DsrPolicy::Answer {
            // 50ms reproduced the answer-loss death 100% of the time on
            // pre-retry frost (5/5 sessions, 2026-06-10 evidence runs).
            latency: Duration::from_millis(50),
        },
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

/// E2 codified: a terminal that never answers CPR must not kill the shell.
/// (Frozen-but-alive is the accepted M0 tier; no-freeze is the M1 fork work.)
#[test]
fn mute_dsr_never_fatal() {
    let Some(o) = drive(DsrPolicy::Mute, &[], Duration::from_secs(6), None) else {
        return;
    };
    assert!(
        o.alive_at_end,
        "mute-DSR terminal must never be fatal: alive={} read_error={} cpr_queries={}",
        o.alive_at_end,
        o.read_error,
        o.cpr_queries
    );
    assert!(
        o.cpr_queries >= 2,
        "expected the bounded CPR retry to re-query (≥2 queries), saw {}",
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
        DsrPolicy::SplitReply {
            gap: Duration::from_millis(10),
        },
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
/// before EVERY prompt, and that execution wedges the next read_line's CPR
/// read — the shell survives (retry) but eats all input forever, even with
/// 0ms-latency DSR answers. Bisection proved the single form is necessary
/// and sufficient. IGNORED until the engine-side fix lands (frostmourne
/// shipped with the form disabled meanwhile); un-ignore + re-enable the rc
/// form together.
#[test]
#[ignore = "defnotify precmd hook wedges post-accept CPR — engine fix pending (incident 2026-06-10)"]
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
        DsrPolicy::Answer {
            latency: Duration::from_millis(0),
        },
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

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while let Some(p) = find(&haystack[i..], needle) {
        n += 1;
        i += p + needle.len();
    }
    n
}
