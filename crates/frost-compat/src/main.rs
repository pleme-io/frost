mod runner;
mod ztst;

use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;

use runner::{Summary, TestResult};

#[derive(Parser)]
#[command(
    name = "frost-compat",
    version,
    about = "Run zsh .ztst compatibility tests against the frost shell"
)]
struct Cli {
    /// Path to directory containing .ztst files.
    test_dir: PathBuf,

    /// Only run tests from files matching these patterns (e.g., "A01" "B01").
    filter: Vec<String>,

    /// Path to the frost binary (default: searches PATH).
    #[arg(long = "frost")]
    frost_path: Option<PathBuf>,

    /// Output results as JSON.
    #[arg(long)]
    json: bool,

    /// Show individual test details.
    #[arg(long)]
    verbose: bool,
}

fn main() {
    let cli = Cli::parse();

    // Resolve the frost binary.
    let frost_path = resolve_frost(&cli.frost_path);

    // Discover .ztst files.
    let test_files = discover_ztst_files(&cli.test_dir, &cli.filter);
    if test_files.is_empty() {
        eprintln!("No .ztst files found in {}", cli.test_dir.display());
        process::exit(1);
    }

    if !cli.json {
        eprintln!(
            "frost-compat: found {} test file(s), using frost at {}",
            test_files.len(),
            frost_path.display()
        );
    }

    let mut all_results: Vec<TestResult> = Vec::new();

    for path in &test_files {
        if !cli.json && cli.verbose {
            eprintln!("\n--- {} ---", path.display());
        }

        match ztst::parse_ztst(path) {
            Ok(tf) => {
                if !cli.json && !cli.verbose {
                    eprint!("{}: ", tf.name);
                }
                let results = runner::run_test_file(&tf, &frost_path, cli.verbose);
                if !cli.json && !cli.verbose {
                    // Print compact status line.
                    let file_summary = runner::summarize(&results);
                    eprintln!(
                        "{}/{} passed",
                        file_summary.passed, file_summary.total
                    );
                }
                all_results.extend(results);
            }
            Err(e) => {
                eprintln!("ERROR parsing {}: {e}", path.display());
            }
        }
    }

    let summary = runner::summarize(&all_results);

    if cli.json {
        print_json(&all_results, &summary);
    } else {
        print_summary(&summary);
    }

    // Exit with non-zero if any tests failed or crashed.
    if summary.failed > 0 || summary.crashed > 0 {
        process::exit(1);
    }
}

/// Resolve the frost binary path.
fn resolve_frost(explicit: &Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        if p.exists() {
            return p.clone();
        }
        eprintln!(
            "WARNING: specified frost path {} does not exist, searching PATH",
            p.display()
        );
    }

    // Search PATH for `frost`.
    if let Ok(path) = which("frost") {
        return path;
    }

    // Try `./target/debug/frost` and `./target/release/frost`.
    for candidate in &["target/debug/frost", "target/release/frost"] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return p;
        }
    }

    eprintln!("ERROR: could not find frost binary. Use --frost <path> or ensure it is in PATH.");
    process::exit(1);
}

/// Simple `which` implementation — find an executable in PATH.
fn which(name: &str) -> Result<PathBuf, ()> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let candidate = Path::new(dir).join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(())
}

/// Discover `.ztst` files in a directory, optionally filtered by patterns.
fn discover_ztst_files(dir: &Path, filters: &[String]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ERROR: cannot read directory {}: {e}", dir.display());
            return files;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("ztst") {
            if filters.is_empty() {
                files.push(path);
            } else {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if filters.iter().any(|f| name.contains(f.as_str())) {
                    files.push(path);
                }
            }
        }
    }

    files.sort();
    files
}

/// Print results and summary as JSON to stdout.
fn print_json(results: &[TestResult], summary: &Summary) {
    #[derive(serde::Serialize)]
    struct JsonReport<'a> {
        results: &'a [TestResult],
        summary: &'a Summary,
    }

    let report = JsonReport { results, summary };
    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("ERROR: failed to serialize JSON: {e}"),
    }
}

/// Print a human-readable summary to stderr.
fn print_summary(summary: &Summary) {
    eprintln!();
    eprintln!("=== Frost Compatibility Summary ===");
    eprintln!("  Total:         {}", summary.total);
    eprintln!("  Passed:        {}", summary.passed);
    eprintln!("  Failed:        {}", summary.failed);
    eprintln!("  Crashed:       {}", summary.crashed);
    eprintln!("  Skipped:       {}", summary.skipped);
    eprintln!("  Parse Errors:  {}", summary.parse_errors);
    eprintln!(
        "  Compatibility: {:.1}%",
        summary.compatibility_pct
    );
    eprintln!("===================================");
}
