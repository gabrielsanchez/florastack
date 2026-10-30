//! Workspace tooling, run as `cargo xtask <command>` (the alias is in `.cargo/config.toml`). It may depend on no
//! FloraStack crate: it checks the workspace from the outside, through `cargo metadata` and the manifests themselves.

mod bench;
mod branding;
mod ci;
mod corpus;
mod layers;
mod quality;
mod sha256;

use std::path::Path;
use std::process::{Command, ExitCode};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  check-layers  check every Cargo.toml against the crate dependency allowlist (RULES in xtask/src/layers.rs),
                check that every crate inherits the workspace lints, that test-only crates and features are
                used from [dev-dependencies] only, and that the binaries named in branding.toml exist
  ci            run what CI runs (.github/workflows/ci.yml): cargo fmt --check, clippy, check-layers, the tests,
                the quality gate, cargo deny and cargo audit, stopping at the first failure, then print a summary
                that lists the tests skipped for want of hardware or corpus files (cargo-deny and cargo-audit must
                be installed)
  corpus        download the sample focus stacks pinned in corpus/manifest.toml (sha256-verified) and check
                the local stacks in corpus/local/ (`cargo xtask corpus --help` for the options)
  bench         run the criterion benchmarks and write target/bench-summary.json, with the quality metrics
                that benches record (`cargo xtask bench --help` for the options)
  quality       stack every synthetic and corpus stack by every method and slab combination, score and time
                each run, write an HTML report with 100% crops to target/quality/index.html, and fail when a
                measure is worse than quality/baseline.json beyond its tolerance (`--gate` in CI; `--help`)
  help          show this message
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["check-layers"] => cargo_metadata().and_then(|meta| layers::run(&meta)),
        ["check-layers", ..] => Err("check-layers takes no arguments".to_owned()),
        ["ci"] => cargo_metadata().and_then(|meta| ci::run(&meta)),
        ["ci", ..] => Err("ci takes no arguments".to_owned()),
        ["corpus", rest @ ..] => workspace_root().and_then(|root| corpus::run(&root, rest)),
        ["bench", rest @ ..] => cargo_metadata().and_then(|meta| bench::run(&meta, rest)),
        ["quality", rest @ ..] => cargo_metadata().and_then(|meta| quality::run(&meta, rest)),
        [] | ["help" | "-h" | "--help"] => {
            print!("{USAGE}");
            Ok(())
        }
        _ => Err(format!("unknown command `{}`\n\n{USAGE}", args.join(" "))),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A command for the cargo that runs xtask (`cargo` sets `CARGO` for the programs it runs).
fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

/// The workspace root: xtask lives one directory below it.
fn workspace_root() -> Result<std::path::PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).ok_or_else(|| "xtask has no parent directory".to_owned())
}

/// The workspace members as `cargo metadata` describes them (no dependency resolution).
fn cargo_metadata() -> Result<serde_json::Value, String> {
    // xtask lives one directory below the workspace root.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
    let output = cargo()
        .args(["metadata", "--format-version", "1", "--no-deps", "--manifest-path"])
        .arg(&manifest)
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&output.stderr)));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("cannot parse the output of cargo metadata: {e}"))
}
