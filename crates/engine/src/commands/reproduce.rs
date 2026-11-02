//! Versioned export manifests and native replay from original, SHA-256-verified inputs.
//! Output pixels are recomputed; non-regenerable project masks are the only binary assets in the sidecar.

use florastack_core::Monitor;
use std::path::{Path, PathBuf};

use florastack_codecs::OutputSpec;
use florastack_project::{CacheDir, CacheRef, ContentHash, FrameFile, FrameId, Input, OutputId, Project};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::export::OutputSettings;
use crate::error::CommandError;
use crate::session::Session;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
pub(super) enum Export {
    #[serde(rename = "export.run")]
    Run { settings: OutputSettings },
    #[serde(rename = "output.save")]
    Save { spec: OutputSpec, map: Option<String>, metadata: bool, metadata_from: Option<FrameId>, settings: OutputSettings },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    reference: CacheRef,
    bytes: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: String,
    kind: String,
    hash: ContentHash,
    bytes: u64,
}

/// All processing settings, immutable per-output input state, input hashes, layers and export options. Input and
/// cache paths are data, never commands. Replay selects its destination separately and never writes a recorded path.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Sidecar {
    format: String,
    version: u32,
    software: String,
    software_version: String,
    project: Project,
    output: OutputId,
    export: Export,
    compute: Value,
    /// Units and coordinate conventions are explicit for plotting tools.
    conventions: Value,
    assets: Vec<Asset>,
    artifacts: Vec<Artifact>,
}

pub(super) fn path(image: &Path) -> PathBuf {
    let mut name = image.as_os_str().to_os_string();
    name.push(".json");
    PathBuf::from(name)
}

impl Sidecar {
    pub(super) fn new(session: &Session, output: OutputId, export: Export) -> Result<Self, CommandError> {
        let original = session.project()?;
        let mut project = super::scientific::snapshot(session)?;
        let mut visiting = std::collections::BTreeSet::new();
        let mut done = std::collections::BTreeSet::new();
        dependencies(original, output, &mut visiting, &mut done, &mut project.outputs)?;
        Ok(Self {
            format: "florastack-export".into(),
            version: 1,
            software: florastack_project::folder::software(),
            software_version: env!("CARGO_PKG_VERSION").into(),
            project,
            output,
            export,
            compute: json!({"device":session.compute().device,"gpuBudgetBytes":session.compute().budget,"hostBudgetBytes":session.tiling().budget,"replayDevice":"cpu"}),
            conventions: json!({
                "depth": "uint16 / 65535 * (number of direct inputs - 1); zero indexes the first composited input",
                "calibrated-depth": "float32 micrometers; interpolate signed capture steps in recorded capture order, origin at the first capture",
                "confidence": "uint16 / 65535; 0.5 is the contrast threshold, not a probability",
                "contribution": "uint16 / 65535; normalized compositing weight including coverage and nearest-frame fallback; all zero where uncovered",
                "alignment": "master pixel -> input pixel; shift in pixels, isotropic scale, rotation in radians, multiplicative brightness gain, score in [0,1]",
                "calibration": "pixels per micrometer in the master image; scale bars account for the output canvas-to-master scale"
            }),
            assets: Vec::new(),
            artifacts: Vec::new(),
        })
    }

    /// Read just the non-regenerable assets while still on a cancellable worker.
    pub(super) fn prepare(&mut self, cache: &CacheDir) -> Result<(), CommandError> {
        let mut references: Vec<CacheRef> = self.project.maps.values().cloned().collect();
        for output in &self.project.outputs {
            let state = super::scientific::source(&self.project, output)?;
            references.extend(state.maps.values().cloned());
        }
        references.sort_by(|a, b| a.path.cmp(&b.path));
        references.dedup_by(|a, b| a.path == b.path);
        for reference in references {
            self.assets.push(Asset { bytes: cache.read(&reference)?, reference });
        }
        Ok(())
    }

    pub(super) fn artifact(&mut self, main: &Path, file: &Path, kind: &str, bytes: &[u8]) {
        let relative = main.parent().and_then(|p| file.strip_prefix(p).ok()).unwrap_or(file);
        self.artifacts.push(Artifact { path: super::display(relative), kind: kind.into(), hash: ContentHash::of(bytes), bytes: bytes.len() as u64 });
    }

