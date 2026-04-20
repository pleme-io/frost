//! The main execution engine.
//!
//! Walks the AST and executes commands by forking child processes,
//! setting up pipes, and applying redirections. All platform-specific
//! system calls go through [`crate::sys`].

use std::ffi::CString;

use nix::unistd::Pid;

use compact_str::CompactString;
use frost_builtins::BuiltinRegistry;
use frost_expand::ExpandEnv;
use frost_parser::ast::{
    BraceGroup, CForClause, CaseClause, Command, CompleteCommand, CondExpr, CondOp, ForClause,
    IfClause, List, ListOp, Pipeline, ProcessSubKind, Program, RepeatClause, SelectClause,
    SimpleCommand, Subshell, TryAlwaysClause, UntilClause, WhileClause, Word, WordPart,
};
use std::os::fd::RawFd;

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

    /// Control flow signal — not an error, but needs to propagate.
    #[error("control flow")]
    ControlFlow(ControlFlow),
}

/// Control flow signals for return/break/continue.
#[derive(Debug, Clone)]
pub enum ControlFlow {
    Return(i32),
    Break(u32),
    Continue(u32),
    Exit(i32),
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
            // Set $pipestatus for single-command pipelines
            self.set_pipestatus(&[status]);
            return Ok(if pipeline.bang {
                invert(status)
            } else {
                status
            });
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

        let mut statuses = Vec::with_capacity(children.len());
        let mut last_status = 0;
        for pid in children {
            match sys::wait_pid(pid).map_err(ExecError::Wait)? {
                sys::ChildStatus::Exited(code) => {
                    statuses.push(code);
                    last_status = code;
                }
                sys::ChildStatus::Signaled(code) => {
                    statuses.push(code);
                    last_status = code;
                }
                _ => {
                    statuses.push(0);
                }
            }
        }

        // Set $pipestatus array
        self.set_pipestatus(&statuses);

        // Check PIPE_FAIL option: if set, return nonzero if any command failed
        if self.env.is_option_set(frost_options::ShellOption::PipeFail) {
            let pipe_fail_status = statuses
                .iter()
                .copied()
                .find(|&s| s != 0)
                .unwrap_or(last_status);
            last_status = pipe_fail_status;
        }

