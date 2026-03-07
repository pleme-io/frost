//! Command execution engine for frost.
//!
//! Takes an AST produced by `frost-parser` and executes it, managing
//! processes, pipelines, redirections, and the shell environment.
//!
//! Platform-specific system calls are isolated in the [`sys`] module
//! so the rest of the engine remains portable across Unix variants.

pub mod env;
pub mod execute;
pub mod job;
pub mod redirect;
pub mod sys;

pub use env::ShellEnv;
pub use execute::Executor;
pub use job::{Job, JobTable};

/// Convenience entry point: create a fresh environment, execute the
/// program, and return the exit status.
pub fn execute(program: &frost_parser::ast::Program) -> i32 {
    let mut env = ShellEnv::new();
    let mut executor = Executor::new(&mut env);
    executor.execute_program(program).unwrap_or(1)
}
