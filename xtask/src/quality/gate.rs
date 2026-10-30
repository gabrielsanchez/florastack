//! The regression gate: the baseline in `quality/baseline.json`, and the comparison of a run with it.
//!
//! The baseline holds every run of the stacks anyone can run (the synthetic stacks, and the manifest's downloaded
//! stacks), never a local stack's, which are the user's own. Each of its runs has its metrics, its runtime and its peak
//! memory, and the file has the tolerance of each measure: how far a measure may move the wrong way before the gate
//! fails, the larger of an absolute amount and a share of the baseline's value. Runtime is compared only against a
//! baseline measured on the same machine with the same build profile, since it says nothing across machines, and only
//! a run that takes more than twice as long, and at least half a second longer, fails: one desktop's single-threaded
//! runs of a second vary by up to half under load. Every other measure is the same on every platform within its tolerance (memory because the synthetic stacks
//! run on one thread).
//!
//! A change that makes something worse on purpose updates the baseline in the same commit
//! (`cargo xtask quality --update-baseline`), which keeps the tolerances and replaces the runs: the gate then compares
//! with the new values, and the diff shows what moved.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The baseline's path in the repository.
pub const BASELINE: &str = "quality/baseline.json";
/// The baseline's format; bump it when a field changes meaning.
pub const SCHEMA_VERSION: u32 = 1;

/// Which way a measure gets better.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Better {
    Higher,
    Lower,
}

/// A measure the gate compares.
pub struct Measure {
    pub key: &'static str,
    /// Its name in tables.
    pub name: &'static str,
    pub better: Better,
    /// The decimals it is shown with.
    pub digits: usize,
    /// The tolerance a new baseline starts with.
    pub tolerance: Tolerance,
    /// Compared only with a baseline measured on this machine with this build profile.
    pub machine: bool,
}

/// How far a measure may move the wrong way: the larger of `absolute` and `relative` times the baseline's value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tolerance {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub absolute: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub relative: f64,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes a reference.
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl Tolerance {
    const fn absolute(absolute: f64) -> Tolerance {
        Tolerance { absolute, relative: 0.0 }
    }

    const fn relative(relative: f64, absolute: f64) -> Tolerance {
        Tolerance { absolute, relative }
    }

    /// How far a value may move from `baseline`.
    pub fn allowed(&self, baseline: f64) -> f64 {
        self.absolute.max(self.relative * baseline.abs())
    }
}

/// Every measure, in the order tables show them. The quality metrics come from florastack-metrics (against the truth),
/// `out_of_range` is the share of pixels with a sample outside [0, 1], `align_error` the mean transform error of the
/// aligned frames in pixels, and `rejected` the frames alignment rejected.
pub const MEASURES: &[Measure] = &[
    Measure { key: "ssim", name: "SSIM", better: Better::Higher, digits: 4, tolerance: Tolerance::absolute(0.002), machine: false },
    Measure { key: "psnr", name: "PSNR", better: Better::Higher, digits: 2, tolerance: Tolerance::absolute(0.1), machine: false },
    Measure { key: "halo", name: "halo", better: Better::Lower, digits: 4, tolerance: Tolerance::absolute(0.003), machine: false },
    Measure { key: "noise", name: "noise", better: Better::Lower, digits: 5, tolerance: Tolerance::relative(0.05, 0.0), machine: false },
    Measure { key: "delta_e", name: "ΔE", better: Better::Lower, digits: 3, tolerance: Tolerance::absolute(0.05), machine: false },
    Measure { key: "out_of_range", name: "out", better: Better::Lower, digits: 4, tolerance: Tolerance::absolute(0.001), machine: false },
    Measure { key: "align_error", name: "align px", better: Better::Lower, digits: 3, tolerance: Tolerance::absolute(0.02), machine: false },
    Measure { key: "rejected", name: "rejected", better: Better::Lower, digits: 0, tolerance: Tolerance::absolute(0.0), machine: false },
    Measure { key: "runtime_s", name: "time s", better: Better::Lower, digits: 2, tolerance: Tolerance::relative(1.0, 0.5), machine: true },
    Measure { key: "peak_mib", name: "peak MiB", better: Better::Lower, digits: 1, tolerance: Tolerance::relative(0.1, 0.5), machine: false },
];

