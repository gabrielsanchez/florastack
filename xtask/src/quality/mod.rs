//! `cargo xtask quality`: the quality report and the regression gate.
//!
//! xtask depends on no FloraStack crate, so the work is done by a runner it builds and runs: the `quality` example of
//! florastack-engine (`crates/engine/examples/quality/`), which drives the engine's commands as the apps do. xtask
//! lists the stacks, runs the runner once per stack (each in a process of its own, so that one stack's memory is never
//! another's), reads what it wrote, compares it with the baseline (`gate.rs`) and writes the report (`report.rs`).
//!
//! - **Stacks.** The synthetic stacks (the runner's `synthetic.rs`, which the runner lists), on one thread, so that
//!   their peak memory is the same everywhere; the manifest's downloaded stacks (`corpus/<name>`) and the local ones
//!   (`local/<name>`), on every core.
//! - **Modes.** Without one, every stack in a release build: the full report, compared with the baseline. `--gate`
//!   runs what the baseline has and is here (in CI, the synthetic stacks) in the baseline's build profile, the dev
//!   profile, whose build `cargo test` has already made. `--update-baseline` runs the same and writes the baseline.
//!   Every mode fails when something is worse than the baseline by more than its tolerance.

pub mod gate;
mod report;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use gate::{Baseline, Comparison, Host, MEASURES, Outcome, StackResult, State};

pub const USAGE: &str = "\
usage: cargo xtask quality [--gate | --update-baseline] [--release | --dev] [--stack PATTERN]... [--run PATTERN]...
                           [--settings JSON] [--threads N] [--no-local] [--out DIR]

Stacks every stack by every method (weighted average, pyramid, depth map) and every pair of slab and final methods,
through the engine's commands (florastack-engine's `quality` example), scores the outputs against their truth where
there is one (SSIM, PSNR, halo, noise, color shift, out-of-range pixels, alignment error), measures each command's
runtime and peak heap, and writes target/quality/index.html: every run's measures against the baseline, and 100% crops
of the same regions of every output side by side. It fails when a measure is worse than quality/baseline.json by more
than its tolerance (runtimes only against a baseline made on this machine with this build profile).

  (no mode)          every stack: the synthetic ones, the manifest's downloaded ones and the local ones in corpus/local
                     (release build)
  --gate             only what the baseline has (the synthetic stacks, and the manifest's when downloaded), in the
                     baseline's build profile: what CI runs
  --update-baseline  run what --gate runs and write quality/baseline.json from it, keeping its tolerances; commit it
                     with the change that moves what it measures
  --release, --dev   the build profile (the dev profile is the tests', with the pixel and codec crates optimized)
  --stack PATTERN    only the stacks whose id contains PATTERN (`synthetic/layered-clean`, `local/wasp-jpg`; repeatable)
  --run PATTERN      only the runs whose name contains PATTERN (`pyramid`, `slabbed-pyramid-`; repeatable)
  --settings JSON    `stack.set`'s parameters for every stack, to see a setting against the baseline:
                     --settings '{\"pyramid\": {\"grit\": 0}}'
  --threads N        threads for the real stacks (default: one per core); the synthetic stacks run on one
  --no-local         leave out the local stacks
  --out DIR          where the report goes (default: target/quality)
  -h, --help         show this message
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Report,
    Gate,
    Update,
}

#[derive(Debug, PartialEq)]
struct Options<'a> {
    help: bool,
    mode: Mode,
    release: Option<bool>,
    stacks: Vec<&'a str>,
    runs: Vec<&'a str>,
    settings: Option<&'a str>,
    threads: usize,
    local: bool,
    out: Option<&'a str>,
}

