//! Tatara-Lisp ↔ frost bridge.
//!
//! Every concept in a `.zshrc` — aliases, options, environment variables,
//! prompt templates, hooks, keybindings, completions, functions — lives in
//! frost as typed Rust state inside [`frost_exec::ShellEnv`] (plus side
//! registries owned by `frost-zle` / `frost-complete` / `frost-prompt`).
//!
//! This crate lets users DECLARE that state in Lisp via tatara-lisp, then
//! applies the declarations to the shell at startup time. Per the
//! pleme-io Rust + Lisp pattern:
//!
//! - **Rust** owns the types, invariants, and execution.
//! - **Lisp** owns the authoring surface — how humans express what they
//!   want the shell to do.
//!
//! The top-level entry is [`apply_source`] / [`load_rc`]; each Lisp
//! `def…` form has a dedicated spec type with `#[derive(DeriveTataraDomain)]`.
//!
//! # Current domains
//!
//! | Keyword     | Spec type          | Effect |
//! |-------------|--------------------|--------|
//! | `defalias`  | [`AliasSpec`]      | Add an alias to `env.aliases` |
//! | `defopts`   | [`OptionSetSpec`]  | Enable/disable shell options |
//! | `defenv`    | [`EnvSpec`]        | Set (and optionally export) a variable |
//!
//! Future domains — keep adding new `#[tatara(keyword = "…")]` structs
//! plus a pass in [`apply_source`]: `defprompt`, `defhook`, `defbind`,
//! `defcompletion`, `defun`, `deftrap`.

mod alias;
mod bind;
mod completion;
mod env;
mod function;
mod hook;
mod option;
mod path;
mod picker;
mod prompt;
mod trap;

pub use alias::AliasSpec;
pub use bind::{bind_function_name, BindSpec};
pub use completion::CompletionSpec;
pub use env::EnvSpec;
pub use function::FunctionSpec;
pub use hook::{hook_function_name, HookSpec};
pub use option::OptionSetSpec;
pub use path::{apply_path, expand_vars, PathSpec};
pub use picker::{is_valid_action, picker_sentinel, PickerSpec, VALID_ACTIONS};
pub use prompt::PromptSpec;
pub use trap::TrapSpec;

use frost_exec::ShellEnv;
use std::path::Path;

pub type LispResult<T> = Result<T, LispError>;

#[derive(Debug, thiserror::Error)]
pub enum LispError {
    #[error("io error reading rc file {path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("tatara-lisp parse error: {0}")]
    Parse(String),
    #[error("unknown option name: {0}")]
    UnknownOption(String),
    #[error("unknown hook event: {0} (valid: precmd, preexec, chpwd)")]
    UnknownHook(String),
    #[error("unknown signal: {0}")]
    UnknownSignal(String),
    #[error("unknown picker action: {0} (valid: replace, append, cd-submit, submit)")]
    UnknownPickerAction(String),
}

/// Summary of what a rc-application round actually changed. Returned by
/// [`apply_source`] so callers can log, validate, or plumb through to
/// runtime consumers (the completion arg map is the notable one —
/// `frost-complete` reads it to populate per-command Tab suggestions).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplySummary {
    pub aliases: usize,
    pub options_enabled: usize,
    pub options_disabled: usize,
    pub env_vars: usize,
    pub env_exports: usize,
    pub prompts_set: usize,
    pub hooks: usize,
    pub traps: usize,
    pub binds: usize,
    pub completions: usize,
    pub functions: usize,
    /// command → argument list, extracted from every `(defcompletion …)`
    /// applied. Preserved so the REPL can hand the map to
    /// `frost-complete::FrostCompleter` without re-parsing the JSON
    /// blobs we also wrote to shell vars.
    pub completion_map: std::collections::HashMap<String, Vec<String>>,

    /// chord (`"C-l"`, `"M-?"`) → function-name pairs from every
    /// `(defbind …)` form. The REPL hands these to `ZleEngine::with_bindings`
    /// which registers them with reedline.
    pub bind_map: Vec<(String, String)>,

    /// command → description string from `(defcompletion :description …)`.
    /// Shown in the completion menu at command position.
    pub completion_descriptions: std::collections::HashMap<String, String>,

    /// Picker widgets from `(defpicker …)` forms. The REPL walks this
    /// list to (a) bind keys to the picker sentinels and (b) populate
    /// its dispatch table so hitting a key runs the right binary with
    /// the right action semantics.
    pub pickers: Vec<PickerSpec>,

    /// How many `(defpath …)` forms modified PATH. The apply logic
    /// mutates `env.PATH` in place so there's no other side-channel
    /// for consumers; this field is informational only.
    pub path_ops: usize,
}

