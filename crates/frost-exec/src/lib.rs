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
pub mod redirect;
pub mod sys;
pub mod trap;

pub use env::ShellEnv;
pub use execute::{ControlFlow, ExecError, Executor};
pub use job::{Job, JobTable};
pub use trap::{TrapAction, TrapTable};

/// Tokenize a string into a token stream.
pub fn tokenize(input: &str) -> Vec<frost_lexer::Token> {
    let mut lexer = frost_lexer::Lexer::new(input.as_bytes());
    let mut tokens = Vec::new();
    loop {
        let tok = lexer.next_token();
        let eof = tok.kind == frost_lexer::TokenKind::Eof;
        tokens.push(tok);
        if eof { break; }
    }
    tokens
}

/// Convenience entry point: create a fresh environment, execute the
/// program, and return the exit status.
pub fn execute(program: &frost_parser::ast::Program) -> i32 {
    let mut env = ShellEnv::new();
    let mut executor = Executor::new(&mut env);
    executor.execute_program(program).unwrap_or(1)
}
