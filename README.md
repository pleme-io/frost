# Frost

A zsh-compatible shell written in Rust. Frost aims for behavioral parity with zsh 5.9, validated against the vendored zsh test suite.

## Status

**Alpha** — core execution works, expanding feature coverage toward full zsh compatibility.

- 289 unit tests passing
- 53+ built-in commands
- 113 shell options
- 33/70 zsh compatibility tests passing
- 11 crates in the workspace

## Features

- **Full compound command support** — if/elif/else/fi, for/while/until, case, select, C-style for `(( ))`, repeat, try-always
- **Parameter expansion** — `$var`, `${var:-default}`, `${var##pattern}`, `${var/old/new}`, `${var:offset:len}`, `${#var}`, array subscripts `${arr[n]}`
- **Brace expansion** — `{a,b,c}`, `{1..10}`, `{a..z}`, step values, zero-padding
- **Command substitution** — `$(command)`, arithmetic `$((expr))`
- **Pipelines** — `cmd1 | cmd2 |& cmd3`, `$pipestatus` array, PIPE_FAIL
- **Redirections** — `<`, `>`, `>>`, `&>`, `&>>`, `<>`, `<<<`, `<<`, fd duplication
- **Arrays** — indexed (1-based) and associative, subscript assignment `arr[n]=val`
- **Shell options** — 113 options with `setopt`/`unsetopt`, wired to behavior
- **Conditionals** — `[[ ]]` with file/string/integer/regex tests, `-o` option checks
- **53+ builtins** — echo, cd, typeset, printf, eval, source, trap, read, export, kill, getopts, pushd/popd, umask, and more
- **Trap infrastructure** — signal handlers, pseudo-signals (EXIT, DEBUG, ERR, ZERR)
- **Job control** — job table, `jobs`, `fg`, `bg`, `wait`, `disown` (stubs)

## Install

```bash
cargo install frost                   # from crates.io
nix run github:pleme-io/frost         # without installing
```

Prebuilt binaries for Linux and macOS (x86_64, aarch64) are attached to each
[GitHub release](https://github.com/pleme-io/frost/releases).

## Building

### With Cargo

```bash
cargo build --release
cargo test --workspace --lib          # unit tests
cargo test --test ztst_bridge         # zsh compatibility tests
```

### With Nix

```bash
nix build                             # build frost
nix run                               # launch frost shell
nix run .#test                        # cargo test
nix run .#clippy                      # cargo clippy
nix run .#fmt                         # cargo fmt --check
nix run .#ci                          # all checks
nix run .#compat                      # zsh compatibility suite
nix flake check                       # pure nix-sandbox checks
```

## Usage

```bash
# Interactive shell
frost

# Run a command
frost -c 'echo hello world'

# Run a script
frost script.zsh
```

## Architecture

Frost is organized as a Cargo workspace with 11 crates:

```
frost-lexer     → Token stream
frost-parser    → AST (Program, Command, Word, ParamExpansion, ...)
frost-expand    → Word expansion (tilde, param, brace, glob, cmd sub)
frost-exec      → Execution engine (fork/exec, pipes, env, traps, jobs)
frost-builtins  → 53+ built-in commands
frost-options   → 113 shell options
frost-glob      → Glob matching (planned)
frost-zle       → Line editor (planned)
frost-complete  → Tab completion (planned)
frost-compat    → Zsh test runner
frost            → Binary entry point
```

See [DECISIONS.md](DECISIONS.md) for detailed technical decisions.

## Zsh Compatibility

Frost validates against the vendored zsh 5.9 test suite (`Test/*.ztst`). Current progress:

| Category | Tests | Status |
|----------|-------|--------|
| Grammar (A01) | 107 | Partial |
| Redirects (A04) | 78 | Partial |
| Assignments (A06) | 103 | Partial |
| Arithmetic (C01) | 73 | ~22% |
| Conditionals (C02) | 59 | Partial |
| Parameters (D04) | 225 | Partial |
| Arrays (D05) | — | Partial |
| Subscripts (D06) | — | Partial |
| Glob (D02) | — | Stub |

## License

MIT
