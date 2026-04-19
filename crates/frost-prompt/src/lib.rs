//! Zsh-compatible prompt templating.
//!
//! Zsh prompts mix three substitution languages:
//!
//! 1. `%`-escapes — static substitutions baked into zsh (`%~`, `%n`, `%#`).
//! 2. `$`-expansion — variable expansion (`$USER`, `$PWD`) when
//!    `PROMPT_SUBST` is on.
//! 3. Conditional groups — `%(cond.true.false)`.
//!
//! This crate owns the pure-function translation of a template string +
//! an [`Env`] snapshot to the string that reedline should render. It
//! never touches filesystem state or mutates anything; callers snapshot
//! the environment and then render.
//!
//! # Why a separate crate
//!
//! frost-prompt is consumed by both `frost` (binary) and future test
//! harnesses; keeping it free of the rest of the executor's dependencies
//! means it stays trivially testable and fast to build. Following the
//! pleme-io "one domain, one crate" rule: the prompt is its own
//! concern — not a subroutine of line editing.
//!
//! # Supported today
//!
//! | Escape | Meaning |
//! |--------|---------|
//! | `%%`  | literal `%` |
//! | `%n`  | username (`$USER`, falls back to `getlogin`) |
//! | `%m`  | hostname, short (up to first `.`) |
//! | `%M`  | hostname, full |
//! | `%d` / `%/` | cwd, absolute |
//! | `%~`  | cwd with `$HOME` replaced by `~` |
//! | `%c` / `%C` | trailing component of cwd (no `~`) |
//! | `%#`  | `#` if effective uid is 0, else `%` |
//! | `%?`  | last command exit status |
//! | `%(c.true.false)` | conditional — `c` is one of `?`, `!`, `#` (see below) |
//! | `$VAR` / `${VAR}` | environment variable (always enabled; gate with `prompt_subst` if you need strict zsh semantics) |
//!
//! Conditional `%(c.T.F)`:
//! - `?` — last exit status is zero
//! - `!` — running as root (uid 0)
//! - `#` — same as `!` (zsh shorthand)
//!
//! # Future work
//!
//! * `%F{color}`, `%K{color}`, `%B`, `%U`, `%f`, `%k`, `%b`, `%u` — colour
//!   and attributes.
//! * `%D{fmt}` — strftime dates.
//! * `%D` without braces, `%T`, `%*`.
//! * Numeric truncation (`%30<...<`, `%-5~`).
//! * `%j` (jobs count), `%l` (tty), `%y` (tty full), `%h` (history number).

use std::path::Path;

/// Pure snapshot of the environment as the prompt sees it.
///
/// The caller populates this once per prompt render so [`render`] is a
/// deterministic pure function.
#[derive(Debug, Clone, Default)]
pub struct PromptEnv {
    pub user: String,
    pub hostname: String,
    pub home: String,
    pub cwd: String,
    pub exit_status: i32,
    pub is_root: bool,
    /// Additional variables to make available for `$VAR` expansion.
    pub extra_vars: std::collections::HashMap<String, String>,
}

impl PromptEnv {
    /// Snapshot the ambient OS state — env vars, cwd, uid, hostname.
    /// `exit_status` and `extra_vars` are caller-supplied since the
    /// prompt crate has no notion of the shell's internal state.
    pub fn snapshot(exit_status: i32) -> Self {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_default();
        let home = std::env::var("HOME").unwrap_or_default();
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| p.into_os_string().into_string().ok())
            .unwrap_or_default();
        let hostname = read_hostname();
        let is_root = is_effective_root();
        Self {
            user,
            hostname,
            home,
            cwd,
            exit_status,
            is_root,
            extra_vars: std::collections::HashMap::new(),
        }
    }

    /// Look up a variable, checking `extra_vars` first and falling back
    /// to the process environment. Empty string on miss.
    pub fn lookup(&self, name: &str) -> String {
        if let Some(v) = self.extra_vars.get(name) {
            return v.clone();
        }
        std::env::var(name).unwrap_or_default()
    }
}

/// Render `template` against `env`. Always performs `%` expansion.
/// `$` expansion runs only when `prompt_subst` is true (zsh default: off).
///
/// Malformed escapes are written through literally — prompts should
/// never panic and shouldn't obscure the user's template on error.
pub fn render(template: &str, env: &PromptEnv, prompt_subst: bool) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '%' => render_percent(&mut chars, env, &mut out),
            '$' if prompt_subst => render_dollar(&mut chars, env, &mut out),
            other => out.push(other),
        }
    }
    out
}

