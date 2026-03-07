//! The main execution engine.
//!
//! Walks the AST and executes commands by forking child processes,
//! setting up pipes, and applying redirections. All platform-specific
//! system calls go through [`crate::sys`].

use std::ffi::CString;

use nix::unistd::Pid;

use frost_builtins::BuiltinRegistry;
use frost_parser::ast::{
    Command, CompleteCommand, List, ListOp, Pipeline, Program, SimpleCommand, Word, WordPart,
};

use crate::env::ShellEnv;
use crate::job::JobTable;
use crate::redirect;
use crate::sys;

/// Execution errors.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("command not found: {0}")]
    CommandNotFound(String),

    #[error("fork failed: {0}")]
    Fork(nix::errno::Errno),

    #[error("exec failed: {0}")]
    Exec(nix::errno::Errno),

    #[error("pipe failed: {0}")]
    Pipe(nix::errno::Errno),

    #[error("wait failed: {0}")]
    Wait(nix::errno::Errno),

    #[error("redirect error: {0}")]
    Redirect(#[from] redirect::RedirectError),
}

/// Result alias for execution operations.
pub type ExecResult = Result<i32, ExecError>;

/// The command executor.
///
/// Holds a mutable reference to the shell environment and a builtin
/// registry. Create one per top-level evaluation.
pub struct Executor<'env> {
    pub env: &'env mut ShellEnv,
    pub builtins: BuiltinRegistry,
    pub jobs: JobTable,
}

impl<'env> Executor<'env> {
    /// Create a new executor with the default builtins.
    pub fn new(env: &'env mut ShellEnv) -> Self {
        Self {
            env,
            builtins: frost_builtins::default_builtins(),
            jobs: JobTable::new(),
        }
    }

    // ── Top-level entry ──────────────────────────────────────────

    /// Execute an entire program (a list of complete commands).
    pub fn execute_program(&mut self, program: &Program) -> ExecResult {
        let mut status = 0;
        for cmd in &program.commands {
            status = self.execute_complete_command(cmd)?;
        }
        Ok(status)
    }

    /// Execute a single complete command (which may be async / `&`).
    fn execute_complete_command(&mut self, cmd: &CompleteCommand) -> ExecResult {
        let status = self.execute_list(&cmd.list)?;

        if cmd.is_async {
            self.env.exit_status = 0;
            Ok(0)
        } else {
            self.env.exit_status = status;
            Ok(status)
        }
    }

    /// Execute a list (pipelines joined by `&&` / `||`).
    fn execute_list(&mut self, list: &List) -> ExecResult {
        let mut status = self.execute_pipeline(&list.first)?;

        for (op, pipeline) in &list.rest {
            match op {
                ListOp::And if status == 0 => {
                    status = self.execute_pipeline(pipeline)?;
                }
                ListOp::Or if status != 0 => {
                    status = self.execute_pipeline(pipeline)?;
                }
                _ => {}
            }
        }

        Ok(status)
    }

    // ── Pipeline ─────────────────────────────────────────────────

