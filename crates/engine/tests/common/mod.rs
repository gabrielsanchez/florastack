//! Helpers shared by the engine's tests: temporary folders, engines over them, fixture projects with frames and an
//! output, test-only commands (gated jobs, which wait for the test at each step), and a checker of the JSON schema
//! keywords that schemars emits.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use florastack_codecs::{OutputMetadata, OutputSpec};
use florastack_core::{AnyImage, ColorSpace, Image};
use florastack_engine::model::{FrameId, OutputId};
use florastack_engine::{
    CommandError, CommandSpec, Compute, Dispatched, Engine, EngineConfig, Executor, JobId, JobPlan, Lane, Meta, Monitor, Params, Registry, Requires,
    Session, SystemClock, ThreadExecutor, Value, commit,
};
use florastack_project::{Area, Aspects, Edit, FolderTrash, Input, LayerId, NewOutput, ProjectFolder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A folder of its own in the system's temporary directory, removed with everything in it when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("florastack-engine-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An engine's settings over `dir`: temporary projects in `dir/temp`, a folder trash in `dir/trash`, threads.
pub fn config(dir: &Path) -> EngineConfig {
    config_with(dir, Arc::new(ThreadExecutor::new()))
}

pub fn config_with(dir: &Path, executor: Arc<dyn Executor>) -> EngineConfig {
    EngineConfig {
        temporary_projects: Some(dir.join("temp")),
        trash: Arc::new(FolderTrash::new(dir.join("trash"))),
        executor,
        clock: Arc::new(SystemClock::new()),
        waker: None,
        max_heavy_jobs: 1,
        max_preview_jobs: 2,
        // The CPU, whatever the machine has: results and memory the same everywhere. tests/compute.rs stacks on the GPU.
        compute: Compute::CPU,
        tiling: florastack_engine::Tiling::default(),
    }
}

/// A 16-bit TIFF whose pixels depend on `seed`, so that every seed gives other contents.
pub fn write_tiff(path: &Path, seed: u16) {
    let image = Image::from_fn(12, 8, 3, ColorSpace::SRGB, |x, y, c| {
        u16::try_from((x * 1000 + y * 100 + c * 10) % 60_000).unwrap().wrapping_add(seed.wrapping_mul(7))
    })
    .unwrap();
    let spec = OutputSpec::new(florastack_codecs::Format::Tiff).unwrap();
    let bytes = florastack_codecs::write(&AnyImage::U16(image), &spec, &OutputMetadata::default()).unwrap().bytes;
    std::fs::write(path, bytes).unwrap();
}

/// `n` frame files in `dir/frames`, `frame-00.tif` and on.
pub fn frame_files(dir: &Path, n: u16) -> Vec<PathBuf> {
    let frames = dir.join("frames");
    std::fs::create_dir_all(&frames).unwrap();
    (0..n)
        .map(|i| {
            let path = frames.join(format!("frame-{i:02}.tif"));
            write_tiff(&path, i);
            path
        })
        .collect()
}

/// The margin, in pixels, synthetic frames are generated with on every side and cropped of, so that no frame shows the
/// generator's clamped scene edge.
pub const MARGIN: usize = 8;

/// The frames of a synthetic stack (`florastack_core::testutil`) of `width` x `height` pixels, as 16-bit sRGB TIFF files
/// `dir/<name>/frame-00.tif` and on, in focus order.
pub fn synthetic_frames(dir: &Path, name: &str, width: usize, height: usize, params: &florastack_core::testutil::StackParams) -> Vec<PathBuf> {
    use florastack_core::testutil::scene;
    let (sharp, depth) = scene::textured(width + 2 * MARGIN, height + 2 * MARGIN, params.seed).unwrap();
    scene_frames(dir, name, &sharp, &depth, params)
}

/// The frames of a synthetic stack of the scene `sharp` with its `depth`, generated `MARGIN` pixels larger on every
/// side than the frames, written as [`synthetic_frames`] writes them.
pub fn scene_frames(dir: &Path, name: &str, sharp: &Image<f32>, depth: &Image<f32>, params: &florastack_core::testutil::StackParams) -> Vec<PathBuf> {
    let stack = florastack_core::testutil::generate(sharp, depth, params).unwrap();
    let (width, height) = (sharp.width() - 2 * MARGIN, sharp.height() - 2 * MARGIN);
    let cropped: Vec<Image<f32>> = stack.frames.iter().map(|f| f.crop(florastack_core::Rect::new(MARGIN, MARGIN, width, height)).unwrap()).collect();
    write_frames(dir, name, &cropped)
}

/// Linear frames as 16-bit sRGB TIFF files `dir/<name>/frame-00.tif` and on.
pub fn write_frames(dir: &Path, name: &str, frames: &[Image<f32>]) -> Vec<PathBuf> {
    let folder = dir.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let spec = OutputSpec::new(florastack_codecs::Format::Tiff).unwrap();
    frames
        .iter()
        .enumerate()
        .map(|(i, frame)| {
            let encoded = frame.to_encoding::<u16>(florastack_core::Encoding::Srgb).unwrap();
            let path = folder.join(format!("frame-{i:02}.tif"));
            std::fs::write(&path, florastack_codecs::write(&AnyImage::U16(encoded), &spec, &OutputMetadata::default()).unwrap().bytes).unwrap();
            path
        })
        .collect()
}

/// A clean stack of `n` frames (`StackParams::clean`) that breathes: each frame `step` larger than the one before (0.01
/// is 1%), defocused through the scene's depth (`focus` gives the first and last frames' focus depths; the scene's
/// run from 0 to 1; `None` is the scene's own).
pub fn breathing(n: usize, step: f32, focus: Option<(f32, f32)>, seed: u64) -> florastack_core::testutil::StackParams {
    let breathing = step * n.saturating_sub(1) as f32;
    florastack_core::testutil::StackParams { focus_range: focus, breathing, ..florastack_core::testutil::StackParams::clean(n, seed) }
}

/// Paths as the JSON list `addFrames` takes.
pub fn path_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| serde_json::to_string(&p.display().to_string()).unwrap()).collect::<Vec<_>>().join(", ")
}