/// Parse a Lisp source string and apply every recognized form to `env`.
///
/// Forms with unknown keywords are silently ignored (tatara-lisp's
/// `compile_typed` filters by keyword, so mixing `defalias`/`defopts`/
/// `defenv` in the same file is expected).
pub fn apply_source(src: &str, env: &mut ShellEnv) -> LispResult<ApplySummary> {
    let mut summary = ApplySummary::default();

    // Aliases
    let aliases: Vec<AliasSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for a in aliases {
        env.aliases.insert(a.name, a.value);
        summary.aliases += 1;
    }

    // Shell options
    let opts: Vec<OptionSetSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for o in opts {
        for name in &o.enable {
            let opt = frost_options::Options::from_name(name)
                .ok_or_else(|| LispError::UnknownOption(name.clone()))?;
            env.set_option(opt);
            summary.options_enabled += 1;
        }
        for name in &o.disable {
            let opt = frost_options::Options::from_name(name)
                .ok_or_else(|| LispError::UnknownOption(name.clone()))?;
            env.unset_option(opt);
            summary.options_disabled += 1;
        }
    }

    // Environment variables (with optional export).
    let envs: Vec<EnvSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for e in envs {
        env.set_var(&e.name, &e.value);
        summary.env_vars += 1;
        if e.export {
            env.export_var(&e.name);
            summary.env_exports += 1;
        }
    }

    // PATH manipulation — `(defpath …)` forms compose. Each spec is
    // applied in source order against the current PATH, so later forms
    // see earlier prepends/appends. `$VAR` references in paths expand
    // against the already-set env vars above. Falls back to
    // `std::env::var` for vars frost doesn't own internally (e.g.
    // `HOME` set by the login session).
    let paths: Vec<PathSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for p in paths {
        // Build a per-form var snapshot covering everything referenced
        // in the spec. Cheap — path specs name a handful of variables.
        let mut refs: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in p.prepend.iter().chain(p.append.iter()) {
            collect_var_refs(s, &mut refs);
        }
        let snapshot: std::collections::HashMap<String, String> = refs
            .into_iter()
            .filter_map(|name| {
                let v = env
                    .get_var(&name)
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var(&name).ok());
                v.map(|val| (name, val))
            })
            .collect();
        let lookup = |name: &str| snapshot.get(name).cloned();
        let current = env.get_var("PATH").unwrap_or("").to_string();
        let next = path::apply_path(&current, &p, &lookup);
        env.set_var("PATH", &next);
        env.export_var("PATH");
        summary.path_ops += 1;
    }

    // Prompts — PS1/PS2 land as regular shell vars (the interactive loop
    // reads them each iteration) and optionally flip PROMPT_SUBST. If
    // `:command` is set, we also synthesize a `precmd` hook that runs
    // the command and assigns its stdout to PS1 — clean starship /
    // oh-my-posh / any-prompt-generator integration.
    let prompts: Vec<PromptSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    let mut synthetic_precmd: Option<String> = None;
    for p in prompts {
        if let Some(ps1) = &p.ps1 {
            env.set_var("PS1", ps1);
            summary.prompts_set += 1;
        }
        if let Some(ps2) = &p.ps2 {
            env.set_var("PS2", ps2);
            summary.prompts_set += 1;
        }
        if let Some(subst) = p.prompt_subst {
            if subst {
                env.set_option(frost_options::ShellOption::PromptSubst);
            } else {
                env.unset_option(frost_options::ShellOption::PromptSubst);
            }
        }
        if let Some(cmd) = &p.command {
            // Compose with any existing synthetic precmd from a prior
            // defprompt — last writer still wins at PS1 assignment time.
            let piece = format!("PS1=\"$({cmd})\"");
            match &mut synthetic_precmd {
                Some(existing) => {
                    existing.push('\n');
                    existing.push_str(&piece);
                }
                None => synthetic_precmd = Some(piece),
            }
        }
    }

    // Hooks — each stored under a well-known function name the REPL
    // checks. Multiple `(defhook :event "precmd" …)` forms compose —
    // later bodies append to earlier ones separated by newlines, so
    // frostmourne's tool-integration files can each register a chpwd
    // hook without stepping on the base prompt-info hook.
    //
    // Synthetic precmd from `(defprompt :command …)` joins the compose
    // pile: it becomes another line in the composed body, so the
    // frost-native hook that captures FROST_GIT_BRANCH/FROST_CMD_DURATION
    // runs BEFORE starship reads them (file load order: 20-hooks before
    // 63-tools-starship).
    let hooks: Vec<HookSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    let mut hook_bodies: std::collections::HashMap<&'static str, String> =
        std::collections::HashMap::new();
    for h in hooks {
        let fn_name = hook_function_name(&h.event)
            .ok_or_else(|| LispError::UnknownHook(h.event.clone()))?;
        hook_bodies
            .entry(fn_name)
            .and_modify(|existing| {
                existing.push('\n');
                existing.push_str(&h.body);
            })
            .or_insert_with(|| h.body.clone());
        summary.hooks += 1;
    }
    if let Some(body) = synthetic_precmd {
        hook_bodies
            .entry("__frost_hook_precmd")
            .and_modify(|existing| {
                existing.push('\n');
                existing.push_str(&body);
            })
            .or_insert(body);
        summary.hooks += 1;
    }
    for (fn_name, body) in hook_bodies {
        install_body_as_function(env, fn_name, &body);
    }

    // Signal traps — validate the signal name, then register the body as
    // a function under `__frost_trap_<SIGNAL>`. Runtime dispatch (actual
    // signal delivery → function invocation) lands in a follow-up.
    let traps: Vec<TrapSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for t in traps {
        let is_pseudo = frost_exec::trap::PseudoSignal::from_name(&t.signal).is_some();
        let is_real = frost_exec::trap::signal_name_to_number(&t.signal).is_some();
        if !is_pseudo && !is_real {
            return Err(LispError::UnknownSignal(t.signal));
        }
        let fn_name = format!("__frost_trap_{}", t.signal.to_ascii_uppercase());
        install_body_as_function(env, &fn_name, &t.body);
        summary.traps += 1;
    }

    // Keybindings — stored as `__frost_bind_<KEY>`. ZLE wire-up is a
    // follow-up; this establishes the authoring surface so a rc file can
    // declare its key map without waiting on the dispatcher.
    let binds: Vec<BindSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for b in binds {
        let fn_name = bind_function_name(&b.key);
        install_body_as_function(env, &fn_name, &b.action);
        summary.bind_map.push((b.key.clone(), fn_name));
        summary.binds += 1;
    }

    // Per-command completions — stored as a JSON-serialized CompletionSpec
    // under a variable `__frost_complete_<COMMAND>`. Keeps the runtime
    // side trivially consumable from frost-complete without a new
    // dependency on frost-lisp.
    let completions: Vec<CompletionSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for c in completions {
        let var = format!("__frost_complete_{}", c.command);
        let payload = serde_json::to_string(&c).unwrap_or_default();
        env.set_var(&var, &payload);
        summary.completion_map.insert(c.command.clone(), c.args.clone());
        if let Some(desc) = c.description.clone() {
            summary.completion_descriptions.insert(c.command.clone(), desc);
        }
        summary.completions += 1;
    }

    // Pickers — each spec registers a reedline keybinding that fires a
    // `__frost_picker_<name>__` sentinel straight into the REPL. Unlike
    // `defbind`, we deliberately DO NOT wrap the sentinel in a shell
    // function — the REPL must see the sentinel verbatim as the
    // ExecuteHostCommand payload so its dispatcher can intercept
    // before `!`-expansion and exec.
    let pickers: Vec<PickerSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for p in pickers {
        if !is_valid_action(&p.action) {
            return Err(LispError::UnknownPickerAction(p.action));
        }
        let sentinel = picker_sentinel(&p.name);
        // bind_map entry uses (key, sentinel) directly — reedline's
        // ExecuteHostCommand will return `sentinel` on key press.
        summary.bind_map.push((p.key.clone(), sentinel));
        summary.binds += 1;
        summary.pickers.push(p);
    }

    // Lisp-authored functions — same shape as the shell's own `function`
    // keyword but declarative: one form per function, body is shell
    // source. Registered under the user-visible name in `env.functions`
    // so everything else (aliasing, calls from hooks, completion) sees it.
    let funcs: Vec<FunctionSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for f in funcs {
        install_body_as_function(env, &f.name, &f.body);
        summary.functions += 1;
    }

    Ok(summary)
}

