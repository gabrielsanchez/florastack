//! `cargo xtask bench`: runs the criterion benchmarks and writes `target/bench-summary.json`, one machine-readable
//! record of the run that later steps compare their performance with.
//!
//! criterion saves each benchmark's estimates in `<target>/criterion/<id>/new/` (`benchmark.json` and
//! `estimates.json`, times in nanoseconds). The summary takes the benchmarks whose estimates this run wrote, so stale
//! results of renamed or deleted benchmarks never leak in, and adds what the numbers depend on: git revision, rustc,
//! OS, architecture and CPU count.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub const USAGE: &str = "\
usage: cargo xtask bench [--quick] [-p PACKAGE]... [FILTER] [-- CRITERION_ARGS...]

Runs the criterion benchmarks (`cargo bench --bench '*'`: every bench target of the workspace, or of the packages
named) and writes target/bench-summary.json, with criterion's mean, median, standard deviation and throughput for
each benchmark measured in this run, and the git revision, rustc version and machine they were measured on.

  --quick              criterion's --quick mode: sample only until the estimate is significant
  -p, --package NAME   only the benches of this package (repeatable)
  FILTER               only the benchmarks whose id contains FILTER
  -- ARGS              pass ARGS on to criterion (see `cargo bench --bench synth -- --help`)
  -h, --help           show this message
";

/// The summary's file name in the target directory.
pub const SUMMARY: &str = "bench-summary.json";
/// The summary format; bump it when a field changes meaning.
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, PartialEq)]
struct Options<'a> {
    help: bool,
    quick: bool,
    packages: Vec<&'a str>,
    filter: Option<&'a str>,
    criterion: Vec<&'a str>,
}

fn parse_args<'a>(args: &[&'a str]) -> Result<Options<'a>, String> {
    let mut options = Options::default();
    let mut it = args.iter();
    while let Some(&arg) = it.next() {
        match arg {
            "-h" | "--help" => options.help = true,
            "--quick" => options.quick = true,
            "-p" | "--package" => options.packages.push(it.next().ok_or("--package needs a package name")?),
            "--" => options.criterion.extend(it.by_ref()),
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            filter if options.filter.is_none() => options.filter = Some(filter),
            extra => return Err(format!("one filter only, not also `{extra}`")),
        }
    }
    Ok(options)
}

/// `cargo xtask bench`, for the workspace that `meta` (the output of `cargo metadata`) describes.
pub fn run(meta: &Value, args: &[&str]) -> Result<(), String> {
    let options = parse_args(args)?;
    if options.help {
        print!("{USAGE}");
        return Ok(());
    }
    let root = PathBuf::from(meta["workspace_root"].as_str().ok_or("cargo metadata: no workspace_root")?);
    let target = meta["target_directory"].as_str().map_or_else(|| root.join("target"), PathBuf::from);
    let criterion_dir = std::env::var_os("CRITERION_HOME").map_or_else(|| target.join("criterion"), PathBuf::from);

    let mut cargo_args: Vec<&str> = vec!["bench"];
    if options.packages.is_empty() {
        cargo_args.push("--workspace");
    }
    for &package in &options.packages {
        cargo_args.extend(["--package", package]);
    }
    // Bench targets only: the library harnesses would reject criterion's options.
    cargo_args.extend(["--bench", "*", "--", "--noplot"]);
    if options.quick {
        cargo_args.push("--quick");
    }
    cargo_args.extend(&options.criterion);
    cargo_args.extend(options.filter);
    eprintln!("$ cargo {}", cargo_args.iter().map(|a| if *a == "*" { "'*'" } else { a }).collect::<Vec<_>>().join(" "));
    let started = SystemTime::now();
    let status = crate::cargo().args(&cargo_args).current_dir(&root).status().map_err(|e| format!("cannot run cargo bench: {e}"))?;
    if !status.success() {
        return Err(format!("cargo bench exited with {status}"));
    }

    // Some file systems store modification times coarsely.
    let since = started.checked_sub(Duration::from_secs(2)).unwrap_or(UNIX_EPOCH);
    let benchmarks = collect(&criterion_dir, since)?;
    if benchmarks.is_empty() {
        return Err(format!("cargo bench ran, but no benchmark wrote its estimates under {} in this run", criterion_dir.display()));
    }
    let summary = json!({
        "schema_version": SCHEMA_VERSION,
        "tool": "cargo xtask bench",
        "created_unix_s": SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        "git": git(&root),
        "rustc": command_line(&root, "rustc", &["-V"]),
        "host": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "logical_cpus": std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        },
        "options": { "quick": options.quick, "packages": options.packages, "filter": options.filter, "criterion_args": options.criterion },
        "benchmarks": benchmarks,
    });
    let path = target.join(SUMMARY);
    let text = serde_json::to_string_pretty(&summary).map_err(|e| format!("cannot write the summary: {e}"))?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    print!("{}", table(&benchmarks));
    println!("bench: {} benchmark(s) summarized in {}", benchmarks.len(), path.display());
    Ok(())
}

