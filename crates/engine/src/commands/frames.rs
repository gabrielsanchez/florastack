//! The frame list: adding frames (a job: every file is read and hashed), removing, reordering and enabling them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use florastack_codecs::ExifSummary;
use florastack_core::Monitor;
use florastack_project::{Aspects, ContentHash, Edit, FrameId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::project::NotDone;
use super::{MAX_ITEMS, absolute, check_len, check_path, check_unique, display, reason};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
use crate::project::{FrameRead, read_frame_file};
use crate::registry::{CommandSpec, Meta, NoParams, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::job(
            Meta::new("project.addFrames", "Add frames", "Adds image and camera RAW files to the end of the frame list.")
                .help(
                    "`paths` lists files and folders; a folder adds the frame files directly in it (by extension, \
                     sorted by name). Every file is read once, to check that FloraStack reads it, to hash it and to \
                     keep its metadata summary. Files that can't be read, or that are in the project already (the \
                     same path or the same contents), are reported in `skipped`. Runs as a job; the frames are added \
                     in one undo step when it ends, or not at all when it is cancelled.",
                )
                .undoable()
                .examples(&[r#"{ "paths": ["C:/Stacks/bee/IMG_0001.CR3", "C:/Stacks/bee/IMG_0002.CR3"] }"#, r#"{ "paths": ["C:/Stacks/bee"] }"#]),
            Lane::Heavy,
            add_frames,
        ),
        CommandSpec::instant(
            Meta::new("project.removeFrames", "Remove frames", "Removes frames from the frame list.")
                .help("The files stay where they are. Outputs made from the frames keep their pixels and their record of what they were made from.")
                .undoable()
                .examples(&[r#"{ "frames": [3, 4] }"#]),
            remove_frames,
        ),
        CommandSpec::instant(
            Meta::new("project.moveFrames", "Move frames", "Moves frames to another place in the frame list.")
                .help(
                    "The frames, in the order given, go before the frame now at position `to` of the list without them \
                     (from 0; past the end means at the end).",
                )
                .undoable()
                .examples(&[r#"{ "frames": [7], "to": 0 }"#]),
            move_frames,
        ),
        CommandSpec::instant(
            Meta::new("project.setFramesEnabled", "Enable or disable frames", "Chooses whether frames go into stacks.")
                .undoable()
                .examples(&[r#"{ "frames": [1, 2], "enabled": false }"#]),
            set_frames_enabled,
        ),
        CommandSpec::query(
            Meta::new("project.frames", "List frames", "Lists the frames in order, with their files, hashes and metadata summaries.")
                .requires(Requires::Project),
            list,
        ),
    ]
}

/// Adds files to the end of the frame list.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AddFramesParams {
    /// Files and folders (at most 100000): a folder adds the frame files directly in it, sorted by name.
    #[schemars(length(min = 1, max = 100_000))]
    paths: Vec<String>,
}

impl Params for AddFramesParams {
    fn check(&self) -> Result<(), Invalid> {
        check_len("paths", &self.paths, 1)?;
        self.paths.iter().try_for_each(|p| check_path("paths", p))
    }
}

/// The frames added.
#[derive(Debug, Serialize, JsonSchema)]
struct Added {
    /// The new frames' ids, in order.
    added: Vec<FrameId>,
    /// The files not added, and why.
    skipped: Vec<NotDone>,
}

/// The files that `paths` name: files as they are, folders' frame files sorted by name.
fn expand(paths: &[PathBuf], skipped: &mut Vec<NotDone>) -> Result<Vec<PathBuf>, CommandError> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut found: Vec<PathBuf> =
                        entries.flatten().map(|e| e.path()).filter(|p| p.is_file() && crate::frames::is_frame_name(p)).collect();
                    found.sort();
                    files.extend(found);
                }
                Err(e) => skipped.push(NotDone { path: display(path), reason: format!("the folder can't be listed: {e}") }),
            }
        } else {
            files.push(path.clone());
        }
        if files.len() > MAX_ITEMS {
            return Err(CommandError::Invalid { command: "project.addFrames".into(), source: Invalid::new("paths", "name more than 100000 files") });
        }
    }
    Ok(files)
}

fn add_frames(session: &Session, p: AddFramesParams) -> Result<JobPlan<Added>, CommandError> {
    session.project()?;
    let paths = p.paths.iter().map(|path| absolute(path)).collect::<Result<Vec<_>, _>>()?;
    let plan = JobPlan::new(move |ctx| {
        ctx.stage(0);
        let mut skipped = Vec::new();
        let files = expand(&paths, &mut skipped)?;
        ctx.stage(1);
        let total = files.len() as u64;
        let mut read: Vec<FrameRead> = Vec::with_capacity(files.len());
        for (n, file) in files.iter().enumerate() {
            ctx.checkpoint()?;
            match read_frame_file(file) {
                Ok(frame) => read.push(frame),
                Err(e) => skipped.push(NotDone { path: display(file), reason: reason(&e) }),
            }
            ctx.progress(n as u64 + 1, total);
        }
        Ok(commit(move |session: &mut Session| {
            // What the project holds now, which may have changed since the job started (but not its frames: they
            // are locked).
            let project = session.project()?;
            let paths: BTreeMap<&str, FrameId> = project.frames.iter().map(|f| (f.path.as_str(), f.id())).collect();
            let hashes: BTreeMap<ContentHash, FrameId> = project.frames.iter().map(|f| (f.hash, f.id())).collect();
            let mut edits = Vec::new();
            let (mut seen_paths, mut seen_hashes) = (BTreeSet::new(), BTreeSet::new());
            for frame in read {
                let path = frame.file.path.clone();
                if let Some(id) = paths.get(path.as_str()).or_else(|| hashes.get(&frame.file.hash)) {
                    skipped.push(NotDone { path, reason: format!("it is in the project already, as {id}") });
                    continue;
                }
                if !seen_paths.insert(path.clone()) || !seen_hashes.insert(frame.file.hash) {
                    skipped.push(NotDone { path, reason: "it is listed twice (the same file, or the same contents)".into() });
                    continue;
                }
                edits.push(Edit::AddFrame { file: frame.file, exif: frame.exif });
            }
            let count = edits.len();
            session.apply(Edit::Batch(edits))?;
            let frames = &session.project()?.frames;
            let added = frames.get(frames.len().saturating_sub(count)..).unwrap_or_default().iter().map(florastack_project::Frame::id).collect();
            Ok(Added { added, skipped })
        }))
    });
    Ok(plan.stages(&[("Finding files", 0.02), ("Reading frames", 0.98)]).locks(Aspects::FRAMES))
}

/// Frames, by id.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FramesParams {
    /// The frames' ids, as `project.frames` lists them.
    #[schemars(length(min = 1, max = 100_000))]
    frames: Vec<FrameId>,
}

impl Params for FramesParams {
    fn check(&self) -> Result<(), Invalid> {
        check_len("frames", &self.frames, 1)?;
        check_unique("frames", &self.frames)
    }
}

fn check_frames(session: &Session, frames: &[FrameId]) -> Result<(), CommandError> {
    let project = session.project()?;
    match frames.iter().find(|id| project.frame(**id).is_none()) {
        Some(id) => Err(CommandError::NotFound { what: id.to_string() }),
        None => Ok(()),
    }
}

/// How many frames changed.
#[derive(Debug, Serialize, JsonSchema)]
struct Count {
    changed: usize,
}

fn remove_frames(session: &mut Session, p: FramesParams) -> Result<Count, CommandError> {
    check_frames(session, &p.frames)?;
    let count = p.frames.len();
    session.apply(Edit::Batch(p.frames.into_iter().map(Edit::RemoveFrame).collect()))?;
    Ok(Count { changed: count })
}

/// Moves frames.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MoveParams {
    /// The frames to move, in the order they go in.
    #[schemars(length(min = 1, max = 100_000))]
    frames: Vec<FrameId>,
    /// Where they go: before the frame at this position of the list without them, from 0.
    to: usize,
}

