//! The quality runner of `cargo xtask quality` (`xtask/src/quality.rs`), which builds it and runs it once per stack:
//! it stacks the stack by every method, and by every pair of slab and final methods, through the engine's commands as
//! the apps do, measures each command, scores each output where there is a truth to score it against, and cuts 100%
//! crops of the same regions of every output.
//!
//! - **Stacks.** A synthetic stack (`synthetic.rs`), generated here and written as 16-bit TIFF frames, scored against
//!   its ground truth brought onto the output's canvas; or a real one, frame files in focus order, scored against its
//!   reference image when it has one of the output's size.
//! - **Runs.** Alignment (scored by its transforms' error on synthetic stacks), then the weighted average, the pyramid
//!   and the depth map, then slabs by each method stacked by each method (`stack.slabbed`), all at the defaults or at
//!   the settings the plan gives (`stack.set`). One slab method's three final stacks run in a row: the first stacks
//!   the slabs, the other two reuse them, as exploring the final stack's settings does.
//! - **Measures.** The runtime is the command's wall time, from its dispatch to the end of its job. The peak memory is
//!   the most heap the command held at once beyond what was allocated when it started, counted by `peak_alloc`'s
//!   global allocator (every allocation, on every thread). On one thread, as the synthetic stacks run, it is the same on
//!   every platform and run, as florastack-stack's memory test measures it; on several, it varies a little with the
//!   order the threads happen to run in.
//! - **Crops.** The regions (`regions.rs`) are chosen on the three methods' outputs, which are then cut there; every
//!   later output is cut as soon as it is scored. An output's files are deleted once it is cut (a slab's once its slab
//!   method's final stacks are done), so that the disk holds one slab method's outputs at a time: a 20 MP stack's 30
//!   outputs would take 7 GB.
//!
//! `quality list` prints the synthetic stacks as JSON; `quality PLAN.json` runs the plan xtask wrote and writes the
//! result to `<out>/results/<stack>.json` and the crops to `<out>/crops/<stack>/`, both named by the stack's id with `/`
//! written `--`.

mod regions;
mod synthetic;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use florastack_core::ops::{Border, Interpolation, warp_into};
use florastack_core::testutil::{GroundTruth, transform_error};
use florastack_core::{Affine, AnyImage, ColorSpace, Encoding, Image, Rect};
use florastack_engine::model::FolderTrash;
use florastack_engine::{Dispatched, Engine, EngineConfig, SystemClock, ThreadExecutor, Value, json};
use florastack_metrics::{HaloParams, NoiseParams, color_shift, halo, noise, psnr, ssim};
use regions::{Plane, Window};
use serde::{Deserialize, Serialize};

#[global_allocator]
static HEAP: peak_alloc::PeakAlloc = peak_alloc::PeakAlloc;

const USAGE: &str = "usage: quality list | quality PLAN.json (written by `cargo xtask quality`)";

/// The stacking methods: their names in `stack.slabbed`'s parameters, their commands and their titles.
const METHODS: [(&str, &str, &str); 3] =
    [("average", "stack.average", "Weighted average"), ("pyramid", "stack.pyramid", "Pyramid"), ("depth-map", "stack.depthMap", "Depth map")];

/// What xtask asks for: one stack.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    /// The stack's id in the report: `synthetic/<name>`, `corpus/<name>` or `local/<name>`.
    id: String,
    source: Source,
    /// The folder the result, the crops and the work folder go in.
    out: PathBuf,
    /// Threads for the stacking: 0 for one per core.
    threads: usize,
    /// `stack.set`'s parameters, applied before the runs.
    settings: Option<Value>,
    /// The runs to do: those whose names contain one of these, or all when there are none. Alignment always runs.
    runs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    /// One of `synthetic::STACKS`.
    Synthetic(String),
    /// Frame files in focus order, and a trusted result to score against.
    Files { frames: Vec<PathBuf>, reference: Option<PathBuf> },
}

