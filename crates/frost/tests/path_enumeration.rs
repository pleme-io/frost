//! The success path must not enumerate `$PATH`.
//!
//! `run()` used to build the did-you-mean corpus *before* dispatching, which
//! meant every successful command paid for `read_dir` + a `stat` per entry on
//! every `$PATH` directory — measured 2026-08-07 on the operator's box: 92
//! `opendir` + 2304 `lstat`, ~17 ms — and then threw the result away. `run()`
//! fires three times per interactive command (`__frost_hook_preexec`, the
//! command, `__frost_hook_precmd`), so that was ~50 ms per prompt spent on a
//! corpus only a typo could ever consume.
//!
//! **How this test detects a regression without counting syscalls.** Reading
//! a directory updates the directory's *access time*. So: stamp a baseline,
//! run a command that SUCCEEDS, and require the atime to be unmoved. On its
//! own that assertion is vacuous — deleting the suggestion feature entirely
//! would also pass it — so every case is paired with a **positive control**
//! that runs a command that FAILS and requires the same atime to move. The
//! error path paying is the proof the probe can see enumeration at all.
//!
//! Filesystems mounted `noatime` (or Linux `relatime`, which only bumps atime
//! once a day) do not track this. The probe detects that up front by reading
//! the directory from the test process itself; if the atime does not move
//! there, the test reports SKIPPED rather than asserting on a signal the
//! filesystem is not producing. Verified live on macOS 25.6 / APFS
//! (`/System/Volumes/Data`), where readdir does move it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

/// A row must never outlive this. A hang is a failure, not a slow test.
const ROW_TIMEOUT: Duration = Duration::from_secs(10);

fn frost_bin() -> &'static str {
    env!("CARGO_BIN_EXE_frost")
}

