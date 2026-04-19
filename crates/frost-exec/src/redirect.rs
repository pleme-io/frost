//! Redirection handling — applies I/O redirections via `dup2(2)`.
//!
//! Called after `fork(2)` and before `exec(2)` in the child process.
//! All system calls go through [`crate::sys`] for portability.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nix::fcntl::OFlag;
use nix::sys::stat::Mode;

use frost_parser::ast::{Redirect, RedirectOp, WordPart};

use crate::sys;

/// Error type for redirection failures.
#[derive(Debug, thiserror::Error)]
pub enum RedirectError {
    #[error("failed to open `{path}`: {source}")]
    Open {
        path: String,
        source: nix::errno::Errno,
    },

    #[error("dup2 failed: {0}")]
    Dup2(nix::errno::Errno),

    #[error("close failed: {0}")]
    Close(nix::errno::Errno),

    #[error("bad file descriptor: {0}")]
    BadFd(String),
}

/// Apply a list of redirections in the current process.
///
/// Typically called in the child after `fork()`. Each redirection
/// opens/creates the target file and dups it onto the appropriate fd.
///
/// `expanded_targets` provides pre-expanded strings for redirect targets.
/// If provided and non-empty, the i-th entry is used as the target text
/// instead of resolving the word from the AST. This allows the executor
/// to pass in variable-expanded herestring content.
pub fn apply_redirects(redirects: &[Redirect]) -> Result<(), RedirectError> {
    for redir in redirects {
        apply_one(redir, None)?;
    }
    Ok(())
}

/// Apply redirections with pre-expanded target text for each redirect.
pub fn apply_redirects_expanded(
    redirects: &[Redirect],
    expanded_targets: &[String],
) -> Result<(), RedirectError> {
    for (i, redir) in redirects.iter().enumerate() {
        let expanded = expanded_targets.get(i).map(|s| s.as_str());
        apply_one(redir, expanded)?;
    }
    Ok(())
}

fn apply_one(redir: &Redirect, expanded_target: Option<&str>) -> Result<(), RedirectError> {
    let target_text = || expanded_target.map(|s| s.to_owned()).unwrap_or_else(|| resolve_word(&redir.target));

    match redir.op {
        // < file  (input)
        RedirectOp::Less => {
            let target_fd = redir.fd.unwrap_or(0) as i32;
            let path = target_text();
            let fd = open_file(&path, OFlag::O_RDONLY, Mode::empty())?;
            dup2_and_close(fd, target_fd)?;
        }

        // > file  (output, truncate)
        RedirectOp::Greater | RedirectOp::GreaterPipe | RedirectOp::GreaterBang => {
            let target_fd = redir.fd.unwrap_or(1) as i32;
            let path = target_text();
            let fd = open_file(
                &path,
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
                Mode::from_bits_truncate(0o666),
            )?;
            dup2_and_close(fd, target_fd)?;
        }

        // >> file  (append)
        RedirectOp::DoubleGreater => {
            let target_fd = redir.fd.unwrap_or(1) as i32;
            let path = target_text();
            let fd = open_file(
                &path,
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_APPEND,
                Mode::from_bits_truncate(0o666),
            )?;
            dup2_and_close(fd, target_fd)?;
        }

        // &> file  (stdout + stderr)
        RedirectOp::AmpGreater => {
            let path = target_text();
            let fd = open_file(
                &path,
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
                Mode::from_bits_truncate(0o666),
            )?;
            dup2_and_close(fd, 1)?;
            sys::dup2(1, 2).map_err(RedirectError::Dup2)?;
        }

        // &>> file  (append stdout + stderr)
        RedirectOp::AmpDoubleGreater => {
            let path = target_text();
            let fd = open_file(
                &path,
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_APPEND,
                Mode::from_bits_truncate(0o666),
            )?;
            dup2_and_close(fd, 1)?;
            sys::dup2(1, 2).map_err(RedirectError::Dup2)?;
        }

        // <> file  (read-write)
        RedirectOp::LessGreater => {
            let target_fd = redir.fd.unwrap_or(0) as i32;
            let path = target_text();
            let fd = open_file(
                &path,
                OFlag::O_RDWR | OFlag::O_CREAT,
                Mode::from_bits_truncate(0o666),
            )?;
            dup2_and_close(fd, target_fd)?;
        }

        // N>&M  (fd duplication)
        RedirectOp::FdDup => {
            let target_fd = redir.fd.unwrap_or(1) as i32;
            let src_text = target_text();
            if src_text == "-" {
                sys::close(target_fd).map_err(RedirectError::Close)?;
            } else {
                let src_fd: i32 = src_text
                    .parse()
                    .map_err(|_| RedirectError::BadFd(src_text))?;
                sys::dup2(src_fd, target_fd).map_err(RedirectError::Dup2)?;
            }
        }

        // <<< herestring — feed the word as stdin
        RedirectOp::TripleLess => {
            let target_fd = redir.fd.unwrap_or(0) as i32;
            let text = target_text();
            let content = format!("{text}\n");
            let fd = write_to_pipe(content.as_bytes())?;
            dup2_and_close(fd, target_fd)?;
        }

        // << heredoc / <<- heredoc (strip tabs) — treat body as herestring for now
        RedirectOp::DoubleLess | RedirectOp::DoubleLessDash => {
            let target_fd = redir.fd.unwrap_or(0) as i32;
            let text = target_text();
            let content = format!("{text}\n");
            let fd = write_to_pipe(content.as_bytes())?;
            dup2_and_close(fd, target_fd)?;
        }
    }

    Ok(())
}