/// What the runner measured on one stack.
#[derive(Serialize)]
struct StackResult {
    id: String,
    synthetic: bool,
    description: Option<String>,
    frames: usize,
    width: usize,
    height: usize,
    /// The threads the stacking ran on.
    threads: usize,
    /// Whether the outputs were scored against a truth.
    truth: bool,
    runs: Vec<RunResult>,
    /// The regions every output is cropped at, with what each shows and the truth's crop.
    regions: Vec<RegionResult>,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct RunResult {
    /// `<stack id>/<run>`: the key the baseline uses.
    id: String,
    run: String,
    title: String,
    runtime_s: f64,
    peak_mib: f64,
    metrics: BTreeMap<String, f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uncovered: Option<u64>,
    /// For a slabbed run: its slabs, and how many of them it took from earlier runs' outputs.
    #[serde(skip_serializing_if = "Option::is_none")]
    slabs: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slabs_reused: Option<usize>,
    /// The crops, one per region, as paths relative to the output folder.
    crops: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct RegionResult {
    #[serde(flatten)]
    window: Window,
    why: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    truth: Option<String>,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [command] if command == "list" => {
            list();
            Ok(())
        }
        [plan] if !plan.starts_with('-') => run_plan(Path::new(plan)),
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("quality: {e}");
            ExitCode::FAILURE
        }
    }
}

fn list() {
    let stacks: Vec<Value> = synthetic::STACKS.iter().map(|(name, about)| json!({ "name": name, "description": about })).collect();
    println!("{}", Value::Array(stacks));
}

/// A stack's id as a file name: `synthetic/layered-clean` is `synthetic--layered-clean`.
fn file_name(id: &str) -> String {
    id.replace('/', "--")
}

fn run_plan(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let plan: Plan = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    rayon::ThreadPoolBuilder::new().num_threads(plan.threads).build_global().map_err(|e| format!("cannot start the threads: {e}"))?;
    let work = plan.out.join("work").join(file_name(&plan.id));
    remove_dir(&work)?;
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let result = run_stack(&plan, &work);
    remove_dir(&work)?;
    let results = plan.out.join("results");
    std::fs::create_dir_all(&results).map_err(|e| format!("cannot create {}: {e}", results.display()))?;
    let path = results.join(format!("{}.json", file_name(&plan.id)));
    // A stack that can't be run leaves its reason for xtask, and fails.
    let (value, failed) = match result {
        Ok(result) => (serde_json::to_value(&result).map_err(|e| e.to_string())?, None),
        Err(e) => (json!({ "id": plan.id, "error": e }), Some(e)),
    };
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    failed.map_or(Ok(()), Err)
}

fn remove_dir(dir: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("cannot remove {}: {e}", dir.display())),
        _ => Ok(()),
    }
}

/// What an output is scored against.
enum Truth {
    /// A synthetic stack's truth, in the uncropped scene's coordinates.
    Synthetic(GroundTruth),
    /// A trusted result, read from its file.
    Reference(Image<f32>),
}

