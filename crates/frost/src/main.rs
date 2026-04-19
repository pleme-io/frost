use std::io::IsTerminal;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use clap::Parser as ClapParser;

use frost_zle::{EditModeKind, InputStatus, ReadLineOutcome, ZleEngine};

/// Bitmask of signals that fired since the last check. Set by the
/// signal handler (which must be async-signal-safe — `fetch_or` on
/// `AtomicU64` is) and drained by the REPL between commands.
static PENDING_SIGNALS: AtomicU64 = AtomicU64::new(0);

/// Signals frost explicitly traps on behalf of rc-authored
/// `(deftrap :signal …)` forms. `SIGINT` is handled separately by
/// reedline (Ctrl-C on an interactive prompt) so it's not in this list.
/// If a user binds `deftrap INT` they'll still get the trap via the
/// explicit `check_pending_traps(env)` call inside the REPL loop after
/// the signal is recorded — but only when received during a running
/// external child, not during read_line.
const TRAPPED_SIGNALS: &[libc::c_int] = &[
    libc::SIGUSR1, libc::SIGUSR2, libc::SIGTERM, libc::SIGHUP, libc::SIGWINCH,
];

extern "C" fn signal_forwarder(sig: libc::c_int) {
    // Only async-signal-safe operations here. Atomic fetch_or is fine.
    if sig > 0 && (sig as usize) < 64 {
        PENDING_SIGNALS.fetch_or(1u64 << sig, Ordering::SeqCst);
    }
}

