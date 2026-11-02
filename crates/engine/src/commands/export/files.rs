//! Preparing all of an export's artifacts before publishing its completion sidecar.

use super::{Exported, FileWritten, RunParams, extra_spec, file_name, merged, template_values};
use crate::commands::output::{Pixels, Publication, frame_metadata};
use crate::commands::{annotation, display, reproduce, scientific};
use crate::error::CommandError;
use crate::jobs::{JobPlan, commit};
use crate::retouch::SourceLoader;
use crate::session::Session;
use florastack_codecs::{BitDepth, OutputMetadata, OutputProfile, OutputSpec};
use florastack_core::{AnyImage, Monitor};
use florastack_project::{FrameId, Project};
use std::path::{Path, PathBuf};

enum Planned {
    Image { path: PathBuf, what: &'static str, pixels: Box<Pixels>, spec: OutputSpec, metadata: bool },
    Aligned { path: PathBuf, frame: FrameId },
    Contribution { path: PathBuf, frame: FrameId, index: usize },
    Data { path: PathBuf, bytes: Vec<u8>, what: &'static str },
}

fn planned_path(file: &Planned) -> &Path {
    match file {
        Planned::Image { path, .. } | Planned::Aligned { path, .. } | Planned::Contribution { path, .. } | Planned::Data { path, .. } => path,
    }
}

#[allow(clippy::too_many_lines)] // One plan, cancellable preparation and publication.
pub(super) fn run(session: &Session, p: RunParams) -> Result<JobPlan<Exported>, CommandError> {
    let settings = merged(session, p.settings.as_ref().map(|s| &s.0))?;
    let project = session.project()?;
    let output = project.output(p.output).ok_or(CommandError::NotFound { what: p.output.to_string() })?.clone();
    let source_state = scientific::source(project, &output)?;
    let calibration = scientific::canvas_calibration(&source_state, &output, settings.pixels_per_micrometer)?;
    let layers = if settings.annotations { annotation::layers(&output)? } else { Vec::new() };
    let folder = super::super::absolute(&p.folder)?;
    let base = file_name(&settings.template, &template_values(session, p.output)?);
    let spec = settings.spec();
    let main = folder.join(format!("{base}.{}", settings.format.extension()));
    let sidecar_path = reproduce::path(&main);
    let mut sidecar = reproduce::Sidecar::new(session, p.output, reproduce::Export::Run { settings: settings.clone() })?;
    let mut notes = Vec::new();
    let mut planned = vec![Planned::Image {
        path: main.clone(),
        what: "output",
        pixels: Box::new(Pixels::of(session, p.output, None, spec, &mut notes)?),
        spec,
        metadata: settings.metadata,
    }];
    let has = |name: &str| florastack_project::MapName::new(name).is_ok_and(|n| output.maps.contains_key(&n));
    for (wanted, name) in [(settings.depth_map, "depth"), (settings.confidence_map, "confidence")] {
        if !wanted {
            continue;
        }
        if !has(name) {
            notes.push(format!("{} has no {name} map (only depth-map outputs do), so none is written", output.name));
            continue;
        }
        let spec = extra_spec(OutputProfile::Untagged);
        planned.push(Planned::Image {
            path: folder.join(format!("{base}-{name}.tif")),
            what: if name == "depth" { "depth" } else { "confidence" },
            pixels: Box::new(Pixels::of(session, p.output, Some(name), spec, &mut notes)?),
            spec,
            metadata: false,
        });
    }
    if settings.calibrated_depth {
        if settings.z_step_micrometers.is_none() {
            return Err(session.invalid("z_step_micrometers", "is required for calibrated depth"));
        }
        if !has("depth-frames") {
            return Err(session.unavailable("this output has no full-precision depth: stack it again by the depth-map method"));
        }
        if output.inputs.iter().any(|i| !matches!(i, florastack_project::Input::Frame(_))) {
            return Err(session.unavailable("a slab depth indexes slabs, so it cannot be calibrated as capture steps"));
        }
        let spec = OutputSpec { depth: BitDepth::F32, extended_range: true, ..extra_spec(OutputProfile::Untagged) };
        planned.push(Planned::Image {
            path: folder.join(format!("{base}-depth-um.tif")),
            what: "calibrated-depth",
            pixels: Box::new(Pixels::of(session, p.output, Some("depth-frames"), spec, &mut notes)?),
            spec,
            metadata: false,
        });
    }
    let mut frames = crate::retouch::output_frames(project, &output);
    frames.sort_by_key(|id| source_state.frames.iter().position(|f| f.id() == *id).unwrap_or(usize::MAX));
    let direct: Vec<FrameId> =
        output.inputs.iter().filter_map(|i| if let florastack_project::Input::Frame(f) = i { Some(*f) } else { None }).collect();
    if settings.contribution_maps && (!has("depth-frames") || direct.len() != output.inputs.len()) {
        return Err(session.unavailable("per-frame contributions need a full-precision depth-map output made directly from frames"));
    }
    if settings.alignment_csv {
        planned.push(Planned::Data {
            path: folder.join(format!("{base}-alignment.csv")),
            bytes: alignment_csv(&source_state, &frames, &direct)?,
            what: "alignment",
        });
    }
    let mut loader =
        if settings.aligned_frames || settings.contribution_maps { Some(SourceLoader::new(session, "export.run", p.output)?) } else { None };
    for (i, &frame) in frames.iter().enumerate() {
        let stem =
            source_state.frame(frame).and_then(|f| Path::new(&f.path).file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = file_name("{name}", &[("name", stem)]);
        if settings.aligned_frames {
            planned.push(Planned::Aligned { path: folder.join(format!("{base} aligned/{:03} {stem}.tif", i + 1)), frame });
        }
        if settings.contribution_maps {
            let index =
                direct.iter().position(|f| *f == frame).ok_or_else(|| session.unavailable("a contribution frame is missing from the depth map"))?;
            planned.push(Planned::Contribution { path: folder.join(format!("{base} contributions/{:03} {stem}.tif", i + 1)), frame, index });
        }
    }
    if !p.overwrite {
        let existing = planned.iter().map(planned_path).chain(std::iter::once(sidecar_path.as_path())).find(|path| path.exists());
        if let Some(path) = existing {
            return Err(session.unavailable(format!("{} exists: pass `overwrite` to replace it", path.display())));
        }
    }
    let source = frames
        .get(settings.metadata_frame.saturating_sub(1).min(frames.len().saturating_sub(1)))
        .and_then(|f| source_state.frame(*f))
        .map(|f| PathBuf::from(&f.path));
    let frame_count = u32::try_from(frames.len()).unwrap_or(u32::MAX);
    let cache = session.cache_dir()?;
    let depth_ref = output.maps.get(&crate::retouch::map_name("depth-frames")?).cloned();
    let total = planned.len() + 1;
    let job = JobPlan::new(move |ctx| {
        let mut publication = Publication::default();
        let mut written = Vec::with_capacity(total);
        let mut warnings = notes;
        sidecar.prepare(&cache)?;
        let mut contributions = None;
        if settings.contribution_maps {
            let reference = depth_ref.as_ref().ok_or_else(|| scientific::unavailable("missing full-precision depth"))?;
            let depth = scientific::linear(crate::project::read_image(&cache, reference)?)?;
            let size = (depth.width(), depth.height());
            let mut weights = florastack_stack::depth::Contributions::new(depth, direct.len())?;
            let loader = loader.as_mut().ok_or_else(|| scientific::unavailable("missing input reader"))?;
            for (i, &frame) in direct.iter().enumerate() {
                ctx.step(i as u64, direct.len() as u64)?;
                let warped = loader.warp(frame, size)?;
                weights.add_frame(&florastack_stack::Patch { image: warped.image, coverage: warped.coverage })?;
            }
            contributions = Some((weights, size));
        }
        let mut size = None;
        for (i, file) in planned.into_iter().enumerate() {
            ctx.step(i as u64, total as u64)?;
            let (path, what, bytes) = match file {
                Planned::Image { path, what, pixels, spec, metadata } => {
                    let mut pixels = pixels.read(&cache, &mut warnings)?;
                    size.get_or_insert((pixels.width(), pixels.height()));
                    if what == "output" {
                        pixels = annotation::render(pixels, &layers, calibration)?;
                    }
                    if what == "calibrated-depth" {
                        pixels = AnyImage::F32(scientific::calibrated_depth(
                            &scientific::linear(pixels)?,
                            &source_state,
                            &output,
                            settings.z_step_micrometers.ok_or_else(|| scientific::unavailable("capture step is unknown"))?,
                            settings.z_origin_micrometers,
                        )?);
                    }
                    let source = if metadata { frame_metadata(source.as_deref(), &mut warnings) } else { None };
                    let encoded =
                        florastack_codecs::write(&pixels, &spec, &OutputMetadata { source, frame_count: Some(frame_count), modified: None })?;
                    warnings.extend(encoded.warnings.iter().map(|w| format!("{}: {w}", display(&path))));
                    (path, what, encoded.bytes)
                }
                Planned::Aligned { path, frame } => {
                    let loader = loader.as_mut().ok_or_else(|| scientific::unavailable("missing input reader"))?;
                    let warped = loader.warp(frame, size.ok_or_else(|| scientific::unavailable("missing output dimensions"))?)?;
                    let encoded =
                        florastack_codecs::write(&AnyImage::F32(warped.image), &extra_spec(OutputProfile::Input), &OutputMetadata::default())?;
                    (path, "aligned", encoded.bytes)
                }
                Planned::Contribution { path, frame, index } => {
                    let (weights, size) = contributions.as_ref().ok_or_else(|| scientific::unavailable("missing contribution weights"))?;
                    let loader = loader.as_mut().ok_or_else(|| scientific::unavailable("missing input reader"))?;
                    let warped = loader.warp(frame, *size)?;
                    let map = weights.map(index, &florastack_stack::Patch { image: warped.image, coverage: warped.coverage })?;
                    let encoded = florastack_codecs::write(&AnyImage::F32(map), &extra_spec(OutputProfile::Untagged), &OutputMetadata::default())?;
                    (path, "contribution", encoded.bytes)
                }
                Planned::Data { path, bytes, what } => (path, what, bytes),
            };
            publication.stage(&path, &bytes)?;
            sidecar.artifact(&main, &path, what, &bytes);
            written.push(FileWritten { path: display(&path), bytes: bytes.len() as u64, what });
        }
        let bytes = sidecar.bytes()?;
        publication.stage(&sidecar_path, &bytes)?;
        written.push(FileWritten { path: display(&sidecar_path), bytes: bytes.len() as u64, what: "sidecar" });
        publication.publish(ctx, p.overwrite)?;
        let exported = Exported { files: written, warnings };
        Ok(commit(move |_: &mut Session| Ok(exported)))
    });
    Ok(job.stages(&[("Preparing and publishing files", 1.0)]))
}

fn alignment_csv(project: &Project, frames: &[FrameId], direct: &[FrameId]) -> Result<Vec<u8>, CommandError> {
    use std::fmt::Write as _;
    let mut csv = String::from("frame_id,capture_index,stack_index,path,shift_x_px,shift_y_px,scale,rotation_rad,gain,score\r\n");
    for frame in frames {
        let f = project.frame(*frame).ok_or(CommandError::NotFound { what: frame.to_string() })?;
        let a = f.alignment.ok_or_else(|| scientific::unavailable(format!("{frame} has no alignment parameters")))?;
        let [[xx, _, x], [yx, _, y]] = a.transform.m;
        let (scale, rotation) = (xx.hypot(yx), yx.atan2(xx));
        let capture = project.frames.iter().position(|f| f.id() == *frame).unwrap_or(0);
        let stack = direct.iter().position(|f| f == frame).map(|i| i.to_string()).unwrap_or_default();
        let path = f.path.replace('"', "\"\"");
        let _ = writeln!(csv, "{},{capture},{stack},\"{path}\",{x},{y},{scale},{rotation},{},{}\r", frame.get(), a.brightness_gain, a.quality);
    }
    Ok(csv.into_bytes())
}
