//! The main execution engine.
//!
//! Walks the AST and executes commands by forking child processes,
//! setting up pipes, and applying redirections. All platform-specific
//! system calls go through [`crate::sys`].

use std::ffi::CString;

use nix::unistd::Pid;

use frost_builtins::BuiltinRegistry;
use frost_parser::ast::{
    BraceGroup, CaseClause, Command, CompleteCommand, ForClause, IfClause, List, ListOp,
    Pipeline, Program, SelectClause, SimpleCommand, Subshell, UntilClause, WhileClause,
    Word, WordPart,
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

    pub fn execute_program(&mut self, program: &Program) -> ExecResult {
        let mut status = 0;
        for cmd in &program.commands {
            status = self.execute_complete_command(cmd)?;
        }
        Ok(status)
    }

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

    pub fn execute_pipeline(&mut self, pipeline: &Pipeline) -> ExecResult {
        let cmds = &pipeline.commands;

        if cmds.len() == 1 {
            let status = self.execute_command(&cmds[0])?;
            return Ok(if pipeline.bang { invert(status) } else { status });
        }

        let mut pipes = Vec::with_capacity(cmds.len() - 1);
        for _ in 0..cmds.len() - 1 {
            let p = sys::pipe().map_err(ExecError::Pipe)?;
            pipes.push((p.read, p.write));
        }

        let mut children: Vec<Pid> = Vec::with_capacity(cmds.len());

        for (i, cmd) in cmds.iter().enumerate() {
            match unsafe { sys::fork() }.map_err(ExecError::Fork)? {
                sys::ForkOutcome::Child => {
                    if i > 0 {
                        let (rd, _) = pipes[i - 1];
                        sys::dup2(rd, 0).ok();
                    }
                    if i < cmds.len() - 1 {
                        let (_, wr) = pipes[i];
                        sys::dup2(wr, 1).ok();
                        if pipeline.pipe_stderr.get(i).copied().unwrap_or(false) {
                            sys::dup2(wr, 2).ok();
                        }
                    }
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

        for (rd, wr) in pipes {
            sys::close(rd).ok();
            sys::close(wr).ok();
        }

        let mut last_status = 0;
        for pid in children {
            match sys::wait_pid(pid).map_err(ExecError::Wait)? {
                sys::ChildStatus::Exited(code) => last_status = code,
                sys::ChildStatus::Signaled(code) => last_status = code,
                _ => {}
            }
        }

        Ok(if pipeline.bang { invert(last_status) } else { last_status })
    }

    // ── Command dispatch ─────────────────────────────────────────

    pub fn execute_command(&mut self, cmd: &Command) -> ExecResult {
        match cmd {
            Command::Simple(simple) => self.execute_simple(simple),
            Command::Subshell(sub) => self.execute_subshell(sub),
            Command::BraceGroup(bg) => self.execute_brace_group(bg),
            Command::If(clause) => self.execute_if(clause),
            Command::For(clause) => self.execute_for(clause),
            Command::While(clause) => self.execute_while(clause),
            Command::Until(clause) => self.execute_until(clause),
            Command::Case(clause) => self.execute_case(clause),
            Command::Select(clause) => self.execute_select(clause),
            Command::FunctionDef(fdef) => {
                self.env.functions.insert(fdef.name.to_string(), (**fdef).clone());
                Ok(0)
            }
            Command::Coproc(_) => {
                eprintln!("frost: coproc not yet supported");
                Ok(1)
            }
            Command::Time(t) => {
                let start = std::time::Instant::now();
                let status = self.execute_pipeline(&t.pipeline)?;
                let elapsed = start.elapsed();
                eprintln!("real\t{:.3}s", elapsed.as_secs_f64());
                Ok(status)
            }
        }
    }

    // ── Compound commands ────────────────────────────────────────

    fn execute_subshell(&mut self, sub: &Subshell) -> ExecResult {
        match unsafe { sys::fork() }.map_err(ExecError::Fork)? {
            sys::ForkOutcome::Child => {
                if let Err(e) = redirect::apply_redirects(&sub.redirects) {
                    eprintln!("frost: {e}");
                    std::process::exit(1);
                }
                let mut status = 0;
                for cmd in &sub.body {
                    status = self.execute_complete_command(cmd).unwrap_or(1);
                }
                std::process::exit(status);
            }
            sys::ForkOutcome::Parent { child_pid } => {
                match sys::wait_pid(child_pid).map_err(ExecError::Wait)? {
                    sys::ChildStatus::Exited(code) => Ok(code),
                    sys::ChildStatus::Signaled(code) => Ok(code),
                    _ => Ok(0),
                }
            }
        }
    }

    fn execute_brace_group(&mut self, bg: &BraceGroup) -> ExecResult {
        let mut status = 0;
        for cmd in &bg.body {
            status = self.execute_complete_command(cmd)?;
        }
        Ok(status)
    }

    fn execute_if(&mut self, clause: &IfClause) -> ExecResult {
        // Evaluate condition
        let mut cond_status = 0;
        for cmd in &clause.condition {
            cond_status = self.execute_complete_command(cmd)?;
        }

        if cond_status == 0 {
            let mut status = 0;
            for cmd in &clause.then_body {
                status = self.execute_complete_command(cmd)?;
            }
            return Ok(status);
        }

        // Check elifs
        for (elif_cond, elif_body) in &clause.elifs {
            let mut cond_status = 0;
            for cmd in elif_cond {
                cond_status = self.execute_complete_command(cmd)?;
            }
            if cond_status == 0 {
                let mut status = 0;
                for cmd in elif_body {
                    status = self.execute_complete_command(cmd)?;
                }
                return Ok(status);
            }
        }

        // Else branch
        if let Some(else_body) = &clause.else_body {
            let mut status = 0;
            for cmd in else_body {
                status = self.execute_complete_command(cmd)?;
            }
            return Ok(status);
        }

        Ok(0)
    }

    fn execute_for(&mut self, clause: &ForClause) -> ExecResult {
        let words = match &clause.words {
            Some(ws) => ws.iter().map(|w| self.expand_word(w)).collect::<Vec<_>>(),
            None => self.env.positional_params.clone(),
        };

        let mut status = 0;
        for word in &words {
            self.env.set_var(&clause.var, word);
            for cmd in &clause.body {
                status = self.execute_complete_command(cmd)?;
            }
        }
        Ok(status)
    }

    fn execute_while(&mut self, clause: &WhileClause) -> ExecResult {
        let mut status = 0;
        loop {
            let mut cond_status = 0;
            for cmd in &clause.condition {
                cond_status = self.execute_complete_command(cmd)?;
            }
            if cond_status != 0 {
                break;
            }
            for cmd in &clause.body {
                status = self.execute_complete_command(cmd)?;
            }
        }
        Ok(status)
    }

    fn execute_until(&mut self, clause: &UntilClause) -> ExecResult {
        let mut status = 0;
        loop {
            let mut cond_status = 0;
            for cmd in &clause.condition {
                cond_status = self.execute_complete_command(cmd)?;
            }
            if cond_status == 0 {
                break;
            }
            for cmd in &clause.body {
                status = self.execute_complete_command(cmd)?;
            }
        }
        Ok(status)
    }

    fn execute_case(&mut self, clause: &CaseClause) -> ExecResult {
        let word = self.expand_word(&clause.word);
        for item in &clause.items {
            for pattern in &item.patterns {
                let pat = self.expand_word(pattern);
                if simple_pattern_match(&pat, &word) {
                    let mut status = 0;
                    for cmd in &item.body {
                        status = self.execute_complete_command(cmd)?;
                    }
                    return Ok(status);
                }
            }
        }
        Ok(0)
    }

    fn execute_select(&mut self, clause: &SelectClause) -> ExecResult {
        let words = match &clause.words {
            Some(ws) => ws.iter().map(|w| self.expand_word(w)).collect::<Vec<_>>(),
            None => self.env.positional_params.clone(),
        };

        // Print menu
        for (i, word) in words.iter().enumerate() {
            eprintln!("{}) {word}", i + 1);
        }

        // For non-interactive, just select first and exit
        if let Some(first) = words.first() {
            self.env.set_var(&clause.var, first);
            let mut status = 0;
            for cmd in &clause.body {
                status = self.execute_complete_command(cmd)?;
            }
            Ok(status)
        } else {
            Ok(1)
        }
    }

    // ── Simple command ───────────────────────────────────────────

    pub fn execute_simple(&mut self, cmd: &SimpleCommand) -> ExecResult {
        for assign in &cmd.assignments {
            let value = assign
                .value
                .as_ref()
                .map(|w| self.expand_word(w))
                .unwrap_or_default();
            self.env.set_var(&assign.name, &value);
        }

        if cmd.words.is_empty() {
            return Ok(0);
        }

        let argv: Vec<String> = cmd.words.iter().map(|w| self.expand_word(w)).collect();
        let name = &argv[0];

        // Check for functions first
        if let Some(fdef) = self.env.functions.get(name).cloned() {
            // Save and set positional params
            let saved = self.env.positional_params.clone();
            self.env.positional_params = argv[1..].to_vec();
            let result = self.execute_command(&fdef.body);
            self.env.positional_params = saved;
            return result;
        }

        // Check builtins — if redirects are present, fork to apply them
        if self.builtins.contains(name) {
            if cmd.redirects.is_empty() {
                let arg_refs: Vec<&str> = argv[1..].iter().map(|s| s.as_str()).collect();
                let status = self.builtins.get(name).unwrap().execute(&arg_refs, self.env);
                self.env.exit_status = status;
                return Ok(status);
            }
            // Builtins with redirects: fork so we can dup2 without affecting the parent
            return self.fork_exec_builtin(&argv, &cmd.redirects);
        }

        // External command: fork + exec
        self.fork_exec(&argv, &cmd.redirects)
    }

    /// Fork to run a builtin with redirects applied in the child.
    fn fork_exec_builtin(
        &mut self,
        argv: &[String],
        redirects: &[frost_parser::ast::Redirect],
    ) -> ExecResult {
        match unsafe { sys::fork() }.map_err(ExecError::Fork)? {
            sys::ForkOutcome::Child => {
                if let Err(e) = redirect::apply_redirects(redirects) {
                    eprintln!("frost: {e}");
                    std::process::exit(1);
                }
                let arg_refs: Vec<&str> = argv[1..].iter().map(|s| s.as_str()).collect();
                let status = self.builtins.get(&argv[0]).unwrap().execute(&arg_refs, self.env);
                std::process::exit(status);
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
                let err = sys::exec(&c_argv, &c_envp);
                eprintln!("frost: {}: {err}", argv[0]);
                std::process::exit(if err == nix::errno::Errno::ENOENT { 127 } else { 126 });
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

    // ── Word expansion ───────────────────────────────────────────

    /// Expand a Word AST node into a string, resolving variables, tilde, etc.
    pub fn expand_word(&self, word: &Word) -> String {
        let mut out = String::new();
        for part in &word.parts {
            self.expand_part(part, &mut out);
        }
        out
    }

    fn expand_part(&self, part: &WordPart, out: &mut String) {
        match part {
            WordPart::Literal(s) | WordPart::SingleQuoted(s) => out.push_str(s),
            WordPart::DoubleQuoted(parts) => {
                for inner in parts {
                    self.expand_part(inner, out);
                }
            }
            WordPart::DollarVar(name) => {
                let val = match name.as_str() {
                    "?" => self.env.exit_status.to_string(),
                    "$" => self.env.pid.to_string(),
                    "!" => String::new(), // last background PID
                    "#" => self.env.positional_params.len().to_string(),
                    "*" | "@" => self.env.positional_params.join(" "),
                    "0" => "frost".to_string(),
                    n if n.len() == 1 && n.as_bytes()[0].is_ascii_digit() => {
                        let idx = (n.as_bytes()[0] - b'1') as usize;
                        self.env.positional_params.get(idx).cloned().unwrap_or_default()
                    }
                    _ => self.env.get_var(name).unwrap_or("").to_string(),
                };
                out.push_str(&val);
            }
            WordPart::DollarBrace { param, operator, arg } => {
                let val = self.env.get_var(param).unwrap_or("").to_string();
                // Basic parameter expansion operators
                match operator.as_deref() {
                    Some(":-") => {
                        if val.is_empty() {
                            if let Some(a) = arg {
                                out.push_str(&self.expand_word(a));
                            }
                        } else {
                            out.push_str(&val);
                        }
                    }
                    Some(":+") => {
                        if !val.is_empty() {
                            if let Some(a) = arg {
                                out.push_str(&self.expand_word(a));
                            }
                        }
                    }
                    _ => out.push_str(&val),
                }
            }
            WordPart::CommandSub(program) => {
                // Execute in a subshell and capture stdout
                let output = capture_command_sub(program, self.env);
                // Trim trailing newlines (POSIX behavior)
                out.push_str(output.trim_end_matches('\n'));
            }
            WordPart::ArithSub(expr) => {
                let result = eval_arithmetic(expr, self.env);
                out.push_str(&result.to_string());
            }
            WordPart::Tilde(user) => {
                if user.is_empty() {
                    if let Some(home) = self.env.get_var("HOME") {
                        out.push_str(home);
                    } else {
                        out.push('~');
                    }
                } else {
                    // ~user expansion
                    out.push('~');
                    out.push_str(user);
                }
            }
            WordPart::Glob(_) => {
                // Glob expansion happens at a higher level (after word expansion)
                // For now, pass through as literal
                match part {
                    WordPart::Glob(frost_parser::ast::GlobKind::Star) => out.push('*'),
                    WordPart::Glob(frost_parser::ast::GlobKind::Question) => out.push('?'),
                    WordPart::Glob(frost_parser::ast::GlobKind::At) => out.push('@'),
                    _ => {}
                }
            }
        }
    }
}

/// Invert exit status for `!` pipelines.
fn invert(status: i32) -> i32 {
    if status == 0 { 1 } else { 0 }
}

/// Simple glob-style pattern matching for case statements.
fn simple_pattern_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return pattern == text;
    }
    // Basic wildcard matching
    let mut pi = pattern.chars().peekable();
    let mut ti = text.chars().peekable();

    match_pattern(&mut pi.collect::<Vec<_>>(), &ti.collect::<Vec<_>>())
}

fn match_pattern(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let (mut star_p, mut star_t) = (usize::MAX, 0);

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star_p = p;
            star_t = t;
            p += 1;
        } else if star_p != usize::MAX {
            p = star_p + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }

    p == pattern.len()
}

/// Capture stdout from a command substitution.
fn capture_command_sub(program: &Program, env: &ShellEnv) -> String {
    // For command substitutions, we need to fork and capture stdout
    // This is a simplified implementation
    let _ = (program, env);
    String::new()
}

/// Evaluate an arithmetic expression.
fn eval_arithmetic(expr: &str, env: &ShellEnv) -> i64 {
    let expr = expr.trim();
    // Simple integer parsing and variable lookup
    if let Ok(n) = expr.parse::<i64>() {
        return n;
    }
    // Variable reference
    if let Some(val) = env.get_var(expr) {
        if let Ok(n) = val.parse::<i64>() {
            return n;
        }
    }
    // Simple binary operations
    for (op_str, op_fn) in &[
        ("+", (|a: i64, b: i64| a + b) as fn(i64, i64) -> i64),
        ("-", (|a, b| a - b) as fn(i64, i64) -> i64),
        ("*", (|a, b| a * b) as fn(i64, i64) -> i64),
        ("/", (|a, b| if b != 0 { a / b } else { 0 }) as fn(i64, i64) -> i64),
        ("%", (|a, b| if b != 0 { a % b } else { 0 }) as fn(i64, i64) -> i64),
    ] {
        if let Some(pos) = expr.rfind(op_str) {
            if pos > 0 {
                let left = eval_arithmetic(&expr[..pos], env);
                let right = eval_arithmetic(&expr[pos + op_str.len()..], env);
                return op_fn(left, right);
            }
        }
    }
    0
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
        let env = ShellEnv::new();
        let exec = Executor { env: &mut ShellEnv::new(), builtins: frost_builtins::default_builtins(), jobs: JobTable::new() };
        let word = literal_word("hello");
        assert_eq!(exec.expand_word(&word), "hello");
    }

    #[test]
    fn expand_dollar_var() {
        let mut env = ShellEnv::new();
        env.set_var("FOO", "bar");
        let exec = Executor::new(&mut env);
        let word = Word {
            parts: vec![WordPart::DollarVar("FOO".into())],
            span: Span::new(0, 4),
        };
        assert_eq!(exec.expand_word(&word), "bar");
    }

    #[test]
    fn expand_dollar_question() {
        let mut env = ShellEnv::new();
        env.exit_status = 42;
        let exec = Executor::new(&mut env);
        let word = Word {
            parts: vec![WordPart::DollarVar("?".into())],
            span: Span::new(0, 2),
        };
        assert_eq!(exec.expand_word(&word), "42");
    }

    #[test]
    fn expand_tilde() {
        let mut env = ShellEnv::new();
        env.set_var("HOME", "/users/test");
        let exec = Executor::new(&mut env);
        let word = Word {
            parts: vec![WordPart::Tilde("".into())],
            span: Span::new(0, 1),
        };
        assert_eq!(exec.expand_word(&word), "/users/test");
    }

    #[test]
    fn expand_double_quoted_with_var() {
        let mut env = ShellEnv::new();
        env.set_var("NAME", "world");
        let exec = Executor::new(&mut env);
        let word = Word {
            parts: vec![WordPart::DoubleQuoted(vec![
                WordPart::Literal("hello ".into()),
                WordPart::DollarVar("NAME".into()),
            ])],
            span: Span::new(0, 14),
        };
        assert_eq!(exec.expand_word(&word), "hello world");
    }

    #[test]
    fn expand_positional_params() {
        let mut env = ShellEnv::new();
        env.positional_params = vec!["a".into(), "b".into(), "c".into()];
        let exec = Executor::new(&mut env);
        let word = Word {
            parts: vec![WordPart::DollarVar("#".into())],
            span: Span::new(0, 2),
        };
        assert_eq!(exec.expand_word(&word), "3");
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

    #[test]
    fn pattern_match_exact() {
        assert!(simple_pattern_match("hello", "hello"));
        assert!(!simple_pattern_match("hello", "world"));
    }

    #[test]
    fn pattern_match_star() {
        assert!(simple_pattern_match("*", "anything"));
        assert!(simple_pattern_match("hel*", "hello"));
        assert!(simple_pattern_match("*lo", "hello"));
        assert!(!simple_pattern_match("hel*", "world"));
    }

    #[test]
    fn pattern_match_question() {
        assert!(simple_pattern_match("h?llo", "hello"));
        assert!(!simple_pattern_match("h?llo", "hllo"));
    }

    #[test]
    fn arithmetic_basic() {
        let env = ShellEnv::new();
        assert_eq!(eval_arithmetic("42", &env), 42);
        assert_eq!(eval_arithmetic("3+4", &env), 7);
        assert_eq!(eval_arithmetic("10-3", &env), 7);
        assert_eq!(eval_arithmetic("6*7", &env), 42);
    }
}
