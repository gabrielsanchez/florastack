//! Alignment: the alignment settings, a preference group of the project (`align.get`, `align.set`, `align.reset`),
//! aligning the stack (`align.run`, a job), sorting it by scale (`align.sortByScale`, a job), and what the project holds
//! of its alignment (`align.info`).
//!
//! Both jobs read and develop the enabled frames one at a time, inpaint the stack's defect mask in each (detecting one
//! first when the `detect_defects` setting asks for it and the project has none: `defects.rs`), write each frame's
//! preview for the filmstrip, and hand each to florastack-align's `StackAligner`, which keeps nothing of a frame but
//! its small focus and motion maps: memory doesn't grow with the stack. Once every frame is in, the narrow end sets the
//! processing order and the master (`florastack_align::order_stack`), and the commit records the run in the project as
//! one undo step (`Edit::SetAlignment`): every frame's transform, gain and score, with its report (status, flags such
//! as out of focus or moved, sharp-content fraction, the motion found against its neighbor), the run's report (frame
//! size, narrow end, processing order), the frames' previews as shot and aligned onto the canvas of the framing set,
//! and a mask it detected. Frames that aren't enabled lose any alignment they had, so that the frames with an
//! alignment are the run's. Sorting by scale aligns twice, first to measure every frame's scale in the present order,
//! then in scale order, and its commit puts the frames in that order (`Edit::SetFrameOrder`) in the same step.
//!
//! The framing isn't recorded with the run: it follows from the transforms whenever it is asked for (`align.info`,
//! and the stacking of roadmap 3), so that changing it needs no new alignment, and nor does the coverage of the
//! output by each frame (`florastack_align::Coverage`).

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::PathBuf;