    /// Execute a pipeline of one or more commands connected by pipes.
    pub fn execute_pipeline(&mut self, pipeline: &Pipeline) -> ExecResult {
        let cmds = &pipeline.commands;

        if cmds.len() == 1 {
            let status = self.execute_command(&cmds[0])?;
            return Ok(if pipeline.bang { invert(status) } else { status });
        }

        // Multi-command pipeline: create N-1 pipes via sys abstraction.
        let mut pipes = Vec::with_capacity(cmds.len() - 1);
        for _ in 0..cmds.len() - 1 {
            let p = sys::pipe().map_err(ExecError::Pipe)?;
            pipes.push((p.read, p.write));
        }

        let mut children: Vec<Pid> = Vec::with_capacity(cmds.len());

        for (i, cmd) in cmds.iter().enumerate() {
            match unsafe { sys::fork() }.map_err(ExecError::Fork)? {
                sys::ForkOutcome::Child => {
                    // Wire stdin from previous pipe.
                    if i > 0 {
                        let (rd, _) = pipes[i - 1];
                        sys::dup2(rd, 0).ok();
                    }
                    // Wire stdout to next pipe.
                    if i < cmds.len() - 1 {
                        let (_, wr) = pipes[i];
                        sys::dup2(wr, 1).ok();

                        if pipeline.pipe_stderr.get(i).copied().unwrap_or(false) {
                            sys::dup2(wr, 2).ok();
                        }
                    }
                    // Close all pipe fds in the child.
                    for &(rd, wr) in &pipes {
                        sys::close(rd).ok();
                        sys::close(wr).ok();
                    }
                    let status = self.execute_command(cmd).unwrap_or(127);
                    std::process::exit(status);
                }
                sys::ForkOutcome::Parent { child_pid } => {
                    children.push(child_pid);
                }
            }
        }

        // Parent: close all pipe fds.
        for (rd, wr) in pipes {
            sys::close(rd).ok();
            sys::close(wr).ok();
        }

        // Wait for all children, return the exit status of the last.
        let mut last_status = 0;
        for pid in children {
            match sys::wait_pid(pid).map_err(ExecError::Wait)? {
                sys::ChildStatus::Exited(code) => last_status = code,
                sys::ChildStatus::Signaled(code) => last_status = code,
                _ => {}
            }
        }

        Ok(if pipeline.bang {
            invert(last_status)
        } else {
            last_status
        })
    }

    // ── Command dispatch ─────────────────────────────────────────

    /// Execute a single command node from the AST.
    pub fn execute_command(&mut self, cmd: &Command) -> ExecResult {
        match cmd {
            Command::Simple(simple) => self.execute_simple(simple),
            Command::Subshell(_) => todo!("execute_subshell"),
            Command::BraceGroup(_) => todo!("execute_brace_group"),
            Command::If(_) => todo!("execute_if"),
            Command::For(_) => todo!("execute_for"),
            Command::While(_) => todo!("execute_while"),
            Command::Until(_) => todo!("execute_until"),
            Command::Case(_) => todo!("execute_case"),
            Command::Select(_) => todo!("execute_select"),
            Command::FunctionDef(_) => todo!("execute_function_def"),
            Command::Coproc(_) => todo!("execute_coproc"),
            Command::Time(_) => todo!("execute_time"),
        }
    }

    // ── Simple command ───────────────────────────────────────────

    /// Execute a simple command (assignments + words + redirects).
    pub fn execute_simple(&mut self, cmd: &SimpleCommand) -> ExecResult {
        for assign in &cmd.assignments {
            let value = assign
                .value
                .as_ref()
                .map(|w| resolve_word(w))
                .unwrap_or_default();
            self.env.set_var(&assign.name, &value);
        }

        if cmd.words.is_empty() {
            return Ok(0);
        }

        let argv: Vec<String> = cmd.words.iter().map(resolve_word).collect();
        let name = &argv[0];

        // Check builtins first.
        if self.builtins.contains(name) {
            let arg_refs: Vec<&str> = argv[1..].iter().map(|s| s.as_str()).collect();
            let status = self
                .builtins
                .get(name)
                .unwrap()
                .execute(&arg_refs, self.env);
            self.env.exit_status = status;
            return Ok(status);
        }

        // External command: fork + exec.
        self.fork_exec(&argv, &cmd.redirects)
    }

    /// Fork a child process and exec an external command.
    fn fork_exec(
        &mut self,
        argv: &[String],
        redirects: &[frost_parser::ast::Redirect],
    ) -> ExecResult {
        let c_argv: Vec<CString> = argv
            .iter()
            .filter_map(|a| CString::new(a.as_bytes()).ok())
            .collect();

        let c_envp = self.env.to_env_vec();

        match unsafe { sys::fork() }.map_err(ExecError::Fork)? {
            sys::ForkOutcome::Child => {
                if let Err(e) = redirect::apply_redirects(redirects) {
                    eprintln!("frost: {e}");
                    std::process::exit(1);
                }

                // Use sys::exec which handles PATH resolution and
                // works identically across all Unix platforms.
                let err = sys::exec(&c_argv, &c_envp);
                eprintln!("frost: {}: {err}", argv[0]);
                std::process::exit(if err == nix::errno::Errno::ENOENT {
                    127
                } else {
                    126
                });
            }
            sys::ForkOutcome::Parent { child_pid } => {
                match sys::wait_pid(child_pid).map_err(ExecError::Wait)? {
                    sys::ChildStatus::Exited(code) => {
                        self.env.exit_status = code;
                        Ok(code)
                    }
                    sys::ChildStatus::Signaled(code) => {
                        self.env.exit_status = code;
                        Ok(code)
                    }
                    _ => Ok(0),
                }
            }
        }
    }
}