fn render_percent<I>(chars: &mut std::iter::Peekable<I>, env: &PromptEnv, out: &mut String)
where
    I: Iterator<Item = char>,
{
    let Some(&next) = chars.peek() else {
        out.push('%');
        return;
    };
    chars.next();
    match next {
        '%' => out.push('%'),
        'n' => out.push_str(&env.user),
        'm' => {
            // Short hostname — up to first `.`.
            let h = &env.hostname;
            let short = h.split('.').next().unwrap_or(h);
            out.push_str(short);
        }
        'M' => out.push_str(&env.hostname),
        'd' | '/' => out.push_str(&env.cwd),
        '~' => out.push_str(&cwd_with_tilde(&env.cwd, &env.home)),
        'c' | 'C' => {
            let base = Path::new(&env.cwd).file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            out.push_str(base);
        }
        '#' => out.push(if env.is_root { '#' } else { '%' }),
        '?' => {
            let mut buf = itoa::Buffer::new();
            out.push_str(buf.format(env.exit_status));
        }
        '(' => render_conditional(chars, env, out),

        // ── Color + attribute escapes (emit ANSI SGR) ──────────────
        // These match zsh when the terminal supports colors; we unconditionally
        // emit because any modern terminal does, and rendering in "plain"
        // mode would produce something worse than the raw codes.
        'F' => render_color(chars, out, /*background=*/ false),
        'K' => render_color(chars, out, /*background=*/ true),
        'f' => out.push_str("\x1b[39m"),        // default foreground
        'k' => out.push_str("\x1b[49m"),        // default background
        'B' => out.push_str("\x1b[1m"),         // bold on
        'b' => out.push_str("\x1b[22m"),        // bold off
        'U' => out.push_str("\x1b[4m"),         // underline on
        'u' => out.push_str("\x1b[24m"),        // underline off
        'S' => out.push_str("\x1b[7m"),         // standout on (reverse)
        's' => out.push_str("\x1b[27m"),        // standout off

        other => {
            // Unknown escape — pass through literally so the user can see
            // what frost didn't understand.
            out.push('%');
            out.push(other);
        }
    }
}

/// Render `%F{color}` or `%K{color}`. Accepts named colors (`red`, `blue`,
/// `cyan`, …), `black`/`white`, and numeric 256-color indices (`196`,
/// `231`, …). Missing / malformed `{…}` reverts to the default attribute
/// without consuming any characters past the `%F` itself.
fn render_color<I>(chars: &mut std::iter::Peekable<I>, out: &mut String, background: bool)
where
    I: Iterator<Item = char>,
{
    // Expect `{`; otherwise pass the literal escape through.
    if chars.peek() != Some(&'{') {
        out.push('%');
        out.push(if background { 'K' } else { 'F' });
        return;
    }
    chars.next(); // consume '{'
    let mut spec = String::new();
    for c in chars.by_ref() {
        if c == '}' { break; }
        spec.push(c);
    }
    let code = match color_code(&spec, background) {
        Some(c) => c,
        None => return, // silently drop unknown color spec
    };
    out.push_str(&code);
}

fn color_code(name: &str, background: bool) -> Option<String> {
    // Numeric 256-color index
    if let Ok(n) = name.parse::<u16>() {
        if n <= 255 {
            let lead = if background { "48" } else { "38" };
            return Some(format!("\x1b[{lead};5;{n}m"));
        }
        return None;
    }
    // Standard named colors (30–37 fg, 40–47 bg; 90–97 / 100–107 bright)
    let (base_fg, base_bg) = match name.to_ascii_lowercase().as_str() {
        "black"         => (30, 40),
        "red"           => (31, 41),
        "green"         => (32, 42),
        "yellow"        => (33, 43),
        "blue"          => (34, 44),
        "magenta"       => (35, 45),
        "cyan"          => (36, 46),
        "white"         => (37, 47),
        "default"       => (39, 49),
        // zsh accepts a leading `bright` for the 90-series bright variants.
        "bright-black"  | "brightblack"   => (90, 100),
        "bright-red"    | "brightred"     => (91, 101),
        "bright-green"  | "brightgreen"   => (92, 102),
        "bright-yellow" | "brightyellow"  => (93, 103),
        "bright-blue"   | "brightblue"    => (94, 104),
        "bright-magenta"| "brightmagenta" => (95, 105),
        "bright-cyan"   | "brightcyan"    => (96, 106),
        "bright-white"  | "brightwhite"   => (97, 107),
        _ => return None,
    };
    let code = if background { base_bg } else { base_fg };
    Some(format!("\x1b[{code}m"))
}