use florastack_align::{
    AlignError, AlignSettings, DefectDetector, DefectMask, End, Flag, Framing, MIN_DETECTION_FRAMES, Similarity, StackAligner, StackAlignment,
    Status, detection_frames, inpaint, measuring_settings, order_stack, scale_order,
};
use florastack_codecs::ReadOptions;
use florastack_core::{Image, Monitor};
use florastack_project::{Aspects, CacheDir, CacheRef, Edit, Frame, FrameId, Group, Preferences, Table};
use florastack_raw::{DevelopSettings, Quality, RawDevelop};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::CommandError;
use crate::frames::FrameReader;
use crate::jobs::{Commit, JobContext, JobPlan, Lane, commit};
use crate::project::{
    FrameMotion, MaskSource, MaskSummary, ProjectDataError, align_settings, alignment_edit, defect_mask_ref, defects_map, develop_record,
    preview_key, read_defect_mask, store_defect_mask, stored_alignment, write_aligned_preview, write_frame_preview,
};
use crate::registry::{CommandSpec, Meta, NoParams, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("align.get", "Alignment settings", "The stack's alignment, ordering, framing and focus settings.")
                .help(
                    "Every setting, with the keys the preferences don't set at their defaults: which terms are corrected \
                     and their limits, the processing order (`automatic_order`, `reverse_order`), the output's \
                     framing (`narrow`, `wide` or `common`) and the out-of-focus threshold (`min_sharp_fraction`).",
                )
                .requires(Requires::Project),
            get,
        ),
        CommandSpec::instant(
            Meta::new("align.set", "Set alignment settings", "Changes some of the stack's alignment, ordering, framing and focus settings.")
                .help(
                    "Only the keys given change, as when a preset is applied: `{\"framing\": \"wide\"}` keeps the \
                     widest field of view and leaves the rest as it is. The result is checked whole, and a misspelled \
                     key is an error. The framing applies at once, since it follows from the stored alignment \
                     (`align.info`); the other settings apply when the stack is next aligned.",
                )
                .undoable()
                .examples(&[
                    r#"{ "framing": "wide" }"#,
                    r#"{ "rotation": false, "brightness": false }"#,
                    r#"{ "automatic_order": false, "reverse_order": true, "min_sharp_fraction": 0.005 }"#,
                ]),
            set,
        ),
        CommandSpec::instant(
            Meta::new("align.reset", "Reset alignment settings", "Puts every alignment setting back at its default.").undoable(),
            reset,
        ),
        CommandSpec::job(
            Meta::new("align.run", "Align frames", "Aligns the enabled frames and orders the stack from its narrow end.")
                .help(
                    "Reads and develops every enabled frame in turn and aligns each to its neighbor, correcting the \
                     terms the settings say. Then the end of the stack with the narrower field of view becomes its \
                     start, and its frame the master that the others are aligned to (unless `automatic_order` is off, \
                     or `reverse_order` on), and every frame's sharp-content fraction is measured: the frames with no \
                     in-focus detail are flagged `out-of-focus`, for you to disable with `project.setFramesEnabled`. \
                     Frames that aren't enabled lose any alignment they had. The result lists the master, the order and \
                     the flagged and rejected frames; `align.info` has every frame's details. Runs as a job; the \
                     alignment is recorded in one undo step when it ends, or not at all when it is cancelled.",
                )
                .undoable(),
            Lane::Heavy,
            run,
        ),
        CommandSpec::job(
            Meta::new(
                "align.sortByScale",
                "Sort frames by scale",
                "Puts the enabled frames in the order of their scale, then aligns them in that order.",
            )
            .help(
                "Repairs a stack shot or imported out of order. Focus breathing, or a rail, changes the \
                     magnification steadily with the focus, so the frames' scales give their focus order. A first \
                     pass aligns the frames as they are, to measure their scales; the frames are then sorted by scale, \
                     in the direction that keeps the stack's present trend, and aligned again in that order, as \
                     `align.run` does. Disabled frames keep their places in the list. Fails, changing nothing, when \
                     the scales differ too little to sort by. Runs as a job, which reads every frame twice; the new \
                     order and the alignment are one undo step.",
            )
            .undoable(),
            Lane::Heavy,
            sort_by_scale,
        ),
        CommandSpec::query(
            Meta::new(
                "align.info",
                "Alignment",
                "The stack's alignment as the project holds it: each frame's, the processing order and the output's framings.",
            )
            .help(
                "For each aligned frame: its transform from the master's pixels and its terms (scale, rotation, \
                     shift), its brightness gain and score, its status and flags, its sharp-content fraction and what \
                     moved in it against its neighbor. \
                     For the run: the master, the frames' size, the narrow end, the processing order and the settings \
                     it ran with. For each framing (narrow, wide, common): the size of the output it gives and the \
                     transform from its pixels to the master's, or why the frames can't be framed that way; `framing` \
                     is the one the settings choose.",
            )
            .requires(Requires::Project),
            info,
        ),
    ]
}

/// The alignment settings.
#[derive(Debug, Serialize, JsonSchema)]
struct Settings {
    /// Every setting, with the defaults filled in.
    settings: AlignSettings,
}

fn get(session: &Session, _: NoParams) -> Result<Settings, CommandError> {
    Ok(Settings { settings: align_settings(session.project()?)? })
}

/// Some alignment settings: any of [`AlignSettings`]' keys, checked as such.
#[derive(Debug)]
pub(crate) struct AlignPatch(Table);

impl<'de> Deserialize<'de> for AlignPatch {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<AlignPatch, D::Error> {
        let table = Table::deserialize(deserializer)?;
        // Every key, and the type of every value, is checked by the settings type itself.
        AlignSettings::deserialize(serde_json::Value::Object(table.clone())).map_err(serde::de::Error::custom)?;
        Ok(AlignPatch(table))
    }
}

impl JsonSchema for AlignPatch {
    fn schema_name() -> Cow<'static, str> {
        AlignSettings::schema_name()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        AlignSettings::json_schema(generator)
    }
}

impl Params for AlignPatch {}

/// Sets the alignment preferences to `table`, after checking the settings it makes; no step when nothing changes.
fn set_table(session: &mut Session, table: Table) -> Result<Settings, CommandError> {
    let mut preferences = Preferences::new();
    preferences.set(Group::Align, &table)?;
    let settings: AlignSettings = preferences.get(Group::Align)?;
    settings.validate().map_err(|e| session.invalid("settings", e.to_string()))?;
    if session.project()?.preferences.group(Group::Align) != preferences.group(Group::Align) {
        session.apply(Edit::SetPreferences { group: Group::Align, table: preferences.group(Group::Align).clone() })?;
    }
    Ok(Settings { settings })
}