/// Scan a string for `$NAME` / `${NAME}` references and add the
/// names (without the `$` / braces) to `out`. Used by the defpath
/// apply to build a minimal env snapshot for expansion — avoids
/// cloning the entire ShellEnv var table per spec.
fn collect_var_refs(s: &str, out: &mut std::collections::HashSet<String>) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'{' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'}' {
                i += 1;
            }
            if i > start {
                out.insert(String::from_utf8_lossy(&bytes[start..i]).into_owned());
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        if i > start {
            out.insert(String::from_utf8_lossy(&bytes[start..i]).into_owned());
        }
    }
}

/// Parse shell source into an AST and register it under `fn_name` in
/// `env.functions`. Used by defhook / deftrap / defbind / defun, all of
/// which carry a `body` that must round-trip through the frost parser.
fn install_body_as_function(env: &mut ShellEnv, fn_name: &str, body: &str) {
    let tokens = {
        let mut lexer = frost_lexer::Lexer::new(body.as_bytes());
        let mut toks = Vec::new();
        loop {
            let tk = lexer.next_token();
            let eof = tk.kind == frost_lexer::TokenKind::Eof;
            toks.push(tk);
            if eof { break; }
        }
        toks
    };
    let program = frost_parser::Parser::new(&tokens).parse();
    env.functions.insert(
        fn_name.to_string(),
        frost_parser::ast::FunctionDef {
            name: compact_str::CompactString::from(fn_name),
            body: frost_parser::ast::Command::Subshell(frost_parser::ast::Subshell {
                body: program.commands,
                redirects: vec![],
            }),
            redirects: vec![],
        },
    );
}

