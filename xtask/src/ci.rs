//! `cargo xtask ci`: runs locally what CI runs (`.github/workflows/ci.yml`), in the same order, stops at the first
//! failing step, and prints a summary of each step's result and time and of the tests that were skipped for want of
//! hardware or corpus files.
//!
//! libtest has no "skipped" status, so a test that can't run here records why with `florastack_testkit::skip!` and
//! passes. The record goes to the file that `FLORASTACK_SKIP_LOG` names; this command points it at
//! `target/ci/skips.jsonl` and lists what that file holds. Adapted from LightCraft's `cargo xtask ci` and its
//! `xtask/src/ci_report.rs` (MIT OR Apache-2.0; see ATTRIBUTION.md and NOTICE).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;

/// The environment variable that `florastack-testkit` appends skip records to (`florastack_testkit::SKIP_LOG_ENV`).
pub const SKIP_LOG_ENV: &str = "FLORASTACK_SKIP_LOG";

/// A CI step: one cargo command.
pub struct Step {
    pub name: &'static str,
    /// The arguments after `cargo`, exactly as `.github/workflows/ci.yml` runs them.
    pub args: &'static [&'static str],
    /// For a cargo subcommand that isn't part of the toolchain, the crate that installs it.
    pub tool: Option<&'static str>,
}

impl Step {
    /// The command line, spelled as in the workflow file.
    pub fn command(&self) -> String {
        std::iter::once("cargo").chain(self.args.iter().copied()).collect::<Vec<_>>().join(" ")
    }
}

/// The step that runs the tests, the only one whose skips the summary can report.
const TEST_STEP: &str = "test";

/// Everything CI runs, in its order: the `check` job (on every OS), then the `dependencies` job. A test keeps this list
/// and the workflow's `run:` lines the same.
pub const STEPS: &[Step] = &[
    Step { name: "fmt", args: &["fmt", "--all", "--check"], tool: None },
    Step { name: "clippy", args: &["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"], tool: None },
    Step { name: "check-layers", args: &["xtask", "check-layers"], tool: None },
    Step { name: "fixtures", args: &["xtask", "corpus", "--fixtures"], tool: None },
    Step { name: TEST_STEP, args: &["test", "--workspace", "--locked"], tool: None },
    Step { name: "deny", args: &["deny", "check"], tool: Some("cargo-deny") },
    Step { name: "audit", args: &["audit"], tool: Some("cargo-audit") },
];

/// How a step went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Passed(Duration),
    /// The time it took and what went wrong.
    Failed(Duration, String),
    /// An earlier step failed.
    NotRun,
}

/// One recorded skip.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Skip {
    pub need: String,
    pub reason: String,
    pub test: String,
    pub binary: String,
    pub location: String,
}

/// The contents of a skip log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Skips {
    /// Sorted and deduplicated.
    pub list: Vec<Skip>,
    /// Lines that aren't a JSON object.
    pub unreadable: usize,
}

