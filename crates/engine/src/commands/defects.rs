//! Sensor defects: the stack's defect mask, dust and hot pixels at the same sensor pixels in every frame, which the
//! alignment jobs inpaint in each frame before aligning it (florastack-align's `defects.rs`). The mask is one of the
//! project's maps (`defects`), stored as a lossless image of black spots on white, and comes from detection
//! (`defects.detect`, a job, or `align.run` with the `detect_defects` setting) or from an image the user drew or edited
//! (`defects.import`); `defects.export` writes it out for editing, `defects.clear` removes it and `defects.info` tells
//! what the project holds.

use std::path::PathBuf;

use florastack_align::{DefectDetector, MIN_DETECTION_FRAMES, detection_frames};
use florastack_codecs::{BitDepth, OutputFormat, OutputMetadata, OutputProfile, OutputSpec};
use florastack_core::{AnyImage, Monitor};
use florastack_project::{Aspects, Edit};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::align::Plan;
use super::output::write_atomically;
use super::{absolute, check_path, display};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
use crate::project::{
    MaskSource, MaskSummary, align_settings, defect_mask_ref, defects_map, develop_record, mask_summary, read_defect_mask, read_mask_file,
    store_defect_mask, stored_alignment,
};
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("defects.info", "Defect mask", "Whether the stack has a defect mask, where it came from and what it masks.")
                .help(
                    "The defect mask marks the dust and hot pixels that sit at the same sensor pixels in every frame. \
                     The alignment jobs inpaint it in every frame before aligning them, and stacking will read the \
                     frames the same way. `detect` says whether `align.run` detects a mask when the project has none \
                     (the `detect_defects` alignment setting).",
                )
                .requires(Requires::Project),
            info,
        ),
        CommandSpec::job(
            Meta::new(
                "defects.detect",
                "Detect dust and hot pixels",
                "Finds the dust and hot pixels of the enabled frames and makes them the stack's defect mask.",
            )
            .help(
                "Reads up to 32 of the enabled frames, spread over the stack (at least 5 are needed), and marks \
                     the pixels that are darker than all around them by the same factor in every frame (dust) or \
                     brighter than their neighbors in most frames (hot pixels), at the same sensor pixels, grown to \
                     cover the faint rim of each dust shadow. Review the mask before stacking with it: \
                     `defects.export` writes it as an image to look at or edit, and `defects.import` reads it back. \
                     Replaces the project's mask, as one undo step; align again for it to apply.",
            )
            .undoable(),
            Lane::Heavy,
            detect,
        ),
        CommandSpec::job(
            Meta::new("defects.import", "Import defect mask", "Makes an image of black spots on white the stack's defect mask.")
                .help(
                    "The image must have the frames' size: black (or any value below half) where a pixel is \
                     defective, white elsewhere; alpha is ignored. Any format FloraStack reads will do, such as a \
                     PNG drawn over a frame, or one that `defects.export` wrote and you edited. Replaces the \
                     project's mask, as one undo step; align again for it to apply.",
                )
                .undoable()
                .examples(&[r#"{ "path": "C:/Stacks/dust.png" }"#]),
            Lane::Heavy,
            import,
        ),
        CommandSpec::job(
            Meta::new("defects.export", "Export defect mask", "Writes the stack's defect mask as an 8-bit PNG of black spots on white.")
                .help(
                    "For you to look at, edit in any image editor and read back with `defects.import`. An existing \
                     file is replaced only with `overwrite`.",
                )
                .requires(Requires::Project)
                .effects(Effects::Files)
                .examples(&[r#"{ "path": "C:/Stacks/dust.png" }"#, r#"{ "path": "C:/Stacks/dust.png", "overwrite": true }"#]),
            Lane::Heavy,
            export,
        ),
        CommandSpec::instant(
            Meta::new("defects.clear", "Remove defect mask", "Removes the stack's defect mask, so that no pixel is inpainted.")
                .help("Its file stays until the project is closed, so that undo can bring it back. Align again for it to apply.")
                .undoable(),
            clear,
        ),
    ]
}

/// The stack's defect mask.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Info {
    /// Whether the project has a defect mask.
    mask: bool,
    /// What it is, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<MaskSummary>,
    /// Whether `align.run` detects a mask when the project has none.
    detect: bool,
}

fn info(session: &Session, _: NoParams) -> Result<Info, CommandError> {
    let project = session.project()?;
    let reference = defect_mask_ref(project);
    Ok(Info { mask: reference.is_some(), summary: reference.and_then(mask_summary), detect: align_settings(project)?.detect_defects })
}

/// The defect mask the project holds now.
#[derive(Debug, Serialize, JsonSchema)]
struct Mask {
    summary: MaskSummary,
}

fn detect(session: &Session, _: NoParams) -> Result<JobPlan<Mask>, CommandError> {
    let mut plan = Plan::new(session, "defects.detect", MIN_DETECTION_FRAMES)?;
    let cache = session.cache_dir()?;
    let job = JobPlan::new(move |ctx| {
        ctx.stage(0);
        let picked = detection_frames(plan.frame_count());
        let mut detector: Option<DefectDetector> = None;
        for (done, &i) in picked.iter().enumerate() {
            ctx.checkpoint()?;
            let image = plan.read_unmasked(i)?;
            let d = match &mut detector {
                Some(d) => d,
                empty => empty.insert(DefectDetector::new(image.width(), image.height())?),
            };
            d.add(&image, ctx).map_err(|e| plan.frame_error(i, e))?;
            ctx.step(done as u64 + 1, picked.len() as u64)?;
        }
        let mask = detector.ok_or(CommandError::Internal { command: "defects.detect".into() })?.finish()?;
        let reference = store_defect_mask(&cache, &mask, MaskSource::Detected)?;
        let summary = MaskSummary::of(&mask, MaskSource::Detected);
        let develop = match plan.resolved_develop() {
            Some((frame, develop)) => Some(develop_record(frame, develop)?),
            None => None,
        };
        Ok(commit(move |session: &mut Session| {
            let name = defects_map().ok_or(CommandError::Internal { command: "defects.detect".into() })?;
            let mut edits = vec![Edit::SetProjectMap { name, map: Some(reference) }];
            if let Some(develop) = develop {
                edits.push(Edit::SetDevelop(Some(develop)));
            }
            session.apply(Edit::Batch(edits))?;
            Ok(Mask { summary })
        }))
    });
    Ok(job.stages(&[("Finding dust and hot pixels", 1.0)]).locks(Aspects::FRAMES.union(Aspects::DEVELOP).union(Aspects::ALIGN)))
}

/// An image file.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ImportParams {
    /// The image to read: black spots on white, the frames' size.
    path: String,
}

