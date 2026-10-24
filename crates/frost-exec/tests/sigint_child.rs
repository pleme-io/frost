use frost_exec::{Executor, ShellEnv};

fn status_of(code: &str) -> i32 {
    let mut env = ShellEnv::new();
    let mut exec = Executor::new(&mut env);
    let tokens = frost_exec::tokenize(code);
    let program = frost_parser::Parser::new(&tokens).parse();
    exec.execute_program(&program).expect("the script runs")
}

#[test]
fn a_command_started_by_the_interactive_shell_dies_on_sigint() {
    let script = "/bin/sh -c 'kill -INT $$; exit 7'";
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
}
