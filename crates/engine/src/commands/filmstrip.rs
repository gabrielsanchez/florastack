//! The filmstrip: small previews of every frame, as shot and aligned onto the output's canvas, for the user to scrub
//! through the stack and spot what moved (`filmstrip.list`), with each frame's flags and the motion alignment found in
//! it. Previews are cache files that the project refers to, each keyed by what it was made from
//! (`crate::project::preview_key`, `aligned_preview_key`): `align.run` writes them as it reads the frames, and
//! `filmstrip.update` makes the ones that are missing or out of date, after frames are added, the develop settings or
//! the defect mask change, or the framing does. Making previews is bookkeeping, not an edit: it has no undo step.

use std::path::PathBuf;

use florastack_align::{Canvas, DefectMask, Flag, Framing, inpaint};
use florastack_codecs::ReadOptions;
use florastack_core::{Affine, Monitor};
use florastack_project::{CacheRef, Edit, Frame, FrameId, Project};
use florastack_raw::{DevelopSettings, Quality};
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::CommandError;
use crate::frames::FrameReader;
use crate::jobs::{JobPlan, Lane, commit};
use crate::project::{
    FrameMotion, PREVIEW_SIDE, ProjectDataError, align_settings, aligned_preview_key, defect_mask_ref, develop_settings, frame_report, preview_key,
    read_defect_mask, stored_alignment, write_aligned_preview, write_frame_preview,
};
use crate::registry::{CommandSpec, Meta, NoParams, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("filmstrip.list", "Filmstrip", "Every frame's previews, as shot and aligned, with its flags and the motion found in it.")
                .help(
                    "For each frame, in the project's order: its preview as shot (developed, its defects inpainted) \
                     and aligned onto the output's canvas, small JPEGs of at most 512 pixels a side, as paths to \
                     read; its flags (such as `moved` and `out-of-focus`); and what alignment found moved in it \
                     against its neighbor: a score, the largest displacement and the regions, in frame pixels. A \
                     preview that is missing or out of date is left out and counted in `stale`: \
                     `filmstrip.update` makes them.",
                )
                .requires(Requires::Project),
            list,
        ),
        CommandSpec::job(
            Meta::new("filmstrip.update", "Update filmstrip", "Makes the frames' previews that are missing or out of date.")
                .help(
                    "Reads and develops the frames whose preview is missing or out of date (their file, the develop \
                     settings or the defect mask changed), and aligns every aligned frame's preview onto the canvas \
                     of the framing set, which needs no frame read. A frame whose file can't be read is listed in \
                     `unreadable` and keeps its previews. `align.run` makes them too, as it reads the frames. Runs \
                     beside other jobs as a preview; a newer call replaces an older one still running. Previews are \
                     cache files, not edits: no undo step.",
                )
                .requires(Requires::Project)
                .supersede(),
            Lane::Preview,
            update,
        ),
    ]
}

/// What a frame's previews should be made from now.
struct Wanted {
    /// The key a preview made now would have.
    preview: String,
    /// The frame's size, transform and the canvas, when it is aligned and the stack can be framed as set.
    aligned: Option<((usize, usize), Affine, Canvas)>,
}

/// What every frame's previews should be made from now, in the project's order.
fn wanted(project: &Project) -> Result<Vec<(FrameId, Wanted)>, CommandError> {
    let develop = develop_settings(project)?;
    let mask = defect_mask_ref(project);
    let framing = align_settings(project).map_or(Framing::Narrow, |s| s.framing);
    let stored = stored_alignment(project).ok().flatten();
    let canvas = stored.as_ref().and_then(|s| s.canvas(framing).ok());
    project
        .frames
        .iter()
        .map(|f| {
            let aligned = match (&stored, &canvas) {
                (Some(stored), Some(canvas)) => stored
                    .frames
                    .iter()
                    .find(|s| s.frame == f.id())
                    .map(|s| ((stored.report.width, stored.report.height), s.alignment.transform, *canvas)),
                _ => None,
            };
            Ok((f.id(), Wanted { preview: preview_key(f, &develop, mask)?, aligned }))
        })
        .collect()
}

/// Whether `reference` was made from what `key` says.
fn current(reference: Option<&CacheRef>, key: &str) -> bool {
    reference.is_some_and(|r| r.key.as_deref() == Some(key))
}

/// Whether frame `f`'s aligned preview is current, given what it should be made from.
fn aligned_current(f: &Frame, wanted: &Wanted) -> bool {
    match (&wanted.aligned, &f.preview) {
        (Some((_, transform, canvas)), Some(preview)) => current(f.aligned_preview.as_ref(), &aligned_preview_key(preview, transform, canvas)),
        _ => false,
    }
}

/// The filmstrip.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Filmstrip {
    /// The previews' longest side, in pixels.
    side: usize,
    /// The framing the aligned previews show.
    framing: Framing,
    /// How many previews are missing or out of date: `filmstrip.update` makes them.
    stale: usize,
    frames: Vec<FilmstripFrame>,
}

/// A frame of the filmstrip.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct FilmstripFrame {
    frame: FrameId,
    enabled: bool,
    /// The preview as shot, a JPEG file; none when it is missing or out of date.
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<String>,
    /// The preview aligned onto the output's canvas; none when the frame isn't aligned, or the preview is missing or
    /// out of date.
    #[serde(skip_serializing_if = "Option::is_none")]
    aligned_preview: Option<String>,
    /// What alignment flagged about it, such as `moved`.
    flags: Vec<Flag>,
    /// What moved in it against its neighbor, once aligned, when it was aligned.
    #[serde(skip_serializing_if = "Option::is_none")]
    motion: Option<FrameMotion>,
}

