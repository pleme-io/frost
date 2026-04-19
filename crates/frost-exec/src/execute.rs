//! The main execution engine.
//!
//! Walks the AST and executes commands by forking child processes,
//! setting up pipes, and applying redirections. All platform-specific
//! system calls go through [`crate::sys`].

use std::ffi::CString;
use std::os::fd::RawFd;

use nix::unistd::Pid;

use frost_builtins::BuiltinRegistry;
use frost_parser::ast::{
    AlwaysClause, ArithForClause, BraceGroup, CaseClause, CaseTerminator, Command,
    CompleteCommand, ForClause, FunctionDef, GlobKind, IfClause, List, ListOp, Pipeline, Program,
    Redirect, RedirectOp, RepeatClause, SelectClause, SimpleCommand, Subshell, UntilClause,
    WhileClause, Word, WordPart,
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
            Command::Subshell(sub) => self.execute_subshell(sub),
            Command::BraceGroup(bg) => self.execute_brace_group(bg),
            Command::If(clause) => self.execute_if(clause),
            Command::For(clause) => self.execute_for(clause),
            Command::ArithFor(clause) => self.execute_arith_for(clause),
            Command::While(clause) => self.execute_while(clause),
            Command::Until(clause) => self.execute_until(clause),
            Command::Case(clause) => self.execute_case(clause),
            Command::Select(clause) => self.execute_select(clause),
            Command::Repeat(clause) => self.execute_repeat(clause),
            Command::Always(clause) => self.execute_always(clause),
            Command::FunctionDef(fdef) => self.execute_function_def(fdef),
            Command::Coproc(_) => {
                // Coproc is complex — stub for now.
                eprintln!("frost: coproc: not yet implemented");
                Ok(1)
            }
            Command::Time(tc) => {
                let start = std::time::Instant::now();
                let status = self.execute_pipeline(&tc.pipeline)?;
                let elapsed = start.elapsed();
                eprintln!(
                    "\nreal\t{:.3}s\nuser\t0.000s\nsys\t0.000s",
                    elapsed.as_secs_f64()
                );
                Ok(status)
            }
        }
    }

    // ── Simple command ───────────────────────────────────────────

    /// Execute a simple command (assignments + words + redirects).
    pub fn execute_simple(&mut self, cmd: &SimpleCommand) -> ExecResult {
        // Process assignments.
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

        let argv: Vec<String> = cmd
            .words
            .iter()
            .flat_map(|w| self.expand_word_glob(w))
            .filter(|s| !s.is_empty())
            .collect();

        // If all words expanded to empty, it's a no-op (reset exit status to 0).
        if argv.is_empty() {
            self.env.exit_status = 0;
            return Ok(0);
        }

        let name = &argv[0];

        // Check for shell functions first.
        if let Some(func) = self.env.functions.get(name).cloned() {
            return self.call_function(&func, &argv[1..], &cmd.redirects);
        }

        // Special builtins that need executor access.
        match name.as_str() {
            "eval" => return self.execute_eval(&argv[1..], &cmd.redirects),
            "source" | "." => return self.execute_source(&argv[1..], &cmd.redirects),
            "shift" => return self.execute_shift(&argv[1..]),
            "[[" => return self.execute_cond_bracket(&argv[1..]),
            "((" => return self.execute_arith_command(&argv[1..]),
            _ => {}
        }

        // Check builtins.
        if self.builtins.contains(name) {
            return self.execute_builtin(name, &argv[1..], &cmd.redirects);
        }

        // External command: fork + exec.
        self.fork_exec(&argv, &cmd.redirects)
    }

    /// Execute a builtin command with redirect support.
    fn execute_builtin(
        &mut self,
        name: &str,
        args: &[String],
        redirects: &[Redirect],
    ) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(redirects)?;

        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let status = self
            .builtins
            .get(name)
            .unwrap()
            .execute(&arg_refs, self.env);
        self.env.exit_status = status;

        self.restore_fds(saved_fds);
        Ok(status)
    }

    /// Execute `eval` by joining args and re-parsing.
    fn execute_eval(&mut self, args: &[String], redirects: &[Redirect]) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(redirects)?;
        let code = args.join(" ");
        let status = self.eval_string(&code);
        self.restore_fds(saved_fds);
        Ok(status)
    }

    /// Execute `source`/`.` by reading a file and executing it.
    fn execute_source(&mut self, args: &[String], redirects: &[Redirect]) -> ExecResult {
        if args.is_empty() {
            eprintln!("frost: source: filename argument required");
            return Ok(1);
        }
        let saved_fds = self.save_and_apply_redirects(redirects)?;

        let path = &args[0];
        let code = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("frost: {path}: {e}");
                self.restore_fds(saved_fds);
                return Ok(1);
            }
        };

        // Set positional params from remaining args.
        let saved_params = if args.len() > 1 {
            Some(std::mem::replace(
                &mut self.env.positional_params,
                args[1..].to_vec(),
            ))
        } else {
            None
        };

        let status = self.eval_string(&code);

        if let Some(params) = saved_params {
            self.env.positional_params = params;
        }

        self.restore_fds(saved_fds);
        Ok(status)
    }

    /// Execute `shift` — remove positional parameters from the front.
    fn execute_shift(&mut self, args: &[String]) -> ExecResult {
        let n = args
            .first()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
        if n <= self.env.positional_params.len() {
            self.env.positional_params.drain(..n);
            Ok(0)
        } else {
            eprintln!("shift: shift count must be <= $#");
            Ok(1)
        }
    }

    /// Parse and execute a string of shell code.
    fn eval_string(&mut self, code: &str) -> i32 {
        let tokens = tokenize(code);
        let mut parser = frost_parser::Parser::new(&tokens);
        let program = parser.parse();
        self.execute_program(&program).unwrap_or(1)
    }

    /// Call a shell function.
    fn call_function(
        &mut self,
        func: &FunctionDef,
        args: &[String],
        redirects: &[Redirect],
    ) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(redirects)?;

        // Save and set positional parameters.
        let saved_params = std::mem::replace(
            &mut self.env.positional_params,
            args.iter().map(|s| s.to_string()).collect(),
        );

        let status = self.execute_command(&func.body)?;

        // Restore positional parameters.
        self.env.positional_params = saved_params;
        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    // ── Arithmetic (( ... )) ───────────────────────────────────

    /// Execute a (( ... )) arithmetic command.
    fn execute_arith_command(&mut self, args: &[String]) -> ExecResult {
        // Collect the expression, stripping trailing ))
        let expr: String = args
            .iter()
            .filter(|s| s.as_str() != "))")
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");

        // Handle assignment/increment operators BEFORE expanding variables.
        let result = self.eval_arith_with_assignment(expr.trim());
        let status = if result == 0 { 1 } else { 0 }; // arithmetic: 0 is false, non-zero is true
        self.env.exit_status = status;
        Ok(status)
    }

    /// Evaluate arithmetic expression with assignment support (++, --, =, +=, -=).
    /// NOTE: This receives the raw expression (before variable expansion) so that
    /// it can identify variable names for assignment operators.
    fn eval_arith_with_assignment(&mut self, expr: &str) -> i64 {
        let expr = expr.trim();

        // Handle var++ and var--
        if expr.ends_with("++") && !expr.ends_with("+++") {
            let var = expr[..expr.len() - 2].trim();
            if is_valid_var_name(var) {
                let val = self.env.get_var(var).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                self.env.set_var(var, &(val + 1).to_string());
                return val; // post-increment returns old value
            }
        }
        if expr.ends_with("--") && !expr.ends_with("---") {
            let var = expr[..expr.len() - 2].trim();
            if is_valid_var_name(var) {
                let val = self.env.get_var(var).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                self.env.set_var(var, &(val - 1).to_string());
                return val;
            }
        }

        // Handle ++var and --var
        if expr.starts_with("++ ") || (expr.starts_with("++") && expr.len() > 2 && expr.as_bytes()[2].is_ascii_alphabetic()) {
            let var = expr.trim_start_matches("++").trim_start_matches(' ');
            if is_valid_var_name(var) {
                let val = self.env.get_var(var).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                let new_val = val + 1;
                self.env.set_var(var, &new_val.to_string());
                return new_val;
            }
        }
        if expr.starts_with("-- ") || (expr.starts_with("--") && expr.len() > 2 && expr.as_bytes()[2].is_ascii_alphabetic()) {
            let var = expr.trim_start_matches("--").trim_start_matches(' ');
            if is_valid_var_name(var) {
                let val = self.env.get_var(var).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                let new_val = val - 1;
                self.env.set_var(var, &new_val.to_string());
                return new_val;
            }
        }

        // Handle var+=expr, var-=expr, var*=expr, var/=expr
        for op in &["+=", "-=", "*=", "/=", "%="] {
            if let Some(eq_pos) = expr.find(op) {
                let var = expr[..eq_pos].trim();
                if is_valid_var_name(var) {
                    let rhs = expr[eq_pos + op.len()..].trim();
                    let rhs_expanded = self.expand_arith_vars(rhs);
                    let var_val = self.env.get_var(var).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                    let rhs_val = eval_arith_expr(&rhs_expanded).unwrap_or(0);
                    let new_val = match *op {
                        "+=" => var_val + rhs_val,
                        "-=" => var_val - rhs_val,
                        "*=" => var_val * rhs_val,
                        "/=" if rhs_val != 0 => var_val / rhs_val,
                        "%=" if rhs_val != 0 => var_val % rhs_val,
                        _ => var_val,
                    };
                    self.env.set_var(var, &new_val.to_string());
                    return new_val;
                }
            }
        }

        // Handle simple assignment: var = expr (must check for == first)
        // Look for single = that isn't part of ==, !=, <=, >=
        if let Some(eq_pos) = find_assignment_eq(expr) {
            let var = expr[..eq_pos].trim();
            if is_valid_var_name(var) {
                let rhs = expr[eq_pos + 1..].trim();
                let rhs_expanded = self.expand_arith_vars(rhs);
                let val = eval_arith_expr(&rhs_expanded).unwrap_or(0);
                self.env.set_var(var, &val.to_string());
                return val;
            }
        }

        // Regular arithmetic evaluation with variable expansion.
        let expanded = self.expand_arith_vars(expr);
        eval_arith_expr(&expanded).unwrap_or(0)
    }

    // ── Conditional [[ ... ]] ──────────────────────────────────

    /// Execute a [[ ... ]] conditional expression.
    fn execute_cond_bracket(&mut self, args: &[String]) -> ExecResult {
        // Strip trailing ]] if present.
        let args: Vec<&str> = args
            .iter()
            .map(|s| s.as_str())
            .filter(|s| *s != "]]")
            .collect();

        let result = eval_cond_expr(&args, self);
        let status = if result { 0 } else { 1 };
        self.env.exit_status = status;
        Ok(status)
    }

    // ── Compound commands ────────────────────────────────────────

    fn execute_subshell(&mut self, sub: &Subshell) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&sub.redirects)?;

        match unsafe { sys::fork() }.map_err(ExecError::Fork)? {
            sys::ForkOutcome::Child => {
                let mut status = 0;
                for cmd in &sub.body {
                    status = self.execute_complete_command(cmd).unwrap_or(1);
                }
                std::process::exit(status);
            }
            sys::ForkOutcome::Parent { child_pid } => {
                let status = match sys::wait_pid(child_pid).map_err(ExecError::Wait)? {
                    sys::ChildStatus::Exited(code) => code,
                    sys::ChildStatus::Signaled(code) => code,
                    _ => 0,
                };
                self.restore_fds(saved_fds);
                self.env.exit_status = status;
                Ok(status)
            }
        }
    }

    fn execute_brace_group(&mut self, bg: &BraceGroup) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&bg.redirects)?;

        let mut status = 0;
        for cmd in &bg.body {
            status = self.execute_complete_command(cmd)?;
        }

        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    fn execute_if(&mut self, clause: &IfClause) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&clause.redirects)?;

        // Evaluate condition.
        let mut cond_status = 0;
        for cmd in &clause.condition {
            cond_status = self.execute_complete_command(cmd)?;
        }

        let status = if cond_status == 0 {
            // then branch
            let mut s = 0;
            for cmd in &clause.then_body {
                s = self.execute_complete_command(cmd)?;
            }
            s
        } else {
            // Try elif branches.
            let mut found = false;
            let mut s = 0;
            for (elif_cond, elif_body) in &clause.elifs {
                let mut elif_status = 0;
                for cmd in elif_cond {
                    elif_status = self.execute_complete_command(cmd)?;
                }
                if elif_status == 0 {
                    for cmd in elif_body {
                        s = self.execute_complete_command(cmd)?;
                    }
                    found = true;
                    break;
                }
            }
            if !found {
                if let Some(else_body) = &clause.else_body {
                    for cmd in else_body {
                        s = self.execute_complete_command(cmd)?;
                    }
                }
            }
            s
        };

        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    fn execute_for(&mut self, clause: &ForClause) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&clause.redirects)?;

        let words = if let Some(word_list) = &clause.words {
            word_list.iter().map(|w| self.expand_word(w)).collect()
        } else {
            self.env.positional_params.clone()
        };

        let mut status = 0;
        for word in &words {
            self.env.set_var(&clause.var, word);
            for cmd in &clause.body {
                status = self.execute_complete_command(cmd)?;
            }
        }

        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    fn execute_arith_for(&mut self, clause: &ArithForClause) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&clause.redirects)?;

        // Execute the init expression.
        if !clause.init.is_empty() {
            self.eval_arith_with_assignment(&clause.init);
        }

        let mut status = 0;
        loop {
            // Evaluate the condition — empty condition means infinite loop (true).
            if !clause.condition.is_empty() {
                let cond_val = self.eval_arith_with_assignment(&clause.condition);
                if cond_val == 0 {
                    break;
                }
            }

            // Execute the body.
            for cmd in &clause.body {
                status = self.execute_complete_command(cmd)?;
            }

            // Execute the step expression.
            if !clause.step.is_empty() {
                self.eval_arith_with_assignment(&clause.step);
            }
        }

        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    fn execute_while(&mut self, clause: &WhileClause) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&clause.redirects)?;

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

        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    fn execute_until(&mut self, clause: &UntilClause) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&clause.redirects)?;

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

        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    fn execute_repeat(&mut self, clause: &RepeatClause) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&clause.redirects)?;

        let count_str = self.expand_word(&clause.count);
        let count: i64 = count_str.trim().parse().unwrap_or(0);

        let mut status = 0;
        for _ in 0..count {
            for cmd in &clause.body {
                status = self.execute_complete_command(cmd)?;
            }
        }

        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    fn execute_always(&mut self, clause: &AlwaysClause) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&clause.redirects)?;

        // Execute the try body, capturing any error/status.
        let try_result = (|| {
            let mut status = 0;
            for cmd in &clause.try_body {
                status = self.execute_complete_command(cmd)?;
            }
            Ok(status)
        })();

        // Always execute the always body regardless of try body outcome.
        let mut always_status = 0;
        for cmd in &clause.always_body {
            always_status = self.execute_complete_command(cmd)?;
        }

        self.restore_fds(saved_fds);

        // If the try body had an execution error (not just non-zero exit),
        // propagate it. Otherwise, the final status is from the always body.
        match try_result {
            Ok(_try_status) => {
                self.env.exit_status = always_status;
                Ok(always_status)
            }
            Err(e) => {
                // The always body already ran; propagate the original error.
                self.env.exit_status = always_status;
                Err(e)
            }
        }
    }

    fn execute_case(&mut self, clause: &CaseClause) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&clause.redirects)?;

        let word = self.expand_word(&clause.word);
        let mut status = 0;

        let mut i = 0;
        while i < clause.items.len() {
            let item = &clause.items[i];
            let matched = item
                .patterns
                .iter()
                .any(|p| glob_match_word(&self.expand_word(p), &word));

            if matched {
                for cmd in &item.body {
                    status = self.execute_complete_command(cmd)?;
                }
                match item.terminator {
                    CaseTerminator::DoubleSemi => break,
                    CaseTerminator::SemiAnd => {
                        // ;& — fall through to next item body unconditionally
                        i += 1;
                        if i < clause.items.len() {
                            for cmd in &clause.items[i].body {
                                status = self.execute_complete_command(cmd)?;
                            }
                        }
                        break;
                    }
                    CaseTerminator::SemiPipe => {
                        // ;;| — continue testing patterns
                        i += 1;
                        continue;
                    }
                }
            }
            i += 1;
        }

        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    fn execute_select(&mut self, clause: &SelectClause) -> ExecResult {
        let saved_fds = self.save_and_apply_redirects(&clause.redirects)?;

        let words: Vec<String> = if let Some(word_list) = &clause.words {
            word_list.iter().map(|w| self.expand_word(w)).collect()
        } else {
            self.env.positional_params.clone()
        };

        // Print menu and read REPLY. Simplified — just run body once per word.
        let mut status = 0;
        for (i, word) in words.iter().enumerate() {
            eprintln!("{}) {word}", i + 1);
        }
        // Set var to empty and run body once (simplified select).
        self.env.set_var(&clause.var, "");
        for cmd in &clause.body {
            status = self.execute_complete_command(cmd)?;
        }

        self.restore_fds(saved_fds);
        self.env.exit_status = status;
        Ok(status)
    }

    fn execute_function_def(&mut self, fdef: &FunctionDef) -> ExecResult {
        self.env.functions.insert(fdef.name.to_string(), fdef.clone());
        Ok(0)
    }

    // ── Word expansion ──────────────────────────────────────────

    /// Expand a Word AST node into a plain string, performing all expansions.
    pub fn expand_word(&mut self, word: &Word) -> String {
        let mut out = String::new();
        for part in &word.parts {
            self.expand_word_part(part, &mut out);
        }
        out
    }

    /// Expand a word, performing glob expansion if the word contains glob characters.
    /// Returns multiple words if glob matches are found.
    fn expand_word_glob(&mut self, word: &Word) -> Vec<String> {
        let has_glob = word.parts.iter().any(|p| matches!(p, WordPart::Glob(_)));
        let expanded = self.expand_word(word);
        if has_glob {
            let mut matches: Vec<String> = glob::glob(&expanded)
                .ok()
                .map(|paths| {
                    paths
                        .filter_map(|p| p.ok())
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            if matches.is_empty() {
                // No matches — return the pattern literally (zsh behavior depends on options).
                vec![expanded]
            } else {
                matches.sort();
                matches
            }
        } else {
            vec![expanded]
        }
    }

    fn expand_word_part(&mut self, part: &WordPart, out: &mut String) {
        match part {
            WordPart::Literal(s) => out.push_str(s),
            WordPart::SingleQuoted(s) => out.push_str(s),
            WordPart::DoubleQuoted(parts) => {
                for inner in parts {
                    self.expand_word_part(inner, out);
                }
            }
            WordPart::DollarVar(name) => {
                out.push_str(&self.expand_special_var(name));
            }
            WordPart::DollarBrace {
                param,
                operator,
                arg,
            } => {
                let base = self.expand_special_var(param);
                if let Some(op) = operator {
                    let arg_val = arg
                        .as_ref()
                        .map(|w| self.expand_word(w))
                        .unwrap_or_default();
                    out.push_str(&apply_param_op(&base, op, &arg_val));
                } else {
                    out.push_str(&base);
                }
            }
            WordPart::CommandSub(program) => {
                out.push_str(&self.execute_command_sub(program));
            }
            WordPart::ArithSub(expr) => {
                // Basic arithmetic evaluation.
                out.push_str(&self.eval_arith(expr));
            }
            WordPart::Tilde(user) => {
                if user.is_empty() {
                    if let Some(home) = self.env.get_var("HOME") {
                        out.push_str(home);
                    } else {
                        out.push('~');
                    }
                } else {
                    out.push('~');
                    out.push_str(user);
                }
            }
            WordPart::Glob(_) => {
                // In command context, globs should be expanded via filesystem.
                // For now, pass through the literal character.
                match part {
                    WordPart::Glob(frost_parser::ast::GlobKind::Star) => out.push('*'),
                    WordPart::Glob(frost_parser::ast::GlobKind::Question) => out.push('?'),
                    WordPart::Glob(frost_parser::ast::GlobKind::At) => out.push('@'),
                    _ => {}
                }
            }
        }
    }

    /// Expand special shell variables: $?, $$, $#, $@, $*, $0..$9, named vars.
    fn expand_special_var(&self, name: &str) -> String {
        match name {
            "?" => self.env.exit_status.to_string(),
            "$" => self.env.pid.to_string(),
            "#" => self.env.positional_params.len().to_string(),
            "@" | "*" => self.env.positional_params.join(" "),
            "0" => "frost".to_string(),
            "LINENO" => "0".to_string(),
            _ => {
                // Check if it's a positional parameter (1-9).
                if let Ok(n) = name.parse::<usize>() {
                    if n >= 1 {
                        return self
                            .env
                            .positional_params
                            .get(n - 1)
                            .cloned()
                            .unwrap_or_default();
                    }
                }
                self.env.get_var(name).unwrap_or("").to_string()
            }
        }
    }

    /// Execute a command substitution and return its stdout.
    fn execute_command_sub(&mut self, program: &Program) -> String {
        // Reconstruct the command text from the AST and run it via
        // `frost -c` in a subprocess. This avoids complex fd/fork issues
        // when builtins use Rust stdio (which buffers above the fd level).
        //
        // For simple single-command programs, we can reconstruct the text.
        // For complex ones, we serialize via the AST.
        let code = reconstruct_program(program);
        if code.is_empty() {
            return String::new();
        }

        let frost_path = std::env::current_exe().unwrap_or_else(|_| "frost".into());
        let mut cmd = std::process::Command::new(&frost_path);
        cmd.arg("-c").arg(&code);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());

        // Pass the current environment.
        cmd.env_clear();
        for (name, var) in &self.env.variables {
            if var.export {
                cmd.env(name, &var.value);
            }
        }

        match cmd.output() {
            Ok(output) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                // Strip trailing newlines (shell convention).
                while stdout.ends_with('\n') {
                    stdout.pop();
                }
                stdout
            }
            Err(_) => String::new(),
        }
    }

    /// Basic arithmetic evaluation.
    fn eval_arith(&self, expr: &str) -> String {
        // Expand variables in the expression first.
        let expanded = self.expand_arith_vars(expr);
        // Evaluate the numeric expression.
        match eval_arith_expr(&expanded) {
            Some(val) => val.to_string(),
            None => "0".to_string(),
        }
    }

    /// Replace variable names in arithmetic expressions with their values.
    fn expand_arith_vars(&self, expr: &str) -> String {
        let mut out = String::new();
        let chars: Vec<char> = expr.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '$' {
                i += 1;
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                out.push_str(&self.expand_special_var(&name));
            } else if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                // In arithmetic context, bare words are variable names.
                if let Some(val) = self.env.get_var(&name) {
                    out.push_str(val);
                } else {
                    out.push('0');
                }
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    }

    // ── Redirect helpers ─────────────────────────────────────────

    /// Save current fds and apply redirects. Returns saved fd pairs for restore.
    fn save_and_apply_redirects(
        &mut self,
        redirects: &[Redirect],
    ) -> Result<Vec<(RawFd, RawFd)>, ExecError> {
        if redirects.is_empty() {
            return Ok(Vec::new());
        }

        // Pre-expand redirect targets (needed for herestrings with variable expansion).
        let expanded_targets: Vec<String> = redirects
            .iter()
            .map(|r| self.expand_word(&r.target))
            .collect();

        let mut saved = Vec::new();
        for redir in redirects {
            let target_fd = redirect::target_fd_for(redir);
            // Save the current fd by duping it.
            if let Ok(saved_fd) = nix::unistd::dup(target_fd) {
                use std::os::fd::IntoRawFd;
                saved.push((target_fd, saved_fd.into_raw_fd()));
            }
        }
        redirect::apply_redirects_expanded(redirects, &expanded_targets)?;
        Ok(saved)
    }

    /// Restore saved file descriptors.
    fn restore_fds(&self, saved: Vec<(RawFd, RawFd)>) {
        for (target_fd, saved_fd) in saved {
            sys::dup2(saved_fd, target_fd).ok();
            sys::close(saved_fd).ok();
        }
    }

    /// Fork a child process and exec an external command.
    fn fork_exec(
        &mut self,
        argv: &[String],
        redirects: &[Redirect],
    ) -> ExecResult {
        let c_argv: Vec<CString> = argv
            .iter()
            .filter_map(|a| CString::new(a.as_bytes()).ok())
            .collect();

        let c_envp = self.env.to_env_vec();

        // Pre-expand redirect targets for herestrings/heredocs.
        let expanded_targets: Vec<String> = redirects
            .iter()
            .map(|r| self.expand_word(&r.target))
            .collect();

        match unsafe { sys::fork() }.map_err(ExecError::Fork)? {
            sys::ForkOutcome::Child => {
                if let Err(e) = redirect::apply_redirects_expanded(redirects, &expanded_targets) {
                    eprintln!("frost: {e}");
                    std::process::exit(1);
                }

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

// ── Free functions ───────────────────────────────────────────────────

/// Invert exit status for `!` pipelines: 0 -> 1, non-zero -> 0.
fn invert(status: i32) -> i32 {
    if status == 0 { 1 } else { 0 }
}

/// Check if a string is a valid shell variable name.
fn is_valid_var_name(s: &str) -> bool {
    !s.is_empty()
        && (s.as_bytes()[0].is_ascii_alphabetic() || s.as_bytes()[0] == b'_')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Find the position of a simple assignment `=` in an arithmetic expression.
/// Skips `==`, `!=`, `<=`, `>=`, `+=`, `-=`, `*=`, `/=`, `%=`.
fn find_assignment_eq(expr: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'=' {
            // Skip ==
            if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                continue;
            }
            // Skip !=, <=, >=, +=, -=, *=, /=, %=
            if i > 0
                && matches!(
                    bytes[i - 1],
                    b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%'
                )
            {
                continue;
            }
            return Some(i);
        }
    }
    None
}

/// Evaluate a [[ ... ]] conditional expression.
/// Supports:
///   - Unary: -n, -z, -f, -d, -e, -r, -w, -x, -s, -L, -h, -t, -b, -c, -p, -S, -o, -v
///   - Binary: =, ==, !=, -eq, -ne, -lt, -le, -gt, -ge, =~, <, >
///   - Logical: &&, ||, !
///   - Grouping: ( ... )
fn eval_cond_expr(args: &[&str], exec: &Executor<'_>) -> bool {
    if args.is_empty() {
        return false;
    }

    // Simple dispatcher based on arg count and structure.
    let mut pos = 0;
    eval_cond_or(args, &mut pos, exec)
}

fn eval_cond_or(args: &[&str], pos: &mut usize, exec: &Executor<'_>) -> bool {
    let mut result = eval_cond_and(args, pos, exec);
    while *pos < args.len() && args[*pos] == "||" {
        *pos += 1;
        let right = eval_cond_and(args, pos, exec);
        result = result || right;
    }
    result
}

fn eval_cond_and(args: &[&str], pos: &mut usize, exec: &Executor<'_>) -> bool {
    let mut result = eval_cond_not(args, pos, exec);
    while *pos < args.len() && args[*pos] == "&&" {
        *pos += 1;
        let right = eval_cond_not(args, pos, exec);
        result = result && right;
    }
    result
}

fn eval_cond_not(args: &[&str], pos: &mut usize, exec: &Executor<'_>) -> bool {
    if *pos < args.len() && args[*pos] == "!" {
        *pos += 1;
        return !eval_cond_primary(args, pos, exec);
    }
    eval_cond_primary(args, pos, exec)
}

fn eval_cond_primary(args: &[&str], pos: &mut usize, exec: &Executor<'_>) -> bool {
    if *pos >= args.len() {
        return false;
    }

    // Grouping: ( expr )
    if args[*pos] == "(" {
        *pos += 1;
        let result = eval_cond_or(args, pos, exec);
        if *pos < args.len() && args[*pos] == ")" {
            *pos += 1;
        }
        return result;
    }

    // Unary operators
    if args[*pos].starts_with('-') && args[*pos].len() == 2 && *pos + 1 < args.len() {
        let op = args[*pos];
        // Check if next arg is a binary operator — if so, this isn't a unary test
        if *pos + 2 < args.len() {
            let next_next = args[*pos + 2];
            // If the arg after the next is a known binary op, treat this as a value
            if is_binary_cond_op(args[*pos + 1]) {
                // Fall through to binary handling below
            } else {
                return eval_unary_cond(op, args, pos);
            }
        } else {
            return eval_unary_cond(op, args, pos);
        }
    }

    // If only one arg remains, -n test (true if non-empty)
    if *pos + 1 >= args.len() || !is_binary_cond_op_or_end(args.get(*pos + 1).copied()) {
        let val = args[*pos];
        *pos += 1;
        return !val.is_empty();
    }

    // Binary operators
    let left = args[*pos];
    *pos += 1;
    let op = args[*pos];
    *pos += 1;
    let right = if *pos < args.len() {
        let r = args[*pos];
        *pos += 1;
        r
    } else {
        ""
    };

    match op {
        "=" | "==" => left == right,
        "!=" => left != right,
        "-eq" => left.parse::<i64>().unwrap_or(0) == right.parse::<i64>().unwrap_or(0),
        "-ne" => left.parse::<i64>().unwrap_or(0) != right.parse::<i64>().unwrap_or(0),
        "-lt" => left.parse::<i64>().unwrap_or(0) < right.parse::<i64>().unwrap_or(0),
        "-le" => left.parse::<i64>().unwrap_or(0) <= right.parse::<i64>().unwrap_or(0),
        "-gt" => left.parse::<i64>().unwrap_or(0) > right.parse::<i64>().unwrap_or(0),
        "-ge" => left.parse::<i64>().unwrap_or(0) >= right.parse::<i64>().unwrap_or(0),
        "<" => left < right,
        ">" => left > right,
        "=~" => {
            // Regex match — basic implementation
            glob_match_word(right, left)
        }
        _ => false,
    }
}

fn eval_unary_cond(op: &str, args: &[&str], pos: &mut usize) -> bool {
    *pos += 1; // skip operator
    let arg = if *pos < args.len() {
        let a = args[*pos];
        *pos += 1;
        a
    } else {
        return false;
    };

    match op {
        "-n" => !arg.is_empty(),
        "-z" => arg.is_empty(),
        "-f" => std::path::Path::new(arg).is_file(),
        "-d" => std::path::Path::new(arg).is_dir(),
        "-e" => std::path::Path::new(arg).exists(),
        "-s" => std::fs::metadata(arg).map(|m| m.len() > 0).unwrap_or(false),
        "-r" | "-w" | "-x" => std::path::Path::new(arg).exists(), // simplified
        "-L" | "-h" => std::fs::symlink_metadata(arg)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false),
        "-b" | "-c" | "-p" | "-S" => false, // special file types — simplified
        "-t" => false, // is a tty
        "-o" => false, // option set — simplified
        "-v" => false, // variable set — can't check through free function
        _ => false,
    }
}

fn is_binary_cond_op(s: &str) -> bool {
    matches!(
        s,
        "=" | "==" | "!=" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" | "<" | ">" | "=~"
    )
}

fn is_binary_cond_op_or_end(s: Option<&str>) -> bool {
    match s {
        Some(s) => is_binary_cond_op(s) || s == "&&" || s == "||" || s == ")" || s == "]]",
        None => true,
    }
}

/// Apply a parameter expansion operator.
fn apply_param_op(value: &str, op: &str, arg: &str) -> String {
    match op {
        ":-" => {
            if value.is_empty() {
                arg.to_string()
            } else {
                value.to_string()
            }
        }
        "-" => {
            // ${param-word} — use word only if param is unset (we can't distinguish
            // unset from empty here, treat same as :-)
            if value.is_empty() {
                arg.to_string()
            } else {
                value.to_string()
            }
        }
        ":+" => {
            if value.is_empty() {
                String::new()
            } else {
                arg.to_string()
            }
        }
        "+" => {
            if value.is_empty() {
                String::new()
            } else {
                arg.to_string()
            }
        }
        "#" => {
            // ${param#pattern} — remove shortest prefix match
            for i in 0..=value.len() {
                if glob_match_word(arg, &value[..i]) {
                    return value[i..].to_string();
                }
            }
            value.to_string()
        }
        "##" => {
            // ${param##pattern} — remove longest prefix match
            for i in (0..=value.len()).rev() {
                if glob_match_word(arg, &value[..i]) {
                    return value[i..].to_string();
                }
            }
            value.to_string()
        }
        "%" => {
            // ${param%pattern} — remove shortest suffix match
            for i in (0..=value.len()).rev() {
                if glob_match_word(arg, &value[i..]) {
                    return value[..i].to_string();
                }
            }
            value.to_string()
        }
        "%%" => {
            // ${param%%pattern} — remove longest suffix match
            for i in 0..=value.len() {
                if glob_match_word(arg, &value[i..]) {
                    return value[..i].to_string();
                }
            }
            value.to_string()
        }
        "length" => {
            // ${#param} — string length
            value.len().to_string()
        }
        "/" => {
            // ${param/pattern/replacement} — first substitution
            if let Some(slash_pos) = arg.find('/') {
                let pattern = &arg[..slash_pos];
                let replacement = &arg[slash_pos + 1..];
                if let Some(pos) = find_glob_match(value, pattern) {
                    let match_len = glob_match_length(value, pos, pattern);
                    let mut result = String::new();
                    result.push_str(&value[..pos]);
                    result.push_str(replacement);
                    result.push_str(&value[pos + match_len..]);
                    result
                } else {
                    value.to_string()
                }
            } else {
                // ${param/pattern} — remove first match
                if let Some(pos) = find_glob_match(value, arg) {
                    let match_len = glob_match_length(value, pos, arg);
                    let mut result = String::new();
                    result.push_str(&value[..pos]);
                    result.push_str(&value[pos + match_len..]);
                    result
                } else {
                    value.to_string()
                }
            }
        }
        "//" => {
            // ${param//pattern/replacement} — global substitution
            if let Some(slash_pos) = arg.find('/') {
                let pattern = &arg[..slash_pos];
                let replacement = &arg[slash_pos + 1..];
                let mut result = String::new();
                let mut i = 0;
                let chars: Vec<char> = value.chars().collect();
                while i < chars.len() {
                    let remaining: String = chars[i..].iter().collect();
                    if glob_match_word(pattern, &remaining[..glob_match_length(&remaining, 0, pattern).max(1).min(remaining.len())]) {
                        let mlen = glob_match_length(&remaining, 0, pattern);
                        result.push_str(replacement);
                        i += mlen.max(1);
                    } else {
                        result.push(chars[i]);
                        i += 1;
                    }
                }
                result
            } else {
                value.to_string()
            }
        }
        ":="|"=" => {
            // ${param:=word} — assign default (handled in executor, not here)
            if value.is_empty() {
                arg.to_string()
            } else {
                value.to_string()
            }
        }
        ":?" | "?" => {
            // ${param:?msg} — error if unset/empty
            if value.is_empty() {
                let msg = if arg.is_empty() { "parameter not set" } else { arg };
                eprintln!("frost: {msg}");
                value.to_string()
            } else {
                value.to_string()
            }
        }
        _ => value.to_string(),
    }
}

/// Find position of first glob match in text.
fn find_glob_match(text: &str, pattern: &str) -> Option<usize> {
    for i in 0..text.len() {
        for j in (i + 1)..=text.len() {
            if glob_match_word(pattern, &text[i..j]) {
                return Some(i);
            }
        }
    }
    None
}

/// Find the length of a glob match starting at position.
fn glob_match_length(text: &str, start: usize, pattern: &str) -> usize {
    let mut longest = 1;
    for j in (start + 1)..=text.len() {
        if glob_match_word(pattern, &text[start..j]) {
            longest = j - start;
        }
    }
    longest
}

/// Simple glob matching for case patterns and parameter expansion.
fn glob_match_word(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    glob_match_chars(&pat, &txt)
}

fn glob_match_chars(pat: &[char], txt: &[char]) -> bool {
    if pat.is_empty() {
        return txt.is_empty();
    }
    match pat[0] {
        '*' => {
            for i in 0..=txt.len() {
                if glob_match_chars(&pat[1..], &txt[i..]) {
                    return true;
                }
            }
            false
        }
        '?' => {
            if txt.is_empty() {
                false
            } else {
                glob_match_chars(&pat[1..], &txt[1..])
            }
        }
        '\\' if pat.len() > 1 => {
            if txt.is_empty() || txt[0] != pat[1] {
                false
            } else {
                glob_match_chars(&pat[2..], &txt[1..])
            }
        }
        c => {
            if txt.is_empty() || txt[0] != c {
                false
            } else {
                glob_match_chars(&pat[1..], &txt[1..])
            }
        }
    }
}

/// Evaluate a simple arithmetic expression (integers, +, -, *, /, %, comparisons).
fn eval_arith_expr(expr: &str) -> Option<i64> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Some(0);
    }

    // Try to parse as a plain integer first.
    if let Ok(n) = expr.parse::<i64>() {
        return Some(n);
    }

    // Handle comparison operators (lowest precedence).
    for &op in &["==", "!=", "<=", ">=", "<", ">"] {
        if let Some(pos) = find_binary_op(expr, op) {
            let left = eval_arith_expr(&expr[..pos])?;
            let right = eval_arith_expr(&expr[pos + op.len()..])?;
            return Some(match op {
                "==" => (left == right) as i64,
                "!=" => (left != right) as i64,
                "<=" => (left <= right) as i64,
                ">=" => (left >= right) as i64,
                "<" => (left < right) as i64,
                ">" => (left > right) as i64,
                _ => 0,
            });
        }
    }

    // Handle + and - (left to right).
    if let Some(pos) = find_last_additive(expr) {
        let left = eval_arith_expr(&expr[..pos])?;
        let right = eval_arith_expr(&expr[pos + 1..])?;
        return Some(if expr.as_bytes()[pos] == b'+' {
            left + right
        } else {
            left - right
        });
    }

    // Handle * / % (left to right).
    if let Some(pos) = find_last_multiplicative(expr) {
        let left = eval_arith_expr(&expr[..pos])?;
        let right = eval_arith_expr(&expr[pos + 1..])?;
        return Some(match expr.as_bytes()[pos] {
            b'*' => left * right,
            b'/' if right != 0 => left / right,
            b'%' if right != 0 => left % right,
            _ => 0,
        });
    }

    // Handle parentheses.
    if expr.starts_with('(') && expr.ends_with(')') {
        return eval_arith_expr(&expr[1..expr.len() - 1]);
    }

    // Handle unary minus.
    if expr.starts_with('-') {
        return eval_arith_expr(&expr[1..]).map(|v| -v);
    }

    // Handle unary plus.
    if expr.starts_with('+') {
        return eval_arith_expr(&expr[1..]);
    }

    // Handle logical not.
    if expr.starts_with('!') {
        return eval_arith_expr(&expr[1..]).map(|v| if v == 0 { 1 } else { 0 });
    }

    None
}