fn set(session: &mut Session, patch: AlignPatch) -> Result<Settings, CommandError> {
    let mut top = Preferences::new();
    top.set(Group::Align, &patch.0)?;
    let mut merged = session.project()?.preferences.only(&[Group::Align]);
    merged.layer(&top);
    set_table(session, merged.group(Group::Align).clone())
}

fn reset(session: &mut Session, _: NoParams) -> Result<Settings, CommandError> {
    set_table(session, Table::new())
}

/// What an alignment job reads, taken from the project when it starts.
pub(crate) struct Plan {
    command: &'static str,
    /// The enabled frames, in the project's order, and their files.
    ids: Vec<FrameId>,
    paths: Vec<PathBuf>,
    /// The same frames as the project holds them, for their previews' keys.
    frames: Vec<Frame>,
    settings: AlignSettings,
    develop_settings: DevelopSettings,
    reader: FrameReader,
    /// Whether the project held the stack's development already, resolved with the develop settings in force.
    resolved: bool,
    /// The frame the development was resolved on, if this job resolved it.
    developed: Option<FrameId>,
    cache: CacheDir,
    /// The project's defect mask, and the mask itself once read, or detected by this job.
    mask_ref: Option<CacheRef>,
    mask: Option<DefectMask>,
    /// The mask this job detected and stored, with its summary.
    detected: Option<(CacheRef, MaskSummary)>,
    /// The previews this job wrote, by frame.
    previews: Vec<(FrameId, CacheRef)>,
    /// How RAW frames are demosaiced: the fast way for alignment and previews, the settings' way for a full stack.
    quality: Quality,
}

impl Plan {
    pub(crate) fn new(session: &Session, command: &'static str, minimum: usize) -> Result<Plan, CommandError> {
        let project = session.project()?;
        let frames: Vec<&Frame> = project.frames.iter().filter(|f| f.enabled).collect();
        if frames.len() < minimum {
            let reason =
                if minimum > 1 { format!("it needs at least {minimum} enabled frames") } else { "the project has no enabled frames".to_owned() };
            return Err(session.unavailable(format!("{reason}: add frames with `project.addFrames`, or enable some with `project.setFramesEnabled`")));
        }
        Plan::of_frames(session, command, &frames)
    }

    /// A plan that reads `ids`, enabled or not, in the project's order (what retouching paints from).
    pub(crate) fn with_frames(session: &Session, command: &'static str, ids: &[FrameId]) -> Result<Plan, CommandError> {
        let project = session.project()?;
        let frames: Vec<&Frame> = project.frames.iter().filter(|f| ids.contains(&f.id())).collect();
        Plan::of_frames(session, command, &frames)
    }

    fn of_frames(session: &Session, command: &'static str, frames: &[&Frame]) -> Result<Plan, CommandError> {
        let (project, folder) = (session.project()?, session.folder()?);
        let settings = align_settings(project).map_err(|e| session.invalid("align", e.to_string()))?;
        let develop_settings = crate::project::develop_settings(project)?;
        let options = ReadOptions::default();
        let (reader, resolved) = match crate::project::develop(project)? {
            Some(develop) => (FrameReader::with_develop(develop, options), true),
            None => (FrameReader::new(develop_settings.clone(), options), false),
        };
        Ok(Plan {
            command,
            ids: frames.iter().map(|f| f.id()).collect(),
            paths: frames.iter().map(|f| folder.frame_path(f)).collect(),
            frames: frames.iter().map(|&f| f.clone()).collect(),
            settings,
            develop_settings,
            reader,
            resolved,
            developed: None,
            cache: session.cache_dir()?,
            mask_ref: defect_mask_ref(project).cloned(),
            mask: None,
            detected: None,
            previews: Vec::new(),
            quality: Quality::Preview,
        })
    }

    /// The plan reading RAW frames with `quality`'s demosaic.
    pub(crate) fn with_quality(mut self, quality: Quality) -> Plan {
        self.quality = quality;
        self
    }

    /// The enabled frames, in the project's order: the `i` of [`Plan::read`].
    pub(crate) fn ids(&self) -> &[FrameId] {
        &self.ids
    }

