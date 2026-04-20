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

mod abbr;
mod alias;
mod bind;
mod compcomp;
mod completion;
mod env;
mod function;
mod hook;
mod integration;
mod mark;
mod option;
mod path;
mod picker;
mod prompt;
mod source;
mod theme;
mod trap;

pub use abbr::{expand_abbreviation, AbbrSpec};
pub use alias::AliasSpec;
pub use bind::{bind_function_name, BindSpec};
pub use compcomp::{FlagSpec, PositSpec, SubcmdSpec, ValueKind};
pub use completion::CompletionSpec;
pub use env::EnvSpec;
pub use function::FunctionSpec;
pub use hook::{hook_function_name, HookSpec};
pub use integration::{lookup_integration, IntegrationSpec, KNOWN_INTEGRATIONS};
pub use mark::{expand_mark_path, shell_quote_path, MarkSpec};
pub use option::OptionSetSpec;
pub use path::{apply_path, expand_vars, PathSpec};
pub use picker::{is_valid_action, picker_sentinel, PickerSpec, VALID_ACTIONS};
pub use prompt::PromptSpec;
pub use source::SourceSpec;
pub use theme::{merge_theme, nord_default, ThemeSpec};
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
    #[error("unknown integration: {0} (known: zoxide, direnv, starship, atuin)")]
    UnknownIntegration(String),
    #[error("defsource path not found: {path} (from rc at {rc})")]
    SourceNotFound { path: String, rc: String },
    #[error("defsource path unreadable: {path}: {source}")]
    SourceIo { path: String, source: std::io::Error },
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

    /// Subcommand registrations from `(defsubcmd …)` — consumed by
    /// frost-complete to build the rich completion tree.
    pub subcmds: Vec<SubcmdSpec>,
    /// Flag registrations from `(defflag …)`.
    pub flags: Vec<FlagSpec>,
    /// Positional registrations from `(defposit …)`.
    pub positionals: Vec<PositSpec>,

    /// Number of `(defintegration :tool "…")` expansions applied. Each
    /// expansion contributes to aliases / env_vars / hooks /
    /// prompts_set via the recipe in `frost_lisp::lookup_integration`;
    /// those counts are bumped already — `integrations` just records
    /// how many top-level integrations were triggered.
    pub integrations: usize,

    /// Fish-style abbreviations from `(defabbr …)`. The REPL consults
    /// this at submit time: if the first word of the submitted line
    /// matches a key, the line is rewritten (and the expansion
    /// echoed) before exec — like `!`-expansion. Unlike aliases
    /// (hidden), this is visible in terminal output + history.
    pub abbreviations: std::collections::HashMap<String, String>,

    /// Merged theme from every `(deftheme …)` form applied, overlaid
    /// on the built-in Nord default. Downstream consumers (frost-zle
    /// highlighter / hinter / broken-path coloring) read from here
    /// so color changes are a one-form edit instead of a Rust patch.
    pub theme: ThemeSpec,

    /// Two-key chord continuations from `(defbind :key "C-x e" …)` /
    /// `(defbind :key "M-k M-h" …)`. Entries are
    /// `(first_chord, second_chord, fn_name)`. The REPL binds the
    /// FIRST chord to a synthetic `__frost_chord_prefix_<first>__`
    /// sentinel; when that fires it reads ONE more key via
    /// crossterm, looks up the `(first, second)` pair here, and
    /// invokes `fn_name` as a shell function (same execution path as
    /// any single-chord defbind). 3+ key chords (`"C-x y z"`) are
    /// still silently dropped — rare in practice; can extend later.
    pub multi_key_bindings: Vec<(String, String, String)>,

    /// Directory bookmarks from `(defmark …)` forms. Map of name →
    /// resolved-absolute path. Applied as cd-aliases so `defmark
    /// :name "code"` becomes the `code` alias that cd's there.
    /// Preserved in the summary for introspection (future widgets
    /// like a mark picker, a `marks` builtin).
    pub marks: std::collections::HashMap<String, String>,
}

