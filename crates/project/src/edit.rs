//! Changes to a project as values: an [`Edit`] is one change, [`Project::apply`] makes it and returns its inverse, the
//! edit that undoes it. The engine's undo history keeps those inverses (undoing applies one, which returns the edit
//! that redoes it), so that every undoable command is undone by an exact inverse operation rather than by a snapshot
//! of the whole project: a retouch stroke is undone by removing that one stroke ([`Edit::PopStroke`]).
//!
//! [`Project::apply`] is all or nothing: an edit that can't be made (a frame that isn't there, a name that isn't
//! allowed) changes nothing and returns an [`EditError`], and a [`Edit::Batch`] whose part fails undoes the parts made
//! before it. Ids are never reused: an edit that removes a frame, an output or a layer keeps it whole in its inverse,
//! so that undoing puts it back with its id, its cache references and everything worked out for it.
//!
//! Each edit says which [`Aspects`] of the project it changes, which the engine uses to tell its views what to show
//! again and to keep edits away from what a running job reads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::cache::CacheRef;
use crate::error::Problem;
use crate::model::{
    Alignment, AlignmentRun, Develop, Frame, FrameFile, FrameId, LayerId, MapName, Output, OutputId, Project, RetouchLayer, check_frame_path,
    check_name,
};
use crate::prefs::{Group, Table};

/// One change to a project. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Names the project (`Some`), or makes it a temporary, unnamed one again (`None`).
    SetName(Option<String>),
    /// Adds a new frame at the end of the list, enabled, with a new id. Its inverse removes it.
    AddFrame {
        file: FrameFile,
        exif: Option<Value>,
    },
    /// Puts a frame back at `index`, with its own id (the inverse of [`Edit::RemoveFrame`]).
    InsertFrame {
        index: usize,
        frame: Box<Frame>,
    },
    RemoveFrame(FrameId),
    /// Puts the frames in this order, which must list every frame once.
    SetFrameOrder(Vec<FrameId>),
    SetFrameEnabled {
        frame: FrameId,
        enabled: bool,
    },
    /// Replaces a preference group's table.
    SetPreferences {
        group: Group,
        table: Table,
    },
    /// Replaces the stack's resolved development.
    SetDevelop(Option<Develop>),
    /// Replaces the stack's alignment run, and the alignment and report of each frame listed.
    SetAlignment {
        run: Option<AlignmentRun>,
        frames: Vec<AlignedFrame>,
    },
    /// Sets one of the stack's maps (`Some`), such as its defect mask, or removes it (`None`).
    SetProjectMap {
        name: MapName,
        map: Option<CacheRef>,
    },
    /// Replaces a frame's previews: as shot, and aligned.
    SetPreviews {
        frame: FrameId,
        preview: Option<CacheRef>,
        aligned_preview: Option<CacheRef>,
    },
    /// Puts an output back at `index`, with its own id (the inverse of [`Edit::RemoveOutput`]).
    InsertOutput {
        index: usize,
        output: Box<Output>,
    },
    RemoveOutput(OutputId),
    RenameOutput {
        output: OutputId,
        name: String,
    },
    /// Adds an empty, visible retouch layer on top of an output's, with a new id.
    AddLayer {
        output: OutputId,
        name: String,
    },
    /// Puts a retouch layer back at `index` of its output's layers (the inverse of [`Edit::RemoveLayer`]).
    InsertLayer {
        output: OutputId,
        index: usize,
        layer: RetouchLayer,
    },
    RemoveLayer {
        output: OutputId,
        layer: LayerId,
    },
    /// Adds one stroke record on top of a layer's strokes.
    PushStroke {
        output: OutputId,
        layer: LayerId,
        stroke: Value,
    },
    /// Removes a layer's last stroke.
    PopStroke {
        output: OutputId,
        layer: LayerId,
    },
    /// Several edits, made in order as one.
    Batch(Vec<Edit>),
}

/// A frame's alignment and report, as [`Edit::SetAlignment`] sets them.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignedFrame {
    pub frame: FrameId,
    pub alignment: Option<Alignment>,
    pub report: Option<Value>,
}

