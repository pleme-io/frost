//! Miscellaneous builtins: command, builtin, type/whence, shift, colon,
//! alias/unalias, typeset/local/declare/integer/float/readonly.

use crate::{Builtin, BuiltinAction, BuiltinResult, ShellEnvironment};

/// : (colon) — do nothing, return 0.
pub struct Colon;
impl Builtin for Colon {
    fn name(&self) -> &str {
        ":"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// shift — shift positional parameters.
pub struct Shift;
impl Builtin for Shift {
    fn name(&self) -> &str {
        "shift"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let n: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
        // Signal to executor via special var
        env.set_var("__FROST_SHIFT", &n.to_string());
        0
    }
    fn execute_with_action(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> BuiltinResult {
        let n: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
        BuiltinResult::with_action(0, BuiltinAction::Shift(n))
    }
}

/// type/whence — identify commands.
pub struct Type;
impl Builtin for Type {
    fn name(&self) -> &str {
        "type"
    }
    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        for arg in args {
            // Search PATH
            if let Ok(path_var) = std::env::var("PATH") {
                let found = path_var
                    .split(':')
                    .any(|dir| std::path::Path::new(dir).join(arg).is_file());
                if found {
                    println!("{arg} is an external command");
                    continue;
                }
            }
            println!("{arg} not found");
            return 1;
        }
        0
    }
}

/// whence — alias for type with different output format.
pub struct Whence;
impl Builtin for Whence {
    fn name(&self) -> &str {
        "whence"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        Type.execute(args, env)
    }
}

/// command — run command bypassing shell functions.
pub struct CommandBuiltin;
impl Builtin for CommandBuiltin {
    fn name(&self) -> &str {
        "command"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            return 0;
        }
        // Signal to executor to skip function lookup
        env.set_var("__FROST_COMMAND_BYPASS", args[0]);
        0
    }
}

/// builtin — run builtin bypassing functions.
pub struct BuiltinCmd;
impl Builtin for BuiltinCmd {
    fn name(&self) -> &str {
        "builtin"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// alias — define or list aliases.
pub struct Alias;
impl Builtin for Alias {
    fn name(&self) -> &str {
        "alias"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        for arg in args {
            if let Some((name, value)) = arg.split_once('=') {
                env.set_var(&format!("__FROST_ALIAS_{name}"), value);
            }
        }
        0
    }
    fn execute_with_action(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> BuiltinResult {
        let mut aliases = Vec::new();
        for arg in args {
            if let Some((name, value)) = arg.split_once('=') {
                aliases.push((name.to_string(), value.to_string()));
            }
        }
        if aliases.is_empty() {
            BuiltinResult::ok()
        } else {
            BuiltinResult::with_action(0, BuiltinAction::DefineAlias(aliases))
        }
    }
}

/// unalias — remove aliases.
pub struct Unalias;
impl Builtin for Unalias {
    fn name(&self) -> &str {
        "unalias"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        for arg in args {
            env.unset_var(&format!("__FROST_ALIAS_{arg}"));
        }
        0
    }
    fn execute_with_action(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> BuiltinResult {
        let names: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        if names.is_empty() {
            BuiltinResult::ok()
        } else {
            BuiltinResult::with_action(0, BuiltinAction::RemoveAlias(names))
        }
    }
}

// ── typeset / local / declare ────────────────────────────────────────

/// Parsed flags from `typeset [-aAfFgirluxU] [+...] [name[=value] ...]`.
#[derive(Debug, Default)]
struct TypesetFlags {
    global: bool,
    integer: bool,
    float: bool,
    array: bool,
    associative: bool,
    readonly: bool,
    export: bool,
    lower: bool,
    upper: bool,
    print: bool,
}

/// Core implementation shared by typeset/local/declare.
fn execute_typeset(args: &[&str], env: &mut dyn ShellEnvironment, force_local: bool) -> i32 {
    let mut flags = TypesetFlags::default();
    let mut names: Vec<&str> = Vec::new();

    // Parse flags and arguments
    for arg in args {
        if (arg.starts_with('-') || arg.starts_with('+')) && arg.len() > 1 && !arg.contains('=') {
            let unset = arg.starts_with('+');
            for ch in arg[1..].chars() {
                match ch {
                    'g' => flags.global = !unset,
                    'i' => flags.integer = !unset,
                    'F' => flags.float = !unset,
                    'a' => flags.array = !unset,
                    'A' => flags.associative = !unset,
                    'r' => flags.readonly = !unset,
                    'x' => flags.export = !unset,
                    'l' => flags.lower = !unset,
                    'u' | 'U' => flags.upper = !unset,
                    'p' => flags.print = !unset,
                    _ => {} // ignore unknown flags
                }
            }
        } else {
            names.push(arg);
        }
    }

    // If force_local (from `local`/`declare`), never treat as global
    // unless -g was explicitly passed.
    let use_global = flags.global && !force_local;

    if names.is_empty() && flags.print {
        // typeset -p: list variables (stub)
        return 0;
    }

    for arg in &names {
        let (name, value) = if let Some((n, v)) = arg.split_once('=') {
            (n, Some(v))
        } else {
            (*arg, None)
        };

        // Create/set the variable in the appropriate scope
        if let Some(v) = value {
            if use_global {
                env.set_global_var(name, v);
            } else {
                env.declare_var(name, v);
            }
        } else {
            // typeset name (no value): declare with empty/existing value
            if use_global {
                if env.get_var(name).is_none() {
                    env.set_global_var(name, "");
                }
            } else if env.get_var(name).is_none() {
                env.declare_var(name, "");
            }
        }

        // Apply type flags
        if flags.integer {
            env.set_var_integer(name);
        }
        if flags.float {
            env.set_var_float(name);
        }
        if flags.array {
            env.set_var_array(name);
        }
        if flags.associative {
            env.set_var_associative(name);
        }

        // Apply attribute flags
        if flags.readonly {
            env.set_var_readonly(name);
        }
        if flags.export {
            env.export_var(name);
        }
    }

    0
}

/// typeset — declare variables with type/scope attributes.
pub struct Typeset;
impl Builtin for Typeset {
    fn name(&self) -> &str {
        "typeset"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        execute_typeset(args, env, false)
    }
}

/// local — declare function-local variables (alias for `typeset`).
pub struct Local;
impl Builtin for Local {
    fn name(&self) -> &str {
        "local"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        execute_typeset(args, env, true)
    }
}

/// declare — declare variables (alias for `typeset`).
pub struct Declare;
impl Builtin for Declare {
    fn name(&self) -> &str {
        "declare"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        execute_typeset(args, env, false)
    }
}

/// integer — declare integer variable (`typeset -i`).
pub struct Integer;
impl Builtin for Integer {
    fn name(&self) -> &str {
        "integer"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        // Prepend -i to the args
        let mut new_args = vec!["-i"];
        new_args.extend_from_slice(args);
        execute_typeset(&new_args, env, false)
    }
}

/// float — declare float variable (`typeset -F`).
pub struct Float;
impl Builtin for Float {
    fn name(&self) -> &str {
        "float"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let mut new_args = vec!["-F"];
        new_args.extend_from_slice(args);
        execute_typeset(&new_args, env, false)
    }
}

/// readonly — declare read-only variable (`typeset -r`).
pub struct Readonly;
impl Builtin for Readonly {
    fn name(&self) -> &str {
        "readonly"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let mut new_args = vec!["-r"];
        new_args.extend_from_slice(args);
        execute_typeset(&new_args, env, false)
    }
}

// ── setopt / unsetopt ────────────────────────────────────────────────

/// setopt — enable shell options.
pub struct Setopt;
impl Builtin for Setopt {
    fn name(&self) -> &str {
        "setopt"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
    fn execute_with_action(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> BuiltinResult {
        let opts: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        BuiltinResult::with_action(0, BuiltinAction::SetOptions(opts))
    }
}

/// unsetopt — disable shell options.
pub struct Unsetopt;
impl Builtin for Unsetopt {
    fn name(&self) -> &str {
        "unsetopt"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
    fn execute_with_action(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> BuiltinResult {
        let opts: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        BuiltinResult::with_action(0, BuiltinAction::UnsetOptions(opts))
    }
}

// ── Module/autoload stubs ────────────────────────────────────────────

/// autoload — stub returning 0.
pub struct Autoload;
impl Builtin for Autoload {
    fn name(&self) -> &str {
        "autoload"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// zmodload — stub returning 0.
pub struct Zmodload;
impl Builtin for Zmodload {
    fn name(&self) -> &str {
        "zmodload"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// functions — stub returning 0.
pub struct Functions;
impl Builtin for Functions {
    fn name(&self) -> &str {
        "functions"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// let — arithmetic evaluation builtin.
pub struct Let;
impl Builtin for Let {
    fn name(&self) -> &str {
        "let"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        // Signal to executor to evaluate arithmetic
        let expr = args.join(" ");
        env.set_var("__FROST_LET_EXPR", &expr);
        212 // Special signal code for let
    }
    fn execute_with_action(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> BuiltinResult {
        let expr = args.join(" ");
        BuiltinResult::with_action(0, BuiltinAction::Let(expr))
    }
}

/// printf — formatted output.
///
/// Supports format specifiers: `%s`, `%d`/`%i`, `%o`, `%x`/`%X`, `%f`, `%e`,
/// `%g`, `%c`, `%b` (backslash escapes), `%%`.  Width, precision, left-align,
/// and zero-pad modifiers are handled (`%10s`, `%-10s`, `%010d`, `%10.5f`).
/// Argument recycling repeats the format when extra arguments remain.
/// `-v var` assigns output to a variable instead of printing.
pub struct Printf;

impl Builtin for Printf {
    fn name(&self) -> &str {
        "printf"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            eprintln!("printf: usage: printf [-v var] format [arguments]");
            return 1;
        }

        // Check for -v var
        let (var_name, format, params) = if args.first() == Some(&"-v") {
            if args.len() < 3 {
                eprintln!("printf: -v: requires a variable name and format");
                return 1;
            }
            (Some(args[1]), args[2], &args[3..])
        } else {
            (None, args[0], &args[1..])
        };

        let mut full_output = String::new();
        let mut param_idx = 0;

        // Argument recycling: repeat format until all args consumed.
        // Always run at least once (even with no params).
        loop {
            let start_param_idx = param_idx;
            let segment = printf_format(format, params, &mut param_idx);
            full_output.push_str(&segment);

            // Stop if we've consumed all params or made no progress
            if param_idx >= params.len() || param_idx == start_param_idx {
                break;
            }
        }

        if let Some(name) = var_name {
            env.set_var(name, &full_output);
        } else {
            print!("{full_output}");
        }

        0
    }
}

/// Process a printf format string with the given arguments starting at `param_idx`.
/// Updates `param_idx` as arguments are consumed.
fn printf_format(format: &str, params: &[&str], param_idx: &mut usize) -> String {
    let mut output = String::new();
    let mut chars = format.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            // Parse format specifier: %[flags][width][.precision]type
            if chars.peek().is_none() {
                output.push('%');
                break;
            }

            // Check for %%
            if chars.peek() == Some(&'%') {
                chars.next();
                output.push('%');
                continue;
            }

            // Parse flags
            let mut left_align = false;
            let mut zero_pad = false;
            let mut plus_sign = false;
            let mut space_sign = false;
            let mut hash_flag = false;

            loop {
                match chars.peek() {
                    Some('-') => {
                        left_align = true;
                        chars.next();
                    }
                    Some('0') => {
                        zero_pad = true;
                        chars.next();
                    }
                    Some('+') => {
                        plus_sign = true;
                        chars.next();
                    }
                    Some(' ') => {
                        space_sign = true;
                        chars.next();
                    }
                    Some('#') => {
                        hash_flag = true;
                        chars.next();
                    }
                    _ => break,
                }
            }

            // Parse width (may be '*' to consume next arg)
            let width: Option<usize> = if chars.peek() == Some(&'*') {
                chars.next();
                let w = params
                    .get(*param_idx)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                *param_idx += 1;
                Some(w)
            } else {
                parse_number(&mut chars)
            };

            // Parse precision
            let precision: Option<usize> = if chars.peek() == Some(&'.') {
                chars.next();
                if chars.peek() == Some(&'*') {
                    chars.next();
                    let p = params
                        .get(*param_idx)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    *param_idx += 1;
                    Some(p)
                } else {
                    Some(parse_number(&mut chars).unwrap_or(0))
                }
            } else {
                None
            };

            // Parse conversion specifier
            let spec = match chars.next() {
                Some(ch) => ch,
                None => {
                    output.push('%');
                    break;
                }
            };

            let arg = params.get(*param_idx).copied().unwrap_or("");

            let formatted = match spec {
                's' => {
                    *param_idx += 1;
                    let mut s = arg.to_owned();
                    if let Some(prec) = precision {
                        if s.len() > prec {
                            s.truncate(prec);
                        }
                    }
                    apply_width(&s, width, left_align, ' ')
                }
                'b' => {
                    // %b: interpret backslash escapes in the argument
                    *param_idx += 1;
                    let expanded = printf_expand_escapes(arg);
                    apply_width(&expanded, width, left_align, ' ')
                }
                'd' | 'i' => {
                    *param_idx += 1;
                    let n = parse_integer(arg);
                    let s = format_signed(n, plus_sign, space_sign);
                    let pad = if zero_pad && !left_align { '0' } else { ' ' };
                    apply_width(&s, width, left_align, pad)
                }
                'o' => {
                    *param_idx += 1;
                    let n = parse_integer(arg);
                    let abs = (n as u64) & 0xFFFF_FFFF_FFFF_FFFF;
                    let s = if hash_flag && n != 0 {
                        format!("0{abs:o}")
                    } else {
                        format!("{abs:o}")
                    };
                    let pad = if zero_pad && !left_align { '0' } else { ' ' };
                    apply_width(&s, width, left_align, pad)
                }
                'x' => {
                    *param_idx += 1;
                    let n = parse_integer(arg);
                    let abs = (n as u64) & 0xFFFF_FFFF_FFFF_FFFF;
                    let s = if hash_flag && n != 0 {
                        format!("0x{abs:x}")
                    } else {
                        format!("{abs:x}")
                    };
                    let pad = if zero_pad && !left_align { '0' } else { ' ' };
                    apply_width(&s, width, left_align, pad)
                }
                'X' => {
                    *param_idx += 1;
                    let n = parse_integer(arg);
                    let abs = (n as u64) & 0xFFFF_FFFF_FFFF_FFFF;
                    let s = if hash_flag && n != 0 {
                        format!("0X{abs:X}")
                    } else {
                        format!("{abs:X}")
                    };
                    let pad = if zero_pad && !left_align { '0' } else { ' ' };
                    apply_width(&s, width, left_align, pad)
                }
                'f' => {
                    *param_idx += 1;
                    let n: f64 = arg.parse().unwrap_or(0.0);
                    let prec = precision.unwrap_or(6);
                    let s = format_float_f(n, prec, plus_sign, space_sign);
                    let pad = if zero_pad && !left_align { '0' } else { ' ' };
                    apply_width(&s, width, left_align, pad)
                }
                'e' => {
                    *param_idx += 1;
                    let n: f64 = arg.parse().unwrap_or(0.0);
                    let prec = precision.unwrap_or(6);
                    let s = format_float_e(n, prec, false, plus_sign, space_sign);
                    let pad = if zero_pad && !left_align { '0' } else { ' ' };
                    apply_width(&s, width, left_align, pad)
                }
                'E' => {
                    *param_idx += 1;
                    let n: f64 = arg.parse().unwrap_or(0.0);
                    let prec = precision.unwrap_or(6);
                    let s = format_float_e(n, prec, true, plus_sign, space_sign);
                    let pad = if zero_pad && !left_align { '0' } else { ' ' };
                    apply_width(&s, width, left_align, pad)
                }
                'g' => {
                    *param_idx += 1;
                    let n: f64 = arg.parse().unwrap_or(0.0);
                    let prec = precision.unwrap_or(6);
                    let s = format_float_g(n, prec, false, plus_sign, space_sign);
                    let pad = if zero_pad && !left_align { '0' } else { ' ' };
                    apply_width(&s, width, left_align, pad)
                }
                'G' => {
                    *param_idx += 1;
                    let n: f64 = arg.parse().unwrap_or(0.0);
                    let prec = precision.unwrap_or(6);
                    let s = format_float_g(n, prec, true, plus_sign, space_sign);
                    let pad = if zero_pad && !left_align { '0' } else { ' ' };
                    apply_width(&s, width, left_align, pad)
                }
                'c' => {
                    *param_idx += 1;
                    let ch = arg.chars().next().unwrap_or('\0');
                    let s = ch.to_string();
                    apply_width(&s, width, left_align, ' ')
                }
                _ => {
                    // Unknown specifier — output literally
                    let mut s = String::from('%');
                    s.push(spec);
                    s
                }
            };

            output.push_str(&formatted);
        } else if c == '\\' {
            // Backslash escape sequences in the format string
            match chars.next() {
                Some('n') => output.push('\n'),
                Some('t') => output.push('\t'),
                Some('r') => output.push('\r'),
                Some('a') => output.push('\x07'),
                Some('b') => output.push('\x08'),
                Some('f') => output.push('\x0C'),
                Some('v') => output.push('\x0B'),
                Some('\\') => output.push('\\'),
                Some('0') => {
                    // Octal: up to 3 digits
                    let mut val: u8 = 0;
                    for _ in 0..3 {
                        if let Some(&d) = chars.peek() {
                            if ('0'..='7').contains(&d) {
                                val = val * 8 + (d as u8 - b'0');
                                chars.next();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    output.push(val as char);
                }
                Some('x') => {
                    // Hex: up to 2 digits
                    let mut val: u8 = 0;
                    let mut count = 0;
                    while count < 2 {
                        if let Some(&d) = chars.peek() {
                            if d.is_ascii_hexdigit() {
                                let digit = match d {
                                    '0'..='9' => d as u8 - b'0',
                                    'a'..='f' => d as u8 - b'a' + 10,
                                    'A'..='F' => d as u8 - b'A' + 10,
                                    _ => unreachable!(),
                                };
                                val = val * 16 + digit;
                                chars.next();
                                count += 1;
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    output.push(val as char);
                }
                Some(other) => {
                    output.push('\\');
                    output.push(other);
                }
                None => output.push('\\'),
            }
        } else {
            output.push(c);
        }
    }

    output
}

/// Parse consecutive decimal digits from a char iterator into a number.
fn parse_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<usize> {
    let mut num = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            num.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if num.is_empty() {
        None
    } else {
        num.parse().ok()
    }
}

/// Parse a string argument as an integer.
///
/// Handles decimal, octal (0-prefix), hex (0x-prefix), and character constants
/// ('c' or "c").
fn parse_integer(s: &str) -> i64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }

    // Character constant: 'c' or "c"
    if (s.starts_with('\'') || s.starts_with('"')) && s.len() >= 2 {
        return s.chars().nth(1).map(|c| c as i64).unwrap_or(0);
    }

    // Handle sign
    let (negative, s) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        (false, rest)
    } else {
        (false, s)
    };

    let val = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).unwrap_or(0)
    } else if let Some(oct) = s.strip_prefix('0') {
        if oct.is_empty() {
            0
        } else {
            i64::from_str_radix(oct, 8).unwrap_or(0)
        }
    } else {
        s.parse::<i64>().unwrap_or(0)
    };

    if negative { -val } else { val }
}

/// Format a signed integer with optional sign prefix.
fn format_signed(n: i64, plus_sign: bool, space_sign: bool) -> String {
    if n < 0 {
        format!("{n}")
    } else if plus_sign {
        format!("+{n}")
    } else if space_sign {
        format!(" {n}")
    } else {
        format!("{n}")
    }
}

/// Apply width and alignment to a formatted string.
fn apply_width(s: &str, width: Option<usize>, left_align: bool, pad: char) -> String {
    match width {
        Some(w) if w > s.len() => {
            let padding = w - s.len();
            if left_align {
                format!("{s}{}", " ".repeat(padding))
            } else if pad == '0' && (s.starts_with('-') || s.starts_with('+') || s.starts_with(' '))
            {
                // For zero-padding with sign, put sign before zeros
                let (sign, rest) = s.split_at(1);
                format!("{sign}{}{rest}", "0".repeat(padding))
            } else {
                let pad_str: String = std::iter::repeat(pad).take(padding).collect();
                format!("{pad_str}{s}")
            }
        }
        _ => s.to_owned(),
    }
}

/// Format a float in %f style.
fn format_float_f(n: f64, precision: usize, plus_sign: bool, space_sign: bool) -> String {
    let s = format!("{n:.prec$}", prec = precision);
    if n >= 0.0 && !n.is_nan() {
        if plus_sign {
            format!("+{s}")
        } else if space_sign {
            format!(" {s}")
        } else {
            s
        }
    } else {
        s
    }
}

/// Format a float in %e / %E style.
fn format_float_e(
    n: f64,
    precision: usize,
    upper: bool,
    plus_sign: bool,
    space_sign: bool,
) -> String {
    let s = if upper {
        format!("{n:.prec$E}", prec = precision)
    } else {
        format!("{n:.prec$e}", prec = precision)
    };
    if n >= 0.0 && !n.is_nan() {
        if plus_sign {
            format!("+{s}")
        } else if space_sign {
            format!(" {s}")
        } else {
            s
        }
    } else {
        s
    }
}

/// Format a float in %g / %G style (shorter of %f and %e).
fn format_float_g(
    n: f64,
    precision: usize,
    upper: bool,
    plus_sign: bool,
    space_sign: bool,
) -> String {
    let prec = if precision == 0 { 1 } else { precision };

    // %g uses %e if exponent < -4 or >= precision, else %f
    let abs = n.abs();
    let s = if abs == 0.0 {
        format!("{n:.0}")
    } else {
        let exp = abs.log10().floor() as i32;
        if exp < -4 || exp >= prec as i32 {
            let e_str = if upper {
                format!("{n:.prec$E}", prec = prec.saturating_sub(1))
            } else {
                format!("{n:.prec$e}", prec = prec.saturating_sub(1))
            };
            e_str
        } else {
            // Number of digits after decimal = precision - (exponent + 1)
            let digits_after = (prec as i32 - exp - 1).max(0) as usize;
            format!("{n:.prec$}", prec = digits_after)
        }
    };

    // Trim trailing zeros after decimal point (standard %g behavior)
    let s = trim_trailing_zeros_g(&s);

    if n >= 0.0 && !n.is_nan() {
        if plus_sign {
            format!("+{s}")
        } else if space_sign {
            format!(" {s}")
        } else {
            s
        }
    } else {
        s
    }
}

/// Trim trailing zeros from %g output, preserving the exponent part.
fn trim_trailing_zeros_g(s: &str) -> String {
    // Split off exponent part if present
    let (mantissa, exp_part) = if let Some(pos) = s.find('e').or_else(|| s.find('E')) {
        (&s[..pos], &s[pos..])
    } else {
        (s, "")
    };

    if mantissa.contains('.') {
        let trimmed = mantissa.trim_end_matches('0');
        let trimmed = trimmed.trim_end_matches('.');
        format!("{trimmed}{exp_part}")
    } else {
        s.to_owned()
    }
}

/// Expand backslash escapes in a %b argument.
fn printf_expand_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('a') => out.push('\x07'),
                Some('b') => out.push('\x08'),
                Some('f') => out.push('\x0C'),
                Some('v') => out.push('\x0B'),
                Some('\\') => out.push('\\'),
                Some('0') => {
                    let mut val: u8 = 0;
                    for _ in 0..3 {
                        if let Some(&d) = chars.peek() {
                            if ('0'..='7').contains(&d) {
                                val = val * 8 + (d as u8 - b'0');
                                chars.next();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    out.push(val as char);
                }
                Some('x') => {
                    let mut val: u8 = 0;
                    let mut count = 0;
                    while count < 2 {
                        if let Some(&d) = chars.peek() {
                            if d.is_ascii_hexdigit() {
                                let digit = match d {
                                    '0'..='9' => d as u8 - b'0',
                                    'a'..='f' => d as u8 - b'a' + 10,
                                    'A'..='F' => d as u8 - b'A' + 10,
                                    _ => unreachable!(),
                                };
                                val = val * 16 + digit;
                                chars.next();
                                count += 1;
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    out.push(val as char);
                }
                Some('c') => {
                    // \c stops output
                    break;
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }

    out
}

#[cfg(test)]
mod printf_tests {
    use super::*;

    fn run_printf(fmt: &str, args: &[&str]) -> String {
        let mut idx = 0;
        let mut output = String::new();
        loop {
            let start = idx;
            let seg = printf_format(fmt, args, &mut idx);
            output.push_str(&seg);
            if idx >= args.len() || idx == start {
                break;
            }
        }
        output
    }

    #[test]
    fn basic_string() {
        assert_eq!(run_printf("%s", &["hello"]), "hello");
    }

    #[test]
    fn basic_decimal() {
        assert_eq!(run_printf("%d", &["42"]), "42");
    }

    #[test]
    fn basic_octal() {
        assert_eq!(run_printf("%o", &["8"]), "10");
    }

    #[test]
    fn basic_hex_lower() {
        assert_eq!(run_printf("%x", &["255"]), "ff");
    }

    #[test]
    fn basic_hex_upper() {
        assert_eq!(run_printf("%X", &["255"]), "FF");
    }

    #[test]
    fn basic_float() {
        assert_eq!(run_printf("%f", &["3.14"]), "3.140000");
    }

    #[test]
    fn float_precision() {
        assert_eq!(run_printf("%.2f", &["3.14159"]), "3.14");
    }

    #[test]
    fn basic_char() {
        assert_eq!(run_printf("%c", &["abc"]), "a");
    }

    #[test]
    fn percent_literal() {
        assert_eq!(run_printf("100%%", &[]), "100%");
    }

    #[test]
    fn width_string() {
        assert_eq!(run_printf("%10s", &["hi"]), "        hi");
    }

    #[test]
    fn left_align_string() {
        assert_eq!(run_printf("%-10s", &["hi"]), "hi        ");
    }

    #[test]
    fn zero_pad_int() {
        assert_eq!(run_printf("%05d", &["42"]), "00042");
    }

    #[test]
    fn string_precision_truncate() {
        assert_eq!(run_printf("%.3s", &["hello"]), "hel");
    }

    #[test]
    fn backslash_escapes_in_format() {
        assert_eq!(run_printf("a\\nb", &[]), "a\nb");
    }

    #[test]
    fn escape_b_format() {
        assert_eq!(run_printf("%b", &["hello\\nworld"]), "hello\nworld");
    }

    #[test]
    fn argument_recycling() {
        assert_eq!(run_printf("[%s]", &["a", "b", "c"]), "[a][b][c]");
    }

    #[test]
    fn missing_arg_defaults_empty() {
        assert_eq!(run_printf("%s-%s", &["a"]), "a-");
    }

    #[test]
    fn missing_int_defaults_zero() {
        assert_eq!(run_printf("%d", &[]), "0");
    }

    #[test]
    fn char_constant_in_int() {
        assert_eq!(run_printf("%d", &["'A"]), "65");
    }

    #[test]
    fn hex_prefix_hash() {
        assert_eq!(run_printf("%#x", &["255"]), "0xff");
    }

    #[test]
    fn octal_prefix_hash() {
        assert_eq!(run_printf("%#o", &["8"]), "010");
    }

    #[test]
    fn plus_sign_int() {
        assert_eq!(run_printf("%+d", &["42"]), "+42");
    }

    #[test]
    fn space_sign_int() {
        assert_eq!(run_printf("% d", &["42"]), " 42");
    }

    #[test]
    fn g_format() {
        assert_eq!(run_printf("%g", &["100.0"]), "100");
    }

    #[test]
    fn e_format() {
        let result = run_printf("%e", &["1234.5"]);
        assert!(
            result.contains('e'),
            "expected scientific notation, got: {result}"
        );
    }

    #[test]
    fn negative_int() {
        assert_eq!(run_printf("%d", &["-7"]), "-7");
    }

    #[test]
    fn hex_input_int() {
        assert_eq!(run_printf("%d", &["0xff"]), "255");
    }

    #[test]
    fn octal_input_int() {
        assert_eq!(run_printf("%d", &["010"]), "8");
    }
}

// ── Trap builtin ──────────────────────────────────────────────────────

/// trap — register signal handlers (stub that succeeds).
pub struct Trap;
impl Builtin for Trap {
    fn name(&self) -> &str {
        "trap"
    }
    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            // trap — list traps (no traps registered yet)
            return 0;
        }
        if args[0] == "-l" {
            // trap -l — list signal names
            println!("HUP INT QUIT ILL TRAP ABRT BUS FPE KILL USR1 SEGV USR2 PIPE ALRM TERM");
            return 0;
        }
        // trap 'cmd' SIG... or trap - SIG... — silently accept
        0
    }
}

// ── Remaining builtins (Phase 4.2) ────────────────────────────────────

/// umask — set file creation mask.
pub struct Umask;
impl Builtin for Umask {
    fn name(&self) -> &str {
        "umask"
    }
    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            // Print current umask
            let mask = unsafe { libc::umask(0o022) };
            unsafe { libc::umask(mask) };
            println!("{mask:04o}");
            return 0;
        }
        // Set umask
        if let Ok(mask) = u32::from_str_radix(args[0], 8) {
            unsafe { libc::umask(mask as libc::mode_t) };
            0
        } else {
            eprintln!("umask: invalid mask: {}", args[0]);
            1
        }
    }
}

/// fc — stub for history editing.
pub struct Fc;
impl Builtin for Fc {
    fn name(&self) -> &str {
        "fc"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// noglob — run command with GLOB disabled.
pub struct Noglob;
impl Builtin for Noglob {
    fn name(&self) -> &str {
        "noglob"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// emulate — shell emulation (stub).
pub struct Emulate;
impl Builtin for Emulate {
    fn name(&self) -> &str {
        "emulate"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// disable — disable builtins (stub).
pub struct Disable;
impl Builtin for Disable {
    fn name(&self) -> &str {
        "disable"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// enable — enable builtins (stub).
pub struct Enable;
impl Builtin for Enable {
    fn name(&self) -> &str {
        "enable"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// compdef — completion definition (stub).
pub struct Compdef;
impl Builtin for Compdef {
    fn name(&self) -> &str {
        "compdef"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// compctl — completion control (stub).
pub struct Compctl;
impl Builtin for Compctl {
    fn name(&self) -> &str {
        "compctl"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// zle — ZLE widget manipulation (stub).
pub struct Zle;
impl Builtin for Zle {
    fn name(&self) -> &str {
        "zle"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// bindkey — key binding (stub).
pub struct Bindkey;
impl Builtin for Bindkey {
    fn name(&self) -> &str {
        "bindkey"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// zstyle — style configuration (stub).
pub struct Zstyle;
impl Builtin for Zstyle {
    fn name(&self) -> &str {
        "zstyle"
    }
    fn execute(&self, _args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        0
    }
}

/// which — locate a command (like type).
pub struct Which;
impl Builtin for Which {
    fn name(&self) -> &str {
        "which"
    }
    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        Type.execute(args, env)
    }
}