/// Invert exit status for `!` pipelines: 0 -> 1, non-zero -> 0.
fn invert(status: i32) -> i32 {
    if status == 0 { 1 } else { 0 }
}

/// Flatten a [`Word`] AST node into a plain string.
///
/// Handles literal and single-quoted parts. Full expansion (parameter,
/// command substitution, glob, tilde) is handled by the expansion layer
/// before the executor sees the words.
fn resolve_word(word: &Word) -> String {
    let mut out = String::new();
    for part in &word.parts {
        match part {
            WordPart::Literal(s) | WordPart::SingleQuoted(s) => out.push_str(s),
            WordPart::DoubleQuoted(parts) => {
                for inner in parts {
                    if let WordPart::Literal(s) = inner {
                        out.push_str(s);
                    }
                }
            }
            _ => {
                tracing::warn!("unresolved expansion in word");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use frost_lexer::Span;
    use frost_parser::ast::{Assignment, AssignOp, CompleteCommand, List, Pipeline, SimpleCommand, Word, WordPart};
    use pretty_assertions::assert_eq;

    fn literal_word(s: &str) -> Word {
        Word {
            parts: vec![WordPart::Literal(s.into())],
            span: Span::new(0, s.len() as u32),
        }
    }

    fn simple_program(words: Vec<&str>) -> Program {
        Program {
            commands: vec![CompleteCommand {
                list: List {
                    first: Pipeline {
                        bang: false,
                        commands: vec![Command::Simple(SimpleCommand {
                            assignments: vec![],
                            words: words.into_iter().map(literal_word).collect(),
                            redirects: vec![],
                        })],
                        pipe_stderr: vec![],
                    },
                    rest: vec![],
                },
                is_async: false,
            }],
        }
    }

    #[test]
    fn resolve_literal_word() {
        let word = literal_word("hello");
        assert_eq!(resolve_word(&word), "hello");
    }

    #[test]
    fn execute_true_builtin() {
        let mut env = ShellEnv::new();
        let mut exec = Executor::new(&mut env);
        let program = simple_program(vec!["true"]);
        let status = exec.execute_program(&program).unwrap();
        assert_eq!(status, 0);
    }

    #[test]
    fn execute_false_builtin() {
        let mut env = ShellEnv::new();
        let mut exec = Executor::new(&mut env);
        let program = simple_program(vec!["false"]);
        let status = exec.execute_program(&program).unwrap();
        assert_eq!(status, 1);
    }

    #[test]
    fn invert_status() {
        assert_eq!(invert(0), 1);
        assert_eq!(invert(1), 0);
        assert_eq!(invert(42), 0);
    }

    #[test]
    fn bare_assignment() {
        let mut env = ShellEnv::new();
        let mut exec = Executor::new(&mut env);
        let program = Program {
            commands: vec![CompleteCommand {
                list: List {
                    first: Pipeline {
                        bang: false,
                        commands: vec![Command::Simple(SimpleCommand {
                            assignments: vec![Assignment {
                                name: "MY_VAR".into(),
                                op: AssignOp::Assign,
                                value: Some(literal_word("hello")),
                                span: Span::new(0, 12),
                            }],
                            words: vec![],
                            redirects: vec![],
                        })],
                        pipe_stderr: vec![],
                    },
                    rest: vec![],
                },
                is_async: false,
            }],
        };
        let status = exec.execute_program(&program).unwrap();
        assert_eq!(status, 0);
        assert_eq!(exec.env.get_var("MY_VAR"), Some("hello"));
    }
}