/// What a fixture project holds.
pub struct Fixture {
    pub path: PathBuf,
    pub frames: Vec<FrameId>,
    pub output: OutputId,
    pub layer: LayerId,
}

/// A saved project `dir/Bee.florastack` with three frames and an output (with a retouch layer) made from them, built
/// with the project crate, as a stack of roadmap 3 would leave it.
pub fn fixture_project(dir: &Path) -> Fixture {
    let files = frame_files(dir, 3);
    let (mut folder, mut project) = ProjectFolder::create(dir, "Bee").unwrap();
    let frames: Vec<FrameId> = files.iter().map(|f| florastack_engine::project::add_frame(&mut project, f).unwrap()).collect();
    let image = AnyImage::F32(Image::from_fn(12, 8, 3, ColorSpace::LINEAR, |x, y, c| (x + y + c) as f32 / 24.0).unwrap());
    let reference = florastack_engine::project::write_image(&folder, "output-1", &image, None).unwrap();
    let output = project.add_output(NewOutput {
        name: "Pyramid".into(),
        method: "pyramid".into(),
        parameters: serde_json::json!({}),
        inputs: frames.iter().map(|&f| Input::Frame(f)).collect(),
        image: reference,
        preview: None,
        maps: florastack_project::Maps::new(),
    });
    let layer = project.add_layer(output, "Bristles").unwrap();
    folder.save(&mut project).unwrap();
    Fixture { path: folder.path().to_path_buf(), frames, output, layer }
}

/// An engine with `registry`, over `dir`, with the fixture project open.
pub fn open_fixture(dir: &Path, registry: Arc<Registry>) -> (Engine, Fixture) {
    let fixture = fixture_project(dir);
    let mut engine = Engine::with_registry(config(dir), registry);
    engine.dispatch("project.open", serde_json::json!({ "path": fixture.path })).unwrap();
    (engine, fixture)
}

/// The project's JSON, without what saving and adding frames move (times, the next id).
pub fn state(engine: &Engine) -> Value {
    let mut value = serde_json::to_value(engine.project().unwrap()).unwrap();
    if let Some(map) = value.as_object_mut() {
        for key in ["modified", "software", "next_id"] {
            map.remove(key);
        }
    }
    value
}

