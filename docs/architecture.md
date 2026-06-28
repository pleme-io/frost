# Frost Architecture

## Crate Dependency Graph

```
frost (binary)
  └── frost-exec
        ├── frost-parser
        │     └── frost-lexer
        ├── frost-expand
        │     └── frost-parser
        ├── frost-builtins
        ├── frost-options
        └── frost-glob (stub)

frost-zle (planned — line editor)
frost-complete (planned — completion)
frost-compat (test runner)
```

## Pipeline

```
Source Code
    │
    ▼
┌──────────┐
│  Lexer   │  frost-lexer: modal tokenizer
│          │  Produces Token stream with spans
└────┬─────┘
     │
     ▼
┌──────────┐
│  Parser  │  frost-parser: recursive descent
│          │  Produces Program → CompleteCommand → Pipeline → Command
└────┬─────┘
     │
     ▼
┌──────────┐
│ Executor │  frost-exec: walks AST
│          │  fork/exec, pipes, redirects, builtins, functions
└────┬─────┘
     │
     ├── Expansion (frost-expand)
     │   tilde → param → cmd sub → arith → brace → glob → quote removal
     │
     ├── Redirections (frost-exec/redirect)
     │   dup2, pipes, heredocs, herestrings
     │
     ├── Builtins (frost-builtins)
     │   53+ commands via BuiltinRegistry
     │   Returns BuiltinResult { status, action: BuiltinAction }
     │
     └── Shell Environment (frost-exec/env)
         Scope stack, variables, functions, aliases, options, traps, jobs
```

## Key Types

### AST (frost-parser)

```
Program
  └── CompleteCommand { list, is_async }
        └── List { first: Pipeline, rest: [(ListOp, Pipeline)] }
              └── Pipeline { bang, commands: [Command], pipe_stderr }
                    └── Command: Simple | If | For | While | Case | ...
                          └── SimpleCommand { assignments, words, redirects }
                                └── Word { parts: [WordPart] }
```

**WordPart variants:**
- `Literal`, `SingleQuoted`, `DoubleQuoted`
- `DollarVar`, `DollarBrace` (raw fallback), `ParamExp` (structured)
- `CommandSub`, `ArithSub`
- `Glob`, `Tilde`
- `BraceExp`, `ProcessSub`, `ExtGlob`

**ParamExpansion** (14 fields):
- `flags`, `length`, `is_set_test`, `name`, `nested`, `subscript`, `modifier`
- Modifier variants: Default, Assign, Alternative, Error, TrimPrefix, TrimSuffix, Substitute, Substring, Case

### Execution (frost-exec)

```
ShellEnv
  ├── scopes: Vec<Scope>          — variable scope stack (global at [0])
  ├── functions: HashMap           — shell functions (AST nodes)
  ├── aliases: HashMap             — alias table
  ├── options: Options             — 113 shell options
  ├── exit_status: i32             — $?
  ├── positional_params: Vec       — $1, $2, ...
  └── pid, ppid, start_time, random_state

Executor
  ├── env: &mut ShellEnv
  ├── builtins: BuiltinRegistry
  └── jobs: JobTable

TrapTable
  ├── traps: HashMap<i32, TrapAction>        — signal handlers
  └── pseudo_traps: HashMap<PseudoSignal, TrapAction>  — EXIT/DEBUG/ERR/ZERR
```

### Builtins (frost-builtins)

```
trait Builtin: Send + Sync {
    fn name(&self) -> &str;
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32;
    fn execute_with_action(&self, ...) -> BuiltinResult;  // new path
}

enum BuiltinAction {
    None, Eval(String), Source(String), Shift(usize),
    SetPositional(Vec), Let(String),
    DefineAlias(Vec), RemoveAlias(Vec),
    SetOptions(Vec), UnsetOptions(Vec), Exit(i32),
}
```

## Control Flow

Control flow (return, break, continue, exit) propagates via `ExecError::ControlFlow`:

```rust
enum ControlFlow {
    Return(i32),     // return N
    Break(u32),      // break N (levels)
    Continue(u32),   // continue N (levels)
    Exit(i32),       // exit N
}
```

Loops decrement the level and re-raise if > 1.

## Expansion Order

1. **Brace expansion** — `{a,b,c}`, `{1..10}` (runs on expanded strings in `expand_word_multi`)
2. **Tilde expansion** — `~` → `$HOME`
3. **Parameter expansion** — `$var`, `${var:-default}`, `${var[n]}`, etc.
4. **Command substitution** — `$(cmd)`
5. **Arithmetic expansion** — `$((expr))`
6. **Quote removal** — strip remaining quotes
7. *(Future: glob expansion)*

## Word splitting (zsh semantics — by design)

frost targets **zsh 5.9 parity**, so it follows zsh's variable rules, **not**
POSIX/bash. The load-bearing difference operators hit:

> **An unquoted scalar `$var` is a single word — it is NOT split on `$IFS`.**
> (`SH_WORD_SPLIT` is off by default, exactly as in zsh.)

```sh
K="kubectl --kubeconfig=/tmp/x.kube"
$K get nodes
# frost (and real zsh): "command not found: kubectl --kubeconfig=/tmp/x.kube"
#                       — the whole expansion is argv[0]
```

This is not a bug; bash/POSIX would split here, zsh does not. The classic
unquoted-`$var` footgun (filenames with spaces silently splitting into multiple
arguments) is unrepresentable by default. Verified byte-identical to
`/usr/bin/zsh 5.9` (`zsh --no-rcs -c '…'`).

### Running a command stored in a variable

Use one of zsh's explicit-split forms — pick by intent:

| Idiom | Example | When |
|-------|---------|------|
| **Array (preferred)** | `K=(kubectl --kubeconfig=/tmp/x.kube); $K get nodes` | the value is a real argv; no re-splitting of embedded spaces |
| **`${=var}` forced split** | `${=K} get nodes` | a scalar you want IFS-split this one time |
| **`eval`** | `eval "$K get nodes"` | the string is a full command line to re-parse |

Quoting always suppresses splitting: `"$K"` and `"${=K}"`… `"$K"` is one word;
`"${=K}"` still splits (that is the whole point of the `=` flag).

Implemented today: unquoted-array splitting, the `${=var}` flag (incl. inside
double quotes), and `eval`. Known zsh-parity gaps (tracked, **not** the default
footgun): the global `setopt shwordsplit` option is defined but not yet wired to
the unquoted-scalar path, and the bare `$=var` shorthand is not lexed (use the
brace form `${=var}`).