pub fn measure(key: &str) -> Option<&'static Measure> {
    MEASURES.iter().find(|m| m.key == key)
}

/// The machine a run was measured on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub os: String,
    pub arch: String,
    pub logical_cpus: usize,
    /// The processor, as the system names it, when it can tell.
    pub cpu: Option<String>,
}

impl Host {
    /// This machine.
    pub fn this() -> Host {
        Host {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            logical_cpus: std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
            cpu: cpu_name(),
        }
    }

    /// `windows x86_64, 16 threads (AMD64 Family 25 ...)`.
    pub fn describe(&self) -> String {
        let cpu = self.cpu.as_deref().map_or(String::new(), |cpu| format!(" ({cpu})"));
        format!("{} {}, {} threads{cpu}", self.os, self.arch, self.logical_cpus)
    }
}

/// The processor's name: `PROCESSOR_IDENTIFIER` on Windows, `/proc/cpuinfo`'s model name on Linux, `sysctl` on macOS.
fn cpu_name() -> Option<String> {
    let name = if cfg!(windows) {
        std::env::var("PROCESSOR_IDENTIFIER").ok()
    } else if cfg!(target_os = "macos") {
        let output = std::process::Command::new("sysctl").args(["-n", "machdep.cpu.brand_string"]).output().ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        info.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')).map(|(_, name)| name.to_owned())
    };
    name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty())
}

/// `quality/baseline.json`.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub schema_version: u32,
    pub about: String,
    /// Each measure's tolerance; a measure missing here has its default.
    pub tolerances: BTreeMap<String, Tolerance>,
    /// The build profile and the machine the runs were measured with.
    pub profile: String,
    pub host: Host,
    /// Each run's measures, by run id (`synthetic/layered-clean/pyramid`).
    pub runs: BTreeMap<String, BTreeMap<String, f64>>,
}

pub const ABOUT: &str = "The quality gate's baseline: written by `cargo xtask quality --update-baseline`, compared with by \
                         `cargo xtask quality --gate` in CI. Update it in the commit that changes what it measures; the \
                         tolerances are kept.";

impl Baseline {
    pub fn parse(text: &str) -> Result<Baseline, String> {
        let baseline: Baseline = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if baseline.schema_version != SCHEMA_VERSION {
            return Err(format!("schema_version is {}, but this xtask reads {SCHEMA_VERSION}", baseline.schema_version));
        }
        if let Some(unknown) = baseline.tolerances.keys().chain(baseline.runs.values().flat_map(BTreeMap::keys)).find(|k| measure(k).is_none()) {
            return Err(format!("`{unknown}` is no measure this xtask knows"));
        }
        Ok(baseline)
    }

    /// The tolerance of `measure`.
    pub fn tolerance(&self, measure: &Measure) -> Tolerance {
        self.tolerances.get(measure.key).copied().unwrap_or(measure.tolerance)
    }

    /// Whether runtimes measured with `profile` on `host` can be compared with this baseline's.
    pub fn same_machine(&self, profile: &str, host: &Host) -> bool {
        self.profile == profile && self.host == *host && host.cpu.is_some()
    }
}

/// What the runner wrote about one stack (examples/quality in florastack-engine).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StackResult {
    pub id: String,
    pub synthetic: bool,
    pub description: Option<String>,
    pub frames: usize,
    pub width: usize,
    pub height: usize,
    pub threads: usize,
    pub truth: bool,
    pub runs: Vec<RunResult>,
    pub regions: Vec<Region>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunResult {
    pub id: String,
    pub run: String,
    pub title: String,
    pub runtime_s: f64,
    pub peak_mib: f64,
    pub metrics: BTreeMap<String, f64>,
    pub uncovered: Option<u64>,
    pub slabs: Option<usize>,
    pub slabs_reused: Option<usize>,
    pub crops: Vec<String>,
    pub error: Option<String>,
}