/// Runs a command to its end, with parameters as JSON text, as the CLI's `run --cmd` does.
pub fn run(engine: &mut Engine, name: &str, params: &str) -> Value {
    match engine.dispatch_json(name, params).unwrap_or_else(|e| panic!("{name} {params}: {e}")) {
        Dispatched::Done(value) => value,
        Dispatched::Started(job) => engine.wait(job).unwrap_or_else(|e| panic!("{name} {params}: {e}")),
    }
}

/// A gate that test jobs wait at, one permit per step.
#[derive(Clone)]
pub struct Gate {
    tx: Sender<()>,
    rx: Arc<Mutex<Receiver<()>>>,
    /// The jobs running their work now, and the most at once.
    pub running: Arc<AtomicUsize>,
    pub most: Arc<AtomicUsize>,
}

impl Gate {
    pub fn new() -> Gate {
        let (tx, rx) = std::sync::mpsc::channel();
        Gate { tx, rx: Arc::new(Mutex::new(rx)), running: Arc::default(), most: Arc::default() }
    }

    /// Lets `n` steps through.
    pub fn open(&self, n: usize) {
        for _ in 0..n {
            self.tx.send(()).unwrap();
        }
    }

    /// Waits for a permit; a test that forgets one fails after 20 s instead of hanging.
    fn pass(&self) -> Result<(), CommandError> {
        let rx = self.rx.lock().unwrap();
        rx.recv_timeout(Duration::from_secs(20)).map_err(|_| CommandError::Internal { command: "test gate timed out".into() })
    }
}

/// A gated job's parameters.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GateParams {
    /// How many steps, each waiting for a permit.
    #[serde(default)]
    pub steps: u32,
    /// Fail at this step.
    #[serde(default)]
    pub fail_at: Option<u32>,
    /// Panic at this step.
    #[serde(default)]
    pub panic_at: Option<u32>,
    /// Write a cache file at each step.
    #[serde(default)]
    pub write_cache: bool,
    /// Lock the frame list while running.
    #[serde(default)]
    pub lock_frames: bool,
    /// The name the commit gives the project.
    #[serde(default)]
    pub name: Option<String>,
}

impl Params for GateParams {}

/// What a gated job did.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GateResult {
    pub steps: u32,
}

/// The work of a gated job: `steps` steps, each a checkpoint and a wait at the gate; then a commit that renames the
/// project, an undoable change.
fn gated(session: &Session, p: GateParams, gate: Gate) -> Result<JobPlan<GateResult>, CommandError> {
    let cache = if p.write_cache { Some(session.cache_dir()?) } else { None };
    let locks = if p.lock_frames { Aspects::FRAMES } else { Aspects::NONE };
    let plan = JobPlan::new(move |ctx| {
        let now = gate.running.fetch_add(1, Ordering::SeqCst) + 1;
        gate.most.fetch_max(now, Ordering::SeqCst);
        let result = (|| {
            for i in 0..p.steps {
                ctx.step(u64::from(i), u64::from(p.steps))?;
                if let Some(cache) = &cache {
                    cache.store(Area::Cache, "gated", "bin", &i.to_le_bytes(), None)?;
                }
                gate.pass()?;
                if p.fail_at == Some(i) {
                    return Err(CommandError::NotFound { what: "a planned failure".into() });
                }
                assert!(p.panic_at != Some(i), "a planned panic");
            }
            ctx.step(u64::from(p.steps), u64::from(p.steps))?;
            Ok(())
        })();
        gate.running.fetch_sub(1, Ordering::SeqCst);
        result?;
        let (steps, name) = (p.steps, p.name);
        Ok(commit(move |session: &mut Session| {
            if let Some(name) = name {
                session.apply(Edit::SetName(Some(name)))?;
            }
            Ok(GateResult { steps })
        }))
    });
    Ok(plan.stages(&[("Waiting", 1.0)]).locks(locks))
}

/// A stroke for a retouch layer.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StrokeParams {
    pub output: OutputId,
    pub layer: LayerId,
    pub stroke: Value,
}

impl Params for StrokeParams {}

/// Nothing.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Done {}