/// `cargo xtask ci`, for the workspace that `meta` (the output of `cargo metadata`) describes.
pub fn run(meta: &Value) -> Result<(), String> {
    let root = PathBuf::from(meta["workspace_root"].as_str().ok_or("cargo metadata: no workspace_root")?);
    let target = meta["target_directory"].as_str().map_or_else(|| root.join("target"), PathBuf::from);
    let log = prepare_log(&target.join("ci"))?;
    let started = Instant::now();
    let mut outcomes: Vec<(&Step, Outcome)> = Vec::with_capacity(STEPS.len());
    let mut failed = false;
    for step in STEPS {
        if failed {
            outcomes.push((step, Outcome::NotRun));
            continue;
        }
        eprintln!("\n=== ci: {} ===\n$ {}", step.name, step.command());
        let t = Instant::now();
        let outcome = match run_step(step, &root, &log) {
            Ok(()) => Outcome::Passed(t.elapsed()),
            Err(e) => {
                failed = true;
                Outcome::Failed(t.elapsed(), e)
            }
        };
        outcomes.push((step, outcome));
    }

    // The home directory as the tests' own paths spell it: on Windows a shell may set HOME to a Unix-style path.
    let home = if cfg!(windows) { std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) } else { std::env::var_os("HOME") };
    let home = home.map(|h| h.to_string_lossy().into_owned());
    let skips = match std::fs::read_to_string(&log) {
        Ok(text) => parse_skips(&text, &root, home.as_deref()),
        // No test recorded a skip.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Skips::default(),
        Err(e) => {
            eprintln!("ci: cannot read the skip log {}: {e}", log.display());
            Skips::default()
        }
    };
    let shown_log = log.strip_prefix(&root).unwrap_or(&log).display().to_string().replace('\\', "/");
    print!("{}", summary(&outcomes, &skips, started.elapsed(), &shown_log));
    match outcomes.iter().find(|(_, outcome)| matches!(outcome, Outcome::Failed(..))) {
        Some((step, _)) => Err(format!("ci failed at {}", step.name)),
        None => Ok(()),
    }
}

/// Creates `dir` and removes the last run's skip log from it, returning the log's path.
fn prepare_log(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let log = dir.join("skips.jsonl");
    match std::fs::remove_file(&log) {
        Ok(()) => Ok(log),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(log),
        Err(e) => Err(format!("cannot remove the last run's {}: {e}", log.display())),
    }
}

/// Runs `step` in `root`, with the skip log set for the tests.
fn run_step(step: &Step, root: &Path, log: &Path) -> Result<(), String> {
    if let (Some(tool), Some(subcommand)) = (step.tool, step.args.first()) {
        // Without the tool, cargo answers `no such command` and fails.
        let installed = crate::cargo()
            .arg(subcommand)
            .arg("--version")
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !installed {
            return Err(format!("{tool} is not installed: `cargo install --locked {tool}` (CI uses the version in .github/workflows/ci.yml)"));
        }
    }
    let status = crate::cargo().args(step.args).current_dir(root).env(SKIP_LOG_ENV, log).status().map_err(|e| format!("cannot run cargo: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("exited with {status}")) }
}

/// Reads a skip log: one JSON object per line, as `florastack-testkit` writes them. Reasons are shortened with
/// [`shorten_paths`].
pub fn parse_skips(text: &str, root: &Path, home: Option<&str>) -> Skips {
    let mut skips = Skips::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(record) if record.is_object() => {
                let field = |key: &str| record[key].as_str().unwrap_or_default().to_owned();
                skips.list.push(Skip {
                    need: field("need"),
                    reason: shorten_paths(&field("reason"), root, home),
                    test: field("test"),
                    binary: field("binary"),
                    location: field("location"),
                });
            }
            _ => skips.unreadable += 1,
        }
    }
    skips.list.sort();
    skips.list.dedup();
    skips
}

/// `s` with the workspace root cut from the paths in it, the home directory written as `~` and `dir/..` folded, so
/// that the summary stays short and can be pasted into an issue without the local user name.
fn shorten_paths(s: &str, root: &Path, home: Option<&str>) -> String {
    let mut out = s.to_owned();
    let root = root.display().to_string();
    if !root.is_empty() {
        for separator in ['/', '\\'] {
            out = out.replace(&format!("{root}{separator}"), "");
        }
        out = out.replace(&root, ".");
    }
    if let Some(home) = home.filter(|home| home.len() > 1) {
        out = out.replace(home, "~");
    }
    out.split(' ').map(fold_parent_dirs).collect::<Vec<_>>().join(" ")
}