/// Write data to a pipe and return the read end as a raw fd.
fn write_to_pipe(data: &[u8]) -> Result<i32, RedirectError> {
    let pipe = sys::pipe().map_err(|e| RedirectError::Open {
        path: "<herestring>".to_owned(),
        source: e,
    })?;
    // Write data to the write end.
    nix::unistd::write(unsafe { std::os::fd::BorrowedFd::borrow_raw(pipe.write) }, data)
        .map_err(|e| RedirectError::Open {
            path: "<herestring>".to_owned(),
            source: e,
        })?;
    // Close the write end so the reader gets EOF.
    sys::close(pipe.write).ok();
    Ok(pipe.read)
}

/// Open a file by path, returning a raw fd.
fn open_file(path: &str, flags: OFlag, mode: Mode) -> Result<i32, RedirectError> {
    let cpath = CString::new(Path::new(path).as_os_str().as_bytes())
        .map_err(|_| RedirectError::BadFd(path.to_owned()))?;
    sys::open(cpath.as_c_str(), flags, mode).map_err(|e| RedirectError::Open {
        path: path.to_owned(),
        source: e,
    })
}

/// Dup `fd` onto `target`, then close the original if they differ.
fn dup2_and_close(fd: i32, target: i32) -> Result<(), RedirectError> {
    sys::dup2_and_close(fd, target).map_err(RedirectError::Dup2)
}

/// Get the target fd for a redirect (used by save/restore logic).
pub fn target_fd_for(redir: &Redirect) -> i32 {
    match redir.op {
        RedirectOp::Less | RedirectOp::LessGreater => redir.fd.unwrap_or(0) as i32,
        RedirectOp::Greater | RedirectOp::GreaterPipe | RedirectOp::GreaterBang
        | RedirectOp::DoubleGreater | RedirectOp::FdDup => redir.fd.unwrap_or(1) as i32,
        RedirectOp::AmpGreater | RedirectOp::AmpDoubleGreater => 1, // also affects 2
        RedirectOp::DoubleLess | RedirectOp::TripleLess | RedirectOp::DoubleLessDash => {
            redir.fd.unwrap_or(0) as i32
        }
    }
}

/// Extract a plain string from a [`Word`].
fn resolve_word(word: &frost_parser::ast::Word) -> String {
    let mut out = String::new();
    for part in &word.parts {
        match part {
            WordPart::Literal(s) | WordPart::SingleQuoted(s) => out.push_str(s),
            _ => {
                tracing::warn!("unresolved word part in redirect target");
            }
        }
    }
    out
}