/// `%(c.TRUE.FALSE)` — render TRUE if predicate `c` holds, else FALSE.
/// The delimiter after `c` is the character separating the branches
/// (it's commonly `.` but zsh accepts any non-alnum).
fn render_conditional<I>(chars: &mut std::iter::Peekable<I>, env: &PromptEnv, out: &mut String)
where
    I: Iterator<Item = char>,
{
    // Predicate
    let Some(predicate) = chars.next() else {
        out.push_str("%(");
        return;
    };
    // Optional numeric argument for predicates that accept one. We skip
    // digits; the matching predicates here don't require them but future
    // ones (`%(5L.…)`) will.
    let mut num = String::new();
    while let Some(&c) = chars.peek() {
        if !c.is_ascii_digit() { break; }
        num.push(c);
        chars.next();
    }
    let Some(delim) = chars.next() else {
        out.push('%');
        out.push('(');
        out.push(predicate);
        out.push_str(&num);
        return;
    };
    let mut true_branch = String::new();
    let mut false_branch = String::new();
    let mut saw_delim = false;
    let mut depth = 1usize;
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(&next) = chars.peek() {
                if next == '(' {
                    depth += 1;
                } else if next == ')' && depth > 1 {
                    depth -= 1;
                } else if next == delim || next == ')' {
                    // Bubble below
                }
            }
        }
        if c == ')' && depth == 1 {
            break;
        }
        if c == ')' {
            depth -= 1;
            if saw_delim { false_branch.push(c); } else { true_branch.push(c); }
            continue;
        }
        if c == delim && depth == 1 && !saw_delim {
            saw_delim = true;
            continue;
        }
        if saw_delim {
            false_branch.push(c);
        } else {
            true_branch.push(c);
        }
    }

    let chosen = match predicate {
        '?' => env.exit_status == 0,
        '!' | '#' => env.is_root,
        _ => false,
    };
    let branch = if chosen { &true_branch } else { &false_branch };
    // The branches may themselves contain %-escapes. Recurse.
    out.push_str(&render(branch, env, false));
}

fn render_dollar<I>(chars: &mut std::iter::Peekable<I>, env: &PromptEnv, out: &mut String)
where
    I: Iterator<Item = char>,
{
    let Some(&next) = chars.peek() else {
        out.push('$');
        return;
    };

    if next == '{' {
        chars.next(); // consume '{'
        let mut name = String::new();
        for c in chars.by_ref() {
            if c == '}' { break; }
            name.push(c);
        }
        if !name.is_empty() {
            out.push_str(&env.lookup(&name));
        }
        return;
    }

    if next.is_ascii_alphabetic() || next == '_' {
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                name.push(c);
                chars.next();
            } else {
                break;
            }
        }
        out.push_str(&env.lookup(&name));
        return;
    }

    out.push('$');
}

fn cwd_with_tilde(cwd: &str, home: &str) -> String {
    if !home.is_empty() && (cwd == home) {
        return "~".to_string();
    }
    if !home.is_empty() {
        if let Some(rest) = cwd.strip_prefix(&format!("{home}/")) {
            return format!("~/{rest}");
        }
    }
    cwd.to_string()
}