fn parse_args<'a>(args: &[&'a str]) -> Result<Options<'a>, String> {
    let mut o = Options {
        help: false,
        mode: Mode::Report,
        release: None,
        stacks: Vec::new(),
        runs: Vec::new(),
        settings: None,
        threads: 0,
        local: true,
        out: None,
    };
    let mut it = args.iter();
    while let Some(&arg) = it.next() {
        let mut value = |name: &str| it.next().copied().ok_or_else(|| format!("{name} needs a value"));
        match arg {
            "-h" | "--help" => o.help = true,
            "--gate" => set_mode(Mode::Gate, &mut o)?,
            "--update-baseline" => set_mode(Mode::Update, &mut o)?,
            "--release" => o.release = Some(true),
            "--dev" => o.release = Some(false),
            "--stack" => o.stacks.push(value(arg)?),
            "--run" => o.runs.push(value(arg)?),
            "--settings" => o.settings = Some(value(arg)?),
            "--threads" => o.threads = value(arg)?.parse().map_err(|_| "--threads needs a number".to_owned())?,
            "--no-local" => o.local = false,
            "--out" => o.out = Some(value(arg)?),
            other => return Err(format!("unknown argument `{other}`\n\n{USAGE}")),
        }
    }
    if o.mode == Mode::Update && (!o.stacks.is_empty() || !o.runs.is_empty() || o.settings.is_some()) {
        return Err("--update-baseline measures every stack of the baseline at the defaults: it takes no --stack, --run or --settings".to_owned());
    }
    Ok(o)
}

fn set_mode(mode: Mode, o: &mut Options<'_>) -> Result<(), String> {
    if o.mode != Mode::Report && o.mode != mode {
        return Err("--gate and --update-baseline exclude each other".to_owned());
    }
    o.mode = mode;
    Ok(())
}

/// A stack's id as the file name the runner gives its result: `synthetic/layered-clean` is `synthetic--layered-clean`.
fn file_name(id: &str) -> String {
    id.replace('/', "--")
}

