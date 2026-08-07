//! Differential corpus: run the same SCRIPT FILE under `frost` and under
//! `zsh --no-rcs`, and require byte-identical stdout.
//!
//! Why script files rather than `-c`: a `-c` string has to survive this
//! file's own Rust quoting *and* the shell's, so a mismatch can be a harness
//! artifact rather than a real parity bug. Writing the row to a file and
//! handing the path to both shells removes that whole class.
//!
//! **Every row runs under a wall-clock timeout and a timeout is a hard
//! failure, not a slow test.** A parser that stops consuming tokens shows up
//! as a hang, and a hang that merely makes the suite slow is a hang that
//! ships. `eval_cmdsub_inner_dquotes` below is the committed regression row
//! for exactly that (an earlier attempt at the double-quote work wedged
//! `Parser::parse` in a non-advancing loop on it).
//!
//! Rows in `ROWS` must match. Rows in `KNOWN_DIVERGENCES` are recorded, run
//! (so they must still not hang or crash), and reported — never asserted.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Wall-clock budget for one row, per shell.
const ROW_TIMEOUT: Duration = Duration::from_secs(10);

const ZSH: &str = "/bin/zsh";

struct Row {
    name: &'static str,
    script: &'static str,
}

macro_rules! rows {
    ($($name:ident => $script:expr),* $(,)?) => {
        &[ $( Row { name: stringify!($name), script: $script } ),* ]
    };
}

// ── The corpus ──────────────────────────────────────────────────────

