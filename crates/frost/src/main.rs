use std::io::IsTerminal;
use std::process;

use clap::Parser as ClapParser;

use frost_zle::{InputStatus, ReadLineOutcome, ZleEngine};

#[derive(ClapParser)]
#[command(name = "frost", version, about = "A zsh-compatible shell")]
struct Cli {
    /// Execute the given string as a command
    #[arg(short = 'c')]
    command: Option<String>,

    /// Script file to execute
    file: Option<String>,
}

fn run(input: &str, env: &mut frost_exec::ShellEnv) -> i32 {
    let tokens = tokenize(input);
    let mut parser = frost_parser::Parser::new(&tokens);
    let program = parser.parse();
    let mut executor = frost_exec::Executor::new(env);
    match executor.execute_program(&program) {
        Ok(status) => status,
        Err(e) => {
            eprintln!("frost: {e}");
            1
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

    let history_path = frost_zle::default_history_path();
    let mut zle = match ZleEngine::new(&history_path, 10_000) {
        Ok(z) => z,
        Err(e) => {
            eprintln!("frost: ZLE init failed ({e}); falling back to in-memory history");
            ZleEngine::in_memory()
        }
    };

    loop {
        let outcome = zle.read_line(|src| {
            if is_complete(src) { InputStatus::Complete } else { InputStatus::Incomplete }
        });
        match outcome {
            Ok(ReadLineOutcome::Input(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }
                run(&line, env);
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
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let mut env = frost_exec::ShellEnv::new();

    let code = if let Some(cmd) = &cli.command {
        run(cmd, &mut env)
    } else if let Some(path) = &cli.file {
        match std::fs::read_to_string(path) {
            Ok(source) => run(&source, &mut env),
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
            run(&buf, &mut env)
        } else {
            1
        }
    };

    process::exit(code);
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