impl Params for ImportParams {
    fn check(&self) -> Result<(), Invalid> {
        check_path("path", &self.path)
    }
}

fn import(session: &Session, p: ImportParams) -> Result<JobPlan<Mask>, CommandError> {
    let project = session.project()?;
    let path: PathBuf = absolute(&p.path)?;
    // The frames' size, when an alignment run recorded it.
    let size = stored_alignment(project).ok().flatten().map(|a| (a.report.width, a.report.height));
    let cache = session.cache_dir()?;
    let job = JobPlan::new(move |ctx| {
        ctx.stage(0);
        let mask = read_mask_file(&path)?;
        if let Some((width, height)) = size
            && (mask.width(), mask.height()) != (width, height)
        {
            return Err(CommandError::Unavailable {
                command: "defects.import".into(),
                reason: format!(
                    "{} is {}x{} pixels, but the frames are {width}x{height}: a defect mask must have the frames' size",
                    path.display(),
                    mask.width(),
                    mask.height()
                ),
            });
        }
        ctx.checkpoint()?;
        let reference = store_defect_mask(&cache, &mask, MaskSource::Imported)?;
        let summary = MaskSummary::of(&mask, MaskSource::Imported);
        Ok(commit(move |session: &mut Session| {
            let name = defects_map().ok_or(CommandError::Internal { command: "defects.import".into() })?;
            session.apply(Edit::SetProjectMap { name, map: Some(reference) })?;
            Ok(Mask { summary })
        }))
    });
    Ok(job.stages(&[("Reading the mask", 1.0)]).locks(Aspects::ALIGN))
}

/// Where to write the mask.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExportParams {
    /// The PNG file to write.
    path: String,
    /// Replace the file if it exists.
    #[serde(default)]
    overwrite: bool,
}

impl Params for ExportParams {
    fn check(&self) -> Result<(), Invalid> {
        check_path("path", &self.path)
    }
}

/// The file written.
#[derive(Debug, Serialize, JsonSchema)]
struct Written {
    path: String,
    /// Its size.
    bytes: u64,
}

fn export(session: &Session, p: ExportParams) -> Result<JobPlan<Written>, CommandError> {
    let project = session.project()?;
    let reference = defect_mask_ref(project).cloned().ok_or_else(|| {
        session.unavailable("the project has no defect mask: detect one with `defects.detect`, or import one with `defects.import`")
    })?;
    let path = absolute(&p.path)?;
    if !p.overwrite && path.exists() {
        return Err(session.unavailable(format!("{} exists: pass `overwrite` to replace it", path.display())));
    }
    let cache = session.cache_dir()?;
    let job = JobPlan::new(move |ctx| {
        ctx.stage(0);
        let mask = read_defect_mask(&cache, &reference)?;
        let spec = OutputSpec { format: OutputFormat::Png, depth: BitDepth::U8, profile: OutputProfile::Input, extended_range: false };
        let encoded = florastack_codecs::write(&AnyImage::U8(mask.to_image()?), &spec, &OutputMetadata::default())?;
        // From here the file is written: the job can no longer be cancelled.
        ctx.finalize()?;
        write_atomically(&path, &encoded.bytes)?;
        let written = Written { path: display(&path), bytes: encoded.bytes.len() as u64 };
        Ok(commit(move |_: &mut Session| Ok(written)))
    });
    Ok(job.stages(&[("Writing the mask", 1.0)]))
}

/// Whether a mask was removed.
#[derive(Debug, Serialize, JsonSchema)]
struct Cleared {
    /// False when the project had none: nothing changed.
    cleared: bool,
}

fn clear(session: &mut Session, _: NoParams) -> Result<Cleared, CommandError> {
    if defect_mask_ref(session.project()?).is_none() {
        return Ok(Cleared { cleared: false });
    }
    let name = defects_map().ok_or(CommandError::Internal { command: "defects.clear".into() })?;
    session.apply(Edit::SetProjectMap { name, map: None })?;
    Ok(Cleared { cleared: true })
}