/// A path token with each `dir/..` removed, lexically, and `/` as the separator
/// (`crates\align\../../corpus/bee` -> `corpus/bee`). A token without a `..` component, or a leading `..`, stays.
fn fold_parent_dirs(token: &str) -> String {
    let parts: Vec<&str> = token.split(['/', '\\']).collect();
    if !parts.contains(&"..") {
        return token.to_owned();
    }
    let mut kept: Vec<&str> = Vec::with_capacity(parts.len());
    for part in parts {
        if part == ".." && kept.last().is_some_and(|last| *last != ".." && !last.is_empty()) {
            kept.pop();
        } else {
            kept.push(part);
        }
    }
    kept.join("/")
}

/// How many tests the summary names per reason; the log has the rest.
const SHOWN_PER_REASON: usize = 5;

/// The summary printed at the end of the run. `log` is the skip log's path as shown to the reader.
pub fn summary(steps: &[(&Step, Outcome)], skips: &Skips, total: Duration, log: &str) -> String {
    let mut s = String::new();
    let failed = steps.iter().find(|(_, outcome)| matches!(outcome, Outcome::Failed(..)));
    let _ = match failed {
        None => writeln!(s, "\nCI summary: all {} steps passed in {}", steps.len(), duration(total)),
        Some((step, _)) => writeln!(s, "\nCI summary: failed at {} after {}", step.name, duration(total)),
    };
    let width = steps.iter().map(|(step, _)| step.name.len()).max().unwrap_or(0);
    for (step, outcome) in steps {
        let (status, time, note) = match outcome {
            Outcome::Passed(t) => ("ok", duration(*t), String::new()),
            Outcome::Failed(t, e) => ("FAIL", duration(*t), format!(" ({e})")),
            Outcome::NotRun => ("not run", String::new(), String::new()),
        };
        let _ = writeln!(s, "  {status:<7}  {:<width$}  {time:>7}  {}{note}", step.name, step.command());
    }

    if skips.list.is_empty() {
        let test_ran = steps.iter().any(|(step, outcome)| step.name == TEST_STEP && *outcome != Outcome::NotRun);
        let _ = writeln!(s, "Skipped tests: {}", if test_ran { "none recorded" } else { "unknown, the test step did not run" });
    } else {
        let _ = writeln!(s, "Skipped tests: {}, which passed without running their checks (records in {log}):", skips.list.len());
        for group in group(&skips.list) {
            let _ = writeln!(s, "  {} ({}): needs {}", group.need, group.count, hint(group.need));
            for (reason, tests) in &group.reasons {
                let _ = writeln!(s, "    {reason} ({}):", tests.len());
                for test in tests.iter().take(SHOWN_PER_REASON) {
                    let _ = writeln!(s, "      {}  {}", test_name(test), test.location);
                }
                if tests.len() > SHOWN_PER_REASON {
                    let _ = writeln!(s, "      and {} more", tests.len() - SHOWN_PER_REASON);
                }
            }
        }
    }
    if skips.unreadable > 0 {
        let _ = writeln!(s, "  {} line(s) of {log} could not be read", skips.unreadable);
    }
    s
}

/// The skips of one need.
struct Group<'a> {
    need: &'a str,
    count: usize,
    /// Each reason with its tests, largest group first (ties by reason).
    reasons: Vec<(&'a str, Vec<&'a Skip>)>,
}

/// The skips grouped by need and then by reason, largest group first (ties by name).
fn group(skips: &[Skip]) -> Vec<Group<'_>> {
    let mut needs: BTreeMap<&str, BTreeMap<&str, Vec<&Skip>>> = BTreeMap::new();
    for skip in skips {
        needs.entry(&skip.need).or_default().entry(&skip.reason).or_default().push(skip);
    }
    let mut groups: Vec<Group<'_>> = needs
        .into_iter()
        .map(|(need, reasons)| {
            let mut reasons: Vec<(&str, Vec<&Skip>)> = reasons.into_iter().collect();
            reasons.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
            Group { need, count: reasons.iter().map(|(_, tests)| tests.len()).sum(), reasons }
        })
        .collect();
    groups.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.need.cmp(b.need)));
    groups
}

