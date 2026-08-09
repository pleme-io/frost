//! Command execution engine for frost.
//!
//! Takes an AST produced by `frost-parser` and executes it, managing
//! processes, pipelines, redirections, and the shell environment.
//!
//! Platform-specific system calls are isolated in the [`sys`] module
//! so the rest of the engine remains portable across Unix variants.

pub mod arith;
pub mod env;
pub mod execute;
pub mod job;
pub mod modal;
pub mod redirect;
pub mod sys;
pub mod trap;
pub mod tty_takeover;
pub mod usage;

pub use env::{ShellEnv, frecent_dirs};
pub use usage::{frecent_commands, record_command};
pub use execute::{ControlFlow, ExecError, Executor};
pub use job::{Job, JobTable};
pub use modal::{FrostMode, KeyDecision, ModalState};
pub use trap::{TrapAction, TrapTable};

/// Tokenize a string into a token stream.
///
/// Delegates to `frost_lexer::tokenize_str`, which is the ONE drain-to-Eof
/// loop in the tree and carries the cursor-did-not-move guard. This used to
/// be a hand-rolled copy — one of six — and a copy cannot be fixed once.
pub fn tokenize(input: &str) -> Vec<frost_lexer::Token> {
    frost_lexer::tokenize_str(input)
}

/// Convenience entry point: create a fresh environment, execute the
/// program, and return the exit status.
pub fn execute(program: &frost_parser::ast::Program) -> i32 {
    let mut env = ShellEnv::new();
    let mut executor = Executor::new(&mut env);
    executor.execute_program(program).unwrap_or(1)
}
