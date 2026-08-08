//! `~/.local/state/frost` must not grow without bound.
//!
//! Every interactive frost bound `mcp-<pid>.sock` and wrote
//! `state-<pid>.json`, and nothing ever removed either — measured 2026-08-07
//! on the operator's box: 346 entries, 301 snapshots + 45 sockets,
//! accumulating since June, almost all of them owned by pids that died months
//! ago. Two mechanisms close that, and this file drives the real binary on a
//! real PTY to prove both actually fire:
//!
//! 1. **Graceful teardown** — a shell that exits normally removes its own
//!    socket and snapshot (`run_exit_trap` → `mcp_state_teardown`).
//! 2. **Boot sweep** — a shell starting up removes the leftovers of pids that
//!    are gone, which is the only thing that can clean up after a crash, a
//!    `kill -9`, or a closed terminal, since no exit path runs for those.
//!
//! The dangerous half of (2) is deleting a *live* shell's socket, which would
//! silently sever its MCP channel — so the fixture plants a live pid's files
//! alongside the dead ones and requires them to survive.

use std::ffi::CString;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::pty::ForkptyResult;

/// Nothing here may outlive this. A hang is a hard failure, not a slow test,
/// so every wait in this file is a bounded poll — there is no blocking
/// `waitpid`, no blocking read, and no unbounded loop.
const BUDGET: Duration = Duration::from_secs(10);

/// A pid that is definitely not running: spawn a trivial process, reap it,
/// keep its pid. (The OS could recycle it, but not within one test.)
fn a_dead_pid() -> u32 {
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn /bin/sh");
    let pid = child.id();
    child.wait().expect("reap /bin/sh");
    pid
}

fn seed(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, b"").expect("seed state-dir entry");
    p
}

/// Spawn frost interactively on a PTY with `home` as `$HOME`. Returns the
/// child pid and the pty master, or `None` if `forkpty` is unavailable.
fn spawn_interactive(home: &Path) -> Option<(nix::unistd::Pid, std::fs::File)> {
    let exe = CString::new(env!("CARGO_BIN_EXE_frost")).unwrap();
    let argv = [exe.clone()];
    let envp: Vec<CString> = vec![
        CString::new("TERM=xterm-256color").unwrap(),
        CString::new(format!("HOME={}", home.display())).unwrap(),
        CString::new("PATH=/usr/bin:/bin").unwrap(),
        CString::new("FROSTRC=/dev/null").unwrap(),
    ];

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
            eprintln!("SKIPPED: forkpty unavailable in this environment: {e}");
            return None;
        }
    };
    nix::fcntl::fcntl(
        master.as_raw_fd(),
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .expect("set O_NONBLOCK on pty master");
    Some((child, std::fs::File::from(master)))
}

/// Poll until `pred` holds or the budget runs out. Returns whether it held —
/// the caller decides whether that is a failure, so no wait can hang.
fn wait_until(mut pred: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < BUDGET {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    pred()
}

/// Best-effort reap of `child` — **never blocks**.
///
/// The first draft ended with a blocking `waitpid(child, None)` after the
/// SIGKILL and wedged there for minutes; `persona_pty.rs` documents the same
/// macOS kernel-exit-teardown stall on an interactive frost. So every wait
/// here is `WNOHANG` under a deadline, and a child that will not be collected
/// is left to the OS when the test process exits. A leaked zombie is a
/// nuisance; a test that hangs is a defect.
fn reap(child: nix::unistd::Pid) {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    // Deliberately short: by the time this runs the assertions are already
    // decided, so this is pure housekeeping and the stall above is expected.
    const REAP_BUDGET: Duration = Duration::from_secs(3);
    let mut killed = false;
    let start = Instant::now();
    while start.elapsed() < REAP_BUDGET {
        match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {}
            _ => return, // exited, or already collected / gone
        }
        if !killed {
            let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
            killed = true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn interactive_shell_sweeps_dead_pids_and_removes_its_own_files() {
    let home = tempfile::tempdir().expect("tempdir");
    let state = home.path().join(".local/state/frost");
    std::fs::create_dir_all(&state).unwrap();

    let dead = a_dead_pid();
    let live = std::process::id(); // this test process — indisputably alive
    let dead_sock = seed(&state, &format!("mcp-{dead}.sock"));
    let dead_snap = seed(&state, &format!("state-{dead}.json"));
    let dead_tmp = seed(&state, &format!("state-{dead}.json.tmp"));
    let live_sock = seed(&state, &format!("mcp-{live}.sock"));
    let live_snap = seed(&state, &format!("state-{live}.json"));
    let decoy = seed(&state, "not-a-frost-file");

    let Some((child, mut master)) = spawn_interactive(home.path()) else {
        return; // forkpty unavailable — already reported
    };
    let child_sock = state.join(format!("mcp-{child}.sock"));
    let child_snap = state.join(format!("state-{child}.json"));

    let booted = wait_until(|| child_sock.exists() && child_snap.exists());
    if !booted {
        let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
        reap(child);
        panic!(
            "frost never bound {} / wrote {} within {BUDGET:?} — the rest of \
             this test would be vacuous",
            child_sock.display(),
            child_snap.display()
        );
    }

    // ── the sweep ────────────────────────────────────────────────────
    assert!(
        !dead_sock.exists(),
        "a dead pid's socket survived the sweep"
    );
    assert!(
        !dead_snap.exists(),
        "a dead pid's snapshot survived the sweep"
    );
    assert!(
        !dead_tmp.exists(),
        "a dead pid's temp file survived the sweep"
    );
    assert!(
        live_sock.exists(),
        "a LIVE pid's socket was swept — this severs a running shell's MCP channel"
    );
    assert!(live_snap.exists(), "a LIVE pid's snapshot was swept");
    assert!(decoy.exists(), "a non-frost file was swept");

    // ── the graceful teardown ────────────────────────────────────────
    let _ = master.write_all(b"exit\n");
    let _ = master.flush();
    let gone = wait_until(|| !child_sock.exists() && !child_snap.exists());
    reap(child);
    assert!(
        gone,
        "frost exited without removing its own {} / {}",
        child_sock.display(),
        child_snap.display()
    );

    // And it took nothing else with it.
    assert!(live_sock.exists(), "teardown removed another pid's socket");
    assert!(decoy.exists(), "teardown removed a non-frost file");
}