impl Params for MoveParams {
    fn check(&self) -> Result<(), Invalid> {
        check_len("frames", &self.frames, 1)?;
        check_unique("frames", &self.frames)
    }
}

/// The frame order now.
#[derive(Debug, Serialize, JsonSchema)]
struct Order {
    order: Vec<FrameId>,
}

fn move_frames(session: &mut Session, p: MoveParams) -> Result<Order, CommandError> {
    check_frames(session, &p.frames)?;
    let old: Vec<FrameId> = session.project()?.frames.iter().map(florastack_project::Frame::id).collect();
    let moving: BTreeSet<FrameId> = p.frames.iter().copied().collect();
    let mut order: Vec<FrameId> = old.iter().copied().filter(|id| !moving.contains(id)).collect();
    let at = p.to.min(order.len());
    order.splice(at..at, p.frames.iter().copied());
    if order != old {
        session.apply(Edit::SetFrameOrder(order.clone()))?;
    }
    Ok(Order { order })
}

/// Enables or disables frames.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EnableParams {
    #[schemars(length(min = 1, max = 100_000))]
    /// The frames.
    frames: Vec<FrameId>,
    /// Whether they go into stacks.
    enabled: bool,
}

impl Params for EnableParams {
    fn check(&self) -> Result<(), Invalid> {
        check_len("frames", &self.frames, 1)?;
        check_unique("frames", &self.frames)
    }
}