#[rustfmt::skip]
const ROWS: &[Row] = rows![
    // ── baseline sanity ─────────────────────────────────────────────
    echo_plain                  => "echo hi\n",
    echo_dquoted_spaces         => "echo \"a  b\"\n",
    echo_squoted               => "echo 'a $b c'\n",
    param_plain                 => "a=1\necho $a\n",
    param_braced                => "a=1\necho ${a}\n",
    param_in_dquotes            => "a=1\necho \"[$a]\"\n",

    // ── 1. command substitution inside double quotes ────────────────
    dq_cmdsub_bare              => "echo \"$(echo hi)\"\n",
    dq_cmdsub_prefixed          => "echo \"x$(echo hi)\"\n",
    dq_cmdsub_suffixed          => "echo \"$(echo hi)x\"\n",
    dq_cmdsub_assigned          => "v=\"$(echo hi)\"\necho $v\n",
    dq_cmdsub_two               => "echo \"$(echo a)-$(echo b)\"\n",
    dq_cmdsub_multiword         => "echo \"$(echo a b c)\"\n",
    dq_cmdsub_inner_squotes     => "echo \"$(echo 'a)b')\"\n",
    dq_cmdsub_inner_dquotes     => "echo \"$(printf %s \"export D=1\")\"\n",
    dq_cmdsub_nested            => "echo \"$(echo \"$(echo deep)\")\"\n",
    dq_cmdsub_trailing_newlines => "v=\"$(printf 'a\\n\\n\\n')\"\necho \"[$v]\"\n",
    // The frostmourne dirty-marker shape: `-n` must test the OUTPUT, never
    // the literal `$(...)` text.
    test_n_empty_cmdsub         => "if [ -n \"$(printf %s '')\" ]; then echo dirty; else echo clean; fi\n",
    test_n_nonempty_cmdsub      => "if [ -n \"$(printf %s x)\" ]; then echo dirty; else echo clean; fi\n",
    unquoted_cmdsub_still_works => "echo $(echo hi)\n",

    // ── the hang regression ─────────────────────────────────────────
    // `eval` + a command substitution inside double quotes whose inner text
    // itself contains double quotes. MUST complete inside ROW_TIMEOUT.
    eval_cmdsub_inner_dquotes   => "eval \"$(printf %s \"export D=1\")\"\necho \"[$D]\"\n",
    eval_cmdsub_plain           => "eval \"$(echo export E=1)\"\necho \"[$E]\"\n",

    // ── 2. arithmetic expansion inside double quotes ────────────────
    dq_arith_literal            => "echo \"$((2+2))\"\n",
    dq_arith_spaced             => "a=5\necho \"[$(( a * 2 ))]\"\n",
    dq_arith_nested_parens      => "echo \"$(( (1+2) * 3 ))\"\n",
    dq_arith_dollar_var         => "a=5\necho \"[$(($a + 1))]\"\n",
    unquoted_arith_still_works  => "echo $((2+2))\n",

    // ── 3. $'…' ANSI-C quoting ──────────────────────────────────────
    ansi_c_plain                => "x=$'hello'\necho \"$x\"\n",
    ansi_c_newline_len          => "printf %s $'a\\nb' | wc -c\n",
    ansi_c_tab                  => "printf %s $'a\\tb' | od -An -c\n",
    ansi_c_escapes              => "printf '%s|' $'\\\\' $'\\'' $'\\\"' $'\\a' $'\\b' $'\\f' $'\\v' | od -An -c\n",
    ansi_c_hex                  => "printf %s $'\\x41\\x42' \necho\n",
    ansi_c_octal                => "printf %s $'\\101\\102'\necho\n",
    ansi_c_unicode_short        => "printf %s $'\\u0041'\necho\n",
    ansi_c_unicode_long         => "printf %s $'\\U00000041'\necho\n",
    ansi_c_esc                  => "printf %s $'\\e' | od -An -c\n",
    ansi_c_esc_capital          => "printf %s $'\\E' | od -An -c\n",
    ansi_c_nul_terminates       => "printf %s $'a\\0b' | od -An -c\n",
    ansi_c_unknown_escape       => "printf %s $'a\\qb'\necho\n",
    ansi_c_in_dquotes_is_literal=> "echo \"$'a'\"\n",
    // The direnv shape: `export VAR=$'…'` fed through eval.
    ansi_c_export_via_eval      => "eval \"export DIRX=$'/tmp/a b'\"\necho \"[$DIRX]\"\n",
    ansi_c_concat_literal       => "x=$'a'b\necho \"[$x]\"\n",

    // ── 4. adjacent expansions in a BARE assignment word ────────────
    assign_two_braces           => "a=AB\nb=CD\nx=${a}${b}\necho \"[$x]\"\n",
    assign_brace_then_literal   => "a=AB\ny=${a}Z\necho \"[$y]\"\n",
    assign_literal_then_brace   => "a=AB\ny=Z${a}\necho \"[$y]\"\n",
    assign_two_cmdsubs          => "x=$(echo A)$(echo B)\necho \"[$x]\"\n",
    assign_brace_then_cmdsub    => "a=AB\nx=${a}$(echo Z)\necho \"[$x]\"\n",
    assign_brace_then_arith     => "a=AB\nx=${a}$((1+1))\necho \"[$x]\"\n",
    assign_var_then_var         => "a=AB\nb=CD\nx=$a$b\necho \"[$x]\"\n",
    assign_arith_then_literal   => "x=$((1+1))Z\necho \"[$x]\"\n",
    argv_two_braces             => "a=AB\nb=CD\necho \"[${a}${b}]\"\n",
    argv_brace_then_literal     => "a=AB\necho ${a}Z\n",

    // ── 5. unquoted $(…) is word-split ──────────────────────────────
    split_cmdsub_set            => "set -- $(echo a b c)\necho $#\n",
    split_cmdsub_printf         => "printf '[%s]' $(echo 1 2)\necho\n",
    split_cmdsub_for            => "for w in $(echo x y); do echo \"<$w>\"; done\n",
    split_cmdsub_newlines       => "set -- $(printf 'a\\nb\\nc\\n')\necho $#\n",
    quoted_cmdsub_not_split     => "set -- \"$(echo a b c)\"\necho $#\n",
    // zsh does NOT split unquoted PARAMETER expansion (SH_WORD_SPLIT off).
    // Do not "fix" this direction.
    param_not_split             => "v=\"a b\"\nset -- $v\necho $#\n",
    param_braced_not_split      => "v=\"a b\"\nset -- ${v}\necho $#\n",
    assign_cmdsub_not_split     => "x=$(echo a b c)\necho \"[$x]\"\n",

    // ── general regression surface ──────────────────────────────────
    dquote_empty_is_one_arg     => "set -- \"\"\necho $#\n",
    dquote_escapes              => "echo \"a\\\"b\"\n",
    nested_braces_in_dquotes    => "a=x\necho \"${a:-fallback}\"\n",
    default_op_with_cmdsub      => "unset q\necho \"${q:-$(echo dflt)}\"\n",
    pipeline_with_dq_cmdsub     => "echo \"$(echo a b)\" | tr ' ' '-'\n",
    param_with_path_suffix      => "d=/tmp\necho $d/x\n",
    param_with_colon_suffix     => "p=/bin\necho $p:/usr/bin\n",
    squote_suppresses_expansion => "echo '$notavar $(echo x) $((1+1))'\n",
    keyword_as_argument         => "echo done fi then esac\n",
    trailing_lone_backslash     => "echo a\necho \\",

    // ── mined from the abandoned first attempt at this work ─────────
    ansi_c_empty                => "x=$''\necho \"[$x]\"\n",
    ansi_c_escaped_quote        => "echo $'a\\'b'\n",
    // `printf %s`, not `echo` — zsh's `echo` builtin runs its OWN escape
    // pass, so `echo $'a\\b'` would test two features at once (and frost's
    // echo does not do that pass; see `echo_reprocesses_escapes` below).
    ansi_c_escaped_backslash    => "printf %s $'a\\\\b' | od -An -c\n",
    ansi_c_octal_leading_zero   => "echo $'\\0101'\n",
    ansi_c_unicode_accent       => "echo $'\\u00e9'\n",
    ansi_c_unicode_astral_len   => "printf %s $'\\U0001F600' | wc -c\n",
    ansi_c_no_param_expansion   => "x=VAL\necho $'[$x]'\n",
    ansi_c_space_preserved      => "x=$'a b'\necho \"[$x]\"\n",
    ansi_c_cr_len               => "printf %s $'a\\rb' | wc -c\n",
    adj_three_braces            => "a=A\nb=B\nc=C\nx=${a}${b}${c}\necho \"[$x]\"\n",
    adj_var_then_brace          => "a=AB\nb=CD\nx=$a${b}\necho \"[$x]\"\n",
    adj_two_pattern_ops         => "t=12.34\nx=${t%.*}${t#*.}\necho \"[$x]\"\n",
    adj_cmdsub_then_literal     => "x=$(echo AB)Z\necho \"[$x]\"\n",
    adj_argv_brace_then_dquote  => "a=AB\necho ${a}\"Z\"\n",
    pattern_strip_longest_prefix=> "p=/x/y/z.txt\necho ${p##*/}\n",
    pattern_strip_longest_suffix=> "p=a.b.c\necho ${p%%.*}\n",
    pattern_replace_first       => "a=a.b.c\necho ${a/./-}\n",
    pattern_replace_all         => "a=a.b.c\necho ${a//./-}\n",
    pattern_fixed_width_suffix  => "f=abcdefgh\necho ${f%??????}\n",
    param_length                => "a=abc\necho ${#a}\n",
    cmdsub_with_var_inside      => "n=world\necho \"hello $(echo $n)\"\n",
    cmdsub_stderr_redirect      => "echo \"$(echo oops 1>&2; echo ok)\"\n",
    cmdsub_nested_dollar_brace  => "a=AB\necho \"$(echo ${a})\"\n",
    cmdsub_empty_is_empty       => "if [ -z \"$(printf %s '')\" ]; then echo empty; fi\n",
];