/// `cargo xtask quality`, for the workspace that `meta` (the output of `cargo metadata`) describes.
pub fn run(meta: &Value, args: &[&str]) -> Result<(), String> {
    let options = parse_args(args)?;
    if options.help {
        print!("{USAGE}");
        return Ok(());
    }
    let root = PathBuf::from(meta["workspace_root"].as_str().ok_or("cargo metadata: no workspace_root")?);
    let target = meta["target_directory"].as_str().map_or_else(|| root.join("target"), PathBuf::from);
    let out = options.out.map_or_else(|| target.join("quality"), PathBuf::from);
    let baseline_path = root.join(gate::BASELINE);
    let baseline = match std::fs::read_to_string(&baseline_path) {
        Ok(text) => Some(Baseline::parse(&text).map_err(|e| format!("{}: {e}", baseline_path.display()))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", baseline_path.display())),
    };
    if options.mode == Mode::Gate && baseline.is_none() {
        return Err(format!("there is no baseline at {}: `cargo xtask quality --update-baseline` writes one", gate::BASELINE));
    }
    let release = options.release.unwrap_or(match options.mode {
        Mode::Report => true,
        Mode::Gate => baseline.as_ref().is_some_and(|b| b.profile == "release"),
        Mode::Update => false,
    });
    let profile = if release { "release" } else { "dev" };
    let settings: Option<Value> =
        options.settings.map(serde_json::from_str::<Value>).transpose().map_err(|e| format!("--settings is not JSON: {e}"))?;

    let runner = build(&root, release)?;
    let stacks = select(&options, &root, &runner, baseline.as_ref())?;
    let outcomes = run_stacks(&runner, &root, &out, &stacks, &options, settings.as_ref())?;

    let host = Host::this();
    let selected = |run: &str| options.runs.is_empty() || options.runs.iter().any(|p| run.contains(p));
    let comparison = baseline.as_ref().map(|b| gate::compare(b, &outcomes, &selected, b.same_machine(profile, &host)));
    let product = crate::branding::load(&root)?.display_name;
    let git = crate::bench::git(&root);
    let created = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let info = report::Info {
        product: &product,
        command: std::iter::once("cargo xtask quality").chain(args.iter().copied()).collect::<Vec<_>>().join(" "),
        profile,
        host: &host,
        revision: git["revision"].as_str().map(str::to_owned),
        dirty: git["dirty"].as_bool(),
        created_unix_s: created,
        settings: settings.as_ref(),
        baseline: baseline.as_ref(),
    };
    let page = out.join("index.html");
    std::fs::write(&page, report::html(&info, &outcomes, comparison.as_ref())).map_err(|e| format!("cannot write {}: {e}", page.display()))?;
    write_summary(&out, &info, &outcomes, comparison.as_ref())?;
    print!("{}", tables(&outcomes, comparison.as_ref()));
    println!("quality: report in {}", page.display());

    if options.mode == Mode::Update {
        let updated = gate::updated(baseline.as_ref(), &outcomes, profile, &host)?;
        let text = serde_json::to_string_pretty(&updated).map_err(|e| e.to_string())?;
        if let Some(dir) = baseline_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        std::fs::write(&baseline_path, text + "\n").map_err(|e| format!("cannot write {}: {e}", baseline_path.display()))?;
        println!("quality: wrote {} with {} runs; commit it with the change it measures", gate::BASELINE, updated.runs.len());
        return Ok(());
    }
    match &comparison {
        None => {
            println!("quality: no baseline to compare with ({} is missing)", gate::BASELINE);
            Ok(())
        }
        Some(c) if c.passed() => {
            println!("quality: the gate passes ({} runs compared with the baseline)", c.checks.len());
            Ok(())
        }
        Some(c) => {
            for failure in &c.failures {
                println!("  FAIL  {failure}");
            }
            Err(format!(
                "the quality gate failed: {} problem(s), in {}. If the change is meant, run `cargo xtask quality --update-baseline` and \
                 commit {} with it",
                c.failures.len(),
                page.display(),
                gate::BASELINE
            ))
        }
    }
}

/// The stacks to run, as ids and plan sources: the synthetic ones, then the real ones the mode takes, those matching
/// `--stack`.
fn select(options: &Options<'_>, root: &Path, runner: &Path, baseline: Option<&Baseline>) -> Result<Vec<(String, Value)>, String> {
    let mut stacks = synthetic_stacks(runner, root)?;
    let (real, missing) = crate::corpus::real_stacks(root, options.mode == Mode::Report && options.local)?;
    for stack in real {
        let in_baseline = baseline.is_some_and(|b| b.runs.keys().any(|run| gate::stack_of(run) == stack.id));
        if options.mode == Mode::Gate && !in_baseline {
            continue;
        }
        stacks.push((stack.id, json!({ "files": { "frames": stack.frames, "reference": stack.reference } })));
    }
    stacks.retain(|(id, _)| options.stacks.is_empty() || options.stacks.iter().any(|p| id.contains(p)));
    if stacks.is_empty() {
        return Err(format!("no stack matches {}", options.stacks.join(", ")));
    }
    if options.mode == Mode::Report {
        for line in &missing {
            eprintln!("quality: not run: {line}");
        }
    }
    Ok(stacks)
}

/// Runs the runner on each stack, in a process of its own, and reads what it wrote into `out`.
fn run_stacks(
    runner: &Path,
    root: &Path,
    out: &Path,
    stacks: &[(String, Value)],
    options: &Options<'_>,
    settings: Option<&Value>,
) -> Result<Vec<Outcome>, String> {
    for dir in ["results", "crops", "plans", "work"] {
        remove_dir(&out.join(dir))?;
    }
    let plans = out.join("plans");
    std::fs::create_dir_all(&plans).map_err(|e| format!("cannot create {}: {e}", plans.display()))?;
    let mut outcomes = Vec::with_capacity(stacks.len());
    for (id, source) in stacks {
        let threads = if id.starts_with("synthetic/") { 1 } else { options.threads };
        let plan = json!({ "id": id, "source": source, "out": out, "threads": threads, "settings": settings, "runs": options.runs });
        let plan_path = plans.join(format!("{}.json", file_name(id)));
        std::fs::write(&plan_path, plan.to_string()).map_err(|e| format!("cannot write {}: {e}", plan_path.display()))?;
        let status = Command::new(runner).arg(&plan_path).current_dir(root).status().map_err(|e| format!("cannot run {}: {e}", runner.display()))?;
        let path = out.join("results").join(format!("{}.json", file_name(id)));
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()));
        let result = if status.success() {
            text.and_then(|text| serde_json::from_str::<StackResult>(&text).map_err(|e| format!("{}: {e}", path.display())))
        } else {
            // The runner writes why it failed, when it can.
            let reason = text.ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["error"].as_str().map(str::to_owned));
            Err(reason.unwrap_or_else(|| format!("the runner exited with {status}")))
        };
        outcomes.push(Outcome { id: id.clone(), result });
    }
    Ok(outcomes)
}