/// The standard commands, plus `test.gate` (a heavy job), `test.preview` (a superseding preview job) and
/// `test.stroke` (an undoable instant command that paints one retouch stroke, as `retouch.stroke` will in 5.3).
pub fn test_registry(gate: &Gate) -> Arc<Registry> {
    let (heavy, preview) = (gate.clone(), gate.clone());
    Arc::new(
        Registry::builder()
            .standard()
            .command(CommandSpec::job(
                Meta::new("test.gate", "Gated job", "A job that waits at a gate at each step.").requires(Requires::Project),
                Lane::Heavy,
                move |s, p| gated(s, p, heavy.clone()),
            ))
            .command(CommandSpec::job(
                Meta::new("test.preview", "Gated preview", "A preview job that waits at a gate at each step.")
                    .requires(Requires::Project)
                    .supersede(),
                Lane::Preview,
                move |s, p| gated(s, p, preview.clone()),
            ))
            .command(CommandSpec::instant(
                Meta::new("test.stroke", "Retouch stroke", "Paints one stroke.").undoable(),
                |s: &mut Session, p: StrokeParams| {
                    s.apply(Edit::PushStroke { output: p.output, layer: p.layer, stroke: p.stroke })?;
                    Ok(Done {})
                },
            ))
            .build()
            .unwrap(),
    )
}

/// Waits until `f` holds, polling the engine.
pub fn until(engine: &mut Engine, what: &str, mut f: impl FnMut(&mut Engine) -> bool) {
    for _ in 0..2000 {
        engine.poll();
        if f(engine) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

pub fn job_of(dispatched: Dispatched) -> JobId {
    match dispatched {
        Dispatched::Started(job) => job,
        Dispatched::Done(v) => panic!("not a job: {v}"),
    }
}

/// Checks `value` against `schema`, for the keywords schemars emits. An error says where it fails.
pub fn validate(value: &Value, schema: &Value, at: &str) -> Result<(), String> {
    let fail = |why: String| Err(format!("{at}: {why}"));
    let Some(schema) = schema.as_object() else {
        return if schema == &Value::Bool(false) { fail("nothing is allowed".into()) } else { Ok(()) };
    };
    assert!(!schema.contains_key("$ref"), "{at}: schemas must be inlined");
    if let Some(types) = schema.get("type") {
        let types: Vec<&str> = match types {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        let ok = types.iter().any(|t| match *t {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            _ => false,
        });
        if !ok {
            return fail(format!("{value} isn't {types:?}"));
        }
    }
    if let Some(c) = schema.get("const")
        && c != value
    {
        return fail(format!("{value} isn't {c}"));
    }
    if let Some(Value::Array(options)) = schema.get("enum")
        && !options.contains(value)
    {
        return fail(format!("{value} isn't one of {options:?}"));
    }
    if let Some(n) = value.as_f64()
        && (schema.get("minimum").and_then(Value::as_f64).is_some_and(|m| n < m)
            || schema.get("maximum").and_then(Value::as_f64).is_some_and(|m| n > m))
    {
        return fail(format!("{n} is out of range"));
    }
    if let Some(Value::Array(options)) = schema.get("oneOf") {
        let matching = options.iter().filter(|o| validate(value, o, at).is_ok()).count();
        if matching != 1 {
            return fail(format!("{value} matches {matching} of oneOf"));
        }
    }
    if let Some(Value::Array(options)) = schema.get("anyOf")
        && !options.iter().any(|o| validate(value, o, at).is_ok())
    {
        return fail(format!("{value} matches none of anyOf"));
    }
    if let Some(Value::Array(all)) = schema.get("allOf") {
        for s in all {
            validate(value, s, at)?;
        }
    }
    if let Some(items) = value.as_array() {
        if schema.get("maxItems").and_then(Value::as_u64).is_some_and(|m| items.len() as u64 > m)
            || schema.get("minItems").and_then(Value::as_u64).is_some_and(|m| (items.len() as u64) < m)
        {
            return fail("wrong number of items".into());
        }
        if let Some(item) = schema.get("items") {
            for (i, v) in items.iter().enumerate() {
                validate(v, item, &format!("{at}/{i}"))?;
            }
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(Value::Array(required)) = schema.get("required") {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return fail(format!("`{key}` is missing"));
                }
            }
        }
        for (key, v) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(s) => validate(v, s, &format!("{at}/{key}"))?,
                None => match schema.get("additionalProperties") {
                    Some(Value::Bool(false)) => return fail(format!("`{key}` isn't allowed")),
                    Some(s @ Value::Object(_)) => validate(v, s, &format!("{at}/{key}"))?,
                    _ => {}
                },
            }
        }
    }
    Ok(())
}