    /// What the `i`-th frame is read from, for the keys of what is made from it: its file's contents, the develop
    /// settings and the defect mask, as its preview's key says them ([`preview_key`]).
    pub(crate) fn frame_key(&self, i: usize) -> Result<String, CommandError> {
        let frame = self.frames.get(i).ok_or_else(|| self.internal())?;
        Ok(preview_key(frame, &self.develop_settings, self.mask_ref.as_ref())?)
    }

    /// What RAW frames are developed with, for the same keys: the stack's development when the project holds it,
    /// hashed, or "unresolved" when this job resolves it on the first RAW frame it reads.
    pub(crate) fn develop_key(&self) -> Result<String, CommandError> {
        match (self.resolved, self.reader.develop()) {
            (true, Some(develop)) => {
                let bytes = serde_json::to_vec(&(develop.settings(), develop.resolved()))
                    .map_err(|source| florastack_project::ProjectError::Data { what: "resolved development", source })?;
                Ok(format!("resolved {}", florastack_project::ContentHash::of(&bytes)))
            }
            _ => Ok("unresolved".to_owned()),
        }
    }

    /// Reads the project's defect mask, if it has one, for [`Plan::read`] to inpaint; never detects one.
    pub(crate) fn load_mask(&mut self) -> Result<(), CommandError> {
        if let Some(reference) = &self.mask_ref {
            self.mask = Some(read_defect_mask(&self.cache, reference)?);
        }
        Ok(())
    }

    /// How many frames the job reads.
    pub(crate) fn frame_count(&self) -> usize {
        self.ids.len()
    }

    /// The stack's development, when this job resolved it, and the frame it was resolved on.
    pub(crate) fn resolved_develop(&self) -> Option<(FrameId, &RawDevelop)> {
        match (self.resolved, self.developed, self.reader.develop()) {
            (false, Some(frame), Some(develop)) => Some((frame, develop)),
            _ => None,
        }
    }

    fn internal(&self) -> CommandError {
        CommandError::Internal { command: self.command.to_owned() }
    }

    /// Whether this job detects a defect mask before aligning: the settings ask for one and the project has none.
    fn detects(&self) -> bool {
        self.settings.detect_defects && self.mask_ref.is_none() && self.ids.len() >= MIN_DETECTION_FRAMES
    }

    /// Reads and develops the `i`-th frame, without inpainting it.
    pub(crate) fn read_unmasked(&mut self, i: usize) -> Result<Image<f32>, CommandError> {
        let (Some(&id), Some(path)) = (self.ids.get(i), self.paths.get(i)) else { return Err(self.internal()) };
        let bytes =
            std::fs::read(path).map_err(|source| ProjectDataError::from(florastack_project::ProjectError::Read { path: path.clone(), source }))?;
        let had = self.reader.develop().is_some();
        let frame = self.reader.read(&bytes, self.quality).map_err(|source| ProjectDataError::ReadFrame { path: path.clone(), source })?;
        if !had && self.reader.develop().is_some() {
            self.developed = Some(id);
        }
        Ok(frame.image)
    }

    /// Reads and develops the `i`-th frame, with the stack's defects inpainted.
    pub(crate) fn read(&mut self, i: usize) -> Result<Image<f32>, CommandError> {
        let mut image = self.read_unmasked(i)?;
        if let Some(mask) = &self.mask {
            inpaint(&mut image, mask).map_err(|e| self.frame_error(i, e))?;
        }
        Ok(image)
    }

    /// An error about the `i`-th frame's size, as the user can act on it.
    pub(crate) fn frame_error(&self, i: usize, e: AlignError) -> CommandError {
        let (id, path) = (self.ids.get(i).map_or_else(String::new, ToString::to_string), self.paths.get(i).map(|p| p.display().to_string()));
        let path = path.unwrap_or_default();
        match e {
            AlignError::FrameSize { width, height, expected_width, expected_height, .. } => CommandError::Unavailable {
                command: self.command.to_owned(),
                reason: format!(
                    "{id} ({path}) is {width}x{height} pixels, but the stack's frames are {expected_width}x{expected_height}: frames must all have the same size, so disable it with `project.setFramesEnabled`"
                ),
            },
            AlignError::MaskSize { width, height, frame_width, frame_height } => CommandError::Unavailable {
                command: self.command.to_owned(),
                reason: format!(
                    "the defect mask is {width}x{height} pixels, but {id} ({path}) is {frame_width}x{frame_height}: import a mask of the frames' size with `defects.import`, or remove it with `defects.clear`"
                ),
            },
            other => other.into(),
        }
    }