/// The summary entries of the benchmarks whose estimates were written at or after `since`, sorted by id.
fn collect(criterion_dir: &Path, since: SystemTime) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut dirs = vec![criterion_dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                dirs.push(entry.path());
            }
        }
        if dir.file_name().is_none_or(|name| name != "new") {
            continue;
        }
        let estimates_path = dir.join("estimates.json");
        let fresh = std::fs::metadata(&estimates_path).and_then(|m| m.modified()).is_ok_and(|modified| modified >= since);
        if !fresh {
            continue;
        }
        let read = |path: &Path| -> Result<Value, String> {
            let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
        };
        let benchmark = read(&dir.join("benchmark.json"))?;
        let estimates = read(&estimates_path)?;
        out.push(entry(&benchmark, &estimates).ok_or_else(|| format!("{}: not the criterion format this xtask reads", dir.display()))?);
    }
    out.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(out)
}

/// One benchmark's summary entry from criterion's `benchmark.json` and `estimates.json`.
fn entry(benchmark: &Value, estimates: &Value) -> Option<Value> {
    let id = benchmark["full_id"].as_str()?;
    let point = |key: &str| estimates[key]["point_estimate"].as_f64();
    let mean = point("mean")?;
    let interval = &estimates["mean"]["confidence_interval"];
    Some(json!({
        "id": id,
        "group": benchmark["group_id"],
        "function": benchmark["function_id"],
        "parameter": benchmark["value_str"],
        "mean_ns": mean,
        "mean_ci_ns": [interval["lower_bound"], interval["upper_bound"]],
        "confidence_level": interval["confidence_level"],
        "median_ns": point("median"),
        "std_dev_ns": point("std_dev"),
        "slope_ns": point("slope"),
        "throughput": throughput(&benchmark["throughput"], mean),
    }))
}

/// criterion's throughput (`{"Elements": n}`, `{"Bytes": n}`, `{"BytesDecimal": n}` or
/// `{"ElementsAndBytes": {"elements": n, "bytes": m}}`) with its rate at `mean_ns` per iteration.
fn throughput(value: &Value, mean_ns: f64) -> Value {
    let Some((kind, amount)) = value.as_object().and_then(|object| object.iter().next()) else {
        return Value::Null;
    };
    let (unit, per_iteration) = match kind.as_str() {
        "Elements" => ("elements", amount.as_u64()),
        "Bytes" | "BytesDecimal" => ("bytes", amount.as_u64()),
        "ElementsAndBytes" => ("elements", amount["elements"].as_u64()),
        _ => return Value::Null,
    };
    let Some(per_iteration) = per_iteration else {
        return Value::Null;
    };
    let per_second = if mean_ns > 0.0 { per_iteration as f64 / (mean_ns * 1e-9) } else { 0.0 };
    json!({ "unit": unit, "per_iteration": per_iteration, "per_second": per_second })
}

/// The revision benchmarked, and whether tracked files had uncommitted changes.
fn git(root: &Path) -> Value {
    let revision = command_line(root, "git", &["rev-parse", "HEAD"]);
    let dirty = command_line(root, "git", &["status", "--porcelain", "--untracked-files=no"]).map(|status| !status.is_empty());
    json!({ "revision": revision, "dirty": dirty })
}

/// The trimmed standard output of a successful command, if it ran.
fn command_line(root: &Path, program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program).args(args).current_dir(root).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The printed table: id, mean time with its relative spread, and throughput.
fn table(benchmarks: &[Value]) -> String {
    let width = benchmarks.iter().filter_map(|b| b["id"].as_str()).map(str::len).max().unwrap_or(0);
    let mut s = String::new();
    for b in benchmarks {
        let mean = b["mean_ns"].as_f64().unwrap_or(0.0);
        let spread = b["std_dev_ns"].as_f64().filter(|_| mean > 0.0).map_or(String::new(), |sd| format!("±{:.1}%", 100.0 * sd / mean));
        let rate = b["throughput"]["per_second"].as_f64().map_or(String::new(), |rate| {
            let unit = if b["throughput"]["unit"] == "bytes" { "B" } else { "elem" };
            format!("{}{unit}/s", si(rate))
        });
        let _ = writeln!(s, "  {:<width$}  {:>10}  {spread:>7}  {rate}", b["id"].as_str().unwrap_or("?"), time(mean));
    }
    s
}

/// A time in nanoseconds in the largest unit that keeps it at least 1.
fn time(ns: f64) -> String {
    match ns {
        n if n < 1e3 => format!("{n:.1} ns"),
        n if n < 1e6 => format!("{:.1} µs", n / 1e3),
        n if n < 1e9 => format!("{:.1} ms", n / 1e6),
        n => format!("{:.2} s", n / 1e9),
    }
}