        Ok(if pipeline.bang {
            invert(last_status)
        } else {
            last_status
        })
    }

    /// Set the `$pipestatus` array variable.
    fn set_pipestatus(&mut self, statuses: &[i32]) {
        use crate::env::ShellValue;
        let arr: Vec<String> = statuses.iter().map(|s| s.to_string()).collect();
        // Set as string first, then convert to array
        self.env.set_var("pipestatus", "");
        if let Some(var) = self.env.get_shell_var_mut("pipestatus") {
            var.set_value(ShellValue::Array(arr));
        }
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
                self.env
                    .functions
                    .insert(fdef.name.to_string(), (**fdef).clone());
                Ok(0)
            }
            Command::ArithCmd(expr) => {
                // (( expr )) — evaluate and return 0 if nonzero, 1 if zero
                let result = crate::arith::eval_arithmetic_mut(expr, self.env);
                let status = if result != 0 { 0 } else { 1 };
                self.env.exit_status = status;
                Ok(status)
            }
            Command::Cond(expr) => self.execute_cond(expr),
            Command::CFor(clause) => self.execute_c_for(clause),
            Command::Repeat(clause) => self.execute_repeat(clause),
            Command::TryAlways(clause) => self.execute_try_always(clause),
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
            Some(ws) => ws
                .iter()
                .flat_map(|w| self.expand_word_multi(w))
                .collect::<Vec<_>>(),
            None => self.env.positional_params.clone(),
        };

        let mut status = 0;
        'outer: for word in &words {
            self.env.set_var(&clause.var, word);
            for cmd in &clause.body {
                match self.execute_complete_command(cmd) {
                    Ok(s) => status = s,
                    Err(ExecError::ControlFlow(ControlFlow::Break(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Break(n - 1)));
                        }
                        break 'outer;
                    }
                    Err(ExecError::ControlFlow(ControlFlow::Continue(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Continue(n - 1)));
                        }
                        continue 'outer;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(status)
    }

    fn execute_while(&mut self, clause: &WhileClause) -> ExecResult {
        let mut status = 0;
        'outer: loop {
            let mut cond_status = 0;
            for cmd in &clause.condition {
                cond_status = self.execute_complete_command(cmd)?;
            }
            if cond_status != 0 {
                break;
            }
            for cmd in &clause.body {
                match self.execute_complete_command(cmd) {
                    Ok(s) => status = s,
                    Err(ExecError::ControlFlow(ControlFlow::Break(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Break(n - 1)));
                        }
                        break 'outer;
                    }
                    Err(ExecError::ControlFlow(ControlFlow::Continue(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Continue(n - 1)));
                        }
                        continue 'outer;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(status)
    }

    fn execute_until(&mut self, clause: &UntilClause) -> ExecResult {
        let mut status = 0;
        'outer: loop {
            let mut cond_status = 0;
            for cmd in &clause.condition {
                cond_status = self.execute_complete_command(cmd)?;
            }
            if cond_status == 0 {
                break;
            }
            for cmd in &clause.body {
                match self.execute_complete_command(cmd) {
                    Ok(s) => status = s,
                    Err(ExecError::ControlFlow(ControlFlow::Break(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Break(n - 1)));
                        }
                        break 'outer;
                    }
                    Err(ExecError::ControlFlow(ControlFlow::Continue(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Continue(n - 1)));
                        }
                        continue 'outer;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(status)
    }

    // ── Eval / Source ─────────────────────────────────────────────

    fn eval_string(&mut self, code: &str) -> ExecResult {
        let tokens = crate::tokenize(code);
        let mut parser = frost_parser::Parser::new(&tokens);
        let program = parser.parse();
        self.execute_program(&program)
    }

    fn source_file(&mut self, path: &str) -> ExecResult {
        match std::fs::read_to_string(path) {
            Ok(source) => self.eval_string(&source),
            Err(e) => {
                eprintln!("frost: {path}: {e}");
                Ok(1)
            }
        }
    }

    fn execute_case(&mut self, clause: &CaseClause) -> ExecResult {
        use frost_parser::ast::CaseTerminator;
        let word = self.expand_word(&clause.word);
        let mut matched = false;
        let mut status = 0;

        for (idx, item) in clause.items.iter().enumerate() {
            let mut item_matched = matched; // carry forward from ;& fall-through
            if !item_matched {
                for pattern in &item.patterns {
                    let pat = self.expand_word(pattern);
                    if simple_pattern_match(&pat, &word) {
                        item_matched = true;
                        break;
                    }
                }
            }

            if item_matched {
                for cmd in &item.body {
                    status = self.execute_complete_command(cmd)?;
                }
                match item.terminator {
                    CaseTerminator::DoubleSemi => return Ok(status), // ;; — stop
                    CaseTerminator::SemiAnd => {
                        // ;& — fall through to next body unconditionally
                        matched = true;
                    }
                    CaseTerminator::SemiPipe => {
                        // ;| — continue testing remaining patterns
                        matched = false;
                    }
                }
                // If this is the last item, we're done
                if idx == clause.items.len() - 1 {
                    return Ok(status);
                }
            }
        }
        Ok(status)
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

    // ── [[ ]] conditional ────────────────────────────────────────

    fn execute_cond(&mut self, expr: &CondExpr) -> ExecResult {
        let result = self.eval_cond(expr);
        let status = if result { 0 } else { 1 };
        self.env.exit_status = status;
        Ok(status)
    }

    fn eval_cond(&mut self, expr: &CondExpr) -> bool {
        match expr {
            CondExpr::Not(inner) => !self.eval_cond(inner),
            CondExpr::And(left, right) => self.eval_cond(left) && self.eval_cond(right),
            CondExpr::Or(left, right) => self.eval_cond(left) || self.eval_cond(right),
            CondExpr::Unary(op, word) => {
                let val = self.expand_word(word);
                self.eval_unary_cond(op, &val)
            }
            CondExpr::Binary(left, op, right) => {
                let l = self.expand_word(left);
                let r = self.expand_word(right);
                self.eval_binary_cond(op, &l, &r)
            }
        }
    }

    fn eval_unary_cond(&self, op: &CondOp, val: &str) -> bool {
        use std::fs;
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

        match op {
            CondOp::FileExists => fs::symlink_metadata(val).is_ok(),
            CondOp::IsFile => fs::metadata(val).is_ok_and(|m| m.is_file()),
            CondOp::IsDir => fs::metadata(val).is_ok_and(|m| m.is_dir()),
            CondOp::IsSymlink => fs::symlink_metadata(val).is_ok_and(|m| m.is_symlink()),
            CondOp::IsReadable => fs::metadata(val).is_ok(), // simplified
            CondOp::IsWritable => {
                fs::metadata(val).is_ok_and(|m| m.permissions().mode() & 0o200 != 0)
            }
            CondOp::IsExecutable => {
                fs::metadata(val).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
            }
            CondOp::IsNonEmpty => fs::metadata(val).is_ok_and(|m| m.len() > 0),
            CondOp::IsBlockDev => fs::metadata(val).is_ok_and(|m| m.file_type().is_block_device()),
            CondOp::IsCharDev => fs::metadata(val).is_ok_and(|m| m.file_type().is_char_device()),
            CondOp::IsFifo => fs::metadata(val).is_ok_and(|m| m.file_type().is_fifo()),
            CondOp::IsSocket => fs::metadata(val).is_ok_and(|m| m.file_type().is_socket()),
            CondOp::IsSetuid => fs::metadata(val).is_ok_and(|m| m.mode() & 0o4000 != 0),
            CondOp::IsSetgid => fs::metadata(val).is_ok_and(|m| m.mode() & 0o2000 != 0),
            CondOp::IsSticky => fs::metadata(val).is_ok_and(|m| m.mode() & 0o1000 != 0),
            CondOp::OwnedByUser => {
                let uid = unsafe { libc::getuid() };
                fs::metadata(val).is_ok_and(|m| m.uid() == uid)
            }
            CondOp::OwnedByGroup => {
                let gid = unsafe { libc::getgid() };
                fs::metadata(val).is_ok_and(|m| m.gid() == gid)
            }
            CondOp::ModifiedSinceRead => {
                fs::metadata(val).is_ok_and(|m| m.modified().ok() > m.accessed().ok())
            }
            CondOp::IsTty => val
                .parse::<i32>()
                .ok()
                .is_some_and(|fd| nix::unistd::isatty(fd).unwrap_or(false)),
            CondOp::OptionSet => {
                // [[ -o option_name ]] — check if shell option is set
                frost_options::Options::from_name(val)
                    .is_some_and(|opt| self.env.is_option_set(opt))
            }
            CondOp::VarIsSet => self.env.get_var(val).is_some(),
            CondOp::StrEmpty => val.is_empty(),
            CondOp::StrNonEmpty => !val.is_empty(),
            _ => !val.is_empty(),
        }
    }

    fn eval_binary_cond(&self, op: &CondOp, left: &str, right: &str) -> bool {
        match op {
            CondOp::StrEq => simple_pattern_match(right, left),
            CondOp::StrNeq => !simple_pattern_match(right, left),
            CondOp::StrLt => left < right,
            CondOp::StrGt => left > right,
            CondOp::StrMatch => {
                // =~ regex matching
                match fancy_regex::Regex::new(right) {
                    Ok(re) => re.is_match(left).unwrap_or(false),
                    Err(_) => false,
                }
            }
            CondOp::IntEq => parse_int(left) == parse_int(right),
            CondOp::IntNe => parse_int(left) != parse_int(right),
            CondOp::IntLt => parse_int(left) < parse_int(right),
            CondOp::IntLe => parse_int(left) <= parse_int(right),
            CondOp::IntGt => parse_int(left) > parse_int(right),
            CondOp::IntGe => parse_int(left) >= parse_int(right),
            CondOp::NewerThan => {
                let l = std::fs::metadata(left).and_then(|m| m.modified()).ok();
                let r = std::fs::metadata(right).and_then(|m| m.modified()).ok();
                l > r
            }
            CondOp::OlderThan => {
                let l = std::fs::metadata(left).and_then(|m| m.modified()).ok();
                let r = std::fs::metadata(right).and_then(|m| m.modified()).ok();
                l < r
            }
            CondOp::SameFile => {
                use std::os::unix::fs::MetadataExt;
                let l = std::fs::metadata(left).ok();
                let r = std::fs::metadata(right).ok();
                match (l, r) {
                    (Some(a), Some(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
                    _ => false,
                }
            }
            _ => left == right,
        }
    }

    // ── C-style for loop ────────────────────────────────────────

    fn execute_c_for(&mut self, clause: &CForClause) -> ExecResult {
        // Execute init expression
        if !clause.init.is_empty() {
            crate::arith::eval_arithmetic_mut(&clause.init, self.env);
        }

        let mut status = 0;
        'outer: loop {
            // Check condition
            if !clause.condition.is_empty() {
                let cond = crate::arith::eval_arithmetic_mut(&clause.condition, self.env);
                if cond == 0 {
                    break;
                }
            }

            // Execute body
            for cmd in &clause.body {
                match self.execute_complete_command(cmd) {
                    Ok(s) => status = s,
                    Err(ExecError::ControlFlow(ControlFlow::Break(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Break(n - 1)));
                        }
                        break 'outer;
                    }
                    Err(ExecError::ControlFlow(ControlFlow::Continue(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Continue(n - 1)));
                        }
                        // Fall through to step expression
                    }
                    Err(e) => return Err(e),
                }
            }

            // Execute step expression
            if !clause.step.is_empty() {
                crate::arith::eval_arithmetic_mut(&clause.step, self.env);
            }
        }
        Ok(status)
    }

    // ── repeat ──────────────────────────────────────────────────

    fn execute_repeat(&mut self, clause: &RepeatClause) -> ExecResult {
        let count_str = self.expand_word(&clause.count);
        let count: i64 = count_str.parse().unwrap_or(0);
        let mut status = 0;

        'outer: for _ in 0..count {
            for cmd in &clause.body {
                match self.execute_complete_command(cmd) {
                    Ok(s) => status = s,
                    Err(ExecError::ControlFlow(ControlFlow::Break(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Break(n - 1)));
                        }
                        break 'outer;
                    }
                    Err(ExecError::ControlFlow(ControlFlow::Continue(n))) => {
                        if n > 1 {
                            return Err(ExecError::ControlFlow(ControlFlow::Continue(n - 1)));
                        }
                        continue 'outer;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(status)
    }

    // ── try-always ──────────────────────────────────────────────

    fn execute_try_always(&mut self, clause: &TryAlwaysClause) -> ExecResult {
        let try_result = (|| -> ExecResult {
            let mut status = 0;
            for cmd in &clause.try_body {
                status = self.execute_complete_command(cmd)?;
            }
            Ok(status)
        })();

        // Always block runs regardless of try result
        let mut always_status = 0;
        for cmd in &clause.always_body {
            always_status = self.execute_complete_command(cmd).unwrap_or(1);
        }

        // If try succeeded, return always status; if try failed, propagate
        match try_result {
            Ok(_) => Ok(always_status),
            Err(e) => Err(e),
        }
    }

    // ── Simple command ───────────────────────────────────────────

    pub fn execute_simple(&mut self, cmd: &SimpleCommand) -> ExecResult {
        use frost_parser::ast::AssignOp;
        for assign in &cmd.assignments {
            if let Some(ref arr_words) = assign.array_value {
                // Array assignment: name=(word1 word2 ...)
                let elements: Vec<String> = arr_words
                    .iter()
                    .flat_map(|w| self.expand_word_multi(w))
                    .collect();
                use crate::env::ShellValue;
                match assign.op {
                    AssignOp::Append => {
                        // name+=(vals) — append to existing array
                        if let Some(var) = self.env.get_shell_var_mut(&assign.name) {
                            if let ShellValue::Array(ref mut arr) = var.value {
                                arr.extend(elements);
                                var.set_value(var.value.clone());
                            } else {
                                var.set_value(ShellValue::Array(elements));
                            }
                        } else {
                            self.env.set_var(&assign.name, "");
                            if let Some(var) = self.env.get_shell_var_mut(&assign.name) {
                                var.set_value(ShellValue::Array(elements));
                            }
                        }
                    }
                    AssignOp::Assign => {
                        self.env.set_var(&assign.name, "");
                        if let Some(var) = self.env.get_shell_var_mut(&assign.name) {
                            var.set_value(ShellValue::Array(elements));
                        }
                    }
                }
            } else if let Some(ref sub) = assign.subscript {
                // Subscript assignment: name[sub]=value
                let value = assign
                    .value
                    .as_ref()
                    .map(|w| self.expand_word(w))
                    .unwrap_or_default();
                use crate::env::ShellValue;
                // Expand the subscript (it could contain variables)
                let sub_expanded = self.expand_subscript(sub);

                // Ensure the variable exists as an array
                if self.env.get_shell_var(&assign.name).is_none() {
                    self.env.set_var(&assign.name, "");
                    if let Some(var) = self.env.get_shell_var_mut(&assign.name) {
                        var.set_value(ShellValue::Array(Vec::new()));
                    }
                }

                if let Some(var) = self.env.get_shell_var_mut(&assign.name) {
                    match var.value {
                        ShellValue::Array(ref mut arr) => {
                            if let Ok(idx) = sub_expanded.parse::<i64>() {
                                // zsh: 1-indexed, negative from end
                                let real_idx = if idx < 0 {
                                    (arr.len() as i64 + idx) as usize
                                } else if idx > 0 {
                                    (idx - 1) as usize
                                } else {
                                    0
                                };
                                // Extend array if needed
                                while arr.len() <= real_idx {
                                    arr.push(String::new());
                                }
                                match assign.op {
                                    AssignOp::Append => arr[real_idx].push_str(&value),
                                    AssignOp::Assign => arr[real_idx] = value,
                                }
                            }
                            var.refresh_str_cache();
                        }
                        ShellValue::Associative(ref mut map) => {
                            match assign.op {
                                AssignOp::Append => {
                                    let entry = map.entry(sub_expanded).or_default();
                                    entry.push_str(&value);
                                }
                                AssignOp::Assign => {
                                    map.insert(sub_expanded, value);
                                }
                            }
                            var.refresh_str_cache();
                        }
                        _ => {
                            // Convert scalar to array for subscript assignment
                            let existing = var.value.to_scalar_string();
                            let mut arr = vec![existing];
                            if let Ok(idx) = sub_expanded.parse::<i64>() {
                                let real_idx = if idx > 0 { (idx - 1) as usize } else { 0 };
                                while arr.len() <= real_idx {
                                    arr.push(String::new());
                                }
                                match assign.op {
                                    AssignOp::Append => arr[real_idx].push_str(&value),
                                    AssignOp::Assign => arr[real_idx] = value,
                                }
                            }
                            var.set_value(ShellValue::Array(arr));
                        }
                    }
                }
            } else {
                let value = assign
                    .value
                    .as_ref()
                    .map(|w| self.expand_word(w))
                    .unwrap_or_default();
                match assign.op {
                    AssignOp::Append => {
                        // name+=val — append to existing value
                        let existing = self.env.get_var(&assign.name).unwrap_or("").to_string();
                        self.env
                            .set_var(&assign.name, &format!("{existing}{value}"));
                    }
                    AssignOp::Assign => {
                        self.env.set_var(&assign.name, &value);
                    }
                }
            }
        }

        if cmd.words.is_empty() {
            return Ok(0);
        }

        // Process substitution resolves first — each `<(cmd)` / `>(cmd)` in
        // a word spawns a subprocess and is replaced by a `/dev/fd/N`
        // literal. The guard closes the parent-side fds on drop (any return
        // path) so the child subprocess sees EOF / an empty read and exits.
        let mut proc_sub_fds: Vec<RawFd> = Vec::new();
        let resolved_words: Vec<Word> = cmd
            .words
            .iter()
            .map(|w| {
                let (rw, fds) = self.resolve_process_subs(w);
                proc_sub_fds.extend(fds);
                rw
            })
            .collect();
        let _proc_sub_guard = ProcSubFdGuard { fds: proc_sub_fds };

        // Glob expansion runs after all other word expansions. We only glob
        // words that originally contained unquoted glob AST parts — this
        // preserves zsh's GLOB_SUBST-off default (a `*` that came from a
        // variable value is NOT re-globbed).
        let argv: Vec<String> = {
            let mut out = Vec::with_capacity(resolved_words.len());
            for word in &resolved_words {
                let expanded = self.expand_word_multi(word);
                if word_has_unquoted_glob(word)
                    && self.env.is_option_set(frost_options::ShellOption::Glob)
                {
                    for candidate in expanded {
                        self.apply_glob_to(candidate, &mut out);
                    }
                } else {
                    out.extend(expanded);
                }
            }
            out.into_iter()
                .filter(|s| !s.is_empty() || cmd.words.len() == 1)
                .collect()
        };

        if argv.is_empty() {
            return Ok(0);
        }

        // Alias expansion — zsh rule: an alias is expanded iff the name
        // matches argv[0] *and* we haven't already expanded it in this
        // expansion pass (prevents infinite recursion when an alias refers
        // to itself, e.g. `alias ls='ls --color'`). Trailing space in an
        // alias value allows the next word to also be alias-expanded, but
        // for the first pass we implement the common case only.
        let argv = expand_aliases(argv, &self.env.aliases);

        let name = &argv[0];

        // Check for functions first
        if let Some(fdef) = self.env.functions.get(name).cloned() {
            let saved_params = self.env.positional_params.clone();
            self.env.push_scope();
            self.env.positional_params = argv[1..].to_vec();
            let result = match self.execute_command(&fdef.body) {
                Ok(s) => Ok(s),
                Err(ExecError::ControlFlow(ControlFlow::Return(code))) => {
                    self.env.exit_status = code;
                    Ok(code)
                }
                Err(e) => Err(e),
            };
            self.env.pop_scope();
            self.env.positional_params = saved_params;
            return result;
        }

        // Check builtins — if redirects are present, fork to apply them
        if self.builtins.contains(name) {
            if !cmd.redirects.is_empty() {
                return self.fork_exec_builtin(&argv, &cmd.redirects);
            }

            let arg_refs: Vec<&str> = argv[1..].iter().map(|s| s.as_str()).collect();
            let result = self
                .builtins
                .get(name)
                .unwrap()
                .execute_with_action(&arg_refs, self.env);
            let status = result.status;

            // Handle special exit codes from control flow builtins
            use frost_builtins::control::*;
            if status == RETURN_SIGNAL {
                let code = self.env.exit_status;
                return Err(ExecError::ControlFlow(ControlFlow::Return(code)));
            }
            if status >= BREAK_SIGNAL && status < CONTINUE_SIGNAL {
                let levels = (status - BREAK_SIGNAL + 1) as u32;
                return Err(ExecError::ControlFlow(ControlFlow::Break(levels)));
            }
            if status >= CONTINUE_SIGNAL && status < 210 {
                let levels = (status - CONTINUE_SIGNAL + 1) as u32;
                return Err(ExecError::ControlFlow(ControlFlow::Continue(levels)));
            }

            // Handle structured actions from BuiltinAction
            use frost_builtins::BuiltinAction;
            match result.action {
                BuiltinAction::Eval(code) => {
                    return self.eval_string(&code);
                }
                BuiltinAction::Source(path) => {
                    return self.source_file(&path);
                }
                BuiltinAction::Shift(n) => {
                    if n <= self.env.positional_params.len() {
                        self.env.positional_params.drain(..n);
                    } else {
                        self.env.positional_params.clear();
                    }
                }
                BuiltinAction::SetPositional(params) => {
                    self.env.positional_params = params;
                }
                BuiltinAction::Let(expr) => {
                    let arith_result = crate::arith::eval_arithmetic_mut(&expr, self.env);
                    let exit = if arith_result != 0 { 0 } else { 1 };
                    self.env.exit_status = exit;
                    return Ok(exit);
                }
                BuiltinAction::DefineAlias(aliases) => {
                    for (name, value) in aliases {
                        self.env.aliases.insert(name, value);
                    }
                }
                BuiltinAction::RemoveAlias(names) => {
                    for name in names {
                        self.env.aliases.remove(&name);
                    }
                }
                BuiltinAction::SetOptions(opts) => {
                    for opt_name in opts {
                        let negated = frost_options::Options::is_negated(&opt_name);
                        if let Some(opt) = frost_options::Options::from_name(&opt_name) {
                            if negated {
                                self.env.unset_option(opt);
                            } else {
                                self.env.set_option(opt);
                            }
                        }
                    }
                }
                BuiltinAction::UnsetOptions(opts) => {
                    for opt_name in opts {
                        let negated = frost_options::Options::is_negated(&opt_name);
                        if let Some(opt) = frost_options::Options::from_name(&opt_name) {
                            if negated {
                                self.env.set_option(opt);
                            } else {
                                self.env.unset_option(opt);
                            }
                        }
                    }
                }
                BuiltinAction::Exit(code) => {
                    return Err(ExecError::ControlFlow(ControlFlow::Exit(code)));
                }
                BuiltinAction::None => {}
            }

            // Legacy fallback: still check __FROST_* vars for builtins that
            // haven't been migrated yet (will be removed once all use execute_with_action)
            if status == 211 {
                if let Some(code) = self.env.get_var("__FROST_EVAL_CODE").map(String::from) {
                    self.env.unset_var("__FROST_EVAL_CODE");
                    return self.eval_string(&code);
                }
            }
            if status == 210 {
                if let Some(path) = self.env.get_var("__FROST_SOURCE_FILE").map(String::from) {
                    self.env.unset_var("__FROST_SOURCE_FILE");
                    return self.source_file(&path);
                }
            }
            if status == 212 {
                if let Some(expr) = self.env.get_var("__FROST_LET_EXPR").map(String::from) {
                    self.env.unset_var("__FROST_LET_EXPR");
                    let arith_result = crate::arith::eval_arithmetic_mut(&expr, self.env);
                    let exit = if arith_result != 0 { 0 } else { 1 };
                    self.env.exit_status = exit;
                    return Ok(exit);
                }
            }
            if let Some(shift_str) = self.env.get_var("__FROST_SHIFT").map(String::from) {
                self.env.unset_var("__FROST_SHIFT");
                if let Ok(n) = shift_str.parse::<usize>() {
                    if n <= self.env.positional_params.len() {
                        self.env.positional_params.drain(..n);
                    } else {
                        self.env.positional_params.clear();
                    }
                }
            }
            if let Some(params_str) = self.env.get_var("__FROST_SET_POSITIONAL").map(String::from) {
                self.env.unset_var("__FROST_SET_POSITIONAL");
                if params_str.is_empty() {
                    self.env.positional_params.clear();
                } else {
                    self.env.positional_params =
                        params_str.split('\x1f').map(String::from).collect();
                }
            }

            // chpwd hook — fires after a successful `cd`, matching zsh's
            // convention. Authored via `(defhook :event "chpwd" :body …)`
            // in the rc; frost-lisp stores the body under
            // `__frost_hook_chpwd` in env.functions.
            if status == 0 && name == "cd" && self.env.functions.contains_key("__frost_hook_chpwd")
            {
                // Clone the body out of the borrow so we can call
                // execute_command without holding an immutable ref to env.
                let body = self.env.functions["__frost_hook_chpwd"].body.clone();
                // Swallow errors — a broken hook must not break `cd`.
                let _ = self.execute_command(&body);
                // Restore the cd's exit status; the hook's result shouldn't
                // leak into $?.
                self.env.exit_status = 0;
            }

            self.env.exit_status = status;
            return Ok(status);
        }

        // External command: fork + exec. Pre-check PATH resolution
        // so we can surface "command not found" as a structured
        // error (the REPL then prints a "did you mean: …" hint)
        // rather than letting the child's exec-time ENOENT through.
        //
        // Absolute/explicit paths (starting with `/`, `./`, `../`)
        // bypass the lookup — fork_exec's own ENOENT handling will
        // catch "bad path" cases there, which is semantically
        // distinct from "PATH has no such name".
        let name = &argv[0];
        let looks_like_path =
            name.starts_with('/') || name.starts_with("./") || name.starts_with("../");
        if !looks_like_path && path_lookup(&self.env, name).is_none() {
            return Err(ExecError::CommandNotFound(name.clone()));
        }
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
                let status = self
                    .builtins
                    .get(&argv[0])
                    .unwrap()
                    .execute(&arg_refs, self.env);
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

    // ── Word expansion ───────────────────────────────────────────

    /// Expand a subscript expression string (may contain $vars).
    fn expand_subscript(&self, sub: &str) -> String {
        // Simple case: just a literal number or string
        if sub.contains('$') {
            // Contains variable reference — do basic expansion
            let mut result = sub.to_string();
            // Handle $var references
            while let Some(dollar) = result.find('$') {
                let rest = &result[dollar + 1..];
                let end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                let var_name = &rest[..end];
                let var_val = self.env.get_var(var_name).unwrap_or("").to_string();
                result = format!("{}{var_val}{}", &result[..dollar], &rest[end..]);
            }
            result
        } else {
            sub.to_string()
        }
    }

    /// Expand a Word AST node into a string, resolving variables, tilde, etc.
    /// For multi-word results (arrays, `$@`), joins with space.
    pub fn expand_word(&self, word: &Word) -> String {
        let bridge = ExpandBridge::new(self.env);
        let parts = frost_expand::expand_word(word, &bridge);
        parts.join("")
    }

    /// Expand a Word AST node into potentially multiple strings.
    ///
    /// Applies brace expansion after parameter/command substitution.
    pub fn expand_word_multi(&self, word: &Word) -> Vec<String> {
        let bridge = ExpandBridge::new(self.env);
        let parts = frost_expand::expand_word(word, &bridge);
        // Apply brace expansion to each resulting word
        let mut result = Vec::new();
        for part in parts {
            let expanded = frost_expand::expand_braces(&part);
            result.extend(expanded);
        }
        result
    }

    /// Scan a word for `<(cmd)` / `>(cmd)` process substitutions. For each,
    /// fork a subprocess whose I/O is attached to a fresh pipe, then replace
    /// the AST node with a `/dev/fd/N` literal so expansion yields a plain
    /// filename argument. Returns the rewritten word along with the list of
    /// file descriptors the parent kept open — the caller must close them
    /// after the main command completes, otherwise the subprocess will
    /// block on its pipe.
    ///
    /// macOS and Linux both expose `/dev/fd/N`, so the returned path works
    /// without any `mkfifo` dance.
    fn resolve_process_subs(&mut self, word: &Word) -> (Word, Vec<RawFd>) {
        if !word
            .parts
            .iter()
            .any(|p| matches!(p, WordPart::ProcessSub { .. }))
        {
            return (word.clone(), Vec::new());
        }
        let mut new_parts = Vec::with_capacity(word.parts.len());
        let mut open_fds = Vec::new();
        for part in &word.parts {
            match part {
                WordPart::ProcessSub { kind, body } => match self.spawn_process_sub(*kind, body) {
                    Ok((path, fd)) => {
                        open_fds.push(fd);
                        new_parts.push(WordPart::Literal(CompactString::from(path)));
                    }
                    Err(e) => {
                        eprintln!("frost: process substitution failed: {e}");
                        new_parts.push(WordPart::Literal(CompactString::from("")));
                    }
                },
                other => new_parts.push(other.clone()),
            }
        }
        (
            Word {
                parts: new_parts,
                span: word.span,
            },
            open_fds,
        )
    }

    /// Fork a subprocess connected via a pipe and return the parent-side
    /// `/dev/fd/N` path + the fd the parent should close after the main
    /// command finishes.
    fn spawn_process_sub(
        &mut self,
        kind: ProcessSubKind,
        body: &Program,
    ) -> Result<(String, RawFd), ExecError> {
        let pipe = sys::pipe().map_err(ExecError::Pipe)?;
        let body_owned = body.clone();
        match unsafe { sys::fork() }.map_err(ExecError::Fork)? {
            sys::ForkOutcome::Child => {
                // Wire child's stdout/stdin to the pipe according to direction,
                // close the other end, then execute the body. Exit deliberately
                // so we don't fall through into the parent's control flow.
                //
                // Reset SIGPIPE to SIG_DFL so a closed read end produces a
                // clean signal exit instead of Rust's default "panic on
                // Broken pipe from println!". This matches zsh behavior —
                // `echo <(echo foo)` just prints the path; the child's write
                // to a never-read pipe should not be a visible error.
                unsafe {
                    libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                }

                let (src, dst, close_other) = match kind {
                    ProcessSubKind::Input => (pipe.write, 1, pipe.read),
                    ProcessSubKind::Output => (pipe.read, 0, pipe.write),
                };
                let _ = sys::close(close_other);
                if sys::dup2_and_close(src, dst).is_err() {
                    std::process::exit(126);
                }
                let mut child_env = self.env.clone();
                let mut executor = Executor::new(&mut child_env);
                let status = executor.execute_program(&body_owned).unwrap_or(1);
                std::process::exit(status);
            }
            sys::ForkOutcome::Parent { child_pid: _ } => {
                // Keep the parent's end open for the main command; close the
                // other end immediately so the child actually sees EOF / the
                // write end when it's done.
                let (keep, drop_fd) = match kind {
                    ProcessSubKind::Input => (pipe.read, pipe.write),
                    ProcessSubKind::Output => (pipe.write, pipe.read),
                };
                let _ = sys::close(drop_fd);
                let path = format!("/dev/fd/{keep}");
                Ok((path, keep))
            }
        }
    }

    /// Attempt filesystem glob expansion of `pattern` against the current cwd.
    /// Appends matches to `out`. Policy:
    ///
    /// * If the glob matches: append each match path (as a string).
    /// * If it does not match AND `NULL_GLOB` is set: drop the word silently.
    /// * If it does not match AND `NO_MATCH` is set (zsh default): currently
    ///   passes the pattern through literally. This deviates from strict zsh
    ///   (which would error) but matches bash and makes frost useful today;
    ///   strict NOMATCH enforcement can be layered on later.
    /// * Otherwise: pass the pattern through literally.
    fn apply_glob_to(&self, pattern: String, out: &mut Vec<String>) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let opts = frost_glob::GlobOptions {
            dot_glob: self.env.is_option_set(frost_options::ShellOption::GlobDots),
            case_insensitive: !self.env.is_option_set(frost_options::ShellOption::CaseGlob),
        };
        match frost_glob::expand_pattern(&pattern, &cwd, &opts) {
            Ok(matches) if !matches.is_empty() => {
                for m in matches {
                    out.push(m.to_string_lossy().into_owned());
                }
            }
            Ok(_) => {
                if self.env.is_option_set(frost_options::ShellOption::NullGlob) {
                    // NULL_GLOB: drop the word silently.
                } else {
                    // Default & NO_MATCH: pass pattern through.
                    out.push(pattern);
                }
            }
            Err(_) => {
                // Pattern syntax error or I/O issue: fall back to literal.
                out.push(pattern);
            }
        }
    }
}

/// RAII close of the parent-side process-substitution file descriptors.
/// The child subprocess keeps its end of the pipe and runs on its own.
/// When the main command has finished, dropping this guard closes the
/// parent's ends — the child then sees EOF (for `<(cmd)`) or gets its
/// stdin closed (for `>(cmd)`) and exits naturally.
struct ProcSubFdGuard {
    fds: Vec<RawFd>,
}

impl Drop for ProcSubFdGuard {
    fn drop(&mut self) {
        for fd in &self.fds {
            let _ = sys::close(*fd);
        }
    }
}

/// Expand an alias chain in `argv`. zsh's rules:
///
/// * Only the first word of a simple command is matched against the alias
///   table.
/// * An alias value is re-tokenized on whitespace; the resulting words
///   replace argv\[0\] and argv\[1..\] is appended.
/// * Recursion is bounded by tracking which alias names have been expanded
///   in this pass — a self-referential `alias ls='ls --color'` expands once
///   and then falls through to the real `ls`.
/// * A trailing space in the alias value would allow alias expansion to
///   apply to the next word too (`alias sudo='sudo '` ⇒ `sudo ll` expands
///   both); implementing that precisely requires carrying a flag through
///   recursion and is deferred for a follow-up.
/// PATH lookup for a bare command name. Returns the resolved
/// absolute path on success; None when not found / not executable.
/// Used by `execute_simple` to surface [`ExecError::CommandNotFound`]
/// as a structured error before forking (so the REPL can
/// "did-you-mean"-suggest rather than letting a child's ENOENT
/// print `frost: <name>: ENOENT`).
fn path_lookup(env: &ShellEnv, name: &str) -> Option<std::path::PathBuf> {
    let path = env.get_var("PATH")?;
    for dir in path.split(':').filter(|p| !p.is_empty()) {
        let candidate = std::path::Path::new(dir).join(name);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if !meta.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return Some(candidate);
        }
    }
    None
}

fn expand_aliases(
    mut argv: Vec<String>,
    aliases: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for _ in 0..16 {
        // Hard cap: 16 expansion rounds covers real configs and prevents any
        // pathological mutual-recursive aliases from looping forever.
        if argv.is_empty() {
            break;
        }
        let first = &argv[0];
        if seen.contains(first) {
            break;
        }
        let Some(value) = aliases.get(first) else {
            break;
        };
        seen.insert(first.clone());
        // Tokenize the alias value on whitespace. This is intentionally
        // simpler than full shell tokenization — aliases commonly look
        // like `ls -la` or `grep --color=auto` and don't need quoting.
        let mut replacement: Vec<String> =
            value.split_whitespace().map(|s| s.to_string()).collect();
        if replacement.is_empty() {
            break;
        }
        replacement.extend(argv.drain(1..));
        argv = replacement;
    }
    argv
}

/// Returns true if `w` contains an unquoted glob AST node (not an escaped or
/// quoted `*`/`?`). This is the signal that the executor should try
/// filesystem glob expansion after all other expansions complete — a `*`
/// that appears only inside a single-quoted literal or inside a `$var`
/// value is NOT a glob under zsh's default semantics.
fn word_has_unquoted_glob(w: &Word) -> bool {
    use frost_parser::ast::WordPart;
    fn contains(parts: &[WordPart]) -> bool {
        parts.iter().any(|p| match p {
            WordPart::Glob(_) | WordPart::ExtGlob { .. } => true,
            // Quoted parts carry their own parts but they are all literal.
            WordPart::DoubleQuoted(inner) => contains(inner),
            WordPart::SingleQuoted(_)
            | WordPart::Literal(_)
            | WordPart::DollarVar(_)
            | WordPart::DollarBrace { .. }
            | WordPart::ParamExp(_)
            | WordPart::CommandSub(_)
            | WordPart::ArithSub(_)
            | WordPart::Tilde(_)
            | WordPart::BraceExp(_)
            | WordPart::ProcessSub { .. } => false,
        })
    }
    contains(&w.parts)
}

// ── Bridge from ShellEnv to frost_expand::ExpandEnv ─────────────────

/// Adapter that lets the expansion engine access `ShellEnv`.
struct ExpandBridge<'a> {
    env: &'a ShellEnv,
}

impl<'a> ExpandBridge<'a> {
    fn new(env: &'a ShellEnv) -> Self {
        Self { env }
    }
}

impl ExpandEnv for ExpandBridge<'_> {
    fn get_var(&self, name: &str) -> Option<&str> {
        self.env.get_var(name)
    }

    fn get_var_value(&self, name: &str) -> Option<frost_expand::ExpandValue> {
        self.env
            .get_value(name)
            .map(|sv| ShellEnv::to_expand_value(sv))
    }

    fn exit_status(&self) -> i32 {
        self.env.exit_status
    }

    fn pid(&self) -> u32 {
        self.env.pid
    }

    fn positional_params(&self) -> &[String] {
        &self.env.positional_params
    }

    fn capture_command_sub(&self, program: &Program) -> String {
        capture_command_sub(program, self.env)
    }

    fn eval_arithmetic(&self, expr: &str) -> i64 {
        eval_arithmetic(expr, self.env)
    }

    fn random(&self) -> u32 {
        // Use a simple hash of current time for randomness
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut h);
        std::thread::current().id().hash(&mut h);
        (h.finish() & 0x7fff) as u32
    }

    fn seconds_elapsed(&self) -> u64 {
        self.env.seconds_elapsed()
    }
}