    /// Reads the project's defect mask, or detects one when [`Plan::detects`] and stores it.
    fn defects(&mut self, ctx: &JobContext) -> Result<(), CommandError> {
        if let Some(reference) = &self.mask_ref {
            self.mask = Some(read_defect_mask(&self.cache, reference)?);
            return Ok(());
        }
        if !self.detects() {
            return Ok(());
        }
        let picked = detection_frames(self.ids.len());
        let mut detector: Option<DefectDetector> = None;
        for (done, &i) in picked.iter().enumerate() {
            ctx.checkpoint()?;
            let image = self.read_unmasked(i)?;
            let d = match &mut detector {
                Some(d) => d,
                empty => empty.insert(DefectDetector::new(image.width(), image.height())?),
            };
            d.add(&image, ctx).map_err(|e| self.frame_error(i, e))?;
            ctx.step(done as u64 + 1, picked.len() as u64)?;
        }
        let mask = detector.ok_or_else(|| self.internal())?.finish()?;
        let reference = store_defect_mask(&self.cache, &mask, MaskSource::Detected)?;
        self.detected = Some((reference, MaskSummary::of(&mask, MaskSource::Detected)));
        self.mask = Some(mask);
        Ok(())
    }

    /// Aligns the frames in the order `positions` gives (`positions[k]` is the index in `ids` of the frame at position
    /// `k`), from their middle frame, reading each one when the aligner asks for it, inpainted; with `previews`, writes
    /// each frame's preview too.
    fn align(&mut self, ctx: &JobContext, positions: &[usize], settings: &AlignSettings, previews: bool) -> Result<StackAlignment, CommandError> {
        let n = positions.len();
        let mut aligner = StackAligner::new(settings, n, n / 2)?;
        aligner.set_defects(self.mask.clone());
        while let Some(k) = aligner.next_frame() {
            ctx.checkpoint()?;
            let i = positions.get(k).copied().ok_or_else(|| self.internal())?;
            let image = self.read(i)?;
            aligner.add(&image, ctx).map_err(|e| self.frame_error(i, e))?;
            if previews {
                let frame = self.frames.get(i).ok_or_else(|| self.internal())?;
                let key = preview_key(frame, &self.develop_settings, self.mask_ref.as_ref().or(self.detected.as_ref().map(|(r, _)| r)))?;
                let reference = write_frame_preview(&self.cache, frame.id(), &image, key)?;
                self.previews.push((frame.id(), reference));
            }
        }
        Ok(aligner.finish()?)
    }

