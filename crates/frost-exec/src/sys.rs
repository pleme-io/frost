//! Platform abstraction layer.
//!
//! Isolates all OS-specific system calls (fork, exec, pipe, dup2, open,
//! close, wait) behind a clean interface so the rest of frost-exec
//! remains portable across Unix variants and architectures.

use std::ffi::CString;
use std::os::fd::{IntoRawFd, RawFd};

use nix::fcntl::OFlag;
use nix::sys::stat::Mode;
use nix::sys::wait::{self, WaitPidFlag, WaitStatus};
use nix::unistd::{self, ForkResult, Pid};

// ── Pipe ────────────────────────────────────────────────────────────

/// A pipe pair as raw file descriptors.
pub struct Pipe {
    pub read: RawFd,
    pub write: RawFd,
}

/// Create a pipe, returning raw file descriptors.
pub fn pipe() -> Result<Pipe, nix::errno::Errno> {
    let (rd, wr) = unistd::pipe()?;
    Ok(Pipe {
        read: rd.into_raw_fd(),
        write: wr.into_raw_fd(),
    })
}

// ── File descriptor operations ──────────────────────────────────────

/// Duplicate `src` onto `dst`.
pub fn dup2(src: RawFd, dst: RawFd) -> Result<RawFd, nix::errno::Errno> {
    unistd::dup2(src, dst)
}

/// Close a file descriptor.
pub fn close(fd: RawFd) -> Result<(), nix::errno::Errno> {
    unistd::close(fd)
}

/// Open a file, returning a raw file descriptor.
pub fn open(path: &std::ffi::CStr, flags: OFlag, mode: Mode) -> Result<RawFd, nix::errno::Errno> {
    let owned = nix::fcntl::open(path, flags, mode)?;
    Ok(owned.into_raw_fd())
}

/// Duplicate `src` onto `dst`, then close `src` if they differ.
pub fn dup2_and_close(src: RawFd, dst: RawFd) -> Result<(), nix::errno::Errno> {
    if src != dst {
        dup2(src, dst)?;
        close(src)?;
    }
    Ok(())
}

// ── Fork ────────────────────────────────────────────────────────────

/// Result of a fork operation.
pub enum ForkOutcome {
    Child,
    Parent { child_pid: Pid },
}

/// Fork the current process.
///
/// # Safety
///
/// Caller must ensure fork safety — no async-signal-unsafe operations
/// in the child between `fork()` and `exec()`.
pub unsafe fn fork() -> Result<ForkOutcome, nix::errno::Errno> {
    match unsafe { unistd::fork() }? {
        ForkResult::Child => Ok(ForkOutcome::Child),
        ForkResult::Parent { child } => Ok(ForkOutcome::Parent { child_pid: child }),
    }
}

// ── Exec ────────────────────────────────────────────────────────────

/// Replace the current process image with a new program.
///
/// If `argv[0]` contains a `/`, uses it as a direct path. Otherwise
/// searches `PATH` (extracted from `envp`) for the binary.
///
/// Uses `execve(2)` on all platforms — no reliance on platform-specific
/// variants like `execvpe` (Linux-only) or `execvp` (inherits process
/// env instead of using the shell's `envp`).
///
/// Does not return on success.
pub fn exec(argv: &[CString], envp: &[CString]) -> nix::errno::Errno {
    let Some(cmd) = argv.first() else {
        return nix::errno::Errno::ENOENT;
    };

    if cmd.as_bytes().contains(&b'/') {
        // Direct path — exec immediately.
        match unistd::execve(cmd, argv, envp) {
            Ok(infallible) => match infallible {},
            Err(e) => e,
        }
    } else {
        // Search PATH from the shell's environment.
        let path_val = envp
            .iter()
            .find_map(|entry| {
                let bytes = entry.as_bytes();
                bytes
                    .starts_with(b"PATH=")
                    .then(|| String::from_utf8_lossy(&bytes[5..]).into_owned())
            })
            .unwrap_or_else(|| "/usr/bin:/bin".into());

        let cmd_str = cmd.to_string_lossy();
        let mut last_err = nix::errno::Errno::ENOENT;

        for dir in path_val.split(':') {
            let Ok(full_path) = CString::new(format!("{dir}/{cmd_str}")) else {
                continue;
            };
            match unistd::execve(&full_path, argv, envp) {
                Ok(infallible) => match infallible {},
                Err(nix::errno::Errno::ENOENT | nix::errno::Errno::EACCES) => continue,
                Err(e) => {
                    last_err = e;
                    break;
                }
            }
        }
        last_err
    }
}

// ── Wait ────────────────────────────────────────────────────────────

/// Outcome of waiting for a child process.
pub enum ChildStatus {
    /// Exited normally with a status code.
    Exited(i32),
    /// Killed by a signal (value is 128 + signal number).
    Signaled(i32),
    /// Stopped (e.g. SIGTSTP).
    Stopped,
    /// Still alive (non-blocking wait only).
    Running,
}

/// Wait for a specific child process (blocking).
pub fn wait_pid(pid: Pid) -> Result<ChildStatus, nix::errno::Errno> {
    match wait::waitpid(pid, None)? {
        WaitStatus::Exited(_, code) => Ok(ChildStatus::Exited(code)),
        WaitStatus::Signaled(_, sig, _) => Ok(ChildStatus::Signaled(128 + sig as i32)),
        WaitStatus::Stopped(_, _) => Ok(ChildStatus::Stopped),
        _ => Ok(ChildStatus::Exited(0)),
    }
}

/// Wait for a child process (non-blocking).
pub fn try_wait_pid(pid: Pid) -> Result<ChildStatus, nix::errno::Errno> {
    match wait::waitpid(pid, Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED))? {
        WaitStatus::Exited(_, code) => Ok(ChildStatus::Exited(code)),
        WaitStatus::Signaled(_, sig, _) => Ok(ChildStatus::Signaled(128 + sig as i32)),
        WaitStatus::Stopped(_, _) => Ok(ChildStatus::Stopped),
        WaitStatus::StillAlive => Ok(ChildStatus::Running),
        _ => Ok(ChildStatus::Running),
    }
}
