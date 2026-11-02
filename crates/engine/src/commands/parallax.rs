//! 3D previews, textured mesh exports and synthetic stereo/rocking exports from an output snapshot.

use florastack_project::{Aspects, OutputId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::output::{Pixels, Publication};
use crate::jobs::{JobPlan, Lane, commit};
use crate::parallax::{Settings, Stereo, Surface};
use crate::registry::{CommandSpec, Effects, Meta, Params, Requires};
use crate::{CommandError, Invalid, Session};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::job(Meta::new("parallax.preview","View 3D relief","Builds a textured relief for the orbit viewer.")
            .help("Uses the selected output's immutable depth and confidence maps and its visible retouch. Frame zero is nearest unless reverseDepth; geometry is relative, not metric. Samples below the confidence threshold remain holes. The preview is stored in the project cache.")
            .requires(Requires::Project).examples(&[r#"{"output":3}"#]),Lane::Preview,preview),
        CommandSpec::job(Meta::new("parallax.mesh","Export 3D mesh","Exports a textured relative-depth relief as GLB or OBJ.")
            .help("GLB embeds its PNG texture. OBJ writes model.obj, material.mtl and texture.png into the chosen folder. The surface uses confidence masking and weighted smoothing; depth discontinuities can leave holes. These are relative focus depths, not metric geometry. Existing files require overwrite.")
            .requires(Requires::Project).effects(Effects::Files).examples(&[r#"{"output":3,"folder":"exports/model","format":"glb"}"#]),Lane::Heavy,mesh),
        CommandSpec::job(Meta::new("parallax.stereo","Export synthetic stereo","Exports parallel or cross-eyed stereo pairs, or a red-cyan anaglyph PNG.")
            .help("Parallel camera reprojection converges at relative depth 0.5. Baseline is a fraction of image width; smaller depths move right in the left eye and left in the right eye. Cross-eyed output swaps the eyes. Unknown disocclusions and masked samples are transparent; no hidden surfaces are invented. Use reverseDepth for far-to-near captures.")
            .requires(Requires::Project).effects(Effects::Files).examples(&[r#"{"output":3,"folder":"exports","mode":"parallel","settings":{"baseline":0.04}}"#]),Lane::Heavy,stereo),
        CommandSpec::job(Meta::new("parallax.animate","Export rocking animation","Exports a looping back-and-forth parallax animation as GIF.")
            .help("A sinusoidal camera excursion uses the stereo baseline. Frames are rendered and encoded one at a time, with infinite looping; unknown disocclusions appear black. GIF timing is in centiseconds. The selected output and its depth/confidence maps are read from one immutable snapshot.")
            .requires(Requires::Project).effects(Effects::Files).examples(&[r#"{"output":3,"folder":"exports","frames":32,"fps":16}"#]),Lane::Heavy,animate),
    ]
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum MeshFormat {
    #[default]
    Glb,
    Obj,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PreviewParams {
    /// The depth-map output to view.
    output: OutputId,
    /// Confidence, smoothing, depth direction, relief and resolution.
    #[serde(default)]
    settings: Settings,
}
impl Params for PreviewParams {
    fn check(&self) -> Result<(), Invalid> {
        self.settings.check()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExportParams {
    /// The depth-map output to export.
    output: OutputId,
    /// Destination folder, created when needed.
    folder: String,
    /// Confidence, smoothing, depth direction, relief, resolution and baseline.
    #[serde(default)]
    settings: Settings,
    /// Replace existing destination files.
    #[serde(default)]
    overwrite: bool,
    /// Mesh format: glb or obj (mesh command only).
    #[serde(default)]
    format: Option<MeshFormat>,
    /// Stereo packing: parallel, cross-eyed or anaglyph (stereo command only).
    #[serde(default)]
    mode: Option<Stereo>,
    /// Animation frame count, 4 to 120 (animation command only; default 32).
    #[serde(default)]
    #[schemars(range(min = 4, max = 120))]
    frames: Option<u16>,
    /// Animation frames per second, 1 to 50 (animation command only; default 16).
    #[serde(default)]
    #[schemars(range(min = 1, max = 50))]
    fps: Option<u16>,
}
impl Params for ExportParams {
    fn check(&self) -> Result<(), Invalid> {
        super::check_path("folder", &self.folder)?;
        self.settings.check()?;
        if self.frames.is_some_and(|n| !(4..=120).contains(&n)) {
            return Err(Invalid::new("frames", "must be from 4 to 120"));
        }
        if self.fps.is_some_and(|n| !(1..=50).contains(&n)) {
            return Err(Invalid::new("fps", "must be from 1 to 50"));
        }
        Ok(())
    }
}

#[derive(Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Preview {
    path: String,
    output: OutputId,
}
#[derive(Serialize, JsonSchema)]
struct Exported {
    files: Vec<String>,
}

struct Input {
    cache: florastack_project::CacheDir,
    pixels: Pixels,
    depth: Pixels,
    confidence: Pixels,
    divisor: f32,
}
impl Input {
    fn plan(session: &Session, id: OutputId) -> Result<Self, CommandError> {
        let output = session.project()?.output(id).ok_or(CommandError::NotFound { what: id.to_string() })?;
        let full = output.maps.contains_key(&crate::retouch::map_name("depth-frames")?);
        if !output.maps.contains_key(&crate::retouch::map_name("confidence")?)
            || !output.maps.contains_key(&crate::retouch::map_name(if full { "depth-frames" } else { "depth" })?)
        {
            return Err(session.unavailable("this output needs depth and confidence maps: stack it by the depth-map method"));
        }
        let spec = florastack_codecs::OutputSpec::new(florastack_codecs::Format::Png)?;
        let mut notes = Vec::new();
        Ok(Self {
            cache: session.cache_dir()?,
            pixels: Pixels::of(session, id, None, spec, &mut notes)?,
            depth: Pixels::of(session, id, Some(if full { "depth-frames" } else { "depth" }), spec, &mut notes)?,
            confidence: Pixels::of(session, id, Some("confidence"), spec, &mut notes)?,
            divisor: if full { output.inputs.len().saturating_sub(1).max(1) as f32 } else { 1.0 },
        })
    }
    fn surface(self, settings: &Settings, monitor: &dyn florastack_core::Monitor) -> Result<Surface, CommandError> {
        let mut notes = Vec::new();
        let image = super::scientific::linear(self.pixels.read(&self.cache, &mut notes)?)?;
        let mut depth = super::scientific::linear(self.depth.read(&self.cache, &mut notes)?)?;
        for row in depth.rows_mut() {
            for d in row {
                *d /= self.divisor;
            }
        }
        let confidence = super::scientific::linear(self.confidence.read(&self.cache, &mut notes)?)?;
        Surface::new(&image, &depth, &confidence, settings, monitor)
    }
}

fn preview(session: &Session, p: PreviewParams) -> Result<JobPlan<Preview>, CommandError> {
    let input = Input::plan(session, p.output)?;
    let cache = input.cache.clone();
    Ok(JobPlan::new(move |ctx| {
        let surface = input.surface(&p.settings, ctx)?;
        let scene = surface.mesh(&p.settings, ctx)?;
        let bytes = serde_json::to_vec(&scene).map_err(|source| CommandError::Params { command: "parallax.preview".into(), source })?;
        florastack_core::Monitor::step(ctx, 1, 1)?;
        let reference = cache.store(florastack_project::Area::Cache, "relief", "json", &bytes, None)?;
        let path = super::display(&reference.path.resolve(cache.path()));
        Ok(commit(move |_: &mut Session| Ok(Preview { path, output: p.output })))
    })
    .locks(Aspects::ALL))
}

enum Kind {
    Mesh,
    Stereo,
    Animation,
}
fn mesh(session: &Session, p: ExportParams) -> Result<JobPlan<Exported>, CommandError> {
    export(session, p, Kind::Mesh)
}
fn stereo(session: &Session, p: ExportParams) -> Result<JobPlan<Exported>, CommandError> {
    export(session, p, Kind::Stereo)
}
fn animate(session: &Session, p: ExportParams) -> Result<JobPlan<Exported>, CommandError> {
    export(session, p, Kind::Animation)
}

fn export(session: &Session, p: ExportParams, kind: Kind) -> Result<JobPlan<Exported>, CommandError> {
    let inappropriate = match kind {
        Kind::Mesh => p.mode.is_some() || p.frames.is_some() || p.fps.is_some(),
        Kind::Stereo => p.format.is_some() || p.frames.is_some() || p.fps.is_some(),
        Kind::Animation => p.format.is_some() || p.mode.is_some(),
    };
    if inappropriate {
        return Err(session.invalid("options", "contains options for another parallax export command"));
    }
    let input = Input::plan(session, p.output)?;
    let folder = super::absolute(&p.folder)?;
    Ok(JobPlan::new(move |ctx| {
        let surface = input.surface(&p.settings, ctx)?;
        let files = match kind {
            Kind::Mesh => {
                let scene = surface.mesh(&p.settings, ctx)?;
                match p.format.unwrap_or_default() {
                    MeshFormat::Glb => vec![("model.glb", crate::parallax::glb(&scene)?)],
                    MeshFormat::Obj => vec![
                        ("model.obj", crate::parallax::obj(&scene)?),
                        ("material.mtl", b"newmtl composite\nKd 1 1 1\nKa 1 1 1\nd 1\nillum 0\nmap_Kd texture.png\n".to_vec()),
                        ("texture.png", crate::parallax::formats::png(&scene)?),
                    ],
                }
            }
            Kind::Stereo => {
                let left = surface.view(-0.5, p.settings.baseline, ctx)?;
                let right = surface.view(0.5, p.settings.baseline, ctx)?;
                let mode = p.mode.unwrap_or_default();
                let name = match mode {
                    Stereo::Parallel => "parallel.png",
                    Stereo::CrossEyed => "cross-eyed.png",
                    Stereo::Anaglyph => "anaglyph.png",
                };
                vec![(name, crate::parallax::formats::encode_png(crate::parallax::stereo(&left, &right, mode)?)?)]
            }
            Kind::Animation => {
                vec![("rocking.gif", crate::parallax::animation(&surface, &p.settings, p.frames.unwrap_or(32), p.fps.unwrap_or(16), ctx)?)]
            }
        };
        let mut publication = Publication::default();
        let mut paths = Vec::new();
        for (name, bytes) in files {
            let path = folder.join(name);
            publication.stage(&path, &bytes)?;
            paths.push(super::display(&path));
        }
        publication.publish(ctx, p.overwrite)?;
        Ok(commit(move |_: &mut Session| Ok(Exported { files: paths })))
    })
    .locks(Aspects::ALL))
}