    /// The commit of a run that aligned the frames `ids` (in that order) as `alignment`: records it, with the frames
    /// put in that order first when `sort`, the stack's development when this job resolved it, the defect mask when it
    /// detected one, and the frames' previews, aligned onto the canvas the settings frame the output with.
    fn commit(self, ids: Vec<FrameId>, alignment: &StackAlignment, sort: bool) -> Result<Commit<Aligned>, CommandError> {
        let (rebased, order) = order_stack(alignment, &self.settings)?;
        let id = |i: usize| ids.get(i).copied();
        let summary = Aligned {
            master: id(rebased.master).ok_or_else(|| self.internal())?,
            narrow_end: order.narrow,
            start: order.start,
            order: order.frames.iter().filter_map(|&i| id(i)).collect(),
            out_of_focus: flagged(&rebased, &ids, |f| f.flags.contains(&Flag::OutOfFocus)),
            moved: flagged(&rebased, &ids, |f| f.flags.contains(&Flag::Moved)),
            rejected: flagged(&rebased, &ids, |f| f.status == Status::Rejected),
            defects: self.detected.as_ref().map(|(_, s)| s.clone()),
            frame_order: None,
        };
        let develop = match self.resolved_develop() {
            Some((reference, develop)) => Some(develop_record(reference, develop)?),
            None => None,
        };
        // The aligned previews, on the canvas of the framing the settings choose; none when the frames can't be framed so.
        let narrow = order.narrow_frame.filter(|&i| rebased.frames.get(i).is_some_and(|f| f.status != Status::Rejected));
        let canvas = rebased.canvas(self.settings.framing, narrow).ok();
        let mut previews = Vec::with_capacity(self.previews.len());
        for (frame, preview) in self.previews {
            let aligned = match (&canvas, ids.iter().position(|&i| i == frame).and_then(|i| rebased.frames.get(i))) {
                (Some(canvas), Some(f)) => {
                    Some(write_aligned_preview(&self.cache, frame, &preview, (rebased.width, rebased.height), &f.transform, canvas)?)
                }
                _ => None,
            };
            previews.push(Edit::SetPreviews { frame, preview: Some(preview), aligned_preview: aligned });
        }
        let detected = self.detected.map(|(r, _)| r);
        let settings = self.settings;
        Ok(commit(move |session: &mut Session| {
            let project = session.project()?;
            let mut edits = Vec::new();
            let mut summary = summary;
            if sort {
                // The enabled frames take the places that enabled frames had, in their new order.
                let enabled: BTreeSet<FrameId> = ids.iter().copied().collect();
                let mut sorted = ids.iter().copied();
                let now: Vec<FrameId> = project.frames.iter().map(Frame::id).collect();
                let new: Vec<FrameId> = now.iter().map(|&f| if enabled.contains(&f) { sorted.next().unwrap_or(f) } else { f }).collect();
                if new != now {
                    edits.push(Edit::SetFrameOrder(new.clone()));
                }
                summary.frame_order = Some(new);
            }
            edits.push(alignment_edit(project, &ids, &rebased, &order, &settings)?);
            if let Some(develop) = develop {
                edits.push(Edit::SetDevelop(Some(develop)));
            }
            if let (Some(map), Some(name)) = (detected, defects_map()) {
                edits.push(Edit::SetProjectMap { name, map: Some(map) });
            }
            edits.extend(previews);
            session.apply(Edit::Batch(edits))?;
            Ok(summary)
        }))
    }
}

/// The ids of the frames of `alignment` (whose ids are `ids`) for which `f` holds.
fn flagged(alignment: &StackAlignment, ids: &[FrameId], f: impl Fn(&florastack_align::FrameAlignment) -> bool) -> Vec<FrameId> {
    alignment.frames.iter().zip(ids).filter(|(a, _)| f(a)).map(|(_, &id)| id).collect()
}

/// What aligning did.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Aligned {
    /// The frame the others are aligned to, which the output is framed on.
    master: FrameId,
    /// The end of the stack with the narrower field of view, when the frames' scales tell.
    narrow_end: Option<End>,
    /// The end the stack is processed from.
    start: End,
    /// The aligned frames, in the order the stack is processed.
    order: Vec<FrameId>,
    /// The frames with no in-focus detail: flagged for you to disable.
    out_of_focus: Vec<FrameId>,
    /// The frames in which part of the subject moved against their neighbor: `filmstrip.list` shows where.
    moved: Vec<FrameId>,
    /// The frames whose alignment couldn't be trusted, and keep their neighbor's (`align.info` says why).
    rejected: Vec<FrameId>,
    /// The defect mask this run detected and inpainted, when the settings asked for one (`detect_defects`) and the
    /// project had none.
    #[serde(skip_serializing_if = "Option::is_none")]
    defects: Option<MaskSummary>,
    /// After sorting by scale: the project's frame order now.
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_order: Option<Vec<FrameId>>,
}

/// What an alignment job reads, and its commit changes: the frame list, the development, the alignment (with the
/// defect mask) and the previews.
fn locks() -> Aspects {
    Aspects::FRAMES.union(Aspects::DEVELOP).union(Aspects::ALIGN)
}

/// The stages of an alignment job: finding the defects when it detects them, then the passes it names.
fn stages(plan: &Plan, passes: &'static [(&'static str, f32)]) -> Vec<(&'static str, f32)> {
    let mut stages = Vec::with_capacity(passes.len() + 1);
    if plan.detects() {
        stages.push(("Finding dust and hot pixels", 0.3));
    }
    stages.extend(passes.iter().copied());
    stages
}