/// Install `sigaction` forwarders for every signal in `TRAPPED_SIGNALS`.
/// Idempotent — safe to call once at interactive-mode entry.
fn install_signal_traps() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = signal_forwarder as usize;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = libc::SA_RESTART;
        for &sig in TRAPPED_SIGNALS {
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

/// Drain the pending-signal bitmask and fire any rc-authored traps.
/// Called between REPL iterations so traps see a well-defined shell
/// state rather than interrupting mid-execution.
fn check_pending_traps(env: &mut frost_exec::ShellEnv) {
    let pending = PENDING_SIGNALS.swap(0, Ordering::SeqCst);
    if pending == 0 { return; }
    for sig in 1..64i32 {
        if pending & (1u64 << sig) == 0 { continue; }
        let name = frost_exec::trap::signal_number_to_name(sig);
        if name == "UNKNOWN" { continue; }
        let fn_name = format!("__frost_trap_{name}");
        if env.functions.contains_key(&fn_name) {
            let _ = run(&fn_name, env);
        }
    }
}

#[derive(ClapParser)]
#[command(name = "frost", version, about = "A zsh-compatible shell")]
struct Cli {
    /// Execute the given string as a command
    #[arg(short = 'c')]
    command: Option<String>,

    /// Script file to execute
    file: Option<String>,
}

/// Outcome of running one chunk of input through the executor.
enum RunOutcome {
    /// Normal completion — store the command's exit status.
    Completed(i32),
    /// User invoked `exit` / `exit N` — the REPL must stop.
    Exit(i32),
}

fn run(input: &str, env: &mut frost_exec::ShellEnv) -> RunOutcome {
    let tokens = tokenize(input);
    let mut parser = frost_parser::Parser::new(&tokens);
    let program = parser.parse();
    let mut executor = frost_exec::Executor::new(env);
    match executor.execute_program(&program) {
        Ok(status) => RunOutcome::Completed(status),
        Err(frost_exec::ExecError::ControlFlow(frost_exec::ControlFlow::Exit(code))) => {
            RunOutcome::Exit(code)
        }
        Err(e) => {
            eprintln!("frost: {e}");
            RunOutcome::Completed(1)
        }
    }
}

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

/// Cheap "does this input look complete?" check for the interactive REPL.
/// False → re-prompt with PS2 and concatenate the next line.
///
/// Heuristic — counts open/close pairs on the raw source (respecting simple
/// quote context) and checks for trailing `\`. This is intentionally not a
/// full parse: shell grammar is too ambiguous for that and we want the check
/// to be cheap and never panic.
fn is_complete(src: &str) -> bool {
    // Trailing backslash → classic line continuation
    if src.trim_end_matches(|c: char| c == ' ' || c == '\t')
        .ends_with('\\')
    {
        return false;
    }

    let bytes = src.as_bytes();
    let mut i = 0;
    let mut paren = 0i32;
    let mut brace = 0i32;
    let mut bracket = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    // Stack of keyword openers awaiting their closer.
    // `if→fi`, `do→done`, `case→esac`, `{<space>→}`.
    let mut kw: Vec<&'static str> = Vec::new();

    while i < bytes.len() {
        let c = bytes[i];
        if in_single {
            if c == b'\'' { in_single = false; }
            i += 1;
            continue;
        }
        if in_double {
            if c == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == b'"' { in_double = false; }
            i += 1;
            continue;
        }
        match c {
            b'\'' => { in_single = true; i += 1; }
            b'"' => { in_double = true; i += 1; }
            b'\\' if i + 1 < bytes.len() => { i += 2; }
            b'(' => { paren += 1; i += 1; }
            b')' => { paren -= 1; i += 1; }
            b'[' => { bracket += 1; i += 1; }
            b']' => { bracket -= 1; i += 1; }
            b'{' => { brace += 1; i += 1; }
            b'}' => { brace -= 1; i += 1; }
            b'#' => {
                // Line comment — skip to newline
                while i < bytes.len() && bytes[i] != b'\n' { i += 1; }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                {
                    i += 1;
                }
                // Only treat this as a keyword at a command-start boundary:
                // preceded by BOL / whitespace / `;` / `|` / `&` / `(` / `{`.
                let is_command_start = start == 0
                    || matches!(
                        bytes[start - 1],
                        b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b'(' | b'{'
                    );
                if !is_command_start { continue; }
                let word = &src[start..i];
                match word {
                    "if" => kw.push("fi"),
                    "while" | "until" | "for" | "select" | "repeat" => kw.push("done"),
                    "case" => kw.push("esac"),
                    // Intermediate markers — do/then/else/elif/in live
                    // inside an already-open construct; no stack change.
                    "do" | "then" | "else" | "elif" | "in" => {}
                    "fi" if kw.last().copied() == Some("fi") => { kw.pop(); }
                    "done" if kw.last().copied() == Some("done") => { kw.pop(); }
                    "esac" if kw.last().copied() == Some("esac") => { kw.pop(); }
                    _ => {}
                }
            }
            _ => { i += 1; }
        }
    }

    // Only unclosed openers (positive counts) imply incomplete input.
    // `case x in a) … esac` legitimately has more `)` than `(`, and `a}` /
    // `b]` alone aren't real user input at the prompt — so negative counts
    // shouldn't cause us to hang in continuation mode.
    !in_single
        && !in_double
        && paren <= 0
        && brace <= 0
        && bracket <= 0
        && kw.is_empty()
}

fn interactive(env: &mut frost_exec::ShellEnv) {
    // Ignore SIGINT in the shell process itself; reedline handles Ctrl-C
    // by aborting the current line buffer, not killing frost.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
    install_signal_traps();

    let history_path = frost_zle::default_history_path();
    let zle_base = match ZleEngine::new(&history_path, 10_000) {
        Ok(z) => z,
        Err(e) => {
            eprintln!("frost: ZLE init failed ({e}); falling back to in-memory history");
            ZleEngine::in_memory()
        }
    };
    let completer = Box::new(frost_complete::FrostCompleter::with_default_builtins());
    let mut zle = zle_base.with_completer(completer);
    // Separate in-process history for `!` expansion — reedline owns the
    // user-facing navigation buffer, frost-history owns the expansion
    // buffer. They read the same file so `!!` sees the same commands the
    // user could up-arrow to.
    let mut history = frost_history::History::from_file(&history_path)
        .unwrap_or_else(|_| frost_history::History::new());

    loop {
        // Drain and dispatch any signals delivered while we were
        // waiting / running. Fires `deftrap`-authored handlers.
        check_pending_traps(env);

        // `precmd` hook — runs before the next prompt is drawn. Authored
        // via `(defhook :event "precmd" :body …)` in the rc file.
        run_hook("__frost_hook_precmd", env);

        // Re-read PS1 / PS2 each iteration so variable changes mid-session
        // take effect on the next prompt, then run it through frost-prompt
        // for zsh-style % and (optionally) $ substitution.
        let ps1_raw = env
            .get_var("PS1")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "frost> ".to_string());
        let ps2_raw = env
            .get_var("PS2")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "> ".to_string());
        let pe = {
            let mut pe = frost_prompt::PromptEnv::snapshot(env.exit_status);
            // Surface common vars to $-substitution without shelling out.
            for name in ["USER", "HOME", "PWD", "HOST", "HOSTNAME", "SHELL", "STATUS"] {
                if let Some(v) = env.get_var(name) {
                    pe.extra_vars.insert(name.to_string(), v.to_string());
                }
            }
            pe
        };
        let prompt_subst = env.is_option_set(frost_options::ShellOption::PromptSubst);
        let ps1 = frost_prompt::render(&ps1_raw, &pe, prompt_subst);
        let ps2 = frost_prompt::render(&ps2_raw, &pe, prompt_subst);
        zle.set_prompt(ps1, ps2);

        // Honor `setopt vi` / `setopt emacs` on every iteration so
        // `bindkey -v` behavior changes mid-session.
        let wanted = if env.is_option_set(frost_options::ShellOption::Vi) {
            EditModeKind::Vi
        } else {
            EditModeKind::Emacs
        };
        zle.set_edit_mode(wanted);

        let outcome = zle.read_line(|src| {
            if is_complete(src) { InputStatus::Complete } else { InputStatus::Incomplete }
        });
        match outcome {
            Ok(ReadLineOutcome::Input(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }
                // `!`-expansion before parse. zsh's default is on
                // (`setopt BANG_HIST`); once we add a `NoBangHist` option to
                // frost-options, gate here. Until then, always expand.
                // zsh echoes the expanded line when it differs — so do we.
                let (to_run, expansion_failed) = match frost_history::expand(&line, &history) {
                    Ok((expanded, changed)) => {
                        if changed { println!("{expanded}"); }
                        (expanded, false)
                    }
                    Err(e) => {
                        eprintln!("frost: {e}");
                        (line.clone(), true)
                    }
                };
                if expansion_failed { continue; }
                let _ = history.push(to_run.clone());
                // `preexec` — after input is accepted, before execution.
                run_hook("__frost_hook_preexec", env);
                match run(&to_run, env) {
                    RunOutcome::Completed(_) => {}
                    RunOutcome::Exit(code) => {
                        run_exit_trap(env);
                        std::process::exit(code);
                    }
                }
            }
            Ok(ReadLineOutcome::Interrupted) => {
                // Match zsh: Ctrl-C just discards the current line.
                continue;
            }
            Ok(ReadLineOutcome::Eof) => break,
            Err(e) => {
                eprintln!("frost: read error: {e}");
                break;
            }
        }
    }
    // Fall-through exit (Ctrl-D / read error) also fires EXIT trap.
    run_exit_trap(env);
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let mut env = frost_exec::ShellEnv::new();

    // Tatara-Lisp rc file — declarative authoring surface for aliases,
    // options, env vars, and (future) prompt/hook/binding/completion.
    // Missing file is not an error; parse/apply errors print a warning
    // so frost still starts even if the rc has a bug.
    let rc_path = frost_lisp::default_rc_path();
    match frost_lisp::load_rc(&rc_path, &mut env) {
        Ok(summary) if summary == frost_lisp::ApplySummary::default() => { /* no rc, silent */ }
        Ok(summary) => {
            tracing::debug!(
                ?summary,
                rc = %rc_path.display(),
                "loaded frost-lisp rc file"
            );
        }
        Err(e) => {
            eprintln!("frost: warning: failed to load {}: {e}", rc_path.display());
        }
    }

    let code = if let Some(cmd) = &cli.command {
        unwrap_outcome(run(cmd, &mut env))
    } else if let Some(path) = &cli.file {
        match std::fs::read_to_string(path) {
            Ok(source) => unwrap_outcome(run(&source, &mut env)),
            Err(e) => {
                eprintln!("frost: {path}: {e}");
                1
            }
        }
    } else if std::io::stdin().is_terminal() {
        interactive(&mut env);
        0
    } else {
        // Non-interactive stdin (e.g., `frost < script.sh`) — slurp it.
        let mut buf = String::new();
        if std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).is_ok() {
            unwrap_outcome(run(&buf, &mut env))
        } else {
            1
        }
    };

    // EXIT trap fires for every graceful exit path, including `-c`,
    // script-file, and non-interactive stdin modes. matches zsh.
    run_exit_trap(&mut env);
    process::exit(code);
}