    pub(super) fn bytes(&self) -> Result<Vec<u8>, CommandError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|source| florastack_project::ProjectError::Data { what: "export sidecar", source })?;
        if bytes.len() > 64 << 20 {
            return Err(super::scientific::unavailable("the reproducibility sidecar exceeds 64 MiB"));
        }
        Ok(bytes)
    }

    pub(super) fn estimated_bytes(&self) -> Result<u64, CommandError> {
        let mut size = self.bytes()?.len() as u64;
        let mut paths = std::collections::BTreeSet::new();
        for output in &self.project.outputs {
            let state = super::scientific::source(&self.project, output)?;
            for reference in state.maps.values().chain(self.project.maps.values()) {
                if paths.insert(reference.path.clone()) {
                    size = size.saturating_add(reference.size.saturating_mul(4));
                }
            }
        }
        Ok(size)
    }
}

fn dependencies(
    project: &Project,
    id: OutputId,
    visiting: &mut std::collections::BTreeSet<OutputId>,
    done: &mut std::collections::BTreeSet<OutputId>,
    out: &mut Vec<florastack_project::Output>,
) -> Result<(), CommandError> {
    if done.contains(&id) {
        return Ok(());
    }
    if visiting.len() >= 64 || !visiting.insert(id) {
        return Err(super::scientific::unavailable("output dependencies are cyclic or exceed 64 levels"));
    }
    let output = project.output(id).ok_or(CommandError::NotFound { what: id.to_string() })?;
    let mut sources: Vec<OutputId> = output.inputs.iter().filter_map(|i| if let Input::Output(o) = i { Some(*o) } else { None }).collect();
    for stroke in crate::retouch::visible_strokes(output) {
        if let Some(florastack_retouch::Source::Output(id)) = stroke.source {
            sources.push(id);
        }
    }
    for source in sources {
        dependencies(project, source, visiting, done, out)?;
    }
    visiting.remove(&id);
    done.insert(id);
    let mut output = output.clone();
    output.preview = None;
    out.push(output);
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn specs() -> Vec<crate::registry::CommandSpec> {
    use crate::registry::{CommandSpec, Effects, Meta};
    vec![CommandSpec::job(
        Meta::new("export.reproduce", "Reproduce an export", "Recomputes an export from its sidecar and original input files.")
            .help("Verifies the manifest version, software version, original SHA-256 input hashes and embedded masks. Replays each stacking method with its recorded development and alignment, then retouch and annotation layers and the original export settings. No original project or output cache is needed. Destination paths are selected here, never taken from the sidecar. CPU/GPU arithmetic can differ slightly when the device changes.")
            .effects(Effects::Files).examples(&[r#"{"sidecar": "C:/Exports/result.tif.json", "folder": "C:/Reproduced"}"#]),
        crate::jobs::Lane::Heavy, reproduce,
    )]
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReproduceParams {
    /// Versioned sidecar beside the original exported image.
    sidecar: String,
    /// New destination folder. Original paths in the sidecar are never written.
    folder: String,
    /// Replace destination files if they exist.
    #[serde(default)]
    overwrite: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl crate::registry::Params for ReproduceParams {
    fn check(&self) -> Result<(), crate::error::Invalid> {
        super::check_path("sidecar", &self.sidecar)?;
        super::check_path("folder", &self.folder)
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Serialize, schemars::JsonSchema)]
struct Reproduced {
    result: Value,
}

#[cfg(not(target_arch = "wasm32"))]
fn reproduce(_: &Session, p: ReproduceParams) -> Result<crate::jobs::JobPlan<Reproduced>, CommandError> {
    let sidecar = super::absolute(&p.sidecar)?;
    let destination = super::absolute(&p.folder)?;
    Ok(crate::jobs::JobPlan::new(move |ctx| {
        let data = super::calibration::read_capped(&sidecar, 64 << 20)?;
        let mut recipe: Sidecar = serde_json::from_slice(&data).map_err(|source| florastack_project::ProjectError::Data { what: "export sidecar", source })?;
        if recipe.format != "florastack-export" || recipe.version != 1 || recipe.software_version != env!("CARGO_PKG_VERSION") {
            return Err(super::scientific::unavailable("the sidecar format or software version differs: use the version named by the sidecar"));
        }
        recipe.project.validate().map_err(|e| super::scientific::unavailable(e.to_string()))?;
        // Validate the graph before following any references. Reorder to include retouch-source dependencies.
        let mut ordered = Vec::new();
        dependencies(&recipe.project, recipe.output, &mut std::collections::BTreeSet::new(), &mut std::collections::BTreeSet::new(), &mut ordered)?;
        let stage = tempfile::Builder::new().prefix("florastack-reproduce-").tempdir().map_err(|e| super::batch::io_error(&sidecar, e))?;
        let (mut folder, _) = florastack_project::ProjectFolder::create_temporary(stage.path())?;
        let project_path = folder.path().to_path_buf();
        let mut rebuilt = Vec::new();
        for (index, mut output) in ordered.into_iter().enumerate() {
            ctx.step(index as u64, recipe.project.outputs.len() as u64 + 1)?;
            let recorded = super::scientific::source(&recipe.project, &output)?;
            let mut source = recipe.project.clone();
            source.frames = recorded.frames;
            source.develop = recorded.develop;
            source.alignment = recorded.alignment;
            source.preferences = recorded.preferences;
            source.maps = recorded.maps;
            keep_assets(&mut source, &recipe.assets)?;
            let mut needed: std::collections::BTreeSet<FrameId> = output.inputs.iter().filter_map(|i| if let Input::Frame(f) = i { Some(*f) } else { None }).collect();
            for stroke in crate::retouch::visible_strokes(&output) {
                if let Some(florastack_retouch::Source::Frame(id)) = stroke.source { needed.insert(id); }
            }
            for frame in &source.frames {
                ctx.checkpoint()?;
                // Alignment is replayed as recorded, so even a rejected frame needn't be decoded again.
                if !needed.contains(&frame.id()) { continue; }
                let actual = FrameFile::read(Path::new(&frame.path))?;
                if actual.hash != frame.hash || actual.size != frame.size {
                    return Err(super::scientific::unavailable(format!("input hash mismatch: {}", frame.path)));
                }
            }
            source.outputs.clone_from(&rebuilt);
            folder.save(&mut source)?;
            restore_assets(&recipe.assets, &project_path)?;
            drop(folder);
            let mut engine = replay_engine(stage.path());
            engine.run("project.open", json!({"path": project_path}))?;
            let stored = rebuild(engine.session(), &output, ctx)?;
            output.image = stored.image;
            output.preview = Some(stored.preview);
            output.maps = stored.maps;
            rebuilt.push(output);
            drop(engine);
            // Preserve newly computed cache references before opening, whose orphan cleanup follows this document.
            source.outputs.clone_from(&rebuilt);
            let bytes = serde_json::to_vec(&source).map_err(|source| florastack_project::ProjectError::Data { what: "replay project", source })?;
            let file = project_path.join(florastack_project::schema::FILE_NAME);
            std::fs::write(&file, bytes).map_err(|e| super::batch::io_error(&file, e))?;
            (folder, _) = florastack_project::ProjectFolder::open(&project_path)?;
        }
        recipe.project.outputs = rebuilt;
        keep_assets(&mut recipe.project, &recipe.assets)?;
        folder.save(&mut recipe.project)?;
        restore_assets(&recipe.assets, &project_path)?;
        drop(folder);
        let mut engine = replay_engine(stage.path());
        engine.run("project.open", json!({"path": project_path}))?;
        let staged_exports = stage.path().join("export");
        std::fs::create_dir_all(&staged_exports).map_err(|e| super::batch::io_error(&staged_exports, e))?;
        let mut result = match recipe.export {
            Export::Run { settings } => {
                engine.run("export.reset", json!({}))?;
                engine.run("export.set", serde_json::to_value(settings).map_err(|source| florastack_project::ProjectError::Data { what: "export settings", source })?)?;
                super::batch::call(&mut engine, ctx, "export.run", json!({"output": recipe.output, "folder": staged_exports}))?
            }
            Export::Save { spec, map, metadata, metadata_from, settings } => {
                engine.run("export.reset", json!({}))?;
                engine.run("export.set", serde_json::to_value(settings).map_err(|source| florastack_project::ProjectError::Data { what: "export settings", source })?)?;
                let name = recipe.artifacts.iter().find(|a| a.kind == "output" || a.kind == "map").and_then(|a| Path::new(&a.path).file_name()).ok_or_else(|| super::scientific::unavailable("the sidecar has no exported image"))?;
                super::batch::call(&mut engine, ctx, "output.save", json!({"output": recipe.output, "path": staged_exports.join(name), "spec": spec, "map": map, "metadata": metadata, "metadataFrom": metadata_from}))?
            }
        };
        let mut publication = super::output::Publication::default();
        if let Some(files) = result.get_mut("files").and_then(Value::as_array_mut) {
            for file in files {
                publish_copy(&mut publication, &mut file["path"], &staged_exports, &destination, ctx)?;
            }
        } else {
            for field in ["path", "sidecar"] { publish_copy(&mut publication, &mut result[field], &staged_exports, &destination, ctx)?; }
        }
        publication.publish(ctx, p.overwrite)?;
        Ok(crate::jobs::commit(move |_| Ok(Reproduced { result })))
    }).stages(&[("Reproducing export", 1.0)]))
}

#[cfg(not(target_arch = "wasm32"))]
fn publish_copy(
    publication: &mut super::output::Publication,
    value: &mut Value,
    stage: &Path,
    destination: &Path,
    ctx: &crate::jobs::JobContext,
) -> Result<(), CommandError> {
    ctx.checkpoint()?;
    let source = PathBuf::from(value.as_str().ok_or_else(|| super::scientific::unavailable("an exported file has no path"))?);
    let relative = source.strip_prefix(stage).map_err(|_| super::scientific::unavailable("an exported path is outside the replay workspace"))?;
    let target = destination.join(relative);
    let bytes =
        std::fs::read(&source).map_err(|source_error| florastack_project::ProjectError::Read { path: source.clone(), source: source_error })?;
    publication.stage(&target, &bytes)?;
    *value = json!(super::display(&target));
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn replay_engine(stage: &Path) -> crate::Engine {
    crate::Engine::new(crate::EngineConfig {
        temporary_projects: Some(stage.join("temporary")),
        compute: crate::Compute::CPU,
        ..crate::EngineConfig::default()
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn restore_assets(assets: &[Asset], project: &Path) -> Result<(), CommandError> {
    for asset in assets {
        if asset.reference.size != asset.bytes.len() as u64 || asset.reference.hash != ContentHash::of(&asset.bytes) {
            return Err(super::scientific::unavailable("embedded mask hash mismatch"));
        }
        let path = asset.reference.path.resolve(project);
        std::fs::write(&path, &asset.bytes).map_err(|e| super::batch::io_error(&path, e))?;
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn keep_assets(project: &mut Project, assets: &[Asset]) -> Result<(), CommandError> {
    for (i, asset) in assets.iter().enumerate() {
        project.maps.insert(crate::retouch::map_name(&format!("reproduce-{i}"))?, asset.reference.clone());
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn rebuild(session: &Session, output: &florastack_project::Output, ctx: &crate::jobs::JobContext) -> Result<super::stack::Stored, CommandError> {
    use super::stack::{MethodName, MethodParams, StackPlan, Units};
    let mut params = output.parameters.clone();
    let object = params.as_object_mut().ok_or_else(|| super::scientific::unavailable("output parameters are not a table"))?;
    for field in ["sourceState", "framing", "slab", "slabs", "compute"] {
        object.remove(field);
    }
    let method = match output.method.as_str() {
        "average" => MethodName::Average,
        "pyramid" => MethodName::Pyramid,
        "depth-map" => MethodName::DepthMap,
        _ => return Err(super::scientific::unavailable("the sidecar names an unknown stacking method")),
    };
    let params: MethodParams = florastack_project::from_data("stack parameters", &params)?;
    params.check_for(method).map_err(|e| session.invalid(e.field, e.reason))?;
    let settings = params.chosen(method, &crate::project::stack_settings(session.project()?)?);
    settings.validate()?;
    let cache = session.cache_dir()?;
    let (stacked, depth) = if output.inputs.iter().all(|i| matches!(i, Input::Frame(_))) {
        let frames: Vec<FrameId> = output.inputs.iter().filter_map(|i| if let Input::Frame(f) = i { Some(*f) } else { None }).collect();
        let mut plan = StackPlan::of_frames(
            session,
            "export.reproduce",
            settings.clone(),
            florastack_raw::Quality::Output,
            &frames,
            crate::retouch::output_framing(output),
        )?;
        plan.stack_any(ctx, false)?
    } else {
        let sources = output
            .inputs
            .iter()
            .map(|i| match i {
                Input::Output(id) => session
                    .project()?
                    .output(*id)
                    .map(|o| super::stack::unclipped(&o.image, &o.maps).clone())
                    .ok_or(CommandError::NotFound { what: id.to_string() }),
                Input::Frame(_) => Err(super::scientific::unavailable("mixed frame and slab inputs cannot be replayed")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let first = sources.first().ok_or_else(|| super::scientific::unavailable("the output has no inputs"))?;
        let image = crate::project::read_image(&cache, first)?;
        let size = (image.width(), image.height());
        drop(image);
        let mut feed = super::slab::Cached { cache: &cache, sources: &sources, size };
        super::stack::stack_feed(
            &mut feed,
            &settings,
            &session.tiling(),
            &session.compute(),
            ctx,
            ctx,
            &mut Units::new(sources.len() * settings.passes()),
            |_, _| {},
        )?
    };
    let tone = settings.ranged().or_else(|| {
        output
            .parameters
            .get("slabs")
            .and_then(|s| s.get("parameters"))
            .and_then(|p| p.get("tone"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    });
    super::stack::store(&cache, "export.reproduce", "reproduced", stacked, depth, tone, None)
}
