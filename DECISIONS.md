# Frost Technical Decisions

Locked-in architecture and library choices for zsh-compatible shell implementation.

## Library Selections

| Concern | Crate | Version | Rationale |
|---------|-------|---------|-----------|
| Process/job control | `nix` | 0.29+ | Already using. Has fork/exec/waitpid/setpgid/tcsetpgrp. rustix lacks fork/exec. |
| Small strings | `compact_str` | 0.8+ | Already using. 24-byte inline, mutable. smol_str is immutable (disqualified). |
| Ordered maps | `indexmap` | 2.x | Already using. Shell env is insertion-ordered, not sorted. |
| Glob matching | `globset` | 0.4 | BurntSushi/ripgrep ecosystem. Pattern matcher, not filesystem walker. Extend in frost-glob for zsh qualifiers. |
| Brace expansion | `bracoxide` | 0.1.8 | Used by nushell. Handles {a,b}, {1..10}, nesting. |
| Line editor | `reedline` | 0.46+ | Full vi/emacs, crossterm backend, completion/highlight traits. Build ZLE widgets on top. |
| Terminal | `crossterm` | 0.29 | Comes with reedline. Use nix::termios for job control terminal ops. |
| Regex (=~) | `fancy-regex` | 0.17 | Unifies POSIX ERE + PCRE backreferences. Matches zsh's dual-mode =~. |
| Printf | `printf-compat` | 0.3 | Extend for shell-specific %b, %q, argument recycling. |
| Arithmetic | Hand-rolled Pratt | N/A | Shell arithmetic is too specific (variable deref, side-effect ++/--, C-like wrapping). ~200 lines. |

## Zsh Compatibility Semantics

### Variables
- **No word splitting by default** (SH_WORD_SPLIT=off). Unquoted `$var` = single word.
- **1-indexed arrays.** `arr[0]` = empty, `arr[1]` = first element, `arr[-1]` = last.
- **Dynamic scoping** for functions (not lexical). `local`/`typeset` in functions = function-scoped.
- **Unset vars = empty string** (unless NOUNSET).
- **Integer overflow: C `i64` wrapping.** `2**63` wraps to MIN_INT. Division by zero = error.

### ShellVar Type System
```
enum ShellValue {
    Scalar(String),
    Integer(i64),
    Float(f64),
    Array(Vec<String>),       // 1-indexed
    Associative(IndexMap<String, String>),
}
```

### Parameter Expansion Order
1. Parse flags `(L)(U)(C)(s)(j)(o)(P)...`
2. Resolve nested `${...}` inside-out
3. Lookup parameter name
4. Apply `(P)` indirection
5. Apply subscripts `[n]` or `[n,m]`
6. Compute `#` length if `${#name}`
7. Pattern ops: `##`, `#`, `%%`, `%`, `/old/new`
8. Default ops: `:-`, `:=`, `:+`, `:?`
9. Case flags: `(U)`, `(L)`, `(C)`
10. Split/join: `(s)`, `(j)`, `(f)`, `(F)`
11. Sort: `(o)`, `(O)`, `(n)`, `(i)`
12. Unique: `(u)`
13. Quote: `(q)`, `(Q)`
14. Re-evaluate: `(e)`

### Expansion Pipeline
```
history expansion → alias expansion → brace expansion →
tilde expansion → parameter expansion → command substitution →
arithmetic expansion → word splitting → glob expansion →
quote removal
```

### `[[ ]]` vs `[ ]`
- `[[ ]]`: no word splitting, no glob, `==` is pattern match, `=~` is regex, `&&`/`||` native
- `[ ]`: word splitting, glob, POSIX test semantics

### Exit Code Semantics
- Pipeline: `$?` = last command. PIPE_FAIL: rightmost failure.
- `$pipestatus` array: per-command exit codes (1-indexed).
- `if false; then ...; fi` (no else) → exit 0.
- `x=$(exit 5)` → `$?=5`.

### Signal/Trap Rules
- Subshells: traps with actions RESET. Ignored traps inherited.
- `TRAPINT()` function takes precedence over `trap cmd INT`.
- LOCAL_TRAPS: restore traps on function return.
- ERR_EXIT: does NOT exit in if/while conditions or &&/|| chains.

### Options
- ~185 total options. Frost needs at minimum the ~40 most common.
- `emulate sh` flips ~88 options (SH_WORD_SPLIT, KSH_ARRAYS, SH_GLOB, etc.)
- `emulate -L sh` makes changes function-local via LOCAL_OPTIONS.

## Architecture

### Control Flow Signaling
```rust
enum ControlFlow {
    Return(i32),
    Break(u32),      // levels to break
    Continue(u32),   // levels to continue
    Exit(i32),       // exit the shell
}
```
`ExecResult = Result<i32, ExecError>` becomes `Result<ExecOutcome, ExecError>` where
`ExecOutcome` can carry both the status and an optional `ControlFlow`.

### Scope Stack
```rust
struct ShellEnv {
    scopes: Vec<Scope>,     // stack: global at [0], function scopes pushed/popped
    // ... rest unchanged
}
struct Scope {
    variables: IndexMap<String, ShellVar>,
}
```
Lookup walks up the stack. `typeset -g` writes directly to `scopes[0]`.

### Expansion Return Type
`expand_word()` returns `Vec<String>` (not `String`):
- Parameter expansion of arrays → multiple words
- Word splitting → multiple words
- Glob expansion → multiple words (one per match)

### Crate Responsibilities
- `frost-lexer`: tokenization (done)
- `frost-parser`: AST construction (done, extend for [[ ]])
- `frost-expand`: ALL expansion logic (extract from executor)
- `frost-glob`: glob matching + qualifiers (use globset internally)
- `frost-exec`: execution engine, scope management, job control
- `frost-builtins`: builtin commands (grow from 6 to ~40)
- `frost-options`: option registry (~185 options)
- `frost-zle`: line editor (reedline wrapper + ZLE widget system)
- `frost-complete`: completion engine (built on reedline Completer trait)
- `frost-compat`: zsh test runner (done)