fn run(session: &Session, _: NoParams) -> Result<JobPlan<Aligned>, CommandError> {
    let mut plan = Plan::new(session, "align.run", 1)?;
    let stages = stages(&plan, &[("Aligning frames", 1.0)]);
    let job = JobPlan::new(move |ctx| {
        let mut stage = 0;
        if plan.detects() {
            ctx.stage(stage);
            stage += 1;
        }
        plan.defects(ctx)?;
        ctx.stage(stage);
        let positions: Vec<usize> = (0..plan.ids.len()).collect();
        let settings = plan.settings.clone();
        let alignment = plan.align(ctx, &positions, &settings, true)?;
        let ids = plan.ids.clone();
        plan.commit(ids, &alignment, false)
    });
    Ok(job.stages(&stages).locks(locks()))
}

/// An additional command installed only in a watch job's private engine. Its registry owns the capture state,
/// so independent captures never share a chain. Project changes still use the same alignment commit as `align.run`.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn capture_spec() -> CommandSpec {
    use std::sync::{Arc, Mutex};
    type Capture = Option<(Vec<FrameId>, StackAligner)>;
    let capture: Arc<Mutex<Capture>> = Arc::new(Mutex::new(None));
    CommandSpec::job(
        Meta::new("watch.align", "Align arriving frames", "Extends this capture's alignment with its newly ingested frames.").undoable(),
        Lane::Heavy,
        move |session, _: NoParams| {
            let mut plan = Plan::new(session, "watch.align", 1)?;
            let capture = capture.clone();
            Ok(JobPlan::new(move |ctx| {
                let mut guard = capture.lock().map_err(|_| plan.internal())?;
                let mut state = guard.take();
                if state.as_ref().is_some_and(|(ids, _)| !plan.ids.starts_with(ids)) {
                    state = None;
                }
                let (mut ids, mut aligner) = match state {
                    Some(state) => state,
                    None => (Vec::new(), StackAligner::new(&plan.settings, 1, 0)?),
                };
                plan.load_mask()?;
                aligner.set_defects(plan.mask.clone());
                for i in ids.len()..plan.ids.len() {
                    ctx.checkpoint()?;
                    let image = plan.read(i)?;
                    if ids.is_empty() {
                        aligner.add(&image, ctx)?;
                    } else {
                        aligner.append(&image, ctx)?;
                    }
                    let frame = plan.frames.get(i).ok_or_else(|| plan.internal())?;
                    let preview = write_frame_preview(&plan.cache, frame.id(), &image, plan.frame_key(i)?)?;
                    plan.previews.push((frame.id(), preview));
                    ids.push(frame.id());
                }
                let alignment = aligner.snapshot()?;
                *guard = Some((ids.clone(), aligner));
                plan.commit(ids, &alignment, false)
            })
            .stages(&[("Aligning arriving frames", 1.0)])
            .locks(locks()))
        },
    )
}

fn sort_by_scale(session: &Session, _: NoParams) -> Result<JobPlan<Aligned>, CommandError> {
    let mut plan = Plan::new(session, "align.sortByScale", 2)?;
    let stages = stages(&plan, &[("Measuring scales", 0.5), ("Aligning frames", 0.5)]);
    let job = JobPlan::new(move |ctx| {
        let mut stage = 0;
        if plan.detects() {
            ctx.stage(stage);
            stage += 1;
        }
        plan.defects(ctx)?;
        ctx.stage(stage);
        let present: Vec<usize> = (0..plan.ids.len()).collect();
        let measured = plan.align(ctx, &present, &measuring_settings(&plan.settings), false)?;
        let order = scale_order(&measured)?;
        ctx.stage(stage + 1);
        let settings = plan.settings.clone();
        let alignment = plan.align(ctx, &order, &settings, true)?;
        let ids: Vec<FrameId> = order.iter().filter_map(|&i| plan.ids.get(i).copied()).collect();
        plan.commit(ids, &alignment, true)
    });
    Ok(job.stages(&stages).locks(locks()))
}