/// Why an edit can't be made. The project is unchanged.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EditError {
    #[error("the project has no {0}")]
    NoFrame(FrameId),
    #[error("the project has no {0}")]
    NoOutput(OutputId),
    #[error("{output} has no {layer}")]
    NoLayer { output: OutputId, layer: LayerId },
    #[error("position {index} is past the end of the {what} ({len} of them)")]
    Index { what: &'static str, index: usize, len: usize },
    #[error("the id {id} is in use or was never given out")]
    Id { id: u64 },
    #[error("a new frame order must list every frame of the project once")]
    Order,
    #[error("{layer} of {output} has no stroke to remove")]
    NoStroke { output: OutputId, layer: LayerId },
    #[error(transparent)]
    Invalid(#[from] Problem),
}

/// Parts of a project that edits change, as a set.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Aspects(u16);

impl Aspects {
    pub const NONE: Aspects = Aspects(0);
    /// The project's name.
    pub const NAME: Aspects = Aspects(1);
    /// The frame list: which frames, their order and whether they are enabled.
    pub const FRAMES: Aspects = Aspects(1 << 1);
    /// The develop preferences and the stack's resolved development.
    pub const DEVELOP: Aspects = Aspects(1 << 2);
    /// The alignment preferences and results.
    pub const ALIGN: Aspects = Aspects(1 << 3);
    /// The stacking preferences.
    pub const STACK: Aspects = Aspects(1 << 4);
    /// The output preferences (what outputs are written as).
    pub const EXPORT: Aspects = Aspects(1 << 5);
    /// The outputs: which there are, and their names.
    pub const OUTPUTS: Aspects = Aspects(1 << 6);
    /// The retouch layers and their strokes.
    pub const RETOUCH: Aspects = Aspects(1 << 7);
    /// The frames' previews.
    pub const PREVIEWS: Aspects = Aspects(1 << 8);
    pub const ALL: Aspects = Aspects((1 << 9) - 1);

    const NAMES: [(Aspects, &'static str); 9] = [
        (Aspects::NAME, "name"),
        (Aspects::FRAMES, "frames"),
        (Aspects::DEVELOP, "develop"),
        (Aspects::ALIGN, "align"),
        (Aspects::STACK, "stack"),
        (Aspects::EXPORT, "export"),
        (Aspects::OUTPUTS, "outputs"),
        (Aspects::RETOUCH, "retouch"),
        (Aspects::PREVIEWS, "previews"),
    ];

    /// The aspect that a preference group belongs to.
    pub fn of_group(group: Group) -> Aspects {
        match group {
            Group::Develop => Aspects::DEVELOP,
            Group::Align => Aspects::ALIGN,
            Group::Stack => Aspects::STACK,
            Group::Output => Aspects::EXPORT,
        }
    }

    #[must_use]
    pub fn union(self, other: Aspects) -> Aspects {
        Aspects(self.0 | other.0)
    }

    #[must_use]
    pub fn intersection(self, other: Aspects) -> Aspects {
        Aspects(self.0 & other.0)
    }

    pub fn intersects(self, other: Aspects) -> bool {
        self.0 & other.0 != 0
    }

    pub fn contains(self, other: Aspects) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The names of the aspects in the set, such as `frames` and `outputs`.
    pub fn names(self) -> Vec<&'static str> {
        Aspects::NAMES.iter().filter(|(a, _)| self.contains(*a)).map(|&(_, name)| name).collect()
    }

    /// Every aspect name, in order.
    pub fn all_names() -> [&'static str; 9] {
        Aspects::NAMES.map(|(_, name)| name)
    }
}

impl fmt::Debug for Aspects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.names()).finish()
    }
}

impl fmt::Display for Aspects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self.names();
        if names.is_empty() { f.write_str("nothing") } else { f.write_str(&names.join(", ")) }
    }
}

/// An aspect set is written as the list of its names.
impl Serialize for Aspects {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.names())
    }
}

impl schemars::JsonSchema for Aspects {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Aspects".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Parts of the project, by name.",
            "type": "array",
            "items": { "type": "string", "enum": Aspects::all_names() },
            "uniqueItems": true,
        })
    }
}