/// A private PATH directory holding one executable with a distinctive name,
/// plus a private HOME so the child never touches the operator's state dir.
struct Sandbox {
    root: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "frost-path-enum-{tag}-{}-{}",
            std::process::id(),
            // Nanos keep two runs of the same test in the same second from
            // colliding on a shared directory.
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let bin = root.join("bin");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&bin).unwrap();
        // `sentinelcmd` is the suggestion target: `sentinelcm` is one
        // deletion away, so it is a distance-1 match and must be offered.
        let exe = bin.join("sentinelcmd");
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { root, bin }
    }

    /// Run the real frost binary with this sandbox as `$PATH` and `$HOME`.
    /// Returns `(exit_code, stderr)`. Never blocks past `ROW_TIMEOUT`.
    fn run_frost(&self, script: &str) -> (i32, String) {
        let mut child = Command::new(frost_bin())
            .arg("-c")
            .arg(script)
            .env_clear()
            .env("PATH", &self.bin)
            .env("HOME", &self.root)
            .env("FROSTRC", "/dev/null")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn frost");

        let started = Instant::now();
        loop {
            match child.try_wait().expect("try_wait") {
                Some(_) => break,
                None if started.elapsed() > ROW_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "frost -c {script:?} did not finish within {ROW_TIMEOUT:?} — \
                         a hang is a hard failure, not a slow test"
                    );
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        let out = child.wait_with_output().expect("wait_with_output");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn atime(dir: &Path) -> SystemTime {
    std::fs::metadata(dir)
        .expect("stat sandbox bin dir")
        .accessed()
        .expect("filesystem must report an access time")
}

/// Arm the atime probe.
///
/// macOS/APFS behaves like Linux `relatime`: a `readdir` only writes a new
/// access time when the recorded atime is already **older than the mtime**
/// (or a day stale). A naive "read atime, act, read atime" therefore sees a
/// move the first time and nothing afterwards — which is exactly how this
/// test's first draft failed its own positive control. Creating and removing
/// a marker file bumps the directory's mtime past its atime without reading
/// the directory, so the next `readdir` is guaranteed to be recorded.
fn arm_probe(dir: &Path) {
    let marker = dir.join(".atime-probe-arm");
    std::fs::write(&marker, b"").expect("write probe marker");
    std::fs::remove_file(&marker).expect("remove probe marker");
    // Keep the timestamps distinguishable on a coarse-granularity clock.
    std::thread::sleep(Duration::from_millis(20));
}

/// Does `read_dir` move this directory's atime on this filesystem? If not,
/// the probe is blind and the test reports SKIPPED instead of asserting.
fn atime_tracks_readdir(dir: &Path) -> bool {
    arm_probe(dir);
    let before = atime(dir);
    for e in std::fs::read_dir(dir).expect("read sandbox bin dir") {
        let _ = e;
    }
    atime(dir) != before
}

/// The whole point: a command that SUCCEEDS reads no `$PATH` directory.
/// Paired with its own positive control so it cannot pass vacuously.
#[test]
fn successful_command_does_not_enumerate_path() {
    let sb = Sandbox::new("success");
    if !atime_tracks_readdir(&sb.bin) {
        eprintln!(
            "SKIPPED: {} does not update directory atime on readdir \
             (noatime/relatime mount) — the probe cannot see enumeration here",
            sb.bin.display()
        );
        return;
    }

    // ── the assertion ────────────────────────────────────────────────
    arm_probe(&sb.bin);
    let before = atime(&sb.bin);
    let (code, stderr) = sb.run_frost(":");
    assert_eq!(code, 0, "`:` must succeed; stderr={stderr}");
    assert_eq!(
        atime(&sb.bin),
        before,
        "a SUCCESSFUL command read the $PATH directory — the did-you-mean \
         corpus is being built speculatively again"
    );

    // ── the positive control: the probe is not blind ─────────────────
    arm_probe(&sb.bin);
    let before = atime(&sb.bin);
    let (code, stderr) = sb.run_frost("sentinelcm");
    assert_eq!(code, 127, "an unknown command must exit 127; stderr={stderr}");
    assert!(
        stderr.contains("sentinelcmd"),
        "the control must actually reach the suggestion path; stderr={stderr}"
    );
    assert_ne!(
        atime(&sb.bin),
        before,
        "the FAILING command did not read the $PATH directory either — the \
         atime probe is blind, so the assertion above proved nothing"
    );
}

/// The suggestion itself still works, and works for a SYMLINKED target —
/// the second half of the defect. `DirEntry::metadata()` is an `lstat`, so
/// the old enumerator dropped every symlink (measured: 1015 of 2302 entries
/// on this operator's `$PATH`, all nix-store links), and `bxl` could never
/// suggest `blx`. Both PATH readers now share
/// `frost_complete::path_command_names`, which uses `Path::metadata` (a
/// `stat`), so a link resolves exactly as an `exec` would.
#[test]
fn command_not_found_suggests_a_symlinked_path_binary() {
    let sb = Sandbox::new("symlink");
    // `linkedcmd` is a symlink to the regular file; only a stat-based
    // enumerator can see it.
    std::os::unix::fs::symlink(sb.bin.join("sentinelcmd"), sb.bin.join("linkedcmd")).unwrap();

    let (code, stderr) = sb.run_frost("linkedcm");
    assert_eq!(code, 127, "unknown command must exit 127; stderr={stderr}");
    assert!(
        stderr.contains("linkedcmd"),
        "a symlinked PATH binary must be suggested; stderr={stderr}"
    );
}

/// Belt and braces on the hang gate: a plain successful command, a failing
/// one, and the command-substitution-inside-double-quotes row that wedged an
/// earlier parser change, all under `ROW_TIMEOUT`.
#[test]
fn no_row_hangs() {
    let sb = Sandbox::new("nohang");
    for script in [
        ":",
        "true",
        "definitely-not-a-command",
        "eval \"$(printf %s \"export D=1\")\"",
    ] {
        let (_code, _stderr) = sb.run_frost(script);
    }
}