/// A stack in progress: the engine with its project, and what is known about it.
struct Stack {
    engine: Engine,
    out: PathBuf,
    crops: String,
    truth: Option<Truth>,
    /// From align.info: the master's index in the files, the frames' size, and each framing's size and transform to the
    /// master.
    master: usize,
    size: (usize, usize),
    framings: Vec<(String, usize, usize, Affine)>,
    /// The truth on each canvas used so far, by framing.
    truths: BTreeMap<String, Image<f32>>,
    regions: Vec<(Window, &'static str)>,
    notes: Vec<String>,
}

/// The first outputs, kept until the regions are chosen on them.
#[derive(Default)]
struct First {
    /// Their planes, and their size.
    planes: Vec<Plane>,
    size: (usize, usize),
    /// Their runs' indices, and their outputs.
    pending: Vec<(usize, u64)>,
}

/// A run's place in the stack: its name, its title, its command and the command's parameters.
type Planned = (String, String, &'static str, Value);

/// The runs after alignment, in groups: the three methods, then each slab method's three final stacks.
fn groups() -> Vec<Vec<Planned>> {
    let mut groups = vec![METHODS.iter().map(|&(name, command, title)| (name.to_owned(), title.to_owned(), command, json!({}))).collect()];
    for (slabs, _, slab_title) in METHODS {
        groups.push(
            METHODS
                .iter()
                .map(|&(last, _, last_title)| {
                    let params = json!({ "slabs": { "method": slabs }, "final": { "method": last } });
                    (format!("slabbed-{slabs}-{last}"), format!("{slab_title} slabs, then {}", last_title.to_lowercase()), "stack.slabbed", params)
                })
                .collect(),
        );
    }
    groups
}

impl RunResult {
    fn new(stack: &str, run: &str, title: String) -> RunResult {
        RunResult {
            id: format!("{stack}/{run}"),
            run: run.to_owned(),
            title,
            runtime_s: 0.0,
            peak_mib: 0.0,
            metrics: BTreeMap::new(),
            uncovered: None,
            slabs: None,
            slabs_reused: None,
            crops: Vec::new(),
            error: None,
        }
    }
}

fn run_stack(plan: &Plan, work: &Path) -> Result<StackResult, String> {
    let started = Instant::now();
    let (files, truth, description) = match &plan.source {
        Source::Synthetic(name) => {
            let made = synthetic::make(name)?;
            let files = write_frames(&work.join("frames"), &made.frames)?;
            let about = synthetic::STACKS.iter().find(|(n, _)| n == name).map(|(_, about)| (*about).to_owned());
            (files, Some(Truth::Synthetic(made.truth)), about)
        }
        Source::Files { frames, reference } => {
            let truth = match reference {
                Some(path) => Some(Truth::Reference(read_f32(path)?)),
                None => None,
            };
            (frames.clone(), truth, None)
        }
    };
    let (mut stack, ids) = Stack::open(plan, work, &files, truth)?;
    let mut runs = vec![stack.align(&plan.id, &ids)?];
    let wanted = |run: &str| plan.runs.is_empty() || plan.runs.iter().any(|w| run.contains(w.as_str()));
    let mut first = First::default();
    for group in groups() {
        stack.run_group(&plan.id, group.into_iter().filter(|(name, ..)| wanted(name)).collect(), &mut runs, &mut first)?;
    }
    let regions = stack.region_results()?;
    eprintln!("{}: done in {:.1} s", plan.id, started.elapsed().as_secs_f64());
    Ok(StackResult {
        id: plan.id.clone(),
        synthetic: matches!(plan.source, Source::Synthetic(_)),
        description,
        frames: ids.len(),
        width: stack.size.0,
        height: stack.size.1,
        threads: rayon::current_num_threads(),
        truth: stack.truth.is_some(),
        runs,
        regions,
        notes: stack.notes,
    })
}

impl Stack {
    /// A new project in `work` with the frame `files`, at the plan's settings; and the frames' ids.
    fn open(plan: &Plan, work: &Path, files: &[PathBuf], truth: Option<Truth>) -> Result<(Stack, Vec<u64>), String> {
        let config = EngineConfig {
            temporary_projects: Some(work.join("temp")),
            trash: Arc::new(FolderTrash::new(work.join("trash"))),
            executor: Arc::new(ThreadExecutor::new()),
            clock: Arc::new(SystemClock::new()),
            waker: None,
            max_heavy_jobs: 1,
            max_preview_jobs: 1,
        };
        let mut engine = Engine::new(config);
        command(&mut engine, "project.new", json!({ "name": "Quality", "folder": work }))?;
        let added = command(&mut engine, "project.addFrames", json!({ "paths": files }))?;
        let ids: Vec<u64> = added["added"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
        let mut notes: Vec<String> = added["skipped"]
            .as_array()
            .map(|a| a.iter().map(|s| format!("skipped {}: {}", s["path"].as_str().unwrap_or("?"), s["reason"].as_str().unwrap_or("?"))).collect())
            .unwrap_or_default();
        if ids.len() < 2 {
            return Err(format!("{}: fewer than two frames could be read{}", plan.id, notes.first().map_or(String::new(), |n| format!(" ({n})"))));
        }
        if let Some(settings) = &plan.settings {
            command(&mut engine, "stack.set", settings.clone())?;
            notes.push(format!("settings: stack.set {settings}"));
        }
        eprintln!("{}: {} frames", plan.id, ids.len());
        let stack = Stack {
            engine,
            out: plan.out.clone(),
            crops: format!("crops/{}", file_name(&plan.id)),
            truth,
            master: 0,
            size: (0, 0),
            framings: Vec::new(),
            truths: BTreeMap::new(),
            regions: Vec::new(),
            notes,
        };
        Ok((stack, ids))
    }

    /// Aligns the frames, whose ids are `ids`, and learns the master and the framings; the run, scored against the
    /// synthetic stack's transforms.
    fn align(&mut self, stack_id: &str, ids: &[u64]) -> Result<RunResult, String> {
        let (aligned, runtime, peak) = measured(&mut self.engine, "align.run", json!({}))?;
        let info = command(&mut self.engine, "align.info", json!({}))?;
        let run_info = &info["run"];
        let master_id = run_info["master"].as_u64().ok_or("align.info names no master")?;
        self.master = ids.iter().position(|&id| id == master_id).ok_or("the master is none of the frames")?;
        let side = |key: &str| run_info[key].as_u64().and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
        self.size = (side("width"), side("height"));
        self.framings = run_info["framings"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| {
                        let (w, h) = (usize::try_from(f["width"].as_u64()?).ok()?, usize::try_from(f["height"].as_u64()?).ok()?);
                        Some((f["framing"].as_str()?.to_owned(), w, h, affine(&f["toMaster"])?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut run = RunResult::new(stack_id, "alignment", "Alignment".to_owned());
        run.runtime_s = runtime;
        run.peak_mib = peak;
        run.metrics.insert("rejected".to_owned(), aligned["rejected"].as_array().map_or(0.0, |r| r.len() as f64));
        if let Some(Truth::Synthetic(truth)) = &self.truth
            && let Some(error) = alignment_error(truth, run_info, ids, self.master)
        {
            run.metrics.insert("align_error".to_owned(), error);
        }
        report(&run);
        Ok(run)
    }

    /// Runs one group's runs into `runs`, then deletes its slabs' files; the first group's outputs choose the regions.
    fn run_group(&mut self, stack_id: &str, group: Vec<Planned>, runs: &mut Vec<RunResult>, first: &mut First) -> Result<(), String> {
        let mut slab_outputs: Vec<u64> = Vec::new();
        for (name, title, command_name, params) in group {
            let mut run = RunResult::new(stack_id, &name, title);
            let done = measured(&mut self.engine, command_name, params).and_then(|(summary, runtime, peak)| {
                run.runtime_s = runtime;
                run.peak_mib = peak;
                if let Some(slabs) = summary["slabs"].as_array() {
                    slab_outputs.extend(slabs.iter().filter_map(|s| s["output"].as_u64()));
                }
                self.scored(&summary, &mut run, runs.len(), first)
            });
            match done {
                Ok(()) => report(&run),
                Err(e) => {
                    eprintln!("  {}: {e}", run.id);
                    run.error = Some(e);
                }
            }
            runs.push(run);
        }
        if self.regions.is_empty() && !first.pending.is_empty() {
            self.choose_regions(&first.planes, first.size);
            for (index, output) in std::mem::take(&mut first.pending) {
                let image = self.output_image(output)?;
                if let Some(run) = runs.get_mut(index) {
                    run.crops = self.cut(&image, &run.run)?;
                }
                self.remove_output(output)?;
            }
            first.planes.clear();
        }
        for output in slab_outputs {
            self.remove_output(output)?;
        }
        Ok(())
    }
}

impl Stack {
    /// The pixels of output `id`, as stored.
    fn output_image(&self, id: u64) -> Result<Image<f32>, String> {
        let project = self.engine.project().ok_or("no project")?;
        let root = self.engine.project_path().ok_or("no project folder")?;
        let output = project.outputs.iter().find(|o| o.id().get() == id).ok_or_else(|| format!("no output {id}"))?;
        read_f32(&output.image.path.resolve(root))
    }

    /// Deletes the files of output `id`: its image, its preview and its maps.
    fn remove_output(&self, id: u64) -> Result<(), String> {
        let project = self.engine.project().ok_or("no project")?;
        let root = self.engine.project_path().ok_or("no project folder")?;
        let Some(output) = project.outputs.iter().find(|o| o.id().get() == id) else { return Ok(()) };
        for file in std::iter::once(&output.image).chain(&output.preview).chain(output.maps.values()) {
            let path = file.path.resolve(root);
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("cannot remove {}: {e}", path.display())),
                _ => {}
            }
        }
        Ok(())
    }

    /// The truth on the canvas of `framing`, `width` x `height`, made once per framing.
    fn truth_on(&mut self, framing: &str, width: usize, height: usize) -> Result<Option<&Image<f32>>, String> {
        if !self.truths.contains_key(framing) {
            let made = match &self.truth {
                None => return Ok(None),
                Some(Truth::Reference(reference)) if (reference.width(), reference.height()) == (width, height) => Ok(rgb(reference)?.into_owned()),
                Some(Truth::Reference(reference)) => {
                    Err(format!("the reference is {}x{} and the outputs {width}x{height}: not scored", reference.width(), reference.height()))
                }
                Some(Truth::Synthetic(truth)) => {
                    let Some((_, w, h, to_master)) = self.framings.iter().find(|(name, ..)| name == framing) else {
                        return Err(format!("align.info has no `{framing}` framing"));
                    };
                    if (*w, *h) != (width, height) {
                        return Err(format!("the `{framing}` framing is {w}x{h}, but the output is {width}x{height}"));
                    }
                    Ok(truth_on_canvas(truth, self.master, to_master, width, height)?)
                }
            };
            match made {
                Ok(image) => {
                    self.truths.insert(framing.to_owned(), image);
                }
                Err(note) => {
                    self.notes.push(note);
                    self.truth = None;
                    return Ok(None);
                }
            }
        }
        Ok(self.truths.get(framing))
    }

    /// Scores the output a command's `summary` names into `run` (the `index`th of the stack's runs) and cuts it, or
    /// keeps it in `first` to choose the regions on.
    fn scored(&mut self, summary: &Value, run: &mut RunResult, index: usize, first: &mut First) -> Result<(), String> {
        run.uncovered = summary["uncovered"].as_u64();
        if let Some(slabs) = summary["slabs"].as_array() {
            run.slabs = Some(slabs.len());
            run.slabs_reused = Some(slabs.iter().filter(|s| s["reused"] == true).count());
        }
        let output = summary["output"].as_u64().ok_or_else(|| format!("no output in {summary}"))?;
        let framing = summary["framing"].as_str().unwrap_or("narrow");
        let image = self.output_image(output)?;
        run.metrics = self.score(&image, framing)?;
        if self.regions.is_empty() {
            first.size = (image.width(), image.height());
            first.planes.push(Plane::of(&image, regions::reduction(image.width(), image.height())));
            first.pending.push((index, output));
        } else {
            run.crops = self.cut(&image, &run.run)?;
            self.remove_output(output)?;
        }
        Ok(())
    }

    /// The metrics of `image` on the canvas of `framing`: against the truth when there is one, and its share of
    /// samples out of range.
    fn score(&mut self, image: &Image<f32>, framing: &str) -> Result<BTreeMap<String, f64>, String> {
        let color = rgb(image)?;
        let mut metrics = BTreeMap::new();
        let pixels = (color.width() * color.height()).max(1) as f64;
        metrics.insert("out_of_range".to_owned(), florastack_stack::out_of_range(&color) as f64 / pixels);
        if let Some(truth) = self.truth_on(framing, color.width(), color.height())? {
            let e = |e: florastack_metrics::MetricsError| e.to_string();
            metrics.insert("ssim".to_owned(), ssim(&color, truth).map_err(e)?);
            metrics.insert("psnr".to_owned(), psnr(&color, truth).map_err(e)?);
            metrics.insert("halo".to_owned(), halo(&color, truth, &HaloParams::default()).map_err(e)?.relative);
            metrics.insert("delta_e".to_owned(), color_shift(&color, truth).map_err(e)?.mean);
            if let Ok(n) = noise(&color, truth, &NoiseParams::default()) {
                metrics.insert("noise".to_owned(), n.sigma);
            }
        }
        metrics.retain(|_, v| v.is_finite());
        Ok(metrics)
    }

    /// Chooses the regions on the planes of the first outputs, which are `width` x `height`.
    fn choose_regions(&mut self, planes: &[Plane], (width, height): (usize, usize)) {
        let k = regions::reduction(width, height);
        let truth = self.truths.values().next().map(|t| Plane::of(t, k));
        self.regions = regions::choose(planes, truth.as_ref(), k, regions::side(width, height, k));
        if self.regions.is_empty() {
            self.notes.push("no regions to crop".to_owned());
        }
    }

    /// Cuts `image` at every region, as PNG files named after `run`; their paths relative to the output folder.
    fn cut(&self, image: &Image<f32>, run: &str) -> Result<Vec<String>, String> {
        let mut paths = Vec::new();
        for (i, (window, _)) in self.regions.iter().enumerate() {
            let path = format!("{}/r{}-{run}.png", self.crops, i + 1);
            write_crop(image, window, &self.out.join(&path))?;
            paths.push(path);
        }
        Ok(paths)
    }

    fn region_results(&self) -> Result<Vec<RegionResult>, String> {
        let truth = self.truths.values().next();
        self.regions
            .iter()
            .enumerate()
            .map(|(i, (window, why))| {
                let crop = match truth {
                    Some(truth) => {
                        let path = format!("{}/r{}-truth.png", self.crops, i + 1);
                        write_crop(truth, window, &self.out.join(&path))?;
                        Some(path)
                    }
                    None => None,
                };
                Ok(RegionResult { window: *window, why: (*why).to_owned(), truth: crop })
            })
            .collect()
    }
}

/// Runs a command to its end.
fn command(engine: &mut Engine, name: &str, params: Value) -> Result<Value, String> {
    let result = match engine.dispatch(name, params) {
        Ok(Dispatched::Done(value)) => Ok(value),
        Ok(Dispatched::Started(job)) => engine.wait(job),
        Err(e) => Err(e),
    };
    result.map_err(|e| format!("{name}: {e}"))
}

/// Runs a command to its end; its result, its runtime in seconds and its peak heap in MiB beyond what was allocated
/// when it started.
fn measured(engine: &mut Engine, name: &str, params: Value) -> Result<(Value, f64, f64), String> {
    let before = HEAP.current_usage();
    HEAP.reset_peak_usage();
    let start = Instant::now();
    let result = command(engine, name, params)?;
    let runtime = start.elapsed().as_secs_f64();
    let peak = HEAP.peak_usage().saturating_sub(before) as f64 / f64::from(1 << 20);
    Ok((result, runtime, peak))
}

/// Prints what a run measured.
fn report(run: &RunResult) {
    let ssim = run.metrics.get("ssim").map_or(String::new(), |s| format!("  SSIM {s:.4}"));
    eprintln!("  {:<44} {:>8.2} s {:>9.1} MiB{ssim}", run.id, run.runtime_s, run.peak_mib);
}

/// A transform from `align.info`'s rows.
fn affine(rows: &Value) -> Option<Affine> {
    let row = |r: usize| -> Option<[f64; 3]> {
        let values = rows.get(r)?.as_array()?;
        Some([values.first()?.as_f64()?, values.get(1)?.as_f64()?, values.get(2)?.as_f64()?])
    };
    Some(Affine { m: [row(0)?, row(1)?] })
}

fn shift(by: f64) -> Affine {
    Affine { m: [[1.0, 0.0, by], [0.0, 1.0, by]] }
}

/// The mean of the frames' transform errors (`transform_error`, in pixels) against the truth, the master left out.
fn alignment_error(truth: &GroundTruth, run: &Value, ids: &[u64], master: usize) -> Option<f64> {
    let margin = synthetic::MARGIN as f64;
    let (width, height) = (usize::try_from(run["width"].as_u64()?).ok()?, usize::try_from(run["height"].as_u64()?).ok()?);
    let mut errors = Vec::new();
    for frame in run["frames"].as_array()? {
        let index = ids.iter().position(|&id| Some(id) == frame["frame"].as_u64())?;
        if index == master {
            continue;
        }
        // The truth maps the uncropped frames' pixels; the frames are cropped by the margin.
        let expected = shift(margin).then(&truth.relative(index, master)?).then(&shift(-margin));
        errors.push(transform_error(&affine(&frame["transform"])?, &expected, width, height));
    }
    (!errors.is_empty()).then(|| errors.iter().sum::<f64>() / errors.len() as f64)
}

/// The synthetic truth seen on an output canvas of `width` x `height` whose pixels `to_master` maps to the master
/// frame's: the sharp scene resampled there, with the master's brightness, which alignment matches the others to.
fn truth_on_canvas(truth: &GroundTruth, master: usize, to_master: &Affine, width: usize, height: usize) -> Result<Image<f32>, String> {
    let frame = truth.frames.get(master).ok_or("no truth for the master")?;
    let to_scene = to_master.then(&shift(synthetic::MARGIN as f64)).then(&frame.transform.inverse().ok_or("the master's transform has no inverse")?);
    let mut canvas = Image::new(width, height, truth.sharp.channels(), ColorSpace::LINEAR).map_err(|e| e.to_string())?;
    warp_into(&truth.sharp, &to_scene, Interpolation::Bicubic, Border::Clamp, &mut canvas).map_err(|e| e.to_string())?;
    let gain = frame.gain;
    let data: Vec<f32> = canvas.as_raw().iter().map(|v| v * gain).collect();
    Ok(rgb(&Image::from_vec(width, height, truth.sharp.channels(), ColorSpace::LINEAR, data).map_err(|e| e.to_string())?)?.into_owned())
}

/// The color channels of an image, without alpha: three, or one for gray; the image itself when it has no alpha (a 20
/// MP output is 240 MB).
fn rgb(image: &Image<f32>) -> Result<Cow<'_, Image<f32>>, String> {
    let channels = match image.channels() {
        2 => 1,
        4 => 3,
        n => n,
    };
    if channels == image.channels() {
        return Ok(Cow::Borrowed(image));
    }
    let color = Image::from_fn(image.width(), image.height(), channels, image.color_space().clone(), |x, y, c| image.get(x, y, c).unwrap_or(0.0));
    color.map(Cow::Owned).map_err(|e| e.to_string())
}

/// An image file's pixels as linear `f32`.
fn read_f32(path: &Path) -> Result<Image<f32>, String> {
    let options = florastack_codecs::ReadOptions { orientation: florastack_codecs::OrientationPolicy::Ignore, ..Default::default() };
    let decoded = florastack_codecs::read_file(path, &options).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    match decoded.image {
        AnyImage::F32(image) => Ok(image),
        AnyImage::U8(image) => image.to_linear_f32().map_err(|e| e.to_string()),
        AnyImage::U16(image) => image.to_linear_f32().map_err(|e| e.to_string()),
    }
}

/// Linear frames as 16-bit sRGB TIFF files `frame-00.tif` and on in `dir`.
fn write_frames(dir: &Path, frames: &[Image<f32>]) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let spec = florastack_codecs::OutputSpec::new(florastack_codecs::Format::Tiff).map_err(|e| e.to_string())?;
    let mut paths = Vec::with_capacity(frames.len());
    for (i, frame) in frames.iter().enumerate() {
        let encoded = frame.to_encoding::<u16>(Encoding::Srgb).map_err(|e| e.to_string())?;
        let written =
            florastack_codecs::write(&AnyImage::U16(encoded), &spec, &florastack_codecs::OutputMetadata::default()).map_err(|e| e.to_string())?;
        let path = dir.join(format!("frame-{i:02}.tif"));
        std::fs::write(&path, written.bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        paths.push(path);
    }
    Ok(paths)
}

/// `window` of `image`, its color in 8-bit sRGB, as a PNG file at `path`.
fn write_crop(image: &Image<f32>, window: &Window, path: &Path) -> Result<(), String> {
    let width = window.width.min(image.width().saturating_sub(window.x));
    let height = window.height.min(image.height().saturating_sub(window.y));
    let crop = image.crop(Rect::new(window.x, window.y, width, height)).map_err(|e| e.to_string())?;
    let encoded = rgb(&crop)?.to_encoding::<u8>(Encoding::Srgb).map_err(|e| e.to_string())?;
    let spec = florastack_codecs::OutputSpec::new(florastack_codecs::Format::Png).map_err(|e| e.to_string())?;
    let written =
        florastack_codecs::write(&AnyImage::U8(encoded), &spec, &florastack_codecs::OutputMetadata::default()).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, written.bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}