/// Map a `RunOutcome` to a raw exit code for non-interactive entry
/// points where both `exit` and "command finished normally" just collapse
/// to the same "what should the frost process return".
fn unwrap_outcome(outcome: RunOutcome) -> i32 {
    match outcome {
        RunOutcome::Completed(c) | RunOutcome::Exit(c) => c,
    }
}

/// Invoke a named shell function if present. Used for the rc-authored
/// lifecycle hooks (`precmd`, `preexec`, `chpwd`). Errors are swallowed
/// so a broken hook can't kill the interactive loop.
fn run_hook(name: &str, env: &mut frost_exec::ShellEnv) {
    if !env.functions.contains_key(name) { return; }
    // Synthesize a call: `<name>` with no args. Cheap to re-parse each
    // time; the function body itself is pre-parsed and cached in
    // `env.functions`.
    let _ = run(name, env);
}

/// Dispatch the `EXIT` pseudo-signal trap, authored via
/// `(deftrap :signal "EXIT" :body …)` in the rc file. Invoked right
/// before the shell terminates — for the interactive loop's graceful-
/// exit paths (Ctrl-D on empty prompt, `exit` builtin, read error).
/// `process::abort` and kill -9 do NOT run this, matching zsh.
fn run_exit_trap(env: &mut frost_exec::ShellEnv) {
    let name = "__frost_trap_EXIT";
    if env.functions.contains_key(name) {
        let _ = run(name, env);
    }
}