fn list(session: &Session, _: NoParams) -> Result<Filmstrip, CommandError> {
    let (project, folder) = (session.project()?, session.folder()?);
    let framing = align_settings(project).map_or(Framing::Narrow, |s| s.framing);
    let path = |r: &CacheRef| r.path.resolve(folder.path()).display().to_string();
    let mut stale = 0;
    let mut frames = Vec::with_capacity(project.frames.len());
    for (f, (_, wanted)) in project.frames.iter().zip(wanted(project)?) {
        let preview = f.preview.as_ref().filter(|r| current(Some(r), &wanted.preview));
        let aligned = f.aligned_preview.as_ref().filter(|_| preview.is_some() && aligned_current(f, &wanted));
        stale += usize::from(preview.is_none()) + usize::from(wanted.aligned.is_some() && aligned.is_none());
        let report = frame_report(f);
        frames.push(FilmstripFrame {
            frame: f.id(),
            enabled: f.enabled,
            preview: preview.map(path),
            aligned_preview: aligned.map(path),
            flags: report.as_ref().map(|r| r.flags.clone()).unwrap_or_default(),
            motion: report.and_then(|r| r.motion),
        });
    }
    Ok(Filmstrip { side: PREVIEW_SIDE, framing, stale, frames })
}

/// What `filmstrip.update` made.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Updated {
    /// How many previews it made, as shot and aligned.
    previews: usize,
    aligned_previews: usize,
    /// The frames whose file couldn't be read (moved, deleted or damaged), with why: their previews stay as they were.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unreadable: Vec<Unreadable>,
}

/// A frame whose file couldn't be read.
#[derive(Debug, Serialize, JsonSchema)]
struct Unreadable {
    frame: FrameId,
    reason: String,
}

/// A frame whose previews `filmstrip.update` makes.
struct Job {
    id: FrameId,
    path: PathBuf,
    /// Its preview's key, when it needs a new one; else its current preview.
    preview: Result<CacheRef, String>,
    aligned: Option<((usize, usize), Affine, Canvas)>,
}

fn update(session: &Session, _: NoParams) -> Result<JobPlan<Updated>, CommandError> {
    let (project, folder) = (session.project()?, session.folder()?);
    let mut jobs = Vec::new();
    for (f, (id, wanted)) in project.frames.iter().zip(wanted(project)?) {
        let preview = match f.preview.as_ref().filter(|r| current(Some(r), &wanted.preview)) {
            Some(r) => Ok(r.clone()),
            None => Err(wanted.preview.clone()),
        };
        let aligned = wanted.aligned.filter(|_| preview.is_err() || !aligned_current(f, &wanted));
        if preview.is_err() || aligned.is_some() {
            jobs.push(Job { id, path: folder.frame_path(f), preview, aligned });
        }
    }
    let settings: DevelopSettings = develop_settings(project)?;
    let options = ReadOptions::default();
    let mut reader = match crate::project::develop(project)? {
        Some(develop) => FrameReader::with_develop(develop, options),
        None => FrameReader::new(settings, options),
    };
    let mask_ref = defect_mask_ref(project).cloned();
    let cache = session.cache_dir()?;
    let plan = JobPlan::new(move |ctx| {
        ctx.stage(0);
        let mut mask: Option<DefectMask> = None;
        if let Some(reference) = &mask_ref
            && jobs.iter().any(|j| j.preview.is_err())
        {
            mask = Some(read_defect_mask(&cache, reference)?);
        }
        let total = jobs.len() as u64;
        let mut edits = Vec::new();
        let (mut previews, mut aligned_previews, mut unreadable) = (0, 0, Vec::new());
        for (done, job) in jobs.into_iter().enumerate() {
            ctx.checkpoint()?;
            let preview = match job.preview {
                Ok(current) => current,
                Err(key) => {
                    let read = std::fs::read(&job.path)
                        .map_err(|source| ProjectDataError::from(florastack_project::ProjectError::Read { path: job.path.clone(), source }))
                        .and_then(|bytes| {
                            reader.read(&bytes, Quality::Preview).map_err(|source| ProjectDataError::ReadFrame { path: job.path.clone(), source })
                        });
                    let mut image = match read {
                        Ok(frame) => frame.image,
                        Err(e) => {
                            unreadable.push(Unreadable { frame: job.id, reason: super::reason(&e) });
                            ctx.step(done as u64 + 1, total)?;
                            continue;
                        }
                    };
                    if let Some(mask) = &mask {
                        inpaint(&mut image, mask)?;
                    }
                    previews += 1;
                    write_frame_preview(&cache, job.id, &image, key)?
                }
            };
            let aligned = match &job.aligned {
                Some((size, transform, canvas)) => {
                    aligned_previews += 1;
                    Some(write_aligned_preview(&cache, job.id, &preview, *size, transform, canvas)?)
                }
                None => None,
            };
            edits.push((job.id, preview, aligned));
            ctx.step(done as u64 + 1, total)?;
        }
        Ok(commit(move |session: &mut Session| {
            let project = session.project()?;
            // Frames removed meanwhile are left out; an aligned preview not made now stays as it was.
            let edits: Vec<Edit> = edits
                .into_iter()
                .filter_map(|(frame, preview, aligned)| {
                    let f = project.frame(frame)?;
                    let aligned = aligned.or_else(|| f.aligned_preview.clone());
                    Some(Edit::SetPreviews { frame, preview: Some(preview), aligned_preview: aligned })
                })
                .collect();
            if !edits.is_empty() {
                session.apply_system(Edit::Batch(edits))?;
            }
            Ok(Updated { previews, aligned_previews, unreadable })
        }))
    });
    Ok(plan.stages(&[("Making previews", 1.0)]))
}