/// What it takes to run the tests skipped for `need`: one entry per `florastack_testkit::Need`.
fn hint(need: &str) -> &'static str {
    match need {
        "gpu" => "a GPU adapter that passes the self-test",
        "corpus" => "files in the gitignored corpus/ folder, which `cargo xtask corpus` fetches",
        _ => "what the reasons name",
    }
}

/// `binary::test`, or the binary alone when the test's thread had no test name (libtest names each test's thread).
fn test_name(skip: &Skip) -> String {
    if skip.test.is_empty() || skip.test == "main" { skip.binary.clone() } else { format!("{}::{}", skip.binary, skip.test) }
}

/// `12.3 s`, or `2m 05s` from a minute up.
fn duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 { format!("{:.1} s", d.as_secs_f64()) } else { format!("{}m {:02}s", secs / 60, secs % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workflow's single-line `run:` commands that start with `cargo`, in file order.
    fn workflow_commands(workflow: &str) -> Vec<&str> {
        workflow.lines().filter_map(|line| line.trim().strip_prefix("run: ")).map(str::trim).filter(|run| run.starts_with("cargo ")).collect()
    }

    #[test]
    fn ci_runs_exactly_what_the_workflow_runs() {
        let workflow = include_str!("../../.github/workflows/ci.yml");
        let steps: Vec<String> = STEPS.iter().map(Step::command).collect();
        assert_eq!(workflow_commands(workflow), steps, "keep STEPS and .github/workflows/ci.yml in step (one cargo command per `run:` line)");
        assert!(STEPS.iter().any(|step| step.name == TEST_STEP));
    }

    fn step(name: &str) -> &'static Step {
        STEPS.iter().find(|step| step.name == name).expect("a CI step")
    }

    fn skip(need: &str, reason: &str, test: &str) -> Skip {
        Skip {
            need: need.into(),
            reason: reason.into(),
            test: test.into(),
            binary: "agreement".into(),
            location: "crates/gpu/tests/agreement.rs:9".into(),
        }
    }

    #[test]
    fn the_skip_log_is_parsed_deduplicated_and_shortened() {
        let root = Path::new("/work/florastack");
        let gpu = r#"{"need":"gpu","reason":"no Metal adapter","test":"warp","binary":"agreement","location":"crates/gpu/tests/agreement.rs:9"}"#;
        let corpus = r#"{"need":"corpus","reason":"/work/florastack/corpus/local/bee and /home/ann/stacks are missing","test":"tests::bee","binary":"florastack_align","location":"crates/align/src/lib.rs:20"}"#;
        let skips = parse_skips(&format!("{gpu}\n{corpus}\n\nnot json\n[1]\n{gpu}\n"), root, Some("/home/ann"));
        assert_eq!(skips.unreadable, 2);
        assert_eq!(skips.list.len(), 2, "{skips:?}");
        assert_eq!(skips.list[0].need, "corpus", "sorted by need");
        assert_eq!(skips.list[0].reason, "corpus/local/bee and ~/stacks are missing");
        assert_eq!(skips.list[1], skip("gpu", "no Metal adapter", "warp"));
    }

    #[test]
    fn windows_paths_are_shortened_too() {
        let root = Path::new(r"C:\work\florastack");
        assert_eq!(shorten_paths(r"C:\work\florastack\crates\align\../../corpus/local/bee is missing", root, None), "corpus/local/bee is missing");
        assert_eq!(shorten_paths(r"run in C:\work\florastack", root, None), "run in .");
        assert_eq!(shorten_paths(r"C:\Users\ann\stacks", root, Some(r"C:\Users\ann")), r"~\stacks");
    }

    #[test]
    fn parent_directories_fold_in_path_tokens_only() {
        assert_eq!(fold_parent_dirs("crates/align/../../corpus/bee"), "corpus/bee");
        assert_eq!(fold_parent_dirs(r"crates\align\../../corpus"), "corpus");
        assert_eq!(fold_parent_dirs("../corpus/bee"), "../corpus/bee");
        assert_eq!(fold_parent_dirs("/../corpus"), "/../corpus");
        assert_eq!(fold_parent_dirs(r"C:\corpus\bee"), r"C:\corpus\bee", "no `..`: unchanged");
        assert_eq!(fold_parent_dirs("missing"), "missing");
        assert_eq!(shorten_paths("a/../b  and  c", Path::new("/w"), None), "b  and  c", "spacing is kept");
    }

    #[test]
    fn a_passing_run_lists_every_step_and_the_skips_by_need_and_reason() {
        let steps: Vec<(&Step, Outcome)> = STEPS.iter().map(|step| (step, Outcome::Passed(Duration::from_millis(1240)))).collect();
        let mut list = vec![skip("corpus", "corpus/local is empty", "bee")];
        list.extend((0..7).map(|i| skip("gpu", "no Metal adapter", &format!("kernel_{i}"))));
        list.push(skip("gpu", "the adapter failed the self-test", "main"));
        let text = summary(&steps, &Skips { list, unreadable: 0 }, Duration::from_secs(125), "target/ci/skips.jsonl");
        assert!(text.contains("CI summary: all 7 steps passed in 2m 05s"), "{text}");
        assert!(text.contains("  ok       clippy          1.2 s  cargo clippy --workspace --all-targets --locked -- -D warnings\n"), "{text}");
        assert!(text.contains("Skipped tests: 9, which passed without running their checks (records in target/ci/skips.jsonl):"), "{text}");
        let gpu = text.find("  gpu (8): needs a GPU adapter that passes the self-test\n").expect("the gpu group");
        let corpus =
            text.find("  corpus (1): needs files in the gitignored corpus/ folder, which `cargo xtask corpus` fetches\n").expect("the corpus group");
        assert!(gpu < corpus, "the larger group comes first: {text}");
        assert!(text.contains("    no Metal adapter (7):\n      agreement::kernel_0  crates/gpu/tests/agreement.rs:9\n"), "{text}");
        assert!(text.contains("      and 2 more\n"), "{text}");
        assert!(!text.contains("kernel_5"), "{text}");
        assert!(text.contains("    the adapter failed the self-test (1):\n      agreement  crates/gpu/tests/agreement.rs:9\n"), "{text}");
    }

    #[test]
    fn a_failed_run_names_the_failure_and_the_steps_it_did_not_run() {
        let steps = vec![
            (step("fmt"), Outcome::Passed(Duration::from_millis(400))),
            (step("clippy"), Outcome::Failed(Duration::from_secs(61), "exited with exit code: 101".into())),
            (step("check-layers"), Outcome::NotRun),
            (step("test"), Outcome::NotRun),
        ];
        let text = summary(&steps, &Skips::default(), Duration::from_secs(62), "target/ci/skips.jsonl");
        assert!(text.contains("CI summary: failed at clippy after 1m 02s"), "{text}");
        assert!(
            text.contains(
                "  FAIL     clippy         1m 01s  cargo clippy --workspace --all-targets --locked -- -D warnings (exited with exit code: 101)"
            ),
            "{text}"
        );
        assert!(text.contains("  not run  test                   cargo test --workspace --locked\n"), "{text}");
        assert!(text.contains("Skipped tests: unknown, the test step did not run"), "{text}");
        let ran = vec![(step("test"), Outcome::Passed(Duration::ZERO))];
        let text = summary(&ran, &Skips { list: Vec::new(), unreadable: 1 }, Duration::ZERO, "log");
        assert!(text.contains("Skipped tests: none recorded\n  1 line(s) of log could not be read"), "{text}");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(Duration::from_millis(240)), "0.2 s");
        assert_eq!(duration(Duration::from_secs_f64(59.94)), "59.9 s");
        assert_eq!(duration(Duration::from_secs(60)), "1m 00s");
        assert_eq!(duration(Duration::from_secs(3725)), "62m 05s");
    }
}