fn set_frames_enabled(session: &mut Session, p: EnableParams) -> Result<Count, CommandError> {
    check_frames(session, &p.frames)?;
    let project = session.project()?;
    let edits: Vec<Edit> = p
        .frames
        .iter()
        .filter(|id| project.frame(**id).is_some_and(|f| f.enabled != p.enabled))
        .map(|&frame| Edit::SetFrameEnabled { frame, enabled: p.enabled })
        .collect();
    let changed = edits.len();
    session.apply(Edit::Batch(edits))?;
    Ok(Count { changed })
}

/// A frame, listed.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct FrameInfo {
    id: FrameId,
    /// The file, as stored: absolute, or relative to the folder that holds the project folder.
    path: String,
    /// Whether it goes into stacks.
    enabled: bool,
    /// The file's size when it was added, in bytes.
    size: u64,
    /// The SHA-256 of its contents when it was added (`sha256:` and 64 hexadecimal digits).
    hash: String,
    /// Its metadata summary, when it has one.
    exif: Option<ExifSummary>,
    /// Whether it has been aligned.
    aligned: bool,
    /// What alignment flagged about it, such as `out-of-focus` (`align.info` has the details).
    flags: Vec<florastack_align::Flag>,
    /// The share of the stack's in-focus detail on which it is among the sharpest, when alignment could tell.
    #[serde(skip_serializing_if = "Option::is_none")]
    sharp_fraction: Option<f64>,
    /// The share of it that moved against its neighbor, once aligned, from 0 to 1 (`filmstrip.list` shows where).
    #[serde(skip_serializing_if = "Option::is_none")]
    motion_score: Option<f64>,
    /// Whether it has a preview.
    preview: bool,
}

/// The frame list.
#[derive(Debug, Serialize, JsonSchema)]
struct FrameList {
    frames: Vec<FrameInfo>,
}

fn list(session: &Session, _: NoParams) -> Result<FrameList, CommandError> {
    let frames = session
        .project()?
        .frames
        .iter()
        .map(|f| FrameInfo {
            id: f.id(),
            path: f.path.clone(),
            enabled: f.enabled,
            size: f.size,
            hash: f.hash.to_string(),
            // A summary the project can't read back isn't shown, rather than failing the whole list.
            exif: crate::project::exif(f).ok().flatten(),
            aligned: f.alignment.is_some(),
            flags: crate::project::frame_report(f).map(|r| r.flags).unwrap_or_default(),
            sharp_fraction: crate::project::frame_report(f).and_then(|r| r.sharp_fraction),
            motion_score: crate::project::frame_report(f).and_then(|r| r.motion).map(|m| m.score),
            preview: f.preview.is_some(),
        })
        .collect();
    Ok(FrameList { frames })
}
