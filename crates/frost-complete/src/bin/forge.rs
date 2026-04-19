//! `frost-complete-forge` — introspect a CLI's completion surface
//! and emit frost-lisp `(def*)` forms ready for `~/.frostrc.lisp`.
//!
//! Usage:
//!
//! ```text
//! frost-complete-forge fish PATH                # parse one fish completion file
//! frost-complete-forge fish-dir DIR             # parse every *.fish in a dir
//! frost-complete-forge tool TOOL                # run TOOL and locate + parse its fish file
//! ```
//!
//! Output goes to stdout. Pipe into your rc layer:
//!
//! ```text
//! frost-complete-forge fish-dir /opt/homebrew/share/fish/completions \
//!   > ~/.config/frost/rc.d/90-completions.lisp
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use frost_complete::{emit_lisp, parse_fish, parse_skim_yaml, ForgeError, ForgeOutput};

/// One-stop error type for the forge binary.
#[derive(Debug)]
enum CliError {
    Io { path: PathBuf, source: std::io::Error },
    Forge(ForgeError),
    NoFishCompletion { tool: String, searched: usize },
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "read {}: {source}", path.display()),
            Self::Forge(e) => write!(f, "{e}"),
            Self::NoFishCompletion { tool, searched } => write!(
                f,
                "no fish completion file found for {tool:?} (searched {searched} paths)",
            ),
        }
    }
}

impl std::error::Error for CliError {}
impl From<ForgeError> for CliError {
    fn from(e: ForgeError) -> Self { Self::Forge(e) }
}
type Result<T> = std::result::Result<T, CliError>;

#[derive(Parser)]
#[command(
    name = "frost-complete-forge",
    version,
    about = "Generate frost-lisp completion specs from CLI introspection"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Parse a fish completion file and emit frost-lisp (def*) forms.
    Fish {
        /// Path to a fish completion file (usually `<TOOL>.fish`).
        path: PathBuf,
    },
    /// Parse every `*.fish` file in a directory — typical layout for
    /// fish's per-package completions. Emits one combined output.
    FishDir {
        /// Directory of fish completion files.
        dir: PathBuf,
    },
    /// Convenience wrapper: find a tool's fish completion file by
    /// searching common locations (`$HOME/.config/fish/completions/`,
    /// `/usr/share/fish/completions/`, Nix-profile fallbacks), parse it,
    /// emit.
    Tool {
        /// Tool name (e.g. `git`, `kubectl`).
        tool: String,
    },
    /// Parse a `pleme-io/skim-tab` curated YAML spec (as found in
    /// `skim-tab/specs/*.yaml`) and emit frost-lisp (def*) forms.
    /// Each entry in the spec's `commands:` list emits a parallel
    /// branch (aliases get their own tree).
    Yaml {
        /// Path to a skim-tab YAML spec.
        path: PathBuf,
    },
    /// Parse every `*.yaml` file in a directory as a skim-tab spec.
    /// Useful for batch-generating Lisp from `pleme-io/skim-tab/specs/`.
    YamlDir {
        /// Directory of skim-tab YAML specs.
        dir: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Fish { path }      => from_file(&path),
        Cmd::FishDir { dir }    => from_dir(&dir),
        Cmd::Tool { tool }      => from_tool(&tool),
        Cmd::Yaml { path }      => from_yaml_file(&path),
        Cmd::YamlDir { dir }    => from_yaml_dir(&dir),
    };
    match result {
        Ok(out) => {
            print!("{}", emit_lisp(&out));
        }
        Err(e) => {
            eprintln!("frost-complete-forge: {e}");
            std::process::exit(1);
        }
    }
}

fn from_file(path: &Path) -> Result<ForgeOutput> {
    let src = std::fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(parse_fish(&src)?)
}

fn from_dir(dir: &Path) -> Result<ForgeOutput> {
    let mut combined = ForgeOutput::default();
    let entries = std::fs::read_dir(dir).map_err(|source| CliError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("fish") {
            continue;
        }
        let src = std::fs::read_to_string(&path).map_err(|source| CliError::Io {
            path: path.clone(),
            source,
        })?;
        let out = parse_fish(&src)?;
        combined.subcmds.extend(out.subcmds);
        combined.flags.extend(out.flags);
        combined.positionals.extend(out.positionals);
    }
    combined.sort();
    Ok(combined)
}

fn from_yaml_file(path: &Path) -> Result<ForgeOutput> {
    let src = std::fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(parse_skim_yaml(&src)?)
}

fn from_yaml_dir(dir: &Path) -> Result<ForgeOutput> {
    let mut combined = ForgeOutput::default();
    let entries = std::fs::read_dir(dir).map_err(|source| CliError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let src = std::fs::read_to_string(&path).map_err(|source| CliError::Io {
            path: path.clone(),
            source,
        })?;
        let out = parse_skim_yaml(&src)?;
        combined.subcmds.extend(out.subcmds);
        combined.flags.extend(out.flags);
        combined.positionals.extend(out.positionals);
    }
    combined.sort();
    Ok(combined)
}

fn from_tool(tool: &str) -> Result<ForgeOutput> {
    let candidates = tool_fish_candidates(tool);
    for path in &candidates {
        if path.is_file() {
            return from_file(path);
        }
    }
    Err(CliError::NoFishCompletion {
        tool: tool.to_string(),
        searched: candidates.len(),
    })
}

fn tool_fish_candidates(tool: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let name = format!("{tool}.fish");

    if let Ok(home) = std::env::var("HOME") {
        out.push(PathBuf::from(&home).join(".config/fish/completions").join(&name));
        // fish vendor completions (user-level)
        out.push(PathBuf::from(&home).join(".local/share/fish/vendor_completions.d").join(&name));
    }

    let system_dirs = [
        "/opt/homebrew/share/fish/completions",
        "/opt/homebrew/share/fish/vendor_completions.d",
        "/usr/local/share/fish/completions",
        "/usr/local/share/fish/vendor_completions.d",
        "/usr/share/fish/completions",
        "/usr/share/fish/vendor_completions.d",
        "/run/current-system/sw/share/fish/completions",
        "/run/current-system/sw/share/fish/vendor_completions.d",
        "/etc/profiles/per-user/drzzln/share/fish/completions",
    ];
    for d in &system_dirs {
        out.push(PathBuf::from(d).join(&name));
    }

    // NIX_PATH-aware fallback: walk the user's PATH once to find fish's
    // own share dir. Bare scan because fish may not be on PATH.
    if let Ok(path) = std::env::var("PATH") {
        for entry in path.split(':') {
            if entry.ends_with("/bin") {
                let share = PathBuf::from(entry.trim_end_matches("/bin")).join("share/fish/completions");
                out.push(share.join(&name));
            }
        }
    }

    out
}

