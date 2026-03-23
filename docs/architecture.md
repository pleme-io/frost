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
7. *(Future: word splitting, glob expansion)*
