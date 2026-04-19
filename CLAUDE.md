# Frost

Zsh-compatible shell written in Rust. Targets 5.9 behavioral parity via the vendored zsh test suite.

## Quick Start

```bash
cargo run --bin frost               # launch shell
cargo run --bin frost -- -c 'echo hi'  # run command
cargo test --workspace --lib        # 289 unit tests
cargo test --test ztst_bridge       # zsh compat tests (~33 pass, 70 total)
nix run .#ci                        # full CI: test + clippy + fmt
nix run .#compat                    # zsh compat suite
```

## Crate Map

| Crate | Purpose | Key types |
|-------|---------|-----------|
| `frost` | Binary entry point + ztst bridge tests | `main()` |
| `frost-lexer` | Tokenizer (modal, context-sensitive) | `Lexer`, `Token`, `TokenKind` |
| `frost-parser` | Recursive descent → AST | `Parser`, `Program`, `Command`, `Word`, `WordPart`, `ParamExpansion` |
| `frost-expand` | Word expansion pipeline (tilde, param, cmd sub, arith, brace, glob) | `ExpandEnv`, `ExpandValue`, `expand_word()`, `expand_braces()` |
| `frost-exec` | Execution engine: fork/exec, pipes, redirects, env, traps | `Executor`, `ShellEnv`, `TrapTable`, `JobTable` |
| `frost-builtins` | 53+ builtins (echo, cd, typeset, printf, trap, etc.) | `Builtin`, `BuiltinRegistry`, `BuiltinAction`, `ShellEnvironment` |
| `frost-options` | ~113 shell options (GLOB, EXTENDED_GLOB, ERR_EXIT, ...) | `Options`, `ShellOption` |
| `frost-glob` | Zsh-compatible glob matching + filesystem expansion (wired into executor) | `GlobOptions`, `match_pattern()`, `expand_pattern()` |
| `frost-zle` | Line editor (stub — will wrap reedline) | — |
| `frost-complete` | Completion engine (stub) | — |
| `frost-compat` | Zsh test suite runner | — |

## Architecture

```
Input → Lexer → Parser → AST → Executor
                                  ├── Expand (param, brace, glob, cmd sub)
                                  ├── Redirect (dup2, pipes, heredocs)
                                  ├── Builtins (53+ commands)
                                  ├── Fork/Exec (external commands)
                                  └── ShellEnv (scopes, vars, options, traps, jobs)
```

### Key Design Decisions

- **BuiltinAction enum** replaces magic `__FROST_*` variables. Builtins return `BuiltinResult { status, action }` via `execute_with_action()`. All new builtins must implement this.
- **ParamExpansion AST** — 14-field structured type (`flags`, `length`, `name`, `subscript`, `modifier`, etc.) matching mvdan/sh. The parser still produces `DollarBrace` (raw text) as fallback; `expand_dollar_brace_raw()` handles it.
- **Options** live in `ShellEnv.options` (typed `frost_options::Options`). setopt/unsetopt go through `BuiltinAction::SetOptions`/`UnsetOptions`.
- **1-indexed arrays**, no word splitting by default, dynamic scoping.
- **Control flow** via `ExecError::ControlFlow(Return|Break|Continue|Exit)`.

### Expansion Pipeline Order

```
brace expansion → tilde → parameter → command substitution →
arithmetic → word splitting → glob → quote removal
```

Brace expansion runs on expanded strings in `expand_word_multi()`.

## Test Strategy

- **Unit tests** (`cargo test --workspace --lib`): 289 passing. Each crate has inline tests.
- **ztst bridge** (`cargo test --test ztst_bridge`): Runs vendored zsh 5.9 test files. 33 passing out of 70 enabled.
- **Smoke test**: `cargo run --bin frost -- -c '<command>'`

## Current Status (289 unit tests, 53+ builtins)

### Working Features
- Lexer, parser, AST for all compound commands (if/for/while/until/case/select/cfor/repeat/try-always)
- Simple/compound command execution, pipelines, subshells, brace groups
- Parameter expansion: `$var`, `${var:-default}`, `${#var}`, `${var/pat/rep}`, `${var:offset:len}`, subscripts `${arr[n]}`
- Brace expansion: `{a,b,c}`, numeric/char ranges (when not at word start)
- Command substitution `$(cmd)`, arithmetic `$((expr))`, tilde expansion
- Array/associative array variables, subscript assignment `arr[n]=val`
- `$pipestatus` array, PIPE_FAIL option
- 53+ builtins including echo, cd, typeset, printf, eval, source, trap, setopt, pushd/popd
- Trap infrastructure (TrapTable, signal name/number translation)
- Shell options wired to behavior (setopt/unsetopt actually modify state)
- `[[ ]]` conditionals with file/string/integer/regex tests

### Not Yet Implemented
- Process substitution `<(cmd)` / `>(cmd)` execution
- Structured `${}` parser (uses raw-text fallback)
- Full alias expansion in executor
- ZLE / line editing / completion
- History expansion
- `emulate` (stub)
- Namerefs, MULTIOS

## Conventions

- Edition 2024, Rust 1.89.0+, MIT license
- `cargo clippy --workspace -- -D warnings` must pass
- All builtins: implement both `execute()` (legacy) and `execute_with_action()` (new path)
- Tests: add unit tests for new features; never regress existing count
