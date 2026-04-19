//! CLI entry point for frost-verify.

use std::path::PathBuf;
use std::process;

use clap::Parser;

use frost_verify::manifest::Manifest;
use frost_verify::merkle;
use frost_verify::trace;
use frost_verify::verify;

#[derive(Parser)]
#[command(
    name = "frost-verify",
    version,
    about = "Verify shell configuration loading order and integrity"
)]
struct Cli {
    /// Path to the manifest file
    #[arg(
        long,
        default_value = "~/.config/shell/manifest.json",
        value_name = "PATH"
    )]
    manifest: String,

    /// Path to the trace file
    #[arg(
        long,
        default_value = "~/.local/state/shell/trace.jsonl",
        value_name = "PATH"
    )]
    trace: String,

    /// Re-compute BLAKE3 of all files on disk (no trace needed)
    #[arg(long)]
    rehash: bool,

    /// Output results as JSON
    #[arg(long)]
    json: bool,

    /// Show per-file details
    #[arg(long, short)]
    verbose: bool,

    /// Exit non-zero on warnings (unexpected files, missing deferred)
    #[arg(long)]
    strict: bool,

    /// Print the Merkle attestation root and exit (tameshi-compatible).
    #[arg(long)]
    root: bool,
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn main() {
    let cli = Cli::parse();

    let manifest_path = expand_tilde(&cli.manifest);
    let manifest = match Manifest::load(&manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("frost-verify: {e}");
            process::exit(2);
        }
    };

    if cli.root {
        match merkle::compute_manifest_root(&manifest.entries) {
            Ok(root) => {
                println!("{}", merkle::encode_hex(&root));
                process::exit(0);
            }
            Err(e) => {
                eprintln!("frost-verify: {e}");
                process::exit(2);
            }
        }
    }

    let report = if cli.rehash {
        verify::rehash(&manifest)
    } else {
        let trace_path = expand_tilde(&cli.trace);
        let trace_entries = match trace::load_trace(&trace_path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("frost-verify: {e}");
                process::exit(2);
            }
        };
        verify::verify(&manifest, &trace_entries)
    };

    if cli.json {
        println!("{}", report.to_json());
    } else {
        eprint!("{}", report.display(cli.verbose));
    }

    let exit_code = match report.verdict {
        verify::Verdict::Pass => 0,
        verify::Verdict::Warn => {
            if cli.strict {
                1
            } else {
                0
            }
        }
        verify::Verdict::Fail => 1,
    };

    process::exit(exit_code);
}