/// Builds the runner with `cargo build` and returns its executable.
fn build(root: &Path, release: bool) -> Result<PathBuf, String> {
    let mut args = vec!["build", "--package", "florastack-engine", "--example", "quality", "--locked", "--message-format=json-render-diagnostics"];
    if release {
        args.push("--release");
    }
    eprintln!("$ cargo {}", args.join(" "));
    let output =
        crate::cargo().args(&args).current_dir(root).stderr(Stdio::inherit()).output().map_err(|e| format!("cannot run cargo build: {e}"))?;
    if !output.status.success() {
        return Err(format!("building the quality runner failed ({})", output.status));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "quality")
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| "cargo built the quality runner but named no executable".to_owned())
}

/// The runner's synthetic stacks, as ids and plan sources.
fn synthetic_stacks(runner: &Path, root: &Path) -> Result<Vec<(String, Value)>, String> {
    let output = Command::new(runner).arg("list").current_dir(root).output().map_err(|e| format!("cannot run {}: {e}", runner.display()))?;
    if !output.status.success() {
        return Err(format!("`{} list` failed: {}", runner.display(), String::from_utf8_lossy(&output.stderr)));
    }
    let list: Value = serde_json::from_slice(&output.stdout).map_err(|e| format!("`quality list`: {e}"))?;
    let names = list.as_array().ok_or("`quality list` printed no list")?.iter().filter_map(|s| s["name"].as_str());
    Ok(names.map(|name| (format!("synthetic/{name}"), json!({ "synthetic": name }))).collect())
}

fn remove_dir(dir: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("cannot remove {}: {e}", dir.display())),
        _ => Ok(()),
    }
}