impl RunResult {
    /// Every measure of the run: its metrics, its runtime and its peak memory.
    pub fn measures(&self) -> BTreeMap<String, f64> {
        let mut all = self.metrics.clone();
        all.insert("runtime_s".to_owned(), self.runtime_s);
        all.insert("peak_mib".to_owned(), self.peak_mib);
        all
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Region {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub why: String,
    pub truth: Option<String>,
}

/// A stack's result, or why the runner failed on it.
pub struct Outcome {
    pub id: String,
    pub result: Result<StackResult, String>,
}

/// Whether a stack's runs belong in the baseline: the synthetic stacks and the manifest's, never a local one.
pub fn shared(stack_id: &str) -> bool {
    stack_id.starts_with("synthetic/") || stack_id.starts_with("corpus/")
}

/// The stack of a run id: everything before its last `/`.
pub fn stack_of(run_id: &str) -> &str {
    run_id.rsplit_once('/').map_or(run_id, |(stack, _)| stack)
}

/// How a measure compares with the baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Within the tolerance.
    Within,
    /// Worse by more than the tolerance: the gate fails.
    Regressed,
    /// Better by more than the tolerance: the baseline could keep the gain.
    Improved,
    /// Not compared: a runtime measured elsewhere.
    NotCompared,
}

/// One measure of one run against the baseline.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub value: f64,
    pub baseline: f64,
    /// The change the wrong way would have to exceed to fail.
    pub allowed: f64,
    pub state: State,
}

/// A run compared with the baseline.
#[derive(Default)]
pub struct Comparison {
    /// By run id, then measure.
    pub checks: BTreeMap<String, BTreeMap<String, Check>>,
    /// What fails the gate.
    pub failures: Vec<String>,
    /// Measures better than the baseline by more than their tolerance.
    pub improvements: Vec<String>,
    /// Runs of shared stacks the baseline doesn't have.
    pub unbaselined: Vec<String>,
    /// The baseline's stacks this run didn't run (not here, or left out).
    pub not_run: Vec<String>,
    /// Whether runtimes were compared.
    pub runtime_compared: bool,
}