#[cfg(test)]
mod tests {
    use super::is_complete;

    #[test]
    fn simple_commands_are_complete() {
        assert!(is_complete("echo hi"));
        assert!(is_complete("ls | grep foo"));
        assert!(is_complete("a=1; b=2"));
    }

    #[test]
    fn trailing_backslash_is_incomplete() {
        assert!(!is_complete("echo hi \\"));
        assert!(!is_complete("ls \\"));
    }

    #[test]
    fn unclosed_quotes_are_incomplete() {
        assert!(!is_complete("echo 'hello"));
        assert!(!is_complete("echo \"world"));
    }

    #[test]
    fn unbalanced_brackets_are_incomplete() {
        assert!(!is_complete("echo (nested"));
        assert!(!is_complete("arr=(1 2 3"));
        assert!(!is_complete("f() {"));
    }

    #[test]
    fn if_requires_fi() {
        assert!(!is_complete("if true"));
        assert!(!is_complete("if true; then echo yes"));
        assert!(is_complete("if true; then echo yes; fi"));
    }

    #[test]
    fn while_requires_done() {
        assert!(!is_complete("while true; do echo loop"));
        assert!(is_complete("while true; do echo loop; done"));
    }

    #[test]
    fn case_requires_esac() {
        assert!(!is_complete("case $x in a) echo a ;;"));
        assert!(is_complete("case $x in a) echo a ;; esac"));
    }

    #[test]
    fn comments_do_not_affect_balance() {
        assert!(is_complete("echo hi # a ( b { c ["));
    }
}