impl Edit {
    /// The parts of the project the edit changes.
    pub fn aspects(&self) -> Aspects {
        match self {
            Edit::SetName(_) => Aspects::NAME,
            Edit::AddFrame { .. } | Edit::InsertFrame { .. } | Edit::RemoveFrame(_) | Edit::SetFrameOrder(_) | Edit::SetFrameEnabled { .. } => {
                Aspects::FRAMES
            }
            Edit::SetPreferences { group, .. } => Aspects::of_group(*group),
            Edit::SetDevelop(_) => Aspects::DEVELOP,
            // The stack's maps (the defect mask) are what alignment reads its frames with.
            Edit::SetAlignment { .. } | Edit::SetProjectMap { .. } => Aspects::ALIGN,
            Edit::SetPreviews { .. } => Aspects::PREVIEWS,
            // An output's layers go with it.
            Edit::InsertOutput { .. } | Edit::RemoveOutput(_) => Aspects::OUTPUTS.union(Aspects::RETOUCH),
            Edit::RenameOutput { .. } => Aspects::OUTPUTS,
            Edit::AddLayer { .. } | Edit::InsertLayer { .. } | Edit::RemoveLayer { .. } | Edit::PushStroke { .. } | Edit::PopStroke { .. } => {
                Aspects::RETOUCH
            }
            Edit::Batch(edits) => edits.iter().fold(Aspects::NONE, |a, e| a.union(e.aspects())),
        }
    }