impl Comparison {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Compares `outcomes` with `baseline`. `selected` says whether a run name was asked for (`--run`); runtimes are
/// compared when `same_machine`.
pub fn compare(baseline: &Baseline, outcomes: &[Outcome], selected: &dyn Fn(&str) -> bool, same_machine: bool) -> Comparison {
    let mut comparison = Comparison { runtime_compared: same_machine, ..Comparison::default() };
    let by_id: BTreeMap<&str, &Outcome> = outcomes.iter().map(|o| (o.id.as_str(), o)).collect();
    let mut not_run = std::collections::BTreeSet::new();
    for (run_id, expected) in &baseline.runs {
        let stack = stack_of(run_id);
        let result = match by_id.get(stack).map(|o| &o.result) {
            None => {
                not_run.insert(stack.to_owned());
                continue;
            }
            Some(Err(e)) => {
                if !comparison.failures.iter().any(|f| f.starts_with(&format!("{stack}: "))) {
                    comparison.failures.push(format!("{stack}: the runner failed: {e}"));
                }
                continue;
            }
            Some(Ok(result)) => result,
        };
        let Some(run) = result.runs.iter().find(|r| &r.id == run_id) else {
            let name = run_id.rsplit('/').next().unwrap_or(run_id);
            if selected(name) {
                comparison.failures.push(format!("{run_id}: in the baseline, but no longer run"));
            }
            continue;
        };
        if let Some(e) = &run.error {
            comparison.failures.push(format!("{run_id}: failed: {e}"));
            continue;
        }
        let measured = run.measures();
        for (key, &base) in expected {
            let Some(m) = measure(key) else { continue };
            let Some(&value) = measured.get(key) else {
                comparison.failures.push(format!("{run_id}: {} is in the baseline, but no longer measured", m.name));
                continue;
            };
            let allowed = baseline.tolerance(m).allowed(base);
            let worse = match m.better {
                Better::Higher => base - value,
                Better::Lower => value - base,
            };
            // A hair of slack, for the baseline's rounding.
            let rounding = 1e-9 * base.abs().max(1.0);
            let state = if m.machine && !same_machine {
                State::NotCompared
            } else if worse > allowed + rounding {
                State::Regressed
            } else if -worse > allowed + rounding {
                State::Improved
            } else {
                State::Within
            };
            let line = || {
                let digits = m.digits.max(2) + 1;
                format!(
                    "{run_id}: {} {value:.digits$}, {} the baseline's {base:.digits$} by {:.digits$} (tolerance {allowed:.digits$})",
                    m.name,
                    if worse > 0.0 { "worse than" } else { "better than" },
                    worse.abs()
                )
            };
            match state {
                State::Regressed => comparison.failures.push(line()),
                State::Improved => comparison.improvements.push(line()),
                State::Within | State::NotCompared => {}
            }
            comparison.checks.entry(run_id.clone()).or_default().insert(key.clone(), Check { value, baseline: base, allowed, state });
        }
    }
    for outcome in outcomes.iter().filter(|o| shared(&o.id)) {
        if let Ok(result) = &outcome.result {
            comparison.unbaselined.extend(result.runs.iter().filter(|r| !baseline.runs.contains_key(&r.id)).map(|r| r.id.clone()));
        }
    }
    comparison.not_run = not_run.into_iter().collect();
    comparison
}

/// The baseline `outcomes` make, measured with `profile` on `host`, with `old`'s tolerances (or the defaults): every
/// run of the shared stacks. Refused when one of them failed.
pub fn updated(old: Option<&Baseline>, outcomes: &[Outcome], profile: &str, host: &Host) -> Result<Baseline, String> {
    let mut runs = BTreeMap::new();
    for outcome in outcomes.iter().filter(|o| shared(&o.id)) {
        let result = outcome.result.as_ref().map_err(|e| format!("{}: the runner failed, so no baseline was written: {e}", outcome.id))?;
        for run in &result.runs {
            if let Some(e) = &run.error {
                return Err(format!("{}: failed, so no baseline was written: {e}", run.id));
            }
            runs.insert(run.id.clone(), run.measures().into_iter().map(|(k, v)| (k, round(v))).collect());
        }
    }
    if runs.is_empty() {
        return Err("no stack ran, so no baseline was written".to_owned());
    }
    let mut tolerances: BTreeMap<String, Tolerance> = MEASURES.iter().map(|m| (m.key.to_owned(), m.tolerance)).collect();
    if let Some(old) = old {
        tolerances.extend(old.tolerances.iter().map(|(k, t)| (k.clone(), *t)));
    }
    Ok(Baseline { schema_version: SCHEMA_VERSION, about: ABOUT.to_owned(), tolerances, profile: profile.to_owned(), host: host.clone(), runs })
}

/// `v` to 6 significant digits, so that the baseline reads well and its diffs show what moved.
fn round(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    #[allow(clippy::cast_possible_truncation)] // The decimal exponent of a finite f64: within ±330.
    let magnitude = v.abs().log10().floor() as i32;
    let scale = 10f64.powi(5 - magnitude);
    (v * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, ssim: f64, runtime: f64, peak: f64) -> RunResult {
        RunResult {
            id: id.to_owned(),
            run: id.rsplit('/').next().unwrap().to_owned(),
            title: "Pyramid".to_owned(),
            runtime_s: runtime,
            peak_mib: peak,
            metrics: [("ssim".to_owned(), ssim), ("halo".to_owned(), 0.08)].into(),
            uncovered: Some(0),
            slabs: None,
            slabs_reused: None,
            crops: Vec::new(),
            error: None,
        }
    }

    fn stack(id: &str, runs: Vec<RunResult>) -> Outcome {
        Outcome {
            id: id.to_owned(),
            result: Ok(StackResult {
                id: id.to_owned(),
                synthetic: true,
                description: None,
                frames: 12,
                width: 480,
                height: 360,
                threads: 1,
                truth: true,
                runs,
                regions: Vec::new(),
                notes: Vec::new(),
            }),
        }
    }

    fn host() -> Host {
        Host { os: "linux".into(), arch: "x86_64".into(), logical_cpus: 4, cpu: Some("A CPU".into()) }
    }

    fn baseline(outcomes: &[Outcome]) -> Baseline {
        updated(None, outcomes, "dev", &host()).unwrap()
    }

    const ALL: &dyn Fn(&str) -> bool = &|_| true;

    #[test]
    fn the_same_results_pass_and_a_degraded_one_fails() {
        let good = [stack("synthetic/a", vec![run("synthetic/a/pyramid", 0.9305, 0.66, 18.3), run("synthetic/a/average", 0.8608, 0.5, 9.9)])];
        let base = baseline(&good);
        let same = compare(&base, &good, ALL, true);
        assert!(same.passed(), "{:?}", same.failures);
        assert_eq!(same.checks["synthetic/a/pyramid"]["ssim"].state, State::Within);

        // A degraded setting: the pyramid loses 0.01 of SSIM, more than the 0.002 allowed.
        let degraded = [stack("synthetic/a", vec![run("synthetic/a/pyramid", 0.9205, 0.66, 18.3), run("synthetic/a/average", 0.8608, 0.5, 9.9)])];
        let gate = compare(&base, &degraded, ALL, true);
        assert!(!gate.passed());
        assert_eq!(gate.failures.len(), 1, "{:?}", gate.failures);
        assert!(
            gate.failures[0].starts_with("synthetic/a/pyramid: SSIM 0.92050, worse than the baseline's 0.93050 by 0.01000"),
            "{}",
            gate.failures[0]
        );
        assert_eq!(gate.checks["synthetic/a/pyramid"]["ssim"].state, State::Regressed);

        // Within the tolerance either way: no failure, no improvement.
        let close = [stack("synthetic/a", vec![run("synthetic/a/pyramid", 0.9295, 0.66, 18.3), run("synthetic/a/average", 0.8618, 0.5, 9.9)])];
        let gate = compare(&base, &close, ALL, true);
        assert!(gate.passed() && gate.improvements.is_empty(), "{:?} {:?}", gate.failures, gate.improvements);

        // Better by more than the tolerance: noted, not failed.
        let better = [stack("synthetic/a", vec![run("synthetic/a/pyramid", 0.95, 0.66, 18.3), run("synthetic/a/average", 0.8608, 0.5, 9.9)])];
        let gate = compare(&base, &better, ALL, true);
        assert!(gate.passed());
        assert_eq!(gate.improvements.len(), 1, "{:?}", gate.improvements);
    }

    #[test]
    fn memory_is_compared_everywhere_and_runtime_on_the_same_machine_only() {
        let good = [stack("synthetic/a", vec![run("synthetic/a/pyramid", 0.93, 1.0, 20.0)])];
        let base = baseline(&good);
        let slow_and_big = [stack("synthetic/a", vec![run("synthetic/a/pyramid", 0.93, 2.5, 23.0)])];
        let here = compare(&base, &slow_and_big, ALL, true);
        assert_eq!(here.failures.len(), 2, "runtime twice the baseline or 0.5 s, and memory 10%, allowed: {:?}", here.failures);
        let elsewhere = compare(&base, &slow_and_big, ALL, false);
        assert_eq!(elsewhere.failures.len(), 1, "{:?}", elsewhere.failures);
        assert!(elsewhere.failures[0].contains("peak MiB"), "{:?}", elsewhere.failures);
        assert_eq!(elsewhere.checks["synthetic/a/pyramid"]["runtime_s"].state, State::NotCompared);
        let other = Host { cpu: Some("Another CPU".into()), ..host() };
        assert!(base.same_machine("dev", &host()) && !base.same_machine("release", &host()) && !base.same_machine("dev", &other));
        assert!(!Baseline { host: Host { cpu: None, ..host() }, ..baseline(&good) }.same_machine("dev", &Host { cpu: None, ..host() }));
    }

    #[test]
    fn missing_failed_and_new_runs() {
        let good = [stack("synthetic/a", vec![run("synthetic/a/pyramid", 0.93, 1.0, 20.0), run("synthetic/a/average", 0.86, 1.0, 10.0)])];
        let base = baseline(&good);
        // A run that no longer runs fails, unless it was left out on purpose.
        let fewer = [stack("synthetic/a", vec![run("synthetic/a/pyramid", 0.93, 1.0, 20.0)])];
        assert_eq!(compare(&base, &fewer, ALL, true).failures, ["synthetic/a/average: in the baseline, but no longer run"]);
        assert!(compare(&base, &fewer, &|name| name.contains("pyramid"), true).passed());
        // A failed run, a failed stack and a metric no longer measured fail.
        let mut failed = run("synthetic/a/average", 0.0, 0.0, 0.0);
        failed.error = Some("stack.average: out of memory".into());
        let mut unmeasured = run("synthetic/a/pyramid", 0.93, 1.0, 20.0);
        unmeasured.metrics.remove("halo");
        let gate = compare(&base, &[stack("synthetic/a", vec![unmeasured, failed])], ALL, true);
        assert_eq!(gate.failures.len(), 2, "{:?}", gate.failures);
        assert!(gate.failures.iter().any(|f| f.contains("halo is in the baseline, but no longer measured")), "{:?}", gate.failures);
        let crashed = [Outcome { id: "synthetic/a".into(), result: Err("exit code 101".into()) }];
        assert_eq!(compare(&base, &crashed, ALL, true).failures, ["synthetic/a: the runner failed: exit code 101"]);
        // A stack not run here is listed, not failed; a new run of a shared stack is listed; a local stack is neither.
        let elsewhere = [
            stack("synthetic/b", vec![run("synthetic/b/pyramid", 0.9, 1.0, 1.0)]),
            stack("local/wasp", vec![run("local/wasp/pyramid", 0.9, 1.0, 1.0)]),
        ];
        let gate = compare(&base, &elsewhere, ALL, true);
        assert!(gate.passed());
        assert_eq!(gate.not_run, ["synthetic/a"]);
        assert_eq!(gate.unbaselined, ["synthetic/b/pyramid"]);
    }

    #[test]
    fn a_new_baseline_keeps_the_tolerances_and_leaves_local_stacks_out() {
        let outcomes = [
            stack("synthetic/a", vec![run("synthetic/a/pyramid", 0.930_512_345, 0.661_234, 18.312_34)]),
            stack("corpus/bee", vec![run("corpus/bee/pyramid", 0.9, 10.0, 900.0)]),
            stack("local/wasp", vec![run("local/wasp/pyramid", 0.9, 50.0, 1500.0)]),
        ];
        let mut old = baseline(&outcomes);
        old.tolerances.insert("ssim".into(), Tolerance::absolute(0.01));
        let new = updated(Some(&old), &outcomes, "dev", &host()).unwrap();
        assert_eq!(new.tolerances["ssim"], Tolerance::absolute(0.01));
        assert_eq!(new.tolerances["halo"], Tolerance::absolute(0.003));
        assert_eq!(new.runs.keys().collect::<Vec<_>>(), ["corpus/bee/pyramid", "synthetic/a/pyramid"]);
        let a = &new.runs["synthetic/a/pyramid"];
        assert_eq!((a["ssim"], a["runtime_s"], a["peak_mib"]), (0.930_512, 0.661_234, 18.3123));
        let text = serde_json::to_string_pretty(&new).unwrap();
        assert_eq!(Baseline::parse(&text).unwrap(), new, "it reads back");
        assert!(Baseline::parse(&text.replace("\"ssim\": 0.930512", "\"sharpness\": 1.0")).unwrap_err().contains("`sharpness`"));
        assert!(Baseline::parse(&text.replace("\"schema_version\": 1", "\"schema_version\": 9")).is_err());

        let mut failed = run("synthetic/a/pyramid", 0.0, 0.0, 0.0);
        failed.error = Some("broke".into());
        assert!(updated(None, &[stack("synthetic/a", vec![failed])], "dev", &host()).unwrap_err().contains("no baseline was written"));
        assert!(updated(None, &[], "dev", &host()).is_err());
    }

    #[test]
    fn tolerances_and_rounding() {
        assert!((Tolerance::relative(0.5, 0.25).allowed(0.2) - 0.25).abs() < 1e-12, "the absolute floor");
        assert!((Tolerance::relative(0.5, 0.25).allowed(4.0) - 2.0).abs() < 1e-12);
        assert_eq!(round(0.000_123_456_78), 0.000_123_457);
        assert_eq!(round(1234.5678), 1234.57);
        assert_eq!(round(0.0), 0.0);
        assert_eq!(stack_of("synthetic/a/pyramid"), "synthetic/a");
        assert!(shared("corpus/bee") && shared("synthetic/a") && !shared("local/wasp"));
    }
}