/// Read and apply a Lisp rc file. Missing file = Ok with empty summary
/// so callers can unconditionally call this on startup.
pub fn load_rc(path: impl AsRef<Path>, env: &mut ShellEnv) -> LispResult<ApplySummary> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(ApplySummary::default());
    }
    let src = std::fs::read_to_string(path).map_err(|e| LispError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    apply_source(&src, env)
}

/// Resolve the default rc file path — `$FROSTRC` if set, else
/// `$XDG_CONFIG_HOME/frost/rc.lisp`, else `$HOME/.frostrc.lisp`.
pub fn default_rc_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("FROSTRC") {
        if !p.is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let p = std::path::PathBuf::from(xdg).join("frost").join("rc.lisp");
        if p.exists() { return p; }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(".frostrc.lisp")
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_aliases() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defalias :name "ll" :value "ls -la")
            (defalias :name "gst" :value "git status -sb")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.aliases, 2);
        assert_eq!(env.aliases.get("ll").map(String::as_str), Some("ls -la"));
        assert_eq!(env.aliases.get("gst").map(String::as_str), Some("git status -sb"));
    }

    #[test]
    fn apply_options_enable_disable() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defopts :enable ("extendedglob" "globdots")
                     :disable ("beep"))
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.options_enabled, 2);
        assert_eq!(s.options_disabled, 1);
        assert!(env.is_option_set(frost_options::ShellOption::ExtendedGlob));
        assert!(env.is_option_set(frost_options::ShellOption::GlobDots));
    }

    #[test]
    fn apply_unknown_option_errors() {
        let mut env = ShellEnv::new();
        let src = r#"(defopts :enable ("notAnOption"))"#;
        assert!(matches!(apply_source(src, &mut env), Err(LispError::UnknownOption(_))));
    }

    #[test]
    fn apply_env_with_export() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defenv :name "EDITOR" :value "blnvim" :export #t)
            (defenv :name "PAGER"  :value "less -R")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.env_vars, 2);
        assert_eq!(s.env_exports, 1);
        assert_eq!(env.get_var("EDITOR"), Some("blnvim"));
        assert_eq!(env.get_var("PAGER"), Some("less -R"));
    }

    #[test]
    fn apply_mixed_source_in_one_pass() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defalias :name "ll" :value "ls -la")
            (defopts :enable ("globdots"))
            (defenv :name "LANG" :value "en_US.UTF-8" :export #t)
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.aliases, 1);
        assert_eq!(s.options_enabled, 1);
        assert_eq!(s.env_vars, 1);
        assert_eq!(s.env_exports, 1);
    }

    #[test]
    fn missing_rc_is_not_an_error() {
        let mut env = ShellEnv::new();
        let s = load_rc("/nonexistent/path/frostrc.lisp", &mut env).unwrap();
        assert_eq!(s, ApplySummary::default());
    }

    #[test]
    fn default_rc_path_is_nonempty() {
        assert!(!default_rc_path().as_os_str().is_empty());
    }

    #[test]
    fn apply_prompt_sets_ps1_and_subst() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defprompt :ps1 "%F{green}%n%f %# "
                       :ps2 "> "
                       :prompt-subst #t)
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.prompts_set, 2);
        assert_eq!(env.get_var("PS1"), Some("%F{green}%n%f %# "));
        assert_eq!(env.get_var("PS2"), Some("> "));
        assert!(env.is_option_set(frost_options::ShellOption::PromptSubst));
    }

    #[test]
    fn apply_hook_registers_function() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defhook :event "precmd"
                     :body "echo 'before each prompt'")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.hooks, 1);
        assert!(env.functions.contains_key(
            hook_function_name("precmd").unwrap()
        ));
    }

    #[test]
    fn multiple_hooks_for_same_event_compose() {
        // Two chpwd registrations (e.g. one from the base rc, one from a
        // zoxide integration file). Both bodies should execute — not just
        // the last-registered. frostmourne's multi-rc layout relies on
        // this composition.
        let mut env = ShellEnv::new();
        let src = r#"
            (defhook :event "chpwd" :body "echo first")
            (defhook :event "chpwd" :body "echo second")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.hooks, 2);
        // The stored function body should mention both.
        let fn_def = env.functions.get("__frost_hook_chpwd").expect("chpwd registered");
        let rendered = format!("{:?}", fn_def.body);
        assert!(rendered.contains("first"), "first body missing: {rendered}");
        assert!(rendered.contains("second"), "second body missing: {rendered}");
    }

    #[test]
    fn apply_unknown_hook_errors() {
        let mut env = ShellEnv::new();
        let src = r#"(defhook :event "bogus" :body "true")"#;
        assert!(matches!(apply_source(src, &mut env), Err(LispError::UnknownHook(_))));
    }

    #[test]
    fn apply_trap_registers_function() {
        let mut env = ShellEnv::new();
        let src = r#"(deftrap :signal "INT" :body "echo interrupted")"#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.traps, 1);
        assert!(env.functions.contains_key("__frost_trap_INT"));
    }

    #[test]
    fn apply_pseudo_trap_exit_also_ok() {
        let mut env = ShellEnv::new();
        let src = r#"(deftrap :signal "EXIT" :body "echo goodbye")"#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.traps, 1);
        assert!(env.functions.contains_key("__frost_trap_EXIT"));
    }

    #[test]
    fn apply_unknown_signal_errors() {
        let mut env = ShellEnv::new();
        let src = r#"(deftrap :signal "NONESUCH" :body "true")"#;
        assert!(matches!(apply_source(src, &mut env), Err(LispError::UnknownSignal(_))));
    }

    #[test]
    fn apply_bind_registers_function() {
        let mut env = ShellEnv::new();
        let src = r#"(defbind :key "C-x e" :action "echo fire")"#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.binds, 1);
        // Canonicalized key: whitespace stripped, uppercased.
        assert!(env.functions.contains_key("__frost_bind_C-XE"));
    }

    #[test]
    fn apply_completion_stores_as_json_var() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defcompletion :command "git"
                           :args ("status" "diff" "log")
                           :description "version control")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.completions, 1);
        let stored = env.get_var("__frost_complete_git").unwrap();
        assert!(stored.contains("status"));
        assert!(stored.contains("diff"));
        assert!(stored.contains("version control"));
    }

    #[test]
    fn apply_picker_registers_direct_sentinel_binding() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defpicker :name "history" :key "C-r"
                       :binary "skim-history" :action "replace")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.pickers.len(), 1);
        assert_eq!(s.pickers[0].binary, "skim-history");
        // The key binding points straight at the sentinel — NOT at a
        // wrapper function — so the REPL's interceptor sees the exact
        // sentinel string when the user hits C-r.
        let (key, sentinel) = &s.bind_map[0];
        assert_eq!(key, "C-r");
        assert_eq!(sentinel, "__frost_picker_history__");
        assert_eq!(s.binds, 1);
    }

    #[test]
    fn apply_defpath_prepends_entries_to_path() {
        let mut env = ShellEnv::new();
        env.set_var("PATH", "/usr/bin:/bin");
        env.set_var("HOME", "/Users/me");
        let src = r#"
            (defpath :prepend ("$HOME/.local/bin" "/opt/bin"))
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.path_ops, 1);
        assert_eq!(env.get_var("PATH"), Some("/Users/me/.local/bin:/opt/bin:/usr/bin:/bin"));
    }

    #[test]
    fn apply_defpath_dedupes_existing_entries() {
        let mut env = ShellEnv::new();
        env.set_var("PATH", "/usr/bin:/usr/local/bin:/bin");
        let src = r#"
            (defpath :prepend ("/usr/local/bin"))
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.path_ops, 1);
        // usr/local/bin moves to the front; only one copy kept.
        assert_eq!(env.get_var("PATH"), Some("/usr/local/bin:/usr/bin:/bin"));
    }

    #[test]
    fn apply_prompt_command_registers_synthetic_precmd() {
        // `(defprompt :command …)` should synthesize a precmd hook that
        // assigns `$(command)` to PS1 — the integration point for
        // starship / oh-my-posh.
        let mut env = ShellEnv::new();
        let src = r#"
            (defprompt :command "starship prompt --status=$?")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.hooks, 1, "synthetic precmd hook should count toward summary");
        let fn_def = env.functions.get("__frost_hook_precmd")
            .expect("prompt command should register a precmd hook");
        let rendered = format!("{:?}", fn_def.body);
        assert!(rendered.contains("starship prompt"), "starship not in body: {rendered}");
        assert!(rendered.contains("PS1"), "PS1 assignment missing: {rendered}");
    }

    #[test]
    fn apply_picker_rejects_unknown_action() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defpicker :name "x" :key "C-x" :binary "x" :action "nope")
        "#;
        assert!(matches!(
            apply_source(src, &mut env),
            Err(LispError::UnknownPickerAction(_))
        ));
    }

    #[test]
    fn apply_defun_registers_named_function() {
        let mut env = ShellEnv::new();
        let src = r#"(defun :name "greet" :body "echo hello $1")"#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.functions, 1);
        assert!(env.functions.contains_key("greet"));
    }
}