/// A rate with an SI prefix: `12.3 M`.
fn si(v: f64) -> String {
    match v {
        v if v >= 1e9 => format!("{:.2} G", v / 1e9),
        v if v >= 1e6 => format!("{:.2} M", v / 1e6),
        v if v >= 1e3 => format!("{:.2} k", v / 1e3),
        v => format!("{v:.1} "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BENCHMARK: &str = r#"{"group_id":"synth","function_id":"noisy_dusty","value_str":"256x256x8","throughput":{"Elements":524288},"full_id":"synth/noisy_dusty/256x256x8","directory_name":"synth/noisy_dusty/256x256x8","title":"synth/noisy_dusty/256x256x8"}"#;
    const ESTIMATES: &str = r#"{"mean":{"confidence_interval":{"confidence_level":0.95,"lower_bound":220640100.0,"upper_bound":222590600.0},"point_estimate":221615350.0,"standard_error":688016.3},"median":{"confidence_interval":{"confidence_level":0.95,"lower_bound":220640100.0,"upper_bound":222590600.0},"point_estimate":221615350.0,"standard_error":688016.3},"median_abs_dev":{"confidence_interval":{"confidence_level":0.95,"lower_bound":0.0,"upper_bound":1445905.6},"point_estimate":1445905.6,"standard_error":722948.7},"slope":null,"std_dev":{"confidence_interval":{"confidence_level":0.95,"lower_bound":0.0,"upper_bound":1379211.7},"point_estimate":1379211.7,"standard_error":689602.0}}"#;

    #[test]
    fn criterion_results_become_summary_entries() {
        let e = entry(&serde_json::from_str(BENCHMARK).unwrap(), &serde_json::from_str(ESTIMATES).unwrap()).unwrap();
        assert_eq!(e["id"], "synth/noisy_dusty/256x256x8");
        assert_eq!((&e["group"], &e["function"], &e["parameter"]), (&json!("synth"), &json!("noisy_dusty"), &json!("256x256x8")));
        assert_eq!(e["mean_ns"], 221_615_350.0);
        assert_eq!(e["mean_ci_ns"], json!([220_640_100.0, 222_590_600.0]));
        assert_eq!(e["slope_ns"], Value::Null);
        assert_eq!(e["throughput"]["unit"], "elements");
        let rate = e["throughput"]["per_second"].as_f64().unwrap();
        assert!((rate - 524_288.0 / 0.221_615_35).abs() < 1.0, "{rate}");
        assert!(entry(&json!({}), &serde_json::from_str(ESTIMATES).unwrap()).is_none());
    }

    #[test]
    fn every_throughput_kind_is_read() {
        assert_eq!(throughput(&json!({"Bytes": 1000}), 1e9)["per_second"], 1000.0);
        assert_eq!(throughput(&json!({"BytesDecimal": 10}), 1e9)["unit"], "bytes");
        assert_eq!(throughput(&json!({"ElementsAndBytes": {"elements": 4, "bytes": 16}}), 2e9)["per_second"], 2.0);
        assert_eq!(throughput(&Value::Null, 1.0), Value::Null);
        assert_eq!(throughput(&json!({"Pixels": 3}), 1.0), Value::Null);
    }

    #[test]
    fn only_results_written_in_this_run_are_collected() {
        let dir = std::env::temp_dir().join(format!("florastack-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (id, old) in [("g/new_one", false), ("g/old_one", true)] {
            let new = dir.join(id).join("new");
            std::fs::create_dir_all(&new).unwrap();
            std::fs::write(
                new.join("benchmark.json"),
                BENCHMARK.replace("synth/noisy_dusty/256x256x8\",\"directory", &format!("{id}\",\"directory")),
            )
            .unwrap();
            std::fs::write(new.join("estimates.json"), ESTIMATES).unwrap();
            if old {
                let file = std::fs::File::options().write(true).open(new.join("estimates.json")).unwrap();
                file.set_modified(SystemTime::now() - Duration::from_secs(3600)).unwrap();
            }
        }
        let found = collect(&dir, SystemTime::now() - Duration::from_secs(60)).unwrap();
        let missing = collect(&dir.join("nothing-here"), UNIX_EPOCH).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0]["id"], "g/new_one");
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn arguments_and_formatting() {
        let o = parse_args(&["--quick", "-p", "florastack-metrics", "ssim", "--", "--sample-size", "10"]).unwrap();
        assert_eq!(
            o,
            Options { help: false, quick: true, packages: vec!["florastack-metrics"], filter: Some("ssim"), criterion: vec!["--sample-size", "10"] }
        );
        assert!(parse_args(&["a", "b"]).unwrap_err().contains("one filter only"));
        assert!(parse_args(&["--fast"]).is_err());
        assert!(parse_args(&["--package"]).is_err());
        assert_eq!((time(12.0), time(1.5e4), time(2.5e7), time(3.2e9)), ("12.0 ns".into(), "15.0 µs".into(), "25.0 ms".into(), "3.20 s".into()));
        assert_eq!(si(2.5e6), "2.50 M");
    }
}