/// Parse a Lisp source string and apply every recognized form to `env`.
///
/// Forms with unknown keywords are silently ignored (tatara-lisp's
/// `compile_typed` filters by keyword, so mixing `defalias`/`defopts`/
/// `defenv` in the same file is expected).
pub fn apply_source(src: &str, env: &mut ShellEnv) -> LispResult<ApplySummary> {
    apply_source_with_context(src, env, None, &mut std::collections::HashSet::new())
}

/// Full-fat apply entry. `rc_dir` is the directory of the file the
/// source came from (used for `(defsource :path "relative.lisp")`
/// resolution). `visited` carries the canonical paths already sourced
/// in this apply-tree so recursive sourcing terminates.
fn apply_source_with_context(
    src: &str,
    env: &mut ShellEnv,
    rc_dir: Option<&std::path::Path>,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) -> LispResult<ApplySummary> {
    let mut summary = ApplySummary {
        theme: nord_default(),
        ..Default::default()
    };

    // ─── Sourced files (first — their forms fold into the outer pass) ─
    // Sourcing happens ahead of every primitive pass so that later
    // primitive forms in THIS file can override sourced ones — last
    // writer still wins on aliases / env / prompt. Hook bodies
    // compose either way thanks to the hook pass's accumulation.
    let sources: Vec<SourceSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for s in sources {
        let resolved = resolve_source_path(&s.path, rc_dir);
        let canonical = std::fs::canonicalize(&resolved).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                LispError::SourceNotFound {
                    path: resolved.display().to_string(),
                    rc: rc_dir
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<inline>".into()),
                }
            } else {
                LispError::SourceIo {
                    path: resolved.display().to_string(),
                    source: e,
                }
            }
        })?;
        if !visited.insert(canonical.clone()) {
            // Already sourced in this apply-tree — silent skip keeps
            // re-exports from spamming; we aren't a package manager.
            continue;
        }
        let inner_src = std::fs::read_to_string(&canonical).map_err(|e| LispError::SourceIo {
            path: canonical.display().to_string(),
            source: e,
        })?;
        let inner_dir = canonical.parent().map(|p| p.to_path_buf());
        let inner_summary = apply_source_with_context(
            &inner_src,
            env,
            inner_dir.as_deref(),
            visited,
        )?;
        merge_summary(&mut summary, inner_summary);
    }

    // ─── Integrations (first — they contribute primitives) ──────────
    // `(defintegration :tool "zoxide")` expands into the canonical
    // alias+hook+env set for each known tool. Processing happens BEFORE
    // the primitive passes so that (a) integration-contributed aliases
    // land in env.aliases alongside user-authored ones and (b) hook /
    // precmd contributions can be merged in the existing consolidation
    // loops below.
    let integrations: Vec<IntegrationSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    let mut integration_hooks: std::collections::HashMap<&'static str, Vec<String>> =
        std::collections::HashMap::new();
    let mut integration_prompt_commands: Vec<String> = Vec::new();
    for spec in integrations {
        let recipe = lookup_integration(&spec.tool)
            .ok_or_else(|| LispError::UnknownIntegration(spec.tool.clone()))?;
        // Aliases → env.aliases, counted toward summary.aliases.
        for (name, value) in recipe.aliases {
            env.aliases.insert((*name).to_string(), (*value).to_string());
            summary.aliases += 1;
        }
        // Env vars → env.set_var + optional export.
        for (name, value, exp) in recipe.env {
            env.set_var(name, value);
            summary.env_vars += 1;
            if *exp {
                env.export_var(name);
                summary.env_exports += 1;
            }
        }
        // Hook bodies — staged into a side map; the hook pass below
        // merges them with user-declared hooks into one composed body
        // per event.
        if let Some(body) = recipe.precmd_body {
            integration_hooks
                .entry("__frost_hook_precmd")
                .or_default()
                .push(body.to_string());
        }
        if let Some(body) = recipe.preexec_body {
            integration_hooks
                .entry("__frost_hook_preexec")
                .or_default()
                .push(body.to_string());
        }
        if let Some(body) = recipe.chpwd_body {
            integration_hooks
                .entry("__frost_hook_chpwd")
                .or_default()
                .push(body.to_string());
        }
        // Prompt command → synthetic precmd (appended to whatever the
        // prompt pass produces from user defprompts).
        if let Some(cmd) = recipe.prompt_command {
            integration_prompt_commands.push(cmd.to_string());
        }
        summary.integrations += 1;
    }

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
        if let Some(rps1) = &p.rps1 {
            env.set_var("RPS1", rps1);
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
    // Merge in prompt commands contributed by `(defintegration :tool "starship")`.
    for cmd in integration_prompt_commands {
        let piece = format!("PS1=\"$({cmd})\"");
        match &mut synthetic_precmd {
            Some(existing) => {
                existing.push('\n');
                existing.push_str(&piece);
            }
            None => synthetic_precmd = Some(piece),
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
    // Merge `(defintegration …)` hook bodies — each event accumulates
    // across every matching integration + user `(defhook …)` form.
    for (fn_name, bodies) in integration_hooks {
        let joined = bodies.join("\n");
        hook_bodies
            .entry(fn_name)
            .and_modify(|existing| {
                existing.push('\n');
                existing.push_str(&joined);
            })
            .or_insert(joined);
        summary.hooks += bodies.len();
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

    // Keybindings — single-chord bindings land under `__frost_bind_<KEY>`
    // and are bound via reedline; multi-key (`"C-x e"`) bindings install
    // a synthetic prefix sentinel + record the second-chord continuation
    // in `multi_key_bindings` so the REPL can crossterm-read the second
    // key and dispatch. 3+ key sequences remain dropped — rare + complex.
    let binds: Vec<BindSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    let mut chord_prefixes_emitted: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for b in binds {
        match frost_chord_kind(&b.key) {
            ChordKind::Single => {
                let fn_name = bind_function_name(&b.key);
                install_body_as_function(env, &fn_name, &b.action);
                summary.bind_map.push((b.key.clone(), fn_name));
                summary.binds += 1;
            }
            ChordKind::TwoKey { first, second } => {
                // Function name encodes both chords so multiple
                // (C-x e) vs (C-x a) entries don't collide.
                let fn_name = format!(
                    "__frost_bind_{}_{}",
                    sanitize_chord(&first),
                    sanitize_chord(&second),
                );
                install_body_as_function(env, &fn_name, &b.action);
                summary.multi_key_bindings.push((
                    first.clone(),
                    second.clone(),
                    fn_name,
                ));
                // Bind the first chord to the prefix sentinel once
                // per unique first-chord (`C-x e` and `C-x a` share
                // `__frost_chord_prefix_C-x__`).
                if chord_prefixes_emitted.insert(first.clone()) {
                    let sentinel = chord_prefix_sentinel(&first);
                    summary.bind_map.push((first, sentinel));
                }
                summary.binds += 1;
            }
            ChordKind::Unsupported => {
                // Invalid or 3+-key chord — silently skip. The chord
                // classifier warns at the ZLE boundary if it's truly
                // malformed; here we just don't register.
            }
        }
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

    // Rich completion tree — three flat forms that the REPL joins into
    // a dotted-path tree. Each spec is independent: users can mix
    // `(defcompletion …)` (flat args) with `(defsubcmd …)` /
    // `(defflag …)` / `(defposit …)` (rich tree) in the same rc, and
    // everything composes — frost-complete consults both the flat map
    // and the tree, preferring tree-aware candidates when they match.
    let subcmds: Vec<SubcmdSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    summary.subcmds = subcmds;
    let flags: Vec<FlagSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    summary.flags = flags;
    let positionals: Vec<PositSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    summary.positionals = positionals;

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

    // Fish-style abbreviations — collected for the REPL's submit-time
    // expander. Later forms win (last-writer-wins consistent with
    // aliases).
    let abbreviations: Vec<AbbrSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for a in abbreviations {
        summary.abbreviations.insert(a.name, a.expansion);
    }

    // Directory bookmarks — expand the path once at rc-load (tilde
    // + $VAR) and register a cd-alias that holds the resolved
    // absolute path. Also stash in the summary's `marks` map for
    // downstream consumers.
    let marks: Vec<MarkSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for m in marks {
        let resolved = mark::expand_mark_path(&m.path);
        let quoted = mark::shell_quote_path(&resolved);
        env.aliases.insert(m.name.clone(), format!("cd {quoted}"));
        summary.aliases += 1;
        summary.marks.insert(m.name, resolved);
    }

    // Theme overlays — each `(deftheme …)` form merges onto the
    // cumulative theme (Nord base at init). Partial specs work:
    // only the slots the user names override; everything else stays
    // at the prior value. Multiple forms compose left-to-right in
    // source order.
    let themes: Vec<ThemeSpec> =
        tatara_lisp::compile_typed(src).map_err(|e| LispError::Parse(e.to_string()))?;
    for t in themes {
        summary.theme = merge_theme(std::mem::take(&mut summary.theme), t);
    }

    Ok(summary)
}

/// Classify a chord string for routing through the defbind pipeline.
/// Returns the shape without running the full frost-zle classifier
/// (which lives in a downstream crate). Whitespace-separated chord
/// sequences with exactly two parts become `TwoKey`; single chords
/// return `Single`. Everything else is `Unsupported`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChordKind {
    Single,
    TwoKey { first: String, second: String },
    Unsupported,
}

fn frost_chord_kind(s: &str) -> ChordKind {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return ChordKind::Unsupported;
    }
    if trimmed.contains(char::is_whitespace) {
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() == 2 {
            return ChordKind::TwoKey {
                first: parts[0].to_string(),
                second: parts[1].to_string(),
            };
        }
        return ChordKind::Unsupported;
    }
    ChordKind::Single
}

/// Sentinel string emitted to reedline for the first chord of a
/// two-key binding. The REPL's dispatcher pattern-matches on this
/// to know "a chord continuation is expected next".
pub fn chord_prefix_sentinel(first_chord: &str) -> String {
    format!("__frost_chord_prefix_{}__", first_chord)
}

/// Strip chord separators so a chord string is safe as part of a
/// function-name identifier. `"C-x"` → `"C-x"` (kept), whitespace
/// collapsed. Mostly cosmetic — the identifier just has to be
/// consistent for the function registry.
fn sanitize_chord(chord: &str) -> String {
    chord.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Fold a nested-source's summary into the outer one. Hook / trap /
/// alias counts sum; the map-shaped fields (completion_map,
/// completion_descriptions) extend; vec-shaped (bind_map, subcmds,
/// flags, positionals, pickers) extend. This is lossy for `pickers`
/// with the same `name` across inner+outer — both land in the vec and
/// the REPL's lookup uses `find()` which returns the first match, so
/// inner wins when it exists. For most uses (inner files are
/// auto-generated, outer is hand-authored), that's correct.
fn merge_summary(dst: &mut ApplySummary, src: ApplySummary) {
    dst.aliases += src.aliases;
    dst.options_enabled += src.options_enabled;
    dst.options_disabled += src.options_disabled;
    dst.env_vars += src.env_vars;
    dst.env_exports += src.env_exports;
    dst.prompts_set += src.prompts_set;
    dst.hooks += src.hooks;
    dst.traps += src.traps;
    dst.binds += src.binds;
    dst.completions += src.completions;
    dst.functions += src.functions;
    dst.path_ops += src.path_ops;
    dst.integrations += src.integrations;
    dst.completion_map.extend(src.completion_map);
    dst.bind_map.extend(src.bind_map);
    dst.completion_descriptions.extend(src.completion_descriptions);
    dst.pickers.extend(src.pickers);
    dst.subcmds.extend(src.subcmds);
    dst.flags.extend(src.flags);
    dst.positionals.extend(src.positionals);
    dst.abbreviations.extend(src.abbreviations);
    // Theme: overlay the sourced file's theme onto the outer one.
    // Outer forms still win (`apply_source_with_context` runs the
    // outer file's `deftheme` passes AFTER this merge), consistent
    // with last-writer-wins.
    dst.theme = theme::merge_theme(std::mem::take(&mut dst.theme), src.theme);
    dst.multi_key_bindings.extend(src.multi_key_bindings);
    dst.marks.extend(src.marks);
}

/// Resolve a `(defsource :path …)` string against the sourcing file's
/// directory. `~/` + `$VAR` / `${VAR}` expansion runs against the
/// process env (not `ShellEnv`; sourcing is a build-time-of-rc concept).
fn resolve_source_path(raw: &str, rc_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    // Env expansion — reuse the defpath expand_vars so behavior
    // matches what users already know.
    let expanded = path::expand_vars(raw, &|name| std::env::var(name).ok());

    // Tilde: only `~/` (home-prefix). Full user home (`~user`) is
    // niche; sourcing is mostly the author's own files.
    let tilde_expanded = if let Some(rest) = expanded.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            std::path::PathBuf::from(home).join(rest)
        } else {
            std::path::PathBuf::from(&expanded)
        }
    } else if expanded == "~" {
        std::env::var("HOME").map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(&expanded))
    } else {
        std::path::PathBuf::from(&expanded)
    };

    // Relative paths resolve against rc_dir (not cwd) — stable across
    // frost launch locations.
    if tilde_expanded.is_absolute() {
        tilde_expanded
    } else if let Some(dir) = rc_dir {
        dir.join(tilde_expanded)
    } else {
        tilde_expanded
    }
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
/// so callers can unconditionally call this on startup. `defsource`
/// paths resolve against this file's directory.
pub fn load_rc(path: impl AsRef<Path>, env: &mut ShellEnv) -> LispResult<ApplySummary> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(ApplySummary::default());
    }
    let src = std::fs::read_to_string(path).map_err(|e| LispError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let rc_dir = path.parent();
    let mut visited = std::collections::HashSet::new();
    if let Ok(canonical) = std::fs::canonicalize(path) {
        visited.insert(canonical);
    }
    apply_source_with_context(&src, env, rc_dir, &mut visited)
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
    fn apply_defbind_two_key_populates_multi_key_bindings_and_prefix_sentinel() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defbind :key "C-x e" :action "exec $EDITOR")
            (defbind :key "C-x a" :action "echo another")
            (defbind :key "C-l"   :action "clear")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        // Single-chord C-l goes through the normal bind_map entry.
        assert!(s.bind_map.iter().any(|(k, fn_name)|
            k == "C-l" && fn_name.starts_with("__frost_bind_")
        ));
        // Two-key: ONE prefix sentinel regardless of how many share
        // the C-x prefix (both "C-x e" and "C-x a" add continuations
        // but only one prefix entry).
        let prefix_entries: Vec<_> = s.bind_map.iter()
            .filter(|(_, fn_name)| fn_name.starts_with("__frost_chord_prefix_"))
            .collect();
        assert_eq!(prefix_entries.len(), 1,
            "expected exactly one prefix sentinel, got {prefix_entries:?}");
        assert_eq!(prefix_entries[0].0, "C-x");
        assert_eq!(prefix_entries[0].1, "__frost_chord_prefix_C-x__");

        // The continuation table has both entries.
        assert_eq!(s.multi_key_bindings.len(), 2);
        assert!(s.multi_key_bindings.iter().any(|(f, r, _)| f == "C-x" && r == "e"));
        assert!(s.multi_key_bindings.iter().any(|(f, r, _)| f == "C-x" && r == "a"));

        // Each continuation fn_name is registered in env.functions.
        for (_, _, fn_name) in &s.multi_key_bindings {
            assert!(env.functions.contains_key(fn_name),
                "continuation function {fn_name} not registered");
        }
    }

    #[test]
    fn chord_prefix_sentinel_round_trips() {
        assert_eq!(chord_prefix_sentinel("C-x"), "__frost_chord_prefix_C-x__");
        assert_eq!(chord_prefix_sentinel("M-k"), "__frost_chord_prefix_M-k__");
    }

    #[test]
    fn apply_bind_registers_function() {
        // Multi-key (C-x e) now routes through the two-key pipeline:
        // the function registers under __frost_bind_C-x_e, a prefix
        // sentinel is emitted into bind_map for C-x, and the
        // continuation lands in multi_key_bindings.
        let mut env = ShellEnv::new();
        let src = r#"(defbind :key "C-x e" :action "echo fire")"#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.binds, 1);
        assert!(env.functions.contains_key("__frost_bind_C-x_e"));
        assert_eq!(s.multi_key_bindings.len(), 1);
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
    fn apply_deftheme_merges_onto_nord_default() {
        let mut env = ShellEnv::new();
        // r##"..."## so the embedded "#FFFFFF" doesn't close the raw
        // string on its first `"#` sequence.
        let src = r##"
            (deftheme :name "my-custom"
                      :hint "#FFFFFF"
                      :command "#00FF00")
        "##;
        let s = apply_source(src, &mut env).unwrap();
        // Overlay fields applied.
        assert_eq!(s.theme.name.as_deref(), Some("my-custom"));
        assert_eq!(s.theme.hint.as_deref(), Some("#FFFFFF"));
        assert_eq!(s.theme.command.as_deref(), Some("#00FF00"));
        // Non-overlaid fields retain Nord defaults.
        assert_eq!(s.theme.unknown_command.as_deref(), Some("#EBCB8B"));
        assert_eq!(s.theme.string.as_deref(), Some("#88C0D0"));
    }

    #[test]
    fn apply_empty_rc_yields_pure_nord_theme() {
        let mut env = ShellEnv::new();
        let s = apply_source("", &mut env).unwrap();
        let nord = nord_default();
        assert_eq!(s.theme, nord);
    }

    #[test]
    fn apply_multiple_deftheme_forms_compose_left_to_right() {
        let mut env = ShellEnv::new();
        let src = r##"
            (deftheme :hint "#111111")
            (deftheme :hint "#222222" :command "#333333")
        "##;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.theme.hint.as_deref(), Some("#222222"));
        assert_eq!(s.theme.command.as_deref(), Some("#333333"));
    }

    #[test]
    fn apply_defmark_registers_alias_and_records_mark() {
        // Set a sentinel env var so the test is deterministic.
        unsafe { std::env::set_var("X_MARK_TEST_HOME", "/tmp/marktest"); }
        let mut env = ShellEnv::new();
        let src = r#"
            (defmark :name "tmpmark" :path "$X_MARK_TEST_HOME/sub")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        // Alias registered.
        assert_eq!(
            env.aliases.get("tmpmark").map(String::as_str),
            Some("cd '/tmp/marktest/sub'")
        );
        // Summary map has the mark.
        assert_eq!(s.marks.get("tmpmark").map(String::as_str), Some("/tmp/marktest/sub"));
        unsafe { std::env::remove_var("X_MARK_TEST_HOME"); }
    }

    #[test]
    fn apply_defsource_loads_external_file() {
        let mut env = ShellEnv::new();
        let tmp = std::env::temp_dir().join(format!("frost-defsource-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let inner = tmp.join("inner.lisp");
        std::fs::write(&inner, r#"(defalias :name "aa" :value "aliased-via-source")"#).unwrap();
        let outer = tmp.join("outer.lisp");
        std::fs::write(
            &outer,
            format!(
                "(defsource :path \"{}\")\n(defalias :name \"bb\" :value \"outer-alias\")",
                inner.display()
            ),
        ).unwrap();
        let s = load_rc(&outer, &mut env).unwrap();
        // Both aliases land in env.aliases; both count toward the summary.
        assert_eq!(env.aliases.get("aa").map(String::as_str), Some("aliased-via-source"));
        assert_eq!(env.aliases.get("bb").map(String::as_str), Some("outer-alias"));
        assert_eq!(s.aliases, 2);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn apply_defsource_relative_path_resolves_against_rc_dir() {
        let mut env = ShellEnv::new();
        let tmp = std::env::temp_dir().join(format!("frost-defsource-rel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let inner = tmp.join("inner.lisp");
        std::fs::write(&inner, r#"(defalias :name "rel" :value "from-sibling")"#).unwrap();
        let outer = tmp.join("outer.lisp");
        std::fs::write(&outer, r#"(defsource :path "inner.lisp")"#).unwrap();
        load_rc(&outer, &mut env).unwrap();
        assert_eq!(env.aliases.get("rel").map(String::as_str), Some("from-sibling"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn apply_defsource_missing_file_errors() {
        let mut env = ShellEnv::new();
        let src = r#"(defsource :path "/definitely/not/a/real/path.lisp")"#;
        let err = apply_source(src, &mut env).unwrap_err();
        assert!(matches!(err, LispError::SourceNotFound { .. }));
    }

    #[test]
    fn apply_defsource_cycle_is_skipped() {
        // Two files that source each other — the visited-set must
        // prevent infinite recursion.
        let mut env = ShellEnv::new();
        let tmp = std::env::temp_dir().join(format!("frost-defsource-cycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let a = tmp.join("a.lisp");
        let b = tmp.join("b.lisp");
        std::fs::write(
            &a,
            format!(
                "(defsource :path \"{}\")\n(defalias :name \"a\" :value \"from-a\")",
                b.display()
            ),
        ).unwrap();
        std::fs::write(
            &b,
            format!(
                "(defsource :path \"{}\")\n(defalias :name \"b\" :value \"from-b\")",
                a.display()
            ),
        ).unwrap();
        load_rc(&a, &mut env).unwrap();
        // Both aliases register; the cycle is broken by the visited set.
        assert_eq!(env.aliases.get("a").map(String::as_str), Some("from-a"));
        assert_eq!(env.aliases.get("b").map(String::as_str), Some("from-b"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn apply_integration_zoxide_expands_aliases_and_chpwd() {
        let mut env = ShellEnv::new();
        let src = r#"(defintegration :tool "zoxide")"#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.integrations, 1);
        // Recipe added two aliases — frost-native (no `__zoxide_*`
        // shell-function indirection since frost doesn't run zoxide's
        // bash/zsh init).
        assert_eq!(env.aliases.get("z").map(String::as_str), Some("zoxide query"));
        assert_eq!(env.aliases.get("zi").map(String::as_str), Some("zoxide query -i"));
        // chpwd hook body must include the zoxide add call. The body
        // renders as a parsed AST (Word { parts: [Literal("zoxide")] }
        // + Word { parts: [Literal("add")] }), so assert on the tokens
        // rather than the reconstructed phrase.
        let chpwd = env.functions.get("__frost_hook_chpwd").expect("chpwd registered");
        let rendered = format!("{:?}", chpwd.body);
        assert!(rendered.contains("zoxide"), "body: {rendered}");
        assert!(rendered.contains("add"), "body: {rendered}");
        assert!(rendered.contains("PWD"), "body: {rendered}");
    }

    #[test]
    fn apply_integration_starship_synthesizes_prompt_hook() {
        let mut env = ShellEnv::new();
        let src = r#"(defintegration :tool "starship")"#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.integrations, 1);
        // Starship recipe adds a prompt_command → synthesized precmd
        // that assigns `$(starship prompt …)` to PS1.
        let precmd = env.functions.get("__frost_hook_precmd").expect("precmd registered");
        let rendered = format!("{:?}", precmd.body);
        assert!(rendered.contains("starship"), "body: {rendered}");
        assert!(rendered.contains("prompt"), "body: {rendered}");
        assert!(rendered.contains("PS1"), "body: {rendered}");
    }

    #[test]
    fn apply_integration_unknown_tool_errors() {
        let mut env = ShellEnv::new();
        let src = r#"(defintegration :tool "not-a-real-tool")"#;
        assert!(matches!(
            apply_source(src, &mut env),
            Err(LispError::UnknownIntegration(_))
        ));
    }

    #[test]
    fn apply_rich_completion_forms_collect_into_summary() {
        let mut env = ShellEnv::new();
        let src = r#"
            (defsubcmd :path "git" :name "commit" :description "record changes")
            (defsubcmd :path "git" :name "checkout" :description "switch branches")
            (defflag   :path "git.commit" :name "-m" :takes "string"
                       :description "commit message")
            (defflag   :path "git.commit" :name "--amend"
                       :description "replace last commit")
            (defposit  :path "git.commit" :index 1 :takes "files"
                       :description "paths to commit")
        "#;
        let s = apply_source(src, &mut env).unwrap();
        assert_eq!(s.subcmds.len(), 2);
        assert_eq!(s.flags.len(), 2);
        assert_eq!(s.positionals.len(), 1);
        // Spot-check one of each.
        let commit = s.subcmds.iter().find(|c| c.name == "commit").unwrap();
        assert_eq!(commit.path, "git");
        assert_eq!(commit.description.as_deref(), Some("record changes"));
        let m = s.flags.iter().find(|f| f.name == "-m").unwrap();
        assert_eq!(m.path, "git.commit");
        assert_eq!(m.takes.as_deref(), Some("string"));
        assert_eq!(s.positionals[0].index, 1);
        assert_eq!(s.positionals[0].takes.as_deref(), Some("files"));
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