    /// Whether the edit sets a value whole: applied after `earlier`, which set the same value, it leaves no trace of
    /// it. The engine uses it to keep the undo step of a slider drag small: of the inverses of a run of such edits,
    /// only the oldest matters.
    pub fn overwrites(&self, earlier: &Edit) -> bool {
        match (self, earlier) {
            (Edit::SetName(_), Edit::SetName(_)) | (Edit::SetFrameOrder(_), Edit::SetFrameOrder(_)) | (Edit::SetDevelop(_), Edit::SetDevelop(_)) => {
                true
            }
            (Edit::SetPreferences { group: a, .. }, Edit::SetPreferences { group: b, .. }) => a == b,
            (Edit::SetFrameEnabled { frame: a, .. }, Edit::SetFrameEnabled { frame: b, .. })
            | (Edit::SetPreviews { frame: a, .. }, Edit::SetPreviews { frame: b, .. }) => a == b,
            (Edit::RenameOutput { output: a, .. }, Edit::RenameOutput { output: b, .. }) => a == b,
            (Edit::SetProjectMap { name: a, .. }, Edit::SetProjectMap { name: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl Project {
    /// Makes `edit` and returns its inverse, or changes nothing and returns why it can't be made. See the module
    /// documentation.
    pub fn apply(&mut self, edit: Edit) -> Result<Edit, EditError> {
        match edit {
            Edit::Batch(edits) => self.apply_batch(edits),
            Edit::SetName(name) => {
                if let Some(name) = &name {
                    check_name("project name", name)?;
                }
                Ok(Edit::SetName(std::mem::replace(&mut self.name, name)))
            }
            Edit::AddFrame { file, exif } => {
                check_frame_path(&file.path)?;
                Ok(Edit::RemoveFrame(self.add_frame(file, exif)))
            }
            Edit::InsertFrame { index, frame } => self.insert_frame(index, *frame),
            Edit::RemoveFrame(id) => {
                let index = self.frames.iter().position(|f| f.id() == id).ok_or(EditError::NoFrame(id))?;
                let frame = self.frames.remove(index);
                Ok(Edit::InsertFrame { index, frame: Box::new(frame) })
            }
            Edit::SetFrameOrder(order) => self.set_frame_order(&order),
            Edit::SetFrameEnabled { frame, enabled } => {
                let f = self.frame_mut(frame).ok_or(EditError::NoFrame(frame))?;
                Ok(Edit::SetFrameEnabled { frame, enabled: std::mem::replace(&mut f.enabled, enabled) })
            }
            Edit::SetPreferences { group, table } => {
                let old = self.preferences.replace_group(group, table)?;
                Ok(Edit::SetPreferences { group, table: old })
            }
            Edit::SetDevelop(develop) => Ok(Edit::SetDevelop(std::mem::replace(&mut self.develop, develop))),
            Edit::SetAlignment { run, frames } => self.set_alignment(run, frames),
            Edit::SetProjectMap { name, map } => {
                let old = match map {
                    Some(map) => self.maps.insert(name.clone(), map),
                    None => self.maps.remove(&name),
                };
                Ok(Edit::SetProjectMap { name, map: old })
            }
            Edit::SetPreviews { frame, preview, aligned_preview } => {
                let f = self.frame_mut(frame).ok_or(EditError::NoFrame(frame))?;
                let preview = std::mem::replace(&mut f.preview, preview);
                let aligned_preview = std::mem::replace(&mut f.aligned_preview, aligned_preview);
                Ok(Edit::SetPreviews { frame, preview, aligned_preview })
            }
            Edit::InsertOutput { index, output } => self.insert_output(index, *output),
            Edit::RemoveOutput(id) => {
                let index = self.outputs.iter().position(|o| o.id() == id).ok_or(EditError::NoOutput(id))?;
                let output = self.outputs.remove(index);
                Ok(Edit::InsertOutput { index, output: Box::new(output) })
            }
            Edit::RenameOutput { output, name } => {
                check_name("output name", &name)?;
                let o = self.output_mut(output).ok_or(EditError::NoOutput(output))?;
                Ok(Edit::RenameOutput { output, name: std::mem::replace(&mut o.name, name) })
            }
            Edit::AddLayer { output, name } => {
                check_name("retouch layer name", &name)?;
                let layer = self.add_layer(output, &name).ok_or(EditError::NoOutput(output))?;
                Ok(Edit::RemoveLayer { output, layer })
            }
            Edit::InsertLayer { output, index, layer } => self.insert_layer(output, index, layer),
            Edit::RemoveLayer { output, layer } => {
                let o = self.output_mut(output).ok_or(EditError::NoOutput(output))?;
                let index = o.retouch.iter().position(|l| l.id() == layer).ok_or(EditError::NoLayer { output, layer })?;
                let removed = o.retouch.remove(index);
                Ok(Edit::InsertLayer { output, index, layer: removed })
            }
            Edit::PushStroke { output, layer, stroke } => {
                self.layer_mut(output, layer)?.strokes.push(stroke);
                Ok(Edit::PopStroke { output, layer })
            }
            Edit::PopStroke { output, layer } => {
                let stroke = self.layer_mut(output, layer)?.strokes.pop().ok_or(EditError::NoStroke { output, layer })?;
                Ok(Edit::PushStroke { output, layer, stroke })
            }
        }
    }

    /// Makes the edits in order, or none of them.
    fn apply_batch(&mut self, edits: Vec<Edit>) -> Result<Edit, EditError> {
        let mut inverses = Vec::with_capacity(edits.len());
        for edit in edits {
            match self.apply(edit) {
                Ok(inverse) => inverses.push(inverse),
                Err(e) => {
                    for inverse in inverses.into_iter().rev() {
                        // The inverse of an edit just made can always be made; if it couldn't, say so.
                        if let Err(failed) = self.apply(inverse) {
                            tracing::error!("an edit could not be rolled back: {failed}");
                        }
                    }
                    return Err(e);
                }
            }
        }
        inverses.reverse();
        Ok(Edit::Batch(inverses))
    }

    fn insert_frame(&mut self, index: usize, frame: Frame) -> Result<Edit, EditError> {
        check_frame_path(&frame.path)?;
        self.check_new_id(frame.id().get())?;
        let len = self.frames.len();
        if index > len {
            return Err(EditError::Index { what: "frame list", index, len });
        }
        let id = frame.id();
        self.frames.insert(index, frame);
        Ok(Edit::RemoveFrame(id))
    }

    /// Sets the alignment run and the listed frames' alignments and reports, after checking every frame first; the
    /// inverse lists the old values in the reverse order, so that a frame listed twice ends as it was.
    fn set_alignment(&mut self, run: Option<AlignmentRun>, frames: Vec<AlignedFrame>) -> Result<Edit, EditError> {
        for f in &frames {
            if self.frame(f.frame).is_none() {
                return Err(EditError::NoFrame(f.frame));
            }
            if let Some(a) = &f.alignment {
                let finite = a.transform.m.iter().flatten().all(|v| v.is_finite()) && a.brightness_gain.is_finite() && a.quality.is_finite();
                if !finite {
                    return Err(EditError::Invalid(Problem::NotFinite { what: "frame alignment" }));
                }
            }
        }
        let mut old = Vec::with_capacity(frames.len());
        for f in frames {
            if let Some(frame) = self.frame_mut(f.frame) {
                let alignment = std::mem::replace(&mut frame.alignment, f.alignment);
                let report = std::mem::replace(&mut frame.report, f.report);
                old.push(AlignedFrame { frame: f.frame, alignment, report });
            }
        }
        old.reverse();
        Ok(Edit::SetAlignment { run: std::mem::replace(&mut self.alignment, run), frames: old })
    }

    fn set_frame_order(&mut self, order: &[FrameId]) -> Result<Edit, EditError> {
        let old: Vec<FrameId> = self.frames.iter().map(Frame::id).collect();
        let present: BTreeSet<FrameId> = old.iter().copied().collect();
        let mut seen = BTreeSet::new();
        if order.len() != old.len() || !order.iter().all(|id| present.contains(id) && seen.insert(*id)) {
            return Err(EditError::Order);
        }
        let mut by_id: BTreeMap<FrameId, Frame> = std::mem::take(&mut self.frames).into_iter().map(|f| (f.id(), f)).collect();
        self.frames = order.iter().filter_map(|id| by_id.remove(id)).collect();
        Ok(Edit::SetFrameOrder(old))
    }

    fn insert_output(&mut self, index: usize, output: Output) -> Result<Edit, EditError> {
        check_name("output name", &output.name)?;
        self.check_new_id(output.id().get())?;
        for layer in &output.retouch {
            self.check_new_id(layer.id().get())?;
        }
        let len = self.outputs.len();
        if index > len {
            return Err(EditError::Index { what: "output list", index, len });
        }
        let id = output.id();
        self.outputs.insert(index, output);
        Ok(Edit::RemoveOutput(id))
    }

    fn insert_layer(&mut self, output: OutputId, index: usize, layer: RetouchLayer) -> Result<Edit, EditError> {
        check_name("retouch layer name", &layer.name)?;
        self.check_new_id(layer.id().get())?;
        let o = self.output_mut(output).ok_or(EditError::NoOutput(output))?;
        let len = o.retouch.len();
        if index > len {
            return Err(EditError::Index { what: "retouch layers", index, len });
        }
        let id = layer.id();
        o.retouch.insert(index, layer);
        Ok(Edit::RemoveLayer { output, layer: id })
    }

    fn layer_mut(&mut self, output: OutputId, layer: LayerId) -> Result<&mut RetouchLayer, EditError> {
        let o = self.output_mut(output).ok_or(EditError::NoOutput(output))?;
        o.layer_mut(layer).ok_or(EditError::NoLayer { output, layer })
    }

    /// Checks that an id being put back was given out and isn't in use.
    fn check_new_id(&self, id: u64) -> Result<(), EditError> {
        let used = self.frames.iter().any(|f| f.id().get() == id)
            || self.outputs.iter().any(|o| o.id().get() == id || o.retouch.iter().any(|l| l.id().get() == id));
        if id == 0 || id >= self.next_id || used { Err(EditError::Id { id }) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::ContentHash;

    fn file(n: u8) -> FrameFile {
        FrameFile { path: format!("/photos/{n}.tif"), size: 10, modified: None, hash: ContentHash::from_bytes([n; 32]) }
    }

    fn cache_ref(n: u8) -> CacheRef {
        CacheRef {
            path: crate::cache::CachePath::parse(&format!("cache/defects-{n:016x}.tiff")).unwrap(),
            size: 10,
            hash: ContentHash::from_bytes([n; 32]),
            key: None,
        }
    }

    fn aligned(gain: f64) -> Alignment {
        Alignment { transform: florastack_core::Affine::translation(1.5, -0.25), brightness_gain: gain, quality: 0.9 }
    }

    #[test]
    fn every_edit_is_undone_by_its_inverse() {
        let mut p = Project::new(Some("Bee")).unwrap();
        let a = p.add_frame(file(1), None);
        let b = p.add_frame(file(2), None);
        let before = p.clone();
        let edits = [
            Edit::SetName(None),
            Edit::SetName(Some("Wasp".into())),
            Edit::AddFrame { file: file(3), exif: Some(serde_json::json!({ "iso": 100 })) },
            Edit::RemoveFrame(a),
            Edit::SetFrameOrder(vec![b, a]),
            Edit::SetFrameEnabled { frame: b, enabled: false },
            Edit::SetPreferences { group: Group::Develop, table: serde_json::json!({ "exposure": 1.5 }).as_object().unwrap().clone() },
            Edit::Batch(vec![Edit::RemoveFrame(b), Edit::SetName(Some("Ant".into()))]),
            Edit::SetAlignment {
                run: Some(AlignmentRun { master: b, settings: serde_json::json!({}), report: Some(serde_json::json!({ "start": "last" })) }),
                frames: vec![
                    AlignedFrame { frame: a, alignment: Some(aligned(1.02)), report: Some(serde_json::json!({ "flags": ["out-of-focus"] })) },
                    AlignedFrame { frame: b, alignment: Some(aligned(1.0)), report: None },
                    AlignedFrame { frame: a, alignment: Some(aligned(1.03)), report: None },
                ],
            },
            Edit::SetProjectMap { name: MapName::new("defects").unwrap(), map: Some(cache_ref(1)) },
            Edit::SetPreviews { frame: b, preview: Some(cache_ref(2)), aligned_preview: Some(cache_ref(3)) },
        ];
        for edit in edits {
            let mut q = before.clone();
            let inverse = q.apply(edit.clone()).unwrap();
            assert_ne!(q, before, "{edit:?} changed nothing");
            let redo = q.apply(inverse).unwrap();
            if matches!(edit, Edit::AddFrame { .. }) {
                // Ids are never reused: the next free id stays where adding the frame moved it.
                assert_eq!(q.frames, before.frames, "the inverse of {edit:?} undoes it");
            } else {
                assert_eq!(q, before, "the inverse of {edit:?} undoes it");
            }
            q.apply(redo).unwrap();
            let mut once = before.clone();
            once.apply(edit.clone()).unwrap();
            assert_eq!(q, once, "redoing {edit:?} makes it again");
        }
    }

    #[test]
    fn a_failed_edit_changes_nothing() {
        let mut p = Project::new(None).unwrap();
        let a = p.add_frame(file(1), None);
        let before = p.clone();
        let failing = [
            (Edit::RemoveFrame(FrameId(99)), EditError::NoFrame(FrameId(99))),
            (Edit::SetFrameOrder(vec![a, a]), EditError::Order),
            (Edit::SetFrameOrder(vec![]), EditError::Order),
            (Edit::SetName(Some(" ".into())), EditError::Invalid(Problem::Text { what: "project name", reason: "is empty" })),
            (Edit::Batch(vec![Edit::SetName(Some("Ok".into())), Edit::RemoveFrame(a), Edit::RemoveFrame(a)]), EditError::NoFrame(a)),
            (Edit::PopStroke { output: OutputId(5), layer: LayerId(6) }, EditError::NoOutput(OutputId(5))),
            (Edit::SetPreviews { frame: FrameId(99), preview: None, aligned_preview: None }, EditError::NoFrame(FrameId(99))),
            (
                Edit::SetAlignment {
                    run: None,
                    frames: vec![
                        AlignedFrame { frame: a, alignment: Some(aligned(1.0)), report: None },
                        AlignedFrame { frame: FrameId(99), alignment: None, report: None },
                    ],
                },
                EditError::NoFrame(FrameId(99)),
            ),
            (
                Edit::SetAlignment { run: None, frames: vec![AlignedFrame { frame: a, alignment: Some(aligned(f64::NAN)), report: None }] },
                EditError::Invalid(Problem::NotFinite { what: "frame alignment" }),
            ),
        ];
        for (edit, error) in failing {
            assert_eq!(p.apply(edit.clone()), Err(error), "{edit:?}");
            assert_eq!(p, before, "{edit:?} left the project as it was");
        }
        // A frame put back must keep an id that was given out and is free.
        let frame = p.frames[0].clone();
        assert_eq!(p.apply(Edit::InsertFrame { index: 0, frame: Box::new(frame) }), Err(EditError::Id { id: a.get() }));
        let mut unknown = p.frames[0].clone();
        unknown.id = FrameId(50);
        assert_eq!(p.apply(Edit::InsertFrame { index: 0, frame: Box::new(unknown) }), Err(EditError::Id { id: 50 }));
        let mut bad = file(9);
        bad.path = "nul\0byte".into();
        assert!(matches!(p.apply(Edit::AddFrame { file: bad, exif: None }), Err(EditError::Invalid(_))));
        assert_eq!(p, before);
    }

    #[test]
    fn aspects_name_what_an_edit_changes() {
        let frames = Edit::Batch(vec![Edit::RemoveFrame(FrameId(1)), Edit::SetPreferences { group: Group::Output, table: Table::new() }]);
        assert_eq!(frames.aspects(), Aspects::FRAMES.union(Aspects::EXPORT));
        assert_eq!(frames.aspects().names(), ["frames", "export"]);
        assert_eq!(serde_json::to_value(Aspects::NAME.union(Aspects::RETOUCH)).unwrap(), serde_json::json!(["name", "retouch"]));
        assert!(Aspects::ALL.contains(Aspects::RETOUCH) && !Aspects::NONE.intersects(Aspects::ALL));
        let a = Edit::SetPreferences { group: Group::Develop, table: Table::new() };
        assert!(a.overwrites(&a.clone()));
        assert!(!a.overwrites(&Edit::SetPreferences { group: Group::Align, table: Table::new() }));
    }
}