fn read_hostname() -> String {
    // SAFETY: we allocate a buffer and gethostname never writes past
    // the length we pass. Errors are swallowed — the prompt is a
    // best-effort display, never a correctness-critical path.
    let mut buf = [0u8; 256];
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if ret != 0 { return String::new(); }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn is_effective_root() -> bool {
    // SAFETY: getuid is a pure read.
    unsafe { libc::geteuid() == 0 }
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(cwd: &str, home: &str) -> PromptEnv {
        PromptEnv {
            user: "luis".into(),
            hostname: "blackmatter.local".into(),
            home: home.into(),
            cwd: cwd.into(),
            exit_status: 0,
            is_root: false,
            extra_vars: Default::default(),
        }
    }

    #[test]
    fn literal_percent_and_text_pass_through() {
        let env = PromptEnv::default();
        assert_eq!(render("100%%", &env, false), "100%");
        assert_eq!(render("no-subst-here", &env, false), "no-subst-here");
    }

    #[test]
    fn user_and_hostname_substitutions() {
        let env = env_with("/home/luis", "/home/luis");
        assert_eq!(render("%n@%m", &env, false), "luis@blackmatter");
        assert_eq!(render("%n@%M", &env, false), "luis@blackmatter.local");
    }

    #[test]
    fn cwd_escapes() {
        let env = env_with("/home/luis/code", "/home/luis");
        assert_eq!(render("%d", &env, false), "/home/luis/code");
        assert_eq!(render("%~", &env, false), "~/code");
        assert_eq!(render("%c", &env, false), "code");
    }

    #[test]
    fn cwd_at_home_is_tilde() {
        let env = env_with("/home/luis", "/home/luis");
        assert_eq!(render("%~", &env, false), "~");
    }

    #[test]
    fn prompt_char_depends_on_root() {
        let mut env = PromptEnv::default();
        assert_eq!(render("%#", &env, false), "%");
        env.is_root = true;
        assert_eq!(render("%#", &env, false), "#");
    }

    #[test]
    fn exit_status_and_conditional() {
        let mut env = PromptEnv::default();
        assert_eq!(render("%?", &env, false), "0");
        assert_eq!(render("%(?.ok.err)", &env, false), "ok");
        env.exit_status = 42;
        assert_eq!(render("%?", &env, false), "42");
        assert_eq!(render("%(?.ok.err)", &env, false), "err");
    }

    #[test]
    fn nested_conditional() {
        let env = env_with("/home/luis", "/home/luis");
        // `%(?.[%~].ERR)` — inside-conditional %-escape should still render.
        assert_eq!(render("%(?.[%~].ERR)", &env, false), "[~]");
    }

    #[test]
    fn dollar_expansion_off_by_default() {
        let mut env = PromptEnv::default();
        env.extra_vars.insert("HOST".into(), "cid".into());
        assert_eq!(render("$HOST", &env, false), "$HOST");
    }

    #[test]
    fn dollar_expansion_when_enabled() {
        let mut env = PromptEnv::default();
        env.extra_vars.insert("HOST".into(), "cid".into());
        assert_eq!(render("$HOST", &env, true), "cid");
        assert_eq!(render("${HOST}-local", &env, true), "cid-local");
    }

    #[test]
    fn unknown_escape_passes_through_literally() {
        let env = PromptEnv::default();
        // `%Z` isn't implemented yet — show the user what we didn't parse.
        assert_eq!(render("%Z", &env, false), "%Z");
    }

    #[test]
    fn color_and_attribute_escapes() {
        let env = PromptEnv::default();
        // Named colors — foreground + background.
        assert_eq!(render("%F{red}x%f", &env, false), "\x1b[31mx\x1b[39m");
        assert_eq!(render("%K{blue}x%k", &env, false), "\x1b[44mx\x1b[49m");
        // Numeric 256-color.
        assert_eq!(render("%F{196}!", &env, false), "\x1b[38;5;196m!");
        // Bright variants.
        assert_eq!(render("%F{bright-cyan}/", &env, false), "\x1b[96m/");
        // Attribute toggles.
        assert_eq!(render("%B bold %b", &env, false), "\x1b[1m bold \x1b[22m");
        assert_eq!(render("%U link %u", &env, false), "\x1b[4m link \x1b[24m");
    }

    #[test]
    fn unknown_color_name_silently_drops() {
        let env = PromptEnv::default();
        // Malformed or unrecognized color is dropped — the surrounding text
        // still renders. Better than injecting broken SGR into the prompt.
        assert_eq!(render("%F{not-a-color}x%f", &env, false), "x\x1b[39m");
    }

    #[test]
    fn color_without_brace_is_literal_escape() {
        let env = PromptEnv::default();
        // `%F` without `{...}` just passes through.
        assert_eq!(render("%Fhi", &env, false), "%Fhi");
    }
}