/// Invert exit status for `!` pipelines.
fn invert(status: i32) -> i32 {
    if status == 0 { 1 } else { 0 }
}

/// Parse a string as an integer (for -eq/-lt/etc. comparisons).
fn parse_int(s: &str) -> i64 {
    s.trim().parse().unwrap_or(0)
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
    let pi: Vec<char> = pattern.chars().collect();
    let ti: Vec<char> = text.chars().collect();
    match_pattern(&pi, &ti)
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

/// Capture stdout from a command substitution by forking, piping stdout,
/// executing the program in the child, and reading the output in the parent.
fn capture_command_sub(program: &Program, env: &ShellEnv) -> String {
    // Create a pipe to capture the child's stdout
    let pipe = match sys::pipe() {
        Ok(p) => p,
        Err(_) => return String::new(),
    };

    match unsafe { sys::fork() } {
        Ok(sys::ForkOutcome::Child) => {
            // Child: wire stdout to pipe write end, close read end
            sys::close(pipe.read).ok();
            sys::dup2(pipe.write, 1).ok();
            sys::close(pipe.write).ok();

            // Execute the program in a cloned environment
            let mut child_env = env.clone();
            let mut executor = Executor::new(&mut child_env);
            let status = executor.execute_program(program).unwrap_or(1);
            std::process::exit(status);
        }
        Ok(sys::ForkOutcome::Parent { child_pid }) => {
            // Parent: close write end, read all output from read end
            sys::close(pipe.write).ok();

            let mut output = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = unsafe { libc::read(pipe.read, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                output.extend_from_slice(&buf[..n as usize]);
            }
            sys::close(pipe.read).ok();

            // Wait for the child
            let _ = sys::wait_pid(child_pid);

            String::from_utf8_lossy(&output).into_owned()
        }
        Err(_) => String::new(),
    }
}

/// Evaluate an arithmetic expression.
fn eval_arithmetic(expr: &str, env: &ShellEnv) -> i64 {
    crate::arith::eval_arithmetic(expr, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use frost_lexer::Span;
    use frost_parser::ast::{
        AssignOp, Assignment, CompleteCommand, List, Pipeline, SimpleCommand, Word, WordPart,
    };
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
        let exec = Executor {
            env: &mut ShellEnv::new(),
            builtins: frost_builtins::default_builtins(),
            jobs: JobTable::new(),
        };
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
                                subscript: None,
                                op: AssignOp::Assign,
                                value: Some(literal_word("hello")),
                                array_value: None,
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

    // ── Alias expansion ────────────────────────────────────────────

    fn alias_map(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn alias_expands_first_word() {
        let a = alias_map(&[("ll", "ls -la")]);
        let argv = vec!["ll".into(), "src/".into()];
        assert_eq!(expand_aliases(argv, &a), vec!["ls", "-la", "src/"]);
    }

    #[test]
    fn alias_chain_until_fixed_point() {
        let a = alias_map(&[("ll", "la -l"), ("la", "ls -A")]);
        let argv = vec!["ll".into()];
        assert_eq!(expand_aliases(argv, &a), vec!["ls", "-A", "-l"]);
    }

    #[test]
    fn self_referential_alias_expands_once() {
        // `alias ls='ls --color'` — must expand exactly once, not loop.
        let a = alias_map(&[("ls", "ls --color")]);
        let argv = vec!["ls".into(), "src/".into()];
        assert_eq!(expand_aliases(argv, &a), vec!["ls", "--color", "src/"]);
    }

    #[test]
    fn unknown_command_is_unchanged() {
        let a = alias_map(&[("ll", "ls -la")]);
        let argv = vec!["cat".into(), "file".into()];
        assert_eq!(expand_aliases(argv, &a), vec!["cat", "file"]);
    }

    #[test]
    fn empty_alias_value_is_a_noop() {
        let a = alias_map(&[("nop", "")]);
        let argv = vec!["nop".into(), "arg".into()];
        // An empty-valued alias shouldn't drop argv[0] into nothing — keep
        // the original word so the user gets a clean "command not found"
        // rather than a confusing blank execution.
        assert_eq!(expand_aliases(argv, &a), vec!["nop", "arg"]);
    }
}
