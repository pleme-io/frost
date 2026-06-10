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
//! The no-freeze-under-mute invariant went LIVE with the pleme-io reedline
//! fork (CPR as optimization, never a liveness dependency): under MuteDsr the
//! painter falls back to a safe row, the prompt paints, and commands round-
//! trip. Residual (#[ignore]d): fd0-TARGETING builtin redirects cycle fd 0 and
//! kill the crossterm/mio kqueue registration — an INPUT-liveness class (no
//! keystrokes at all, CPR irrelevant) needing an event-source reset upstream.

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
    let Some(o) = drive(DsrPolicy::Mute, &script, Duration::from_secs(12), None) else {
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
        DsrPolicy::Answer {
            latency: Duration::from_millis(20),
        },
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

/// HONEST RESIDUAL: a builtin redirect that legitimately TARGETS fd 0 must
/// cycle fd 0, which still deletes the tty event-source registration —
/// the wedge persists for this narrow case. The structural fix is event-
/// source re-registration (or the reedline fork making CPR optional) — M1.
#[test]
#[ignore = "fd0-targeting builtin redirects still cycle fd 0 — event-source re-registration pending (M1)"]
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
        DsrPolicy::Answer {
            latency: Duration::from_millis(20),
        },
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
        DsrPolicy::Answer {
            // The latency that reproduced the answer-loss death 5/5 on
            // pre-fix frost under this exact rc.
            latency: Duration::from_millis(50),
        },
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
