//! Byte-parity seal on the FULL `ApplySummary` produced by applying the
//! real frostmourne rc.
//!
//! Why this exists as its own harness rather than one more `assert!` in
//! `frostmourne_rc.rs`: the rc-apply pipeline is a fan of ~24 independent
//! per-form-type passes over the same source, and the failure mode that
//! matters is a *mis-threaded pass* — one pass fed the wrong slice of
//! forms. That yields a plausible-but-wrong shell (aliases still land,
//! `defflag` silently doesn't) which no scalar spot-check catches. The
//! only honest gate is: every counter, every map entry, every vector
//! element, byte-identical.
//!
//! The golden lives beside the fixture as
//! `fixtures/frostmourne-rc.summary`. Regenerate deliberately with
//! `FROST_BLESS_RC_SUMMARY=1 cargo test -p frost --test rc_summary_parity`
//! and READ THE DIFF — a blessed change is a claim that the shell the rc
//! describes genuinely changed.

use std::fmt::Write as _;

const FIXTURE: &str = include_str!("fixtures/frostmourne-rc.lisp");
const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/frostmourne-rc.summary"
);

/// Order-stable rendering of every field of [`frost_lisp::ApplySummary`].
///
/// `HashMap` iteration order is not stable across runs, so every map is
/// emitted sorted by key; every `Vec` is emitted in source order, which
/// IS the meaningful order (a pass that reorders its forms is a real
/// regression and must trip this gate).
fn canonical(s: &frost_lisp::ApplySummary) -> String {
    let mut out = String::new();

    // ── scalar counters ──────────────────────────────────────────────
    let _ = writeln!(out, "aliases = {}", s.aliases);
    let _ = writeln!(out, "options_enabled = {}", s.options_enabled);
    let _ = writeln!(out, "options_disabled = {}", s.options_disabled);
    let _ = writeln!(out, "env_vars = {}", s.env_vars);
    let _ = writeln!(out, "env_exports = {}", s.env_exports);
    let _ = writeln!(out, "prompts_set = {}", s.prompts_set);
    let _ = writeln!(out, "hooks = {}", s.hooks);
    let _ = writeln!(out, "traps = {}", s.traps);
    let _ = writeln!(out, "binds = {}", s.binds);
    let _ = writeln!(out, "completions = {}", s.completions);
    let _ = writeln!(out, "functions = {}", s.functions);
    let _ = writeln!(out, "path_ops = {}", s.path_ops);
    let _ = writeln!(out, "integrations = {}", s.integrations);
    let _ = writeln!(out, "packages = {}", s.packages);
    let _ = writeln!(out, "loads = {}", s.loads);

    // ── maps, sorted by key ──────────────────────────────────────────
    write_map(&mut out, "completion_map", &s.completion_map);
    write_map(
        &mut out,
        "completion_descriptions",
        &s.completion_descriptions,
    );
    write_map(&mut out, "completion_payloads", &s.completion_payloads);
    write_map(&mut out, "abbreviations", &s.abbreviations);
    write_map(&mut out, "marks", &s.marks);

    // ── vectors, in source order ─────────────────────────────────────
    write_vec(&mut out, "bind_map", &s.bind_map);
    write_vec(&mut out, "pickers", &s.pickers);
    write_vec(&mut out, "subcmds", &s.subcmds);
    write_vec(&mut out, "flags", &s.flags);
    write_vec(&mut out, "positionals", &s.positionals);
    write_vec(&mut out, "multi_key_bindings", &s.multi_key_bindings);
    write_vec(&mut out, "declared_packages", &s.declared_packages);
    write_vec(&mut out, "warnings", &s.warnings);

    // ── merged theme ─────────────────────────────────────────────────
    let _ = writeln!(out, "theme = {:?}", s.theme);

    out
}

fn write_map<V: std::fmt::Debug>(
    out: &mut String,
    label: &str,
    map: &std::collections::HashMap<String, V>,
) {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let _ = writeln!(out, "{label}.len = {}", keys.len());
    for k in keys {
        let _ = writeln!(out, "{label}[{k}] = {:?}", map[k]);
    }
}

fn write_vec<T: std::fmt::Debug>(out: &mut String, label: &str, v: &[T]) {
    let _ = writeln!(out, "{label}.len = {}", v.len());
    for (i, item) in v.iter().enumerate() {
        let _ = writeln!(out, "{label}[{i}] = {item:?}");
    }
}

/// Substitute the running operator's `$HOME` for a literal `$HOME` token.
///
/// WHY THIS EXISTS. Without it the golden bakes in whoever last blessed it.
/// Twenty-two of its lines held `/Users/luis.d/...` — the rc's `(defmark …)`
/// forms are written against `~`, and apply expands them — so this seal passed
/// on exactly ONE machine and could not pass on any other operator's box or on
/// any runner. Measured on frost's first CI run (31230794959), where it was the
/// single failure out of 1,015 tests:
///
///   got:      completion_descriptions[bm] = "→ /home/runner/code/…/blackmatter"
///   expected: completion_descriptions[bm] = "→ /Users/luis.d/code/…/blackmatter"
///
/// A byte-parity seal only its author can run is not a seal, it is a local
/// habit — and the failure it produces elsewhere teaches nothing, so it gets
/// muted or skipped, which is how a real gate dies. The home PREFIX is the only
/// machine-dependent part of this summary, so substituting it leaves every
/// other byte — every counter, every map entry, every vector element, which is
/// what the mis-threaded-pass class actually shows up in — still under the gate.
///
/// Longest-match is not a concern: there is one `$HOME` and it is an absolute
/// path, so no shorter path in the summary can contain it as a substring
/// without genuinely being inside it.
fn dehome(rendered: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => rendered.replace(&home, "$HOME"),
        // No HOME (or an empty one) means nothing to substitute; the summary is
        // already machine-independent by accident. Returning the input unchanged
        // is right — replacing an empty needle would corrupt every byte.
        _ => rendered.to_string(),
    }
}

#[test]
fn frostmourne_rc_apply_summary_is_byte_stable() {
    let mut env = frost_exec::ShellEnv::new();
    let summary = frost_lisp::apply_source(FIXTURE, &mut env).expect("rc should apply cleanly");
    let rendered = dehome(&canonical(&summary));

    if std::env::var_os("FROST_BLESS_RC_SUMMARY").is_some() {
        std::fs::write(GOLDEN_PATH, &rendered).expect("write golden");
        eprintln!("blessed {GOLDEN_PATH} ({} bytes)", rendered.len());
        return;
    }

    let golden = std::fs::read_to_string(GOLDEN_PATH).expect(
        "golden missing — regenerate with FROST_BLESS_RC_SUMMARY=1 \
         cargo test -p frost --test rc_summary_parity",
    );

    if rendered != golden {
        // A whole-string assert_eq! on ~1 MB of text is unreadable. Name
        // the first divergent line instead — that line names the pass.
        let first_diff = rendered
            .lines()
            .zip(golden.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("line {}:\n  got:      {a}\n  expected: {b}", i + 1))
            .unwrap_or_else(|| {
                format!(
                    "length differs: got {} lines, expected {} lines",
                    rendered.lines().count(),
                    golden.lines().count()
                )
            });
        panic!(
            "ApplySummary drifted from the golden — a pass is mis-threaded \
             or the rc genuinely changed.\n{first_diff}"
        );
    }
}
