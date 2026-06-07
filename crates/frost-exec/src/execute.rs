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

        // `noglob` precommand modifier. Glob expansion runs HERE — upstream
        // of the precommand-modifier strip below — so `noglob` must be
        // recognized at the word level now, or the glob has already mangled
        // `nix build .#attr` / `^` / `~` before we ever see the modifier.
        // The highest-value interactive parity fix — nix flake refs are
        // hand-typed under the fleet's enabled EXTENDED_GLOB.
        let suppress_glob =
            PrecommandModifiers::scan_suppress_glob(resolved_words.iter().map(leading_literal));

        // Glob expansion runs after all other word expansions. We only glob
        // words that originally contained unquoted glob AST parts — this
        // preserves zsh's GLOB_SUBST-off default (a `*` that came from a
        // variable value is NOT re-globbed).
        let argv: Vec<String> = {
            let mut out = Vec::with_capacity(resolved_words.len());
            for word in &resolved_words {
                let expanded = self.expand_word_multi(word);
                let preserve_empties = word_has_quoted_part(word);
                if word_has_unquoted_glob(word)
                    && !suppress_glob
                    && self.env.is_option_set(frost_options::ShellOption::Glob)
                {
                    for candidate in expanded {
                        self.apply_glob_to(candidate, &mut out);
                    }
                } else if preserve_empties {
                    // Quoted parts keep empty results (`[ -n "" ]` is
                    // three args, not two).
                    out.extend(expanded);
                } else {
                    // Unquoted word that expanded to nothing — drop.
                    // Matches POSIX "null-token removal" for bare vars.
                    out.extend(expanded.into_iter().filter(|s| !s.is_empty()));
                }
            }
            out
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
        let mut argv = expand_aliases(argv, &self.env.aliases);

        // Precommand modifiers (`builtin`/`command`/`noglob`/`nocorrect`/
        // `exec`). zsh resolves these in the executor, not as leaf builtins
        // — a standalone `builtin`/`command` builtin cannot see the registry
        // or the function table, so on its own it no-ops. That silently
        // broke `builtin cd`, the spine of the zoxide cd-integration
        // override (`elif builtin cd "$@" …; then :`): the no-op falsely
        // reported success, so `cd` never changed directory and never fell
        // through to the zoxide teleport (diagnosed 2026-06-06). The typed
        // `PrecommandModifiers` strips the leading run and folds it into one
        // tested value; `noglob`'s glob suppression was already applied
        // pre-glob above.
        let mods = PrecommandModifiers::strip(&mut argv);

        // `command -v NAME` / `command -V NAME` — a resolution query, not
        // execution (needs the registry + function table + PATH).
        if mods.command_modifier
            && matches!(argv.first().map(String::as_str), Some("-v" | "-V"))
        {
            let verbose = argv[0] == "-V";
            let target = argv.get(1).cloned();
            return Ok(self.command_resolve_query(target.as_deref(), verbose));
        }

        if argv.is_empty() {
            return Ok(0);
        }

        // Record this command's last (fully-expanded) word as `$_` for the
        // NEXT command. The current command's own `$_` already expanded
        // above using the previous command's value (argv was built before
        // this point), so updating here is correct zsh ordering.
        self.env.last_arg = argv.last().cloned().unwrap_or_default();

        // `exec CMD …` — replace the shell process image with CMD (no fork),
        // so frostmourne's `reload` (= `exec frostmourne`) is a real re-exec
        // instead of a failed PATH lookup for a nonexistent `exec` binary.
        if mods.exec_replace {
            return self.exec_replace(&argv, &cmd.redirects);
        }

        let name = &argv[0];

        // Check for functions first (bypassed when a `builtin`/`command`
        // precommand modifier was present — those skip the function table).
        if let Some(fdef) = (!mods.bypass_functions)
            .then(|| self.env.functions.get(name).cloned())
            .flatten()
        {
            let saved_params = self.env.positional_params.clone();
            // Hook functions (precmd, preexec, chpwd, prompt-loop hooks)
            // intentionally mutate the caller's env — that's the whole
            // point of `precmd` setting PS1, `chpwd` exporting OLDPWD,
            // etc. Running them in a local scope silently swallows those
            // assignments. Matches zsh: hook functions do NOT push a
            // new scope. (Real incident: 2026-05-21, seki defprompt →
            // synthetic precmd body `PS1=$(seki prompt …)` ran but
            // PS1 stayed empty → prompt fell back to `frost> `.)
            // Hook functions (precmd, preexec, chpwd, prompt loop)
            // intentionally mutate the caller's env — that's how
            // PS1, FROST_CMD_DURATION, OLDPWD etc. propagate.
            // Skip the function-local scope push so those assignments
            // land in the caller's scope (matches zsh hook semantics).
            // (Paired with frost-lisp wrapping hook bodies as
            // BraceGroup not Subshell, so they don't fork either.)
            let is_hook = name.starts_with("__frost_hook_");
            if !is_hook {
                self.env.push_scope();
            }
            self.env.positional_params = argv[1..].to_vec();
            let result = match self.execute_command(&fdef.body) {
                Ok(s) => Ok(s),
                Err(ExecError::ControlFlow(ControlFlow::Return(code))) => {
                    self.env.exit_status = code;
                    Ok(code)
                }
                Err(e) => Err(e),
            };
            if !is_hook {
                self.env.pop_scope();
            }
            self.env.positional_params = saved_params;
            return result;
        }

        // `type` / `whence` / `which` — report how a name resolves (alias /
        // reserved word / function / builtin / PATH). Routed through the
        // executor's resolver because the leaf builtins can't see the
        // registry, functions, or aliases. A user FUNCTION named
        // type/whence/which already won above. The last non-flag arg is the
        // name; `whence` is terse unless `-v`, `type`/`which` are verbose.
        if matches!(name.as_str(), "type" | "whence" | "which") {
            let verbose = match name.as_str() {
                "whence" => argv[1..].iter().any(|a| a == "-v"),
                _ => true,
            };
            let target = argv[1..].iter().rev().find(|a| !a.starts_with('-'));
            return Ok(self.command_resolve_query(target.map(String::as_str), verbose));
        }

        // Check builtins. Builtins run IN-PROCESS even when redirects are
        // present: their effects are shell state (cd, export, setopt,
        // read, …) that must persist in the parent, and the
        // BuiltinAction handling below must still run. We save the
        // affected fds, apply the redirects, run the builtin, then
        // restore. (The old path forked to apply redirects, so
        // `cd x 2>/dev/null` ran in a child and was a no-op in the parent
        // — the load-bearing failure behind the broken zoxide `cd`
        // override, which finalizes jumps with `builtin cd … 2>/dev/null`.)
        if self.builtins.contains(name) {
            let saved_fds = if cmd.redirects.is_empty() {
                Vec::new()
            } else {
                match save_and_apply_redirects(&cmd.redirects) {
                    Ok(saved) => saved,
                    Err(e) => {
                        eprintln!("frost: {e}");
                        self.env.exit_status = 1;
                        return Ok(1);
                    }
                }
            };

            let arg_refs: Vec<&str> = argv[1..].iter().map(|s| s.as_str()).collect();
            let result = self
                .builtins
                .get(name)
                .unwrap()
                .execute_with_action(&arg_refs, self.env);

            // Restore the saved fds before any hooks / BuiltinActions
            // below run (they may perform their own I/O).
            restore_saved_fds(saved_fds);

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

        // `builtin NAME` where NAME is not a registered builtin is an
        // error — it must not fall through to AUTO_CD or external exec.
        if mods.require_builtin {
            eprintln!("frost: builtin: no such builtin: {name}");
            self.env.exit_status = 1;
            return Ok(1);
        }

        // AUTO_CD — a bare single word that names an existing directory
        // is treated as `cd <word>`. This runs only after the function
        // and builtin checks above have failed, so a real command never
        // gets shadowed by a same-named directory. Routes through the
        // same canonicalizing `chdir` that the `cd` builtin uses, so cwd
        // and `$PWD` stay in lock-step, and fires the `chpwd` hook just
        // like an explicit `cd` does.
        if argv.len() == 1 && self.env.is_option_set(frost_options::ShellOption::AutoCd) {
            let candidate = &argv[0];
            if std::path::Path::new(candidate).is_dir() {
                use frost_builtins::ShellEnvironment;
                match self.env.chdir(candidate) {
                    Ok(()) => {
                        if self.env.functions.contains_key("__frost_hook_chpwd") {
                            let body = self.env.functions["__frost_hook_chpwd"].body.clone();
                            // A broken hook must not break the directory change.
                            let _ = self.execute_command(&body);
                        }
                        self.env.exit_status = 0;
                        return Ok(0);
                    }
                    Err(e) => {
                        eprintln!("cd: {candidate}: {e}");
                        self.env.exit_status = 1;
                        return Ok(1);
                    }
                }
            }
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

    /// `command -v NAME` / `command -V NAME` resolution query. Prints
    /// the resolution and returns 0 when NAME is a builtin, a shell
    /// function, or found on PATH; returns 1 with no output otherwise.
    /// Lives on the executor because it needs the registry, the
    /// function table, and PATH — none of which the leaf `command`
    /// builtin can reach.
    fn command_resolve_query(&self, name: Option<&str>, verbose: bool) -> i32 {
        let Some(name) = name else {
            return 1;
        };
        // Resolution order matches zsh: alias → reserved word → function →
        // builtin → PATH. Used by `command -v/-V`, `type`, `whence`, `which`.
        if let Some(value) = self.env.aliases.get(name) {
            if verbose {
                println!("{name} is an alias for {value}");
            } else {
                println!("{name}");
            }
            return 0;
        }
        if is_reserved_word(name) {
            if verbose {
                println!("{name} is a reserved word");
            } else {
                println!("{name}");
            }
            return 0;
        }
        if self.env.functions.contains_key(name) {
            if verbose {
                println!("{name} is a shell function");
            } else {
                println!("{name}");
            }
            return 0;
        }
        if self.builtins.contains(name) {
            if verbose {
                println!("{name} is a shell builtin");
            } else {
                println!("{name}");
            }
            return 0;
        }
        if let Some(path) = path_lookup(&self.env, name) {
            let resolved = path.display();
            if verbose {
                println!("{name} is {resolved}");
            } else {
                println!("{resolved}");
            }
            return 0;
        }
        if verbose {
            eprintln!("{name} not found");
        }
        1
    }

    /// Fork to run a builtin with redirects applied in the child.
    // (Builtins with redirects are now run in-process via
    // `save_and_apply_redirects` / `restore_saved_fds` — see the builtin
    // dispatch in `execute_simple`. The previous `fork_exec_builtin`
    // forked, which discarded shell-state mutations like `cd`.)

    /// `exec CMD args` — replace the current process image with CMD,
    /// **without** forking. Redirects apply to the current process first
    /// (they persist into the replacement, matching zsh). `sys::exec`
    /// PATH-resolves a bare name, so `exec frostmourne` works. exec only
    /// returns on failure — report it as 127 (not found) / 126 (other),
    /// like a normal exec error.
    fn exec_replace(
        &mut self,
        argv: &[String],
        redirects: &[frost_parser::ast::Redirect],
    ) -> ExecResult {
        if !redirects.is_empty() {
            if let Err(e) = redirect::apply_redirects(redirects) {
                eprintln!("frost: {e}");
                self.env.exit_status = 1;
                return Ok(1);
            }
        }
        let c_argv: Vec<CString> = argv
            .iter()
            .filter_map(|a| CString::new(a.as_bytes()).ok())
            .collect();
        let c_envp = self.env.to_env_vec();
        let err = sys::exec(&c_argv, &c_envp);
        eprintln!("frost: exec: {}: {err}", argv[0]);
        let code = if err == nix::errno::Errno::ENOENT {
            127
        } else {
            126
        };
        self.env.exit_status = code;
        Ok(code)
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
/// Stash fds 0/1/2, then apply a builtin's redirects to the current
/// process. Returns the `(original_fd, backup_fd)` pairs to hand to
/// [`restore_saved_fds`] after the builtin runs. On failure the already-
/// stashed fds are restored before returning the error, so the shell's
/// own stdio is never left redirected.
fn save_and_apply_redirects(
    redirects: &[frost_parser::ast::Redirect],
) -> Result<Vec<(std::os::fd::RawFd, std::os::fd::RawFd)>, redirect::RedirectError> {
    // Back up the standard streams (the only fds builtins realistically
    // target) above fd 9 so they don't collide with the low fds the
    // redirect itself allocates.
    let mut saved = Vec::new();
    for fd in [0, 1, 2] {
        if let Ok(backup) = sys::dup_from(fd, 10) {
            saved.push((fd, backup));
        }
    }
    if let Err(e) = redirect::apply_redirects(redirects) {
        restore_saved_fds(saved);
        return Err(e);
    }
    Ok(saved)
}

/// Restore fds saved by [`save_and_apply_redirects`], closing each backup.
fn restore_saved_fds(saved: Vec<(std::os::fd::RawFd, std::os::fd::RawFd)>) {
    for (orig, backup) in saved {
        let _ = sys::dup2(backup, orig);
        let _ = sys::close(backup);
    }
}

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
/// Whether a word contains any quoted part (single- or double-quoted)
/// or a literal. Words that consist purely of unquoted variable/subst
/// references get "null-token removal" when they expand to empty —
/// matching POSIX. Quoted empties are preserved so `[ -n "" ]` stays
/// three arguments and `echo "" foo ""` preserves the empties.
/// Whether `word` is a shell reserved word (keyword). Used by the
/// `type`/`whence`/`command -v` name resolver to report a name's kind.
fn is_reserved_word(word: &str) -> bool {
    matches!(
        word,
        "if" | "then"
            | "elif"
            | "else"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "select"
            | "repeat"
            | "function"
            | "in"
            | "time"
            | "coproc"
            | "{"
            | "}"
            | "[["
            | "]]"
            | "!"
    )
}

/// The literal text of a word that is exactly one unquoted literal part —
/// e.g. a bareword precommand modifier (`noglob`, `nocorrect`, `builtin`,
/// `command`, `exec`). None for quoted, multi-part, glob, or expansion
/// words (a precommand modifier is never any of those).
fn leading_literal(w: &Word) -> Option<&str> {
    use frost_parser::ast::WordPart;
    match w.parts.as_slice() {
        [WordPart::Literal(s)] => Some(s.as_str()),
        _ => None,
    }
}

/// The set of leading **precommand modifiers** on a simple command. zsh
/// resolves these in the executor — they are not builtins: `builtin` and
/// `command` change name resolution, `noglob`/`nocorrect` change
/// expansion/correction, `exec` replaces the process image. Encapsulated
/// as one typed, tested unit so the parse lives in one place and a new
/// modifier (`time`, …) is one variant in [`is_modifier`] + one arm in
/// [`apply`].
///
/// [`is_modifier`]: PrecommandModifiers::is_modifier
/// [`apply`]: PrecommandModifiers::apply
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PrecommandModifiers {
    /// `builtin`/`command` — skip the function table for the target.
    bypass_functions: bool,
    /// `builtin NAME` — the target must be a registered builtin (else error).
    require_builtin: bool,
    /// `command` was present — gates the `command -v/-V NAME` query form.
    command_modifier: bool,
    /// `noglob` — suppress glob expansion for the command.
    suppress_glob: bool,
    /// `nocorrect` — suppress spelling correction (no-op until CORRECT).
    suppress_correct: bool,
    /// `exec` — replace the shell process with the command (no fork).
    exec_replace: bool,
}

impl PrecommandModifiers {
    /// Whether `word` is a recognized precommand modifier.
    fn is_modifier(word: &str) -> bool {
        matches!(
            word,
            "builtin" | "command" | "noglob" | "nocorrect" | "exec"
        )
    }

    /// Fold one modifier word into the set. Caller guarantees
    /// `is_modifier(word)`.
    fn apply(&mut self, word: &str) {
        match word {
            "builtin" => {
                self.bypass_functions = true;
                self.require_builtin = true;
            }
            "command" => {
                self.bypass_functions = true;
                self.command_modifier = true;
            }
            "noglob" => self.suppress_glob = true,
            "nocorrect" => self.suppress_correct = true,
            "exec" => self.exec_replace = true,
            _ => {}
        }
    }

    /// Strip the leading run of modifier words from `argv`, folding each
    /// into the returned set. `argv` is left with the real command at [0]
    /// (or empty if the command was only modifiers).
    fn strip(argv: &mut Vec<String>) -> Self {
        let mut mods = Self::default();
        while argv.first().is_some_and(|w| Self::is_modifier(w)) {
            let word = argv.remove(0);
            mods.apply(&word);
        }
        mods
    }

    /// Whether the leading modifier run contains `noglob`. Computed from
    /// the literal leading words BEFORE glob expansion, because glob runs
    /// upstream of the argv-level [`strip`](Self::strip).
    fn scan_suppress_glob<'a>(words: impl Iterator<Item = Option<&'a str>>) -> bool {
        let mut suppress = false;
        for w in words {
            match w {
                Some(word) if Self::is_modifier(word) => {
                    if word == "noglob" {
                        suppress = true;
                    }
                }
                _ => break,
            }
        }
        suppress
    }
}

fn word_has_quoted_part(w: &Word) -> bool {
    use frost_parser::ast::WordPart;
    w.parts.iter().any(|p| {
        matches!(
            p,
            WordPart::SingleQuoted(_) | WordPart::DoubleQuoted(_) | WordPart::Literal(_)
        )
    })
}

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

    fn last_arg(&self) -> &str {
        &self.env.last_arg
    }

    fn option_flags(&self) -> String {
        use frost_options::ShellOption;
        // sh-style single-letter flags for the options that have a
        // canonical letter — enough for the common `[[ $- == *i* ]]`
        // interactive check.
        let mut flags = String::new();
        if self.env.is_option_set(ShellOption::Interactive) {
            flags.push('i');
        }
        if self.env.is_option_set(ShellOption::Monitor) {
            flags.push('m');
        }
        flags
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

    // ── cd / AUTO_CD directory-change tests ──────────────────────────
    //
    // These tests mutate the process-global cwd, so they serialize on a
    // shared mutex to stay deterministic when the test harness runs them
    // on parallel threads. Each test restores the cwd to a stable
    // directory BEFORE releasing the lock + removing its temp dir, so a
    // sibling test never observes a cwd pointing at a deleted directory.
    use std::sync::Mutex;
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    /// Create a unique existing directory under the system temp dir and
    /// return its canonical absolute path. Caller cleans up.
    fn unique_existing_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "frost-cd-test-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::canonicalize(&dir).expect("canonicalize temp dir")
    }

    /// A stable directory that exists for the whole test process and is
    /// never removed — tests restore the cwd here before releasing the
    /// CWD_LOCK so siblings never see a dangling cwd.
    fn stable_dir() -> std::path::PathBuf {
        std::fs::canonicalize(std::env::temp_dir()).expect("canonicalize temp_dir")
    }

    /// (a) `cd <dir>` updates the process cwd AND `$PWD` to the absolute
    /// LOGICAL path — `.`/`..` collapsed lexically, the raw `/.`-suffixed
    /// spelling normalized away. (`dir` is pre-canonicalized here so logical
    /// == its value; symlink-NON-chasing parity is pinned separately by the
    /// zsh_compat `cd_updates_pwd` test.)
    #[test]
    fn cd_updates_cwd_and_pwd_to_logical_path() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_current_dir(stable_dir()).unwrap();
        let dir = unique_existing_dir("cd");
        // A non-canonical spelling of the same directory: append a
        // redundant `.` component that lexical resolution must collapse away.
        let noncanonical = dir.join(".");

        let mut env = ShellEnv::new();
        let mut exec = Executor::new(&mut env);
        let program = simple_program(vec!["cd", noncanonical.to_str().unwrap()]);
        let status = exec.execute_program(&program).unwrap();
        assert_eq!(status, 0);

        // Process cwd is the resolved directory.
        assert_eq!(std::env::current_dir().unwrap(), dir);
        // $PWD mirrors it — the `/.`-suffixed input lexically normalized away.
        assert_eq!(env.get_var("PWD"), Some(dir.to_str().unwrap()));

        // Restore before releasing the lock + removing the temp dir.
        std::env::set_current_dir(stable_dir()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (b) Entering a bare directory path with AUTO_CD enabled changes
    /// directory (no `cd` keyword, no executable of that name).
    #[test]
    fn autocd_bare_directory_changes_dir() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_current_dir(stable_dir()).unwrap();
        let dir = unique_existing_dir("autocd");

        let mut env = ShellEnv::new();
        env.set_option(frost_options::ShellOption::AutoCd);
        let mut exec = Executor::new(&mut env);
        // Bare directory path as the sole word — would be "command not
        // found" without AUTO_CD.
        let program = simple_program(vec![dir.to_str().unwrap()]);
        let status = exec.execute_program(&program).unwrap();
        assert_eq!(status, 0);

        assert_eq!(std::env::current_dir().unwrap(), dir);
        assert_eq!(env.get_var("PWD"), Some(dir.to_str().unwrap()));

        std::env::set_current_dir(stable_dir()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AUTO_CD goes through the same `chdir` surface as the `cd` builtin,
    /// so it saves `OLDPWD` — `cd -` after an autocd returns to the prior
    /// directory (zsh AUTO_CD behaves identically to `cd`). Regression
    /// guard: the autocd path previously skipped the OLDPWD save.
    #[test]
    fn autocd_saves_oldpwd_so_cd_dash_returns() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let origin = unique_existing_dir("autocd-oldpwd-origin");
        let dest = unique_existing_dir("autocd-oldpwd-dest");
        std::env::set_current_dir(&origin).unwrap();

        let mut env = ShellEnv::new();
        env.set_var("PWD", origin.to_str().unwrap());
        env.set_option(frost_options::ShellOption::AutoCd);
        let mut exec = Executor::new(&mut env);

        // Autocd into dest (bare directory path).
        let status = exec
            .execute_program(&simple_program(vec![dest.to_str().unwrap()]))
            .unwrap();
        assert_eq!(status, 0);
        assert_eq!(env.get_var("PWD"), Some(dest.to_str().unwrap()));
        // OLDPWD was recorded by the shared chdir — proving the autocd path
        // no longer skips it.
        assert_eq!(env.get_var("OLDPWD"), Some(origin.to_str().unwrap()));

        std::env::set_current_dir(stable_dir()).unwrap();
        let _ = std::fs::remove_dir_all(&origin);
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// AUTO_CD is gated on the option: without it set, a bare directory
    /// path is NOT a directory change (it falls through to command
    /// resolution and reports "command not found").
    #[test]
    fn autocd_disabled_does_not_change_dir() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_current_dir(stable_dir()).unwrap();
        let start = std::env::current_dir().unwrap();
        let dir = unique_existing_dir("autocd-off");

        let mut env = ShellEnv::new();
        // AutoCd intentionally NOT set.
        let mut exec = Executor::new(&mut env);
        let program = simple_program(vec![dir.to_str().unwrap()]);
        // Without AUTO_CD a bare directory path falls through to command
        // resolution (an absolute path → fork+exec, which fails because a
        // directory isn't executable). The load-bearing invariant is that
        // it does NOT silently chdir — the cwd is unchanged.
        let _ = exec.execute_program(&program);
        assert_eq!(std::env::current_dir().unwrap(), start);

        std::env::set_current_dir(stable_dir()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (c) A non-existent path is a clean error (status 1), not a silent
    /// no-op — for both the `cd` builtin and AUTO_CD.
    #[test]
    fn cd_nonexistent_path_is_clean_error() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_current_dir(stable_dir()).unwrap();
        let start = std::env::current_dir().unwrap();
        let missing = stable_dir().join(format!(
            "frost-cd-test-missing-{}-no-such-dir",
            std::process::id()
        ));
        // Make sure it really doesn't exist.
        let _ = std::fs::remove_dir_all(&missing);
        let missing_str = missing.to_str().unwrap();

        // `cd <missing>` → status 1, cwd unchanged.
        let mut env = ShellEnv::new();
        let mut exec = Executor::new(&mut env);
        let program = simple_program(vec!["cd", missing_str]);
        let status = exec.execute_program(&program).unwrap();
        assert_eq!(status, 1);
        assert_eq!(std::env::current_dir().unwrap(), start);

        // AUTO_CD on a non-existent path → not a directory, so AUTO_CD
        // does NOT fire and does NOT chdir; it falls through to command
        // resolution. The invariant: no silent cwd mutation.
        let mut env2 = ShellEnv::new();
        env2.set_option(frost_options::ShellOption::AutoCd);
        let mut exec2 = Executor::new(&mut env2);
        let program2 = simple_program(vec![missing_str]);
        let _ = exec2.execute_program(&program2);
        assert_eq!(std::env::current_dir().unwrap(), start);

        std::env::set_current_dir(stable_dir()).unwrap();
    }

    /// `builtin cd <dir>` must dispatch to the `cd` builtin. The bare
    /// `builtin` builtin is a no-op stub — the precommand-modifier
    /// resolution lives in the executor. (Regression: `builtin cd` used
    /// to no-op, which broke the zoxide `cd` override that wraps the
    /// real cd as `builtin cd …`.)
    #[test]
    fn builtin_prefix_dispatches_cd() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_current_dir(stable_dir()).unwrap();
        let dir = unique_existing_dir("builtin-cd");
        let canon = std::fs::canonicalize(&dir).unwrap();

        let mut env = ShellEnv::new();
        let mut exec = Executor::new(&mut env);
        let program = simple_program(vec!["builtin", "cd", dir.to_str().unwrap()]);
        let status = exec.execute_program(&program).unwrap();
        assert_eq!(status, 0);
        assert_eq!(std::env::current_dir().unwrap(), canon);

        std::env::set_current_dir(stable_dir()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `cd <dir> 2>/dev/null` must change the *parent's* directory.
    /// Builtins with redirects run in-process (fd save/restore); the old
    /// path forked, so the chdir happened in a child and was lost — the
    /// load-bearing failure behind the broken zoxide `cd` override, which
    /// finalizes jumps with `builtin cd … 2>/dev/null`.
    #[test]
    fn cd_with_redirect_persists_in_parent() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_current_dir(stable_dir()).unwrap();
        let dir = unique_existing_dir("cd-redirect");
        let canon = std::fs::canonicalize(&dir).unwrap();

        let mut prog = simple_program(vec!["cd", dir.to_str().unwrap()]);
        if let Command::Simple(sc) = &mut prog.commands[0].list.first.commands[0] {
            sc.redirects.push(frost_parser::ast::Redirect {
                fd: Some(2),
                op: frost_parser::ast::RedirectOp::Greater,
                target: literal_word("/dev/null"),
                span: Span::new(0, 0),
            });
        }

        let mut env = ShellEnv::new();
        let mut exec = Executor::new(&mut env);
        let status = exec.execute_program(&prog).unwrap();
        assert_eq!(status, 0);
        assert_eq!(std::env::current_dir().unwrap(), canon);

        std::env::set_current_dir(stable_dir()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn precommand_modifiers_strip_and_fold() {
        // `builtin` → bypass functions + require a registered builtin.
        let mut a = vec!["builtin".to_string(), "cd".into(), "/tmp".into()];
        let m = PrecommandModifiers::strip(&mut a);
        assert!(m.bypass_functions && m.require_builtin && !m.command_modifier);
        assert_eq!(a, ["cd", "/tmp"]);

        // `command` → bypass functions + eligible for the `-v` query.
        let mut a = vec!["command".to_string(), "ls".into()];
        let m = PrecommandModifiers::strip(&mut a);
        assert!(m.bypass_functions && m.command_modifier && !m.require_builtin);
        assert_eq!(a, ["ls"]);

        // Chained modifiers all fold in; the real command is left at [0].
        let mut a = vec![
            "noglob".to_string(),
            "nocorrect".into(),
            "exec".into(),
            "nix".into(),
        ];
        let m = PrecommandModifiers::strip(&mut a);
        assert!(m.suppress_glob && m.suppress_correct && m.exec_replace);
        assert_eq!(a, ["nix"]);

        // No modifiers → default, argv untouched.
        let mut a = vec!["echo".to_string(), "hi".into()];
        let m = PrecommandModifiers::strip(&mut a);
        assert_eq!(m, PrecommandModifiers::default());
        assert_eq!(a, ["echo", "hi"]);

        // Only-modifiers command → argv drained empty.
        let mut a = vec!["builtin".to_string()];
        let _ = PrecommandModifiers::strip(&mut a);
        assert!(a.is_empty());
    }

    #[test]
    fn precommand_modifiers_scan_suppress_glob() {
        // `noglob` first.
        assert!(PrecommandModifiers::scan_suppress_glob(
            [Some("noglob"), Some("nix"), Some("build")].into_iter()
        ));
        // `noglob` after another modifier still counts.
        assert!(PrecommandModifiers::scan_suppress_glob(
            [Some("command"), Some("noglob"), Some("nix")].into_iter()
        ));
        // No `noglob` in the leading run.
        assert!(!PrecommandModifiers::scan_suppress_glob(
            [Some("builtin"), Some("cd")].into_iter()
        ));
        // A `noglob` AFTER the real command does not count (scan stops at
        // the first non-modifier word).
        assert!(!PrecommandModifiers::scan_suppress_glob(
            [Some("echo"), Some("noglob")].into_iter()
        ));
    }

    #[test]
    fn reserved_words_recognized() {
        for kw in ["if", "then", "fi", "for", "while", "do", "done", "case", "function", "[["] {
            assert!(is_reserved_word(kw), "{kw} should be reserved");
        }
        for not in ["cd", "echo", "ls", "foo", ""] {
            assert!(!is_reserved_word(not), "{not} should NOT be reserved");
        }
    }

    #[test]
    fn last_arg_tracks_previous_command() {
        // `$_` — env.last_arg becomes the last (expanded) word of the
        // command that just ran, for the next command to read.
        let mut env = ShellEnv::new();
        {
            let mut exec = Executor::new(&mut env);
            let program = simple_program(vec!["echo", "a", "b", "c"]);
            let _ = exec.execute_program(&program);
        }
        assert_eq!(env.last_arg, "c");
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
