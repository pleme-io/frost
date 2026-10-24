use frost_exec::{Executor, ShellEnv};
use nix::sys::signal::{SigHandler, Signal, signal};

fn status_of(code: &str) -> i32 {
    let mut env = ShellEnv::new();
    let mut exec = Executor::new(&mut env);
    let tokens = frost_exec::tokenize(code);
    let program = frost_parser::Parser::new(&tokens).parse();
    exec.execute_program(&program).expect("the script runs")
}

#[test]
fn the_interactive_shell_ignores_sigint_and_the_commands_it_starts_do_not() {
    let script = "/bin/sh -c 'kill -INT $$; exit 7'";
    unsafe { signal(Signal::SIGINT, SigHandler::SigDfl) }.expect("SIGINT can be set");
    assert_eq!(
        status_of(script),
        130,
        "without the shell's own ignore, SIGINT kills the child"
    );
    frost_exec::sys::ignore_sigint_in_shell();
    assert_eq!(
        status_of(script),
        130,
        "the shell ignores SIGINT for itself, and its child still dies on it"
    );
    assert_eq!(
        status_of("exec /no/such/program"),
        127,
        "an exec that cannot run returns to the shell"
    );
    assert_eq!(
        unsafe { signal(Signal::SIGINT, SigHandler::SigIgn) }.expect("SIGINT can be read"),
        SigHandler::SigIgn,
        "a failed exec leaves the shell ignoring SIGINT, so ctrl-c does not end it"
    );
}
