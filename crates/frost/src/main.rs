use std::io::{self, BufRead, Write};
use std::process;

use clap::Parser as ClapParser;

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

fn interactive(env: &mut frost_exec::ShellEnv) {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    // Ignore SIGINT in the shell process itself.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }

    loop {
        print!("frost> ");
        if stdout.flush().is_err() {
            break;
        }

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF (Ctrl-D)
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                run(trimmed, env);
            }
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
    } else {
        interactive(&mut env);
        0
    };

    process::exit(code);
}