/// `results.json` beside the page: everything it shows, for scripts.
fn write_summary(out: &Path, info: &report::Info<'_>, outcomes: &[Outcome], comparison: Option<&Comparison>) -> Result<(), String> {
    let stacks: Vec<Value> = outcomes
        .iter()
        .map(|o| match &o.result {
            Ok(result) => serde_json::to_value(result).unwrap_or(Value::Null),
            Err(e) => json!({ "id": o.id, "error": e }),
        })
        .collect();
    let summary = json!({
        "tool": info.command,
        "created_unix_s": info.created_unix_s,
        "git": { "revision": info.revision, "dirty": info.dirty },
        "profile": info.profile,
        "host": info.host,
        "settings": info.settings,
        "stacks": stacks,
        "gate": comparison.map(|c| json!({
            "passed": c.passed(),
            "failures": c.failures,
            "improvements": c.improvements,
            "unbaselined": c.unbaselined,
            "not_run": c.not_run,
            "runtime_compared": c.runtime_compared,
        })),
    });
    let path = out.join("results.json");
    let text = serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The printed tables: each stack's runs and measures, a measure worse than the baseline beyond its tolerance marked
/// `!`, one better marked `+`.
fn tables(outcomes: &[Outcome], comparison: Option<&Comparison>) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for outcome in outcomes {
        let result = match &outcome.result {
            Ok(result) => result,
            Err(e) => {
                let _ = writeln!(s, "\n{}: {e}", outcome.id);
                continue;
            }
        };
        let _ = writeln!(s, "\n{} ({} frames, {}x{}, {} thread(s))", result.id, result.frames, result.width, result.height, result.threads);
        let shown: Vec<_> = MEASURES.iter().filter(|m| result.runs.iter().any(|r| r.measures().contains_key(m.key))).collect();
        let width = result.runs.iter().map(|r| r.run.len()).max().unwrap_or(0).max(3);
        let _ = write!(s, "  {:<width$}", "run");
        for m in &shown {
            let _ = write!(s, " {:>10}", m.name);
        }
        s.push('\n');
        for run in &result.runs {
            let _ = write!(s, "  {:<width$}", run.run);
            if let Some(e) = &run.error {
                let _ = writeln!(s, "  failed: {e}");
                continue;
            }
            let measures = run.measures();
            let checks = comparison.and_then(|c| c.checks.get(&run.id));
            for m in &shown {
                let mark = match checks.and_then(|c| c.get(m.key)).map(|c| c.state) {
                    Some(State::Regressed) => '!',
                    Some(State::Improved) => '+',
                    _ => ' ',
                };
                let digits = m.digits;
                match measures.get(m.key) {
                    Some(v) => {
                        let _ = write!(s, " {v:>9.digits$}{mark}");
                    }
                    None => s.push_str("           "),
                }
            }
            s.push('\n');
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments() {
        let o = parse_args(&["--gate", "--stack", "synthetic/", "--run", "pyramid", "--settings", "{}", "--threads", "4", "--no-local"]).unwrap();
        assert_eq!(
            o,
            Options {
                help: false,
                mode: Mode::Gate,
                release: None,
                stacks: vec!["synthetic/"],
                runs: vec!["pyramid"],
                settings: Some("{}"),
                threads: 4,
                local: false,
                out: None
            }
        );
        assert_eq!(parse_args(&[]).unwrap().mode, Mode::Report);
        assert_eq!(parse_args(&["--dev", "--release"]).unwrap().release, Some(true));
        assert!(parse_args(&["--gate", "--update-baseline"]).unwrap_err().contains("exclude"));
        assert!(parse_args(&["--update-baseline", "--settings", "{}"]).unwrap_err().contains("no --stack, --run or --settings"));
        assert!(parse_args(&["--stack"]).unwrap_err().contains("needs a value"));
        assert!(parse_args(&["--threads", "many"]).is_err());
        assert!(parse_args(&["--fast"]).unwrap_err().contains("unknown argument"));
        assert_eq!(file_name("local/wasp-jpg"), "local--wasp-jpg");
    }

    #[test]
    fn the_tables_mark_regressions_and_improvements() {
        let run = |id: &str, ssim: f64| gate::RunResult {
            id: id.into(),
            run: id.rsplit('/').next().unwrap().into(),
            title: "Pyramid".into(),
            runtime_s: 1.0,
            peak_mib: 10.0,
            metrics: [("ssim".to_owned(), ssim)].into(),
            uncovered: None,
            slabs: None,
            slabs_reused: None,
            crops: Vec::new(),
            error: None,
        };
        let outcome = |a: f64, b: f64| Outcome {
            id: "synthetic/a".into(),
            result: Ok(StackResult {
                id: "synthetic/a".into(),
                synthetic: true,
                description: None,
                frames: 3,
                width: 10,
                height: 10,
                threads: 1,
                truth: true,
                runs: vec![run("synthetic/a/pyramid", a), run("synthetic/a/average", b)],
                regions: Vec::new(),
                notes: Vec::new(),
            }),
        };
        let host = Host { os: "linux".into(), arch: "x86_64".into(), logical_cpus: 2, cpu: Some("c".into()) };
        let baseline = gate::updated(None, &[outcome(0.9, 0.8)], "dev", &host).unwrap();
        let now = [outcome(0.85, 0.85)];
        let comparison = gate::compare(&baseline, &now, &|_| true, true);
        let text = tables(&now, Some(&comparison));
        assert!(text.contains("synthetic/a (3 frames, 10x10, 1 thread(s))"), "{text}");
        assert!(text.contains("  pyramid    0.8500!"), "{text}");
        assert!(text.contains("  average    0.8500+"), "{text}");
    }
}