/// The stack's alignment.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Info {
    /// Whether the project holds an alignment run.
    aligned: bool,
    /// The framing the settings choose.
    framing: Framing,
    /// The run, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<RunInfo>,
}

/// An alignment run.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct RunInfo {
    /// The frame the others are aligned to.
    master: FrameId,
    /// The frames' size, in pixels, which the transforms are in.
    width: usize,
    height: usize,
    /// The end with the narrower field of view, when the frames' scales tell.
    narrow_end: Option<End>,
    /// The end the stack is processed from.
    start: End,
    /// The run's frames, in the order the stack is processed.
    order: Vec<FrameId>,
    /// The settings it ran with.
    settings: AlignSettings,
    /// The output each framing gives.
    framings: Vec<FramingInfo>,
    /// The run's frames, in the project's frame order.
    frames: Vec<AlignedFrameInfo>,
}

/// The output a framing gives: its size and where it lies on the master frame, or why the frames can't be framed so.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct FramingInfo {
    framing: Framing,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<usize>,
    /// From the output's pixels to the master frame's, as the rows of an affine transform `[[a, b, c], [d, e, f]]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    to_master: Option<[[f64; 3]; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// A frame's alignment.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct AlignedFrameInfo {
    frame: FrameId,
    /// From the master's pixels to this frame's, as the rows of an affine transform.
    transform: [[f64; 3]; 2],
    /// The transform's terms: the magnification relative to the master (above 1 when this frame shows the scene
    /// larger), the rotation in degrees and the shift in pixels, about the frame's center.
    #[serde(skip_serializing_if = "Option::is_none")]
    scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rotation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shift: Option<[f64; 2]>,
    /// The gain that matches its brightness to the master's.
    gain: f64,
    /// How well it matched its neighbor, from 0 to 1.
    score: f64,
    /// Aligned, rejected or the master (none when the project has no report for it).
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    flags: Vec<Flag>,
    /// The share of the stack's in-focus detail on which it is among the sharpest, when it could be told.
    #[serde(skip_serializing_if = "Option::is_none")]
    sharp_fraction: Option<f64>,
    /// The frame it was aligned to.
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<FrameId>,
    /// What moved in it against that frame: a score, the largest displacement and the regions, in frame pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    motion: Option<FrameMotion>,
}

fn info(session: &Session, _: NoParams) -> Result<Info, CommandError> {
    let project = session.project()?;
    let framing = align_settings(project)?.framing;
    let Some(stored) = stored_alignment(project)? else { return Ok(Info { aligned: false, framing, run: None }) };
    let (w, h) = (stored.report.width, stored.report.height);
    let framings = Framing::ALL
        .into_iter()
        .map(|framing| match stored.canvas(framing) {
            Ok(c) => FramingInfo { framing, width: Some(c.width), height: Some(c.height), to_master: Some(c.to_master.m), error: None },
            Err(e) => FramingInfo { framing, width: None, height: None, to_master: None, error: Some(e.to_string()) },
        })
        .collect();
    let frames = stored
        .frames
        .iter()
        .map(|f| {
            let terms = Similarity::from_affine(&f.alignment.transform, w, h);
            let report = f.report.as_ref();
            AlignedFrameInfo {
                frame: f.frame,
                transform: f.alignment.transform.m,
                scale: terms.map(|t| t.scale),
                rotation: terms.map(|t| t.rotation_deg),
                shift: terms.map(|t| [t.shift_x, t.shift_y]),
                gain: f.alignment.brightness_gain,
                score: f.alignment.quality,
                status: report.map(|r| r.status),
                flags: report.map(|r| r.flags.clone()).unwrap_or_default(),
                sharp_fraction: report.and_then(|r| r.sharp_fraction),
                reference: report.and_then(|r| r.reference),
                motion: report.and_then(|r| r.motion.clone()),
            }
        })
        .collect();
    let run = RunInfo {
        master: stored.master,
        width: w,
        height: h,
        narrow_end: stored.report.narrow_end,
        start: stored.report.start,
        order: stored.report.order.clone(),
        settings: stored.settings.clone(),
        framings,
        frames,
    };
    Ok(Info { aligned: true, framing, run: Some(run) })
}