/// Recorded, exercised, and reported — never asserted. These are known,
/// separate gaps; a row here must still terminate and must not crash.
#[rustfmt::skip]
const KNOWN_DIVERGENCES: &[Row] = rows![
    // zsh array-style substring on a scalar: zsh prints `bcd`, frost prints
    // the whole value. Subscript-range parsing is unimplemented.
    scalar_subscript_range      => "a=abcdef\necho ${a[2,4]}\n",

    // A REDIRECT TARGET gets no expansion at all — not `$var`, not `$(…)`,
    // not `~`. `frost_exec::redirect::resolve_word` walks only Literal and
    // SingleQuoted parts and `tracing::warn!`s away everything else, so
    // `cat < $f` opens the empty path. An `apply_redirects_expanded` exists
    // beside it and is called from NOWHERE; the five `apply_redirects` call
    // sites run post-`fork()` where no `ExpandEnv` is in reach.
    //
    // Left unfixed deliberately: the fix belongs in the executor (resolve
    // each target to a Literal *before* forking, the way
    // `resolve_process_subs` already rewrites process substitutions), and
    // heredoc targets must be exempted from it — a heredoc's "target" is its
    // delimiter, not a filename. That is a separate change to the exec path,
    // not one of the five expansion bugs this corpus was cut for.
    redirect_target_unexpanded  => "printf 'hi\\n' > redirsrc\nf=redirsrc\ncat < $f\n",

    // zsh's `echo` builtin interprets backslash escapes in its ARGUMENTS by
    // default (BSD_ECHO off): `echo 'a\bc'` emits a backspace. frost's echo
    // passes them through. Found while writing this corpus — it is why the
    // `$'…'` rows above use `printf %s` and `od` rather than `echo`.
    echo_reprocesses_escapes    => "echo 'a\\bc' | od -An -c\n",

    // `~/x` expands to a literal `~/x`: the lexer emits `~` then Word(`/x`),
    // and the parser hands the whole `/x` to `WordPart::Tilde` as a
    // USERNAME, which renders back as `~/x`. Same family as the `$d/xx`
    // fix above (a word boundary the lexer does not draw), but on the tilde
    // side, where the split rule is different — `~user`, `~+`, `~-` and
    // named directories all have to be told apart first.
    tilde_slash_path            => "echo ~/frost_tilde_probe\n",
];

