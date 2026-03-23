# Frost Built-in Commands

53+ built-in commands organized by category.

## Core I/O

| Builtin | Description |
|---------|-------------|
| `echo` | Print arguments |
| `print` | Print with zsh extensions (`-n`, `-l`, `-r`) |
| `printf` | Formatted output (`%s`, `%d`, `%f`, `%b`, `%q`, width/precision, `-v var`) |
| `read` | Read input into variables |

## Navigation

| Builtin | Description |
|---------|-------------|
| `cd` | Change directory |
| `pushd` | Push directory onto stack and change |
| `popd` | Pop directory from stack and change |
| `dirs` | Print directory stack |

## Variables

| Builtin | Description |
|---------|-------------|
| `export` | Mark variables for export |
| `typeset` | Declare variables with type/scope attributes |
| `local` | Declare function-local variables |
| `declare` | Alias for typeset |
| `integer` | Declare integer variable (`typeset -i`) |
| `float` | Declare float variable (`typeset -F`) |
| `readonly` | Declare read-only variable |
| `unset` | Remove variables |
| `set` | Set positional parameters (`set -- args`) |

## Control Flow

| Builtin | Description |
|---------|-------------|
| `exit` | Exit the shell |
| `return` | Return from function |
| `break` | Break from loop (supports levels) |
| `continue` | Continue loop (supports levels) |
| `eval` | Evaluate arguments as shell code |
| `source` / `.` | Execute file in current shell |
| `let` | Arithmetic evaluation |
| `true` / `false` | Return 0 / 1 |
| `:` | No-op, return 0 |

## Command Lookup

| Builtin | Description |
|---------|-------------|
| `command` | Run command bypassing functions |
| `builtin` | Run builtin bypassing functions |
| `type` | Identify command type |
| `whence` | Identify command (zsh style) |
| `which` | Locate command |

## Aliases

| Builtin | Description |
|---------|-------------|
| `alias` | Define aliases |
| `unalias` | Remove aliases |

## Options

| Builtin | Description |
|---------|-------------|
| `setopt` | Enable shell options |
| `unsetopt` | Disable shell options |

## Tests

| Builtin | Description |
|---------|-------------|
| `test` / `[` | POSIX test conditions |

## Signals / Processes

| Builtin | Description |
|---------|-------------|
| `trap` | Register signal handlers |
| `kill` | Send signals to processes |

## Job Control

| Builtin | Description |
|---------|-------------|
| `jobs` | List jobs |
| `fg` | Foreground a job (stub) |
| `bg` | Background a job (stub) |
| `wait` | Wait for background jobs |
| `disown` | Remove job from table |

## System

| Builtin | Description |
|---------|-------------|
| `umask` | Set/display file creation mask |
| `hash` | Manage command hash table |
| `getopts` | Parse positional parameters |
| `shift` | Shift positional parameters |

## Module / Compatibility Stubs

| Builtin | Description |
|---------|-------------|
| `autoload` | Autoload functions (stub) |
| `zmodload` | Load zsh modules (stub) |
| `functions` | List functions (stub) |
| `emulate` | Shell emulation mode (stub) |
| `fc` | History editing (stub) |
| `noglob` | Run without globbing (stub) |
| `disable` / `enable` | Disable/enable builtins (stub) |
| `compdef` / `compctl` | Completion definition (stub) |
| `zle` | ZLE widget manipulation (stub) |
| `bindkey` | Key bindings (stub) |
| `zstyle` | Style configuration (stub) |