/// Find the last + or - at the top level (not inside parens).
fn find_last_additive(expr: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let mut depth = 0i32;
    let mut last = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'+' | b'-' if depth == 0 && i > 0 => {
                // Don't match if preceded by another operator.
                let prev = bytes[i - 1];
                if prev != b'*' && prev != b'/' && prev != b'%' && prev != b'('
                    && prev != b'+' && prev != b'-'
                {
                    last = Some(i);
                }
            }
            _ => {}
        }
    }
    last
}

/// Find the last * / % at the top level.
fn find_last_multiplicative(expr: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let mut depth = 0i32;
    let mut last = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'*' | b'/' | b'%' if depth == 0 => {
                // Make sure it's not part of ** or ==
                if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    continue;
                }
                last = Some(i);
            }
            _ => {}
        }
    }
    last
}

/// Find position of a binary operator string at the top level.
fn find_binary_op(expr: &str, op: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let op_bytes = op.as_bytes();
    let mut depth = 0i32;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ if depth == 0 && i + op_bytes.len() <= bytes.len() => {
                if &bytes[i..i + op_bytes.len()] == op_bytes && i > 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Reconstruct shell code from a Program AST.
/// This is used for command substitution where we need to run
/// the code in a subprocess via `frost -c`.
fn reconstruct_program(program: &Program) -> String {
    let mut parts = Vec::new();
    for cmd in &program.commands {
        parts.push(reconstruct_complete_command(cmd));
    }
    parts.join("; ")
}

fn reconstruct_complete_command(cmd: &CompleteCommand) -> String {
    let mut s = reconstruct_list(&cmd.list);
    if cmd.is_async {
        s.push_str(" &");
    }
    s
}

fn reconstruct_list(list: &List) -> String {
    let mut s = reconstruct_pipeline(&list.first);
    for (op, pipeline) in &list.rest {
        match op {
            ListOp::And => s.push_str(" && "),
            ListOp::Or => s.push_str(" || "),
        }
        s.push_str(&reconstruct_pipeline(pipeline));
    }
    s
}

fn reconstruct_pipeline(pipeline: &Pipeline) -> String {
    let mut s = String::new();
    if pipeline.bang {
        s.push_str("! ");
    }
    let cmd_strs: Vec<String> = pipeline.commands.iter().map(reconstruct_command).collect();
    s.push_str(&cmd_strs.join(" | "));
    s
}

fn reconstruct_command(cmd: &Command) -> String {
    match cmd {
        Command::Simple(simple) => {
            let mut parts = Vec::new();
            for assign in &simple.assignments {
                let val = assign.value.as_ref().map(reconstruct_word).unwrap_or_default();
                parts.push(format!("{}={}", assign.name, val));
            }
            for word in &simple.words {
                parts.push(reconstruct_word(word));
            }
            for redir in &simple.redirects {
                parts.push(reconstruct_redirect(redir));
            }
            parts.join(" ")
        }
        Command::Subshell(sub) => {
            let body: Vec<String> = sub.body.iter().map(reconstruct_complete_command).collect();
            format!("( {} )", body.join("; "))
        }
        Command::BraceGroup(bg) => {
            let body: Vec<String> = bg.body.iter().map(reconstruct_complete_command).collect();
            format!("{{ {} }}", body.join("; "))
        }
        Command::If(clause) => {
            let mut s = String::from("if ");
            let cond: Vec<String> = clause.condition.iter().map(reconstruct_complete_command).collect();
            s.push_str(&cond.join("; "));
            s.push_str("; then ");
            let body: Vec<String> = clause.then_body.iter().map(reconstruct_complete_command).collect();
            s.push_str(&body.join("; "));
            for (elif_cond, elif_body) in &clause.elifs {
                s.push_str("; elif ");
                let ec: Vec<String> = elif_cond.iter().map(reconstruct_complete_command).collect();
                s.push_str(&ec.join("; "));
                s.push_str("; then ");
                let eb: Vec<String> = elif_body.iter().map(reconstruct_complete_command).collect();
                s.push_str(&eb.join("; "));
            }
            if let Some(else_body) = &clause.else_body {
                s.push_str("; else ");
                let eb: Vec<String> = else_body.iter().map(reconstruct_complete_command).collect();
                s.push_str(&eb.join("; "));
            }
            s.push_str("; fi");
            s
        }
        Command::For(clause) => {
            let mut s = format!("for {} ", clause.var);
            if let Some(words) = &clause.words {
                s.push_str("in ");
                let w: Vec<String> = words.iter().map(reconstruct_word).collect();
                s.push_str(&w.join(" "));
            }
            s.push_str("; do ");
            let body: Vec<String> = clause.body.iter().map(reconstruct_complete_command).collect();
            s.push_str(&body.join("; "));
            s.push_str("; done");
            s
        }
        Command::ArithFor(clause) => {
            let mut s = format!(
                "for (( {}; {}; {} )); do ",
                clause.init, clause.condition, clause.step
            );
            let body: Vec<String> = clause.body.iter().map(reconstruct_complete_command).collect();
            s.push_str(&body.join("; "));
            s.push_str("; done");
            s
        }
        Command::While(clause) => {
            let mut s = String::from("while ");
            let cond: Vec<String> = clause.condition.iter().map(reconstruct_complete_command).collect();
            s.push_str(&cond.join("; "));
            s.push_str("; do ");
            let body: Vec<String> = clause.body.iter().map(reconstruct_complete_command).collect();
            s.push_str(&body.join("; "));
            s.push_str("; done");
            s
        }
        Command::Until(clause) => {
            let mut s = String::from("until ");
            let cond: Vec<String> = clause.condition.iter().map(reconstruct_complete_command).collect();
            s.push_str(&cond.join("; "));
            s.push_str("; do ");
            let body: Vec<String> = clause.body.iter().map(reconstruct_complete_command).collect();
            s.push_str(&body.join("; "));
            s.push_str("; done");
            s
        }
        Command::Case(clause) => {
            let mut s = format!("case {} in ", reconstruct_word(&clause.word));
            for item in &clause.items {
                let pats: Vec<String> = item.patterns.iter().map(reconstruct_word).collect();
                s.push_str(&pats.join("|"));
                s.push_str(") ");
                let body: Vec<String> = item.body.iter().map(reconstruct_complete_command).collect();
                s.push_str(&body.join("; "));
                s.push_str(";; ");
            }
            s.push_str("esac");
            s
        }
        Command::FunctionDef(fdef) => {
            format!("{} () {}", fdef.name, reconstruct_command(&fdef.body))
        }
        Command::Repeat(clause) => {
            let mut s = format!("repeat {} do ", reconstruct_word(&clause.count));
            let body: Vec<String> = clause.body.iter().map(reconstruct_complete_command).collect();
            s.push_str(&body.join("; "));
            s.push_str("; done");
            s
        }
        Command::Always(clause) => {
            let try_body: Vec<String> = clause.try_body.iter().map(reconstruct_complete_command).collect();
            let always_body: Vec<String> = clause.always_body.iter().map(reconstruct_complete_command).collect();
            format!("{{ {} }} always {{ {} }}", try_body.join("; "), always_body.join("; "))
        }
        Command::Select(_) | Command::Coproc(_) | Command::Time(_) => {
            // Simplified — just return empty for unsupported constructs.
            String::new()
        }
    }
}

fn reconstruct_word(word: &Word) -> String {
    let mut s = String::new();
    for part in &word.parts {
        reconstruct_word_part(part, &mut s);
    }
    s
}

fn reconstruct_word_part(part: &WordPart, out: &mut String) {
    match part {
        WordPart::Literal(s) => out.push_str(s),
        WordPart::SingleQuoted(s) => {
            out.push('\'');
            out.push_str(s);
            out.push('\'');
        }
        WordPart::DoubleQuoted(parts) => {
            out.push('"');
            for p in parts {
                reconstruct_word_part(p, out);
            }
            out.push('"');
        }
        WordPart::DollarVar(name) => {
            out.push('$');
            out.push_str(name);
        }
        WordPart::DollarBrace { param, operator, arg } => {
            out.push_str("${");
            out.push_str(param);
            if let Some(op) = operator {
                out.push_str(op);
                if let Some(a) = arg {
                    out.push_str(&reconstruct_word(a));
                }
            }
            out.push('}');
        }
        WordPart::CommandSub(prog) => {
            out.push_str("$(");
            out.push_str(&reconstruct_program(prog));
            out.push(')');
        }
        WordPart::ArithSub(expr) => {
            out.push_str("$((");
            out.push_str(expr);
            out.push_str("))");
        }
        WordPart::Glob(kind) => match kind {
            GlobKind::Star => out.push('*'),
            GlobKind::Question => out.push('?'),
            GlobKind::At => out.push('@'),
        },
        WordPart::Tilde(user) => {
            out.push('~');
            out.push_str(user);
        }
    }
}

fn reconstruct_redirect(redir: &Redirect) -> String {
    let mut s = String::new();
    if let Some(fd) = redir.fd {
        s.push_str(&fd.to_string());
    }
    match redir.op {
        RedirectOp::Less => s.push('<'),
        RedirectOp::Greater => s.push('>'),
        RedirectOp::DoubleGreater => s.push_str(">>"),
        RedirectOp::AmpGreater => s.push_str("&>"),
        RedirectOp::AmpDoubleGreater => s.push_str("&>>"),
        RedirectOp::FdDup => s.push_str(">&"),
        _ => s.push('>'),
    }
    s.push_str(&reconstruct_word(&redir.target));
    s
}

/// Tokenize a string for eval/source.
fn tokenize(input: &str) -> Vec<frost_lexer::Token> {
    let mut lexer = frost_lexer::Lexer::new(input.as_bytes());
    let mut tokens = Vec::new();
    loop {
        let tok = lexer.next_token();
        let eof = tok.kind == frost_lexer::TokenKind::Eof;
        tokens.push(tok);
        if eof {
            break;
        }
    }
    tokens
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
    fn expand_dollar_var() {
        let mut env = ShellEnv::new();
        env.set_var("FOO", "bar");
        let exec = Executor::new(&mut env);
        let word = Word {
            parts: vec![WordPart::DollarVar("FOO".into())],
            span: Span::new(0, 4),
        };
        assert_eq!(exec.expand_special_var("FOO"), "bar");
        drop(exec);
    }

    #[test]
    fn expand_special_vars() {
        let mut env = ShellEnv::new();
        env.exit_status = 42;
        env.positional_params = vec!["a".into(), "b".into()];
        let exec = Executor::new(&mut env);
        assert_eq!(exec.expand_special_var("?"), "42");
        assert_eq!(exec.expand_special_var("#"), "2");
        assert_eq!(exec.expand_special_var("@"), "a b");
        assert_eq!(exec.expand_special_var("1"), "a");
        assert_eq!(exec.expand_special_var("2"), "b");
        assert_eq!(exec.expand_special_var("3"), "");
        drop(exec);
    }

    #[test]
    fn arith_basic() {
        assert_eq!(eval_arith_expr("1+2"), Some(3));
        assert_eq!(eval_arith_expr("10-3"), Some(7));
        assert_eq!(eval_arith_expr("4*5"), Some(20));
        assert_eq!(eval_arith_expr("10/3"), Some(3));
        assert_eq!(eval_arith_expr("10%3"), Some(1));
    }

    #[test]
    fn arith_comparison() {
        assert_eq!(eval_arith_expr("1==1"), Some(1));
        assert_eq!(eval_arith_expr("1!=2"), Some(1));
        assert_eq!(eval_arith_expr("1<2"), Some(1));
        assert_eq!(eval_arith_expr("2>1"), Some(1));
    }

    #[test]
    fn glob_match_basic() {
        assert!(glob_match_word("hello", "hello"));
        assert!(!glob_match_word("hello", "world"));
        assert!(glob_match_word("*", "anything"));
        assert!(glob_match_word("hel*", "hello"));
        assert!(glob_match_word("h?llo", "hello"));
    }

    #[test]
    fn param_op_default() {
        assert_eq!(apply_param_op("", ":-", "default"), "default");
        assert_eq!(apply_param_op("val", ":-", "default"), "val");
    }

    #[test]
    fn param_op_alternate() {
        assert_eq!(apply_param_op("", ":+", "alt"), "");
        assert_eq!(apply_param_op("val", ":+", "alt"), "alt");
    }
}