// ── Harness ─────────────────────────────────────────────────────────

struct RunResult {
    stdout: String,
    timed_out: bool,
}

/// Run `program script_path` with stdin at /dev/null, bounded by
/// [`ROW_TIMEOUT`]. On expiry the child is killed and `timed_out` is set —
/// the caller treats that as a hard failure.
fn run_script(program: &Path, script: &Path, cwd: &Path) -> RunResult {
    let mut child = Command::new(program)
        .arg(script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", program.display()));

    // Drain stdout on a helper thread so a child that fills the pipe buffer
    // cannot deadlock the poll loop below.
    let mut pipe = child.stdout.take().expect("stdout piped");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        use std::io::Read;
        let _ = pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + ROW_TIMEOUT;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("try_wait failed: {e}"),
        }
    }

    let buf = reader.join().unwrap_or_default();
    RunResult {
        stdout: String::from_utf8_lossy(&buf).into_owned(),
        timed_out,
    }
}

fn frost_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // test binary name
    path.pop(); // deps/
    path.push("frost");
    path
}

fn write_script(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(format!("{name}.sh"));
    let mut f = std::fs::File::create(&path).expect("create script");
    f.write_all(script.as_bytes()).expect("write script");
    path
}

struct Verdict {
    name: &'static str,
    matched: bool,
    frost_timed_out: bool,
    detail: String,
}

fn run_corpus(rows: &[Row], dir: &Path) -> Vec<Verdict> {
    let frost = frost_bin();
    rows.iter()
        .map(|row| {
            let path = write_script(dir, row.name, row.script);
            let z = run_script(Path::new(ZSH), &path, dir);
            let f = run_script(&frost, &path, dir);
            assert!(
                !z.timed_out,
                "row `{}`: zsh itself timed out — the row is bad, not frost",
                row.name
            );
            let matched = !f.timed_out && z.stdout == f.stdout;
            let detail = if f.timed_out {
                format!("  {:<28} TIMEOUT (>{ROW_TIMEOUT:?})", row.name)
            } else if matched {
                String::new()
            } else {
                format!(
                    "  {:<28} zsh={:?}  frost={:?}",
                    row.name, z.stdout, f.stdout
                )
            };
            Verdict {
                name: row.name,
                matched,
                frost_timed_out: f.timed_out,
                detail,
            }
        })
        .collect()
}

/// Skip (not fail) when there is no zsh to compare against, so a Linux CI
/// runner without `/bin/zsh` still passes.
fn zsh_available() -> bool {
    if Path::new(ZSH).exists() {
        return true;
    }
    eprintln!("SKIP: {ZSH} not present — differential corpus not run");
    false
}

#[test]
fn frost_matches_zsh_on_the_corpus() {
    if !zsh_available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("frost-diff-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let verdicts = run_corpus(ROWS, &dir);
    let total = verdicts.len();
    let passed = verdicts.iter().filter(|v| v.matched).count();
    let failures: Vec<&Verdict> = verdicts.iter().filter(|v| !v.matched).collect();

    eprintln!("zsh differential corpus: {passed}/{total} rows match");
    for f in &failures {
        eprintln!("{}", f.detail);
    }

    let _ = std::fs::remove_dir_all(&dir);

    // A hang is its own, louder failure — name it separately so a wedged
    // parser never reads as "just another mismatch".
    let hung: Vec<&str> = failures
        .iter()
        .filter(|v| v.frost_timed_out)
        .map(|v| v.name)
        .collect();
    assert!(hung.is_empty(), "frost HUNG on rows: {hung:?}");

    assert!(
        failures.is_empty(),
        "{}/{total} corpus rows diverge from zsh",
        failures.len()
    );
}

/// Known gaps: exercised so they cannot hang or crash, reported, not asserted.
#[test]
fn known_divergences_are_recorded_not_asserted() {
    if !zsh_available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("frost-diff-known-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let verdicts = run_corpus(KNOWN_DIVERGENCES, &dir);
    for v in &verdicts {
        if v.matched {
            eprintln!(
                "known divergence `{}` now MATCHES — promote it to ROWS",
                v.name
            );
        } else {
            eprintln!(
                "known divergence (documented, not a failure):\n{}",
                v.detail
            );
        }
    }
    let hung: Vec<&str> = verdicts
        .iter()
        .filter(|v| v.frost_timed_out)
        .map(|v| v.name)
        .collect();

    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        hung.is_empty(),
        "frost HUNG on known-divergence rows: {hung:?}"
    );
}
