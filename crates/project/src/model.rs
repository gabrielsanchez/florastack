//! The project model: a [`Project`] and everything in it. It is plain data with serde support, saved as
//! `project.json` (the [`crate::schema`] module), so that the engine's commands can change it and undo their changes.
//!
//! What other crates define is kept here as JSON data ([`Value`]) that this crate stores and never interprets: each
//! frame's EXIF summary (florastack-codecs), the stack's develop settings and their resolution (florastack-raw), the
//! alignment and stacking settings (florastack-align and florastack-stack), each output's parameters
//! (florastack-stack) and its retouch strokes (florastack-retouch). The engine converts them to and from their crates'
//! types. Everything else, the frame list, the alignment results, the outputs and the cache references, is this
//! crate's own.
//!
//! Frames, outputs and retouch layers have ids that are never reused within a project ([`FrameId`], [`OutputId`],
//! [`LayerId`]), so that references to them survive reordering and removal. A reference may name something that has
//! since been removed: it records where a result came from.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use florastack_core::Affine;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::cache::CacheRef;
use crate::error::{Problem, ProjectError};
use crate::hash::ContentHash;
use crate::prefs::Preferences;
use crate::schema::{FormatTag, VersionTag};
use crate::time::Timestamp;

/// Another crate's value as the JSON data a project stores; `what` names it in the error.
pub fn to_data<T: Serialize>(what: &'static str, value: &T) -> Result<Value, ProjectError> {
    serde_json::to_value(value).map_err(|source| ProjectError::Data { what, source })
}

/// JSON data the project stores, read as another crate's type; `what` names it in the error.
pub fn from_data<T: DeserializeOwned>(what: &'static str, data: &Value) -> Result<T, ProjectError> {
    T::deserialize(data).map_err(|source| ProjectError::Data { what, source })
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $what:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub(crate) u64);

        impl $name {
            /// The number in the project file.
            pub fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($what, " {}"), self.0)
            }
        }
    };
}

id_type!(
    /// A frame's id in its project.
    FrameId,
    "frame"
);
id_type!(
    /// An output's id in its project.
    OutputId,
    "output"
);
id_type!(
    /// A retouch layer's id in its project.
    LayerId,
    "layer"
);

/// The name of a map in the cache: lower-case letters, digits and `-`, such as `depth`, `confidence`, `dust` or
/// `motion`. The crate that makes a map chooses its name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MapName(String);

impl MapName {
    pub fn new(name: &str) -> Result<MapName, Problem> {
        let ok = (1..=32).contains(&name.len())
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-');
        if ok {
            Ok(MapName(name.to_owned()))
        } else {
            Err(Problem::Text { what: "map name", reason: "must be 1 to 32 lower-case letters, digits and '-'" })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MapName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for MapName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MapName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<MapName, D::Error> {
        let name = String::deserialize(deserializer)?;
        MapName::new(&name).map_err(|e| serde::de::Error::custom(format!("{name:?}: {e}")))
    }
}

/// Named maps in the cache, such as an output's depth and confidence maps.
pub type Maps = BTreeMap<MapName, CacheRef>;

/// An [`Affine`] as the rows of its matrix, `[[a, b, c], [d, e, f]]`.
mod affine {
    use florastack_core::Affine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(t: &Affine, serializer: S) -> Result<S::Ok, S::Error> {
        t.m.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Affine, D::Error> {
        Ok(Affine { m: <[[f64; 3]; 2]>::deserialize(deserializer)? })
    }
}

/// A frame's alignment, as florastack-align estimates it relative to the stack's master frame.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alignment {
    /// Maps a pixel of the master frame to the same point of the scene in this frame (as
    /// `florastack_core::testutil::GroundTruth::relative` does), so that warping this frame onto the master samples it
    /// there.
    #[serde(with = "affine")]
    pub transform: Affine,
    /// The gain that matches this frame's brightness to the master's.
    pub brightness_gain: f64,
    /// How well the frame matched, from 0 (not at all) to 1 (perfectly).
    pub quality: f64,
}

/// An input frame: a file of the stack, as it was when it was added, and what has been worked out for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub(crate) id: FrameId,
    /// The file: an absolute path, or one relative to the folder that holds the project folder.
    pub path: String,
    /// Its size in bytes when it was added.
    pub size: u64,
    /// Its modification time when it was hashed, which tells a quick check whether to hash it again. It is kept only
    /// when it was already old enough then to be trusted ([`FrameFile`]); without it, every check hashes the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<Timestamp>,
    pub hash: ContentHash,
    /// Its EXIF summary (florastack-codecs' `ExifSummary`, converted by the engine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exif: Option<Value>,
    /// Whether the frame goes into stacks.
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<Alignment>,
    /// The frame developed and aligned, losslessly, as stacking and retouching read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aligned: Option<CacheRef>,
    /// A small JPEG for the frame list and the filmstrip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<CacheRef>,
    /// Other per-frame maps, such as a motion map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub maps: Maps,
}

impl Frame {
    pub fn id(&self) -> FrameId {
        self.id
    }
}

/// A file about to become a frame: what [`FrameFile::read`] finds out about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameFile {
    pub path: String,
    pub size: u64,
    /// The file's modification time, if it was at least [`TRUSTED_AGE`] old when the file was hashed: a quick check may
    /// then take an unchanged size and time for unchanged contents.
    pub modified: Option<Timestamp>,
    pub hash: ContentHash,
}

/// How old a file's modification time must be, when the file is hashed, for a later quick check to trust it. A file
/// rewritten with the same size soon after it was hashed can keep the same modification time, so a recent time can't
/// tell the rewrite apart (git calls it a racy timestamp): [`Timestamp`] keeps milliseconds, and FAT, which many
/// memory cards use, records times in 2-second steps. The extra second covers clock differences on network shares.
pub const TRUSTED_AGE: Duration = Duration::from_secs(3);

/// `modified`, if it is at least [`TRUSTED_AGE`] old now, just after the file was hashed. A time in the future (a
/// camera clock set ahead, say) isn't trusted either.
fn trusted_modified(modified: Option<SystemTime>) -> Option<Timestamp> {
    let modified = modified?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    (age >= TRUSTED_AGE).then(|| Timestamp::from_system_time(modified))
}

impl FrameFile {
    /// The file at `path` whose bytes have been read already (to read its metadata too): hashes them, and takes the
    /// modification time from the file system. `path` is made absolute, relative to the current directory.
    ///
    /// The time is read after the bytes, so a file changed in between has a new, untrusted time
    /// ([`FrameFile::modified`]), and later checks hash it and find that it changed.
    pub fn from_bytes(path: &Path, bytes: &[u8]) -> Result<FrameFile, ProjectError> {
        let absolute = std::path::absolute(path).map_err(|source| ProjectError::Read { path: path.to_path_buf(), source })?;
        let text = absolute.to_str().ok_or_else(|| ProjectError::Path { path: absolute.clone() })?.to_owned();
        let meta = std::fs::metadata(&absolute).map_err(|source| ProjectError::Read { path: absolute.clone(), source })?;
        let hash = ContentHash::of(bytes);
        Ok(FrameFile { path: text, size: bytes.len() as u64, modified: trusted_modified(meta.modified().ok()), hash })
    }

    /// Reads and hashes the file at `path`, which is made absolute (relative to the current directory).
    pub fn read(path: &Path) -> Result<FrameFile, ProjectError> {
        let absolute = std::path::absolute(path).map_err(|source| ProjectError::Read { path: path.to_path_buf(), source })?;
        let text = absolute.to_str().ok_or_else(|| ProjectError::Path { path: absolute.clone() })?.to_owned();
        let read = |source| ProjectError::Read { path: absolute.clone(), source };
        let file = std::fs::File::open(&absolute).map_err(read)?;
        let meta = file.metadata().map_err(read)?;
        let (hash, size) = ContentHash::of_reader(file).map_err(read)?;
        Ok(FrameFile { path: text, size, modified: trusted_modified(meta.modified().ok()), hash })
    }
}

/// The stack's development, resolved once on a reference frame (florastack-raw's `RawDevelop`): the settings it was
/// resolved with and what they came to, so that a reopened project develops its frames exactly as before. When the
/// develop preferences no longer match `settings`, the engine resolves again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Develop {
    /// The frame it was resolved on.
    pub reference: FrameId,
    /// florastack-raw's `DevelopSettings`.
    pub settings: Value,
    /// florastack-raw's `Resolved`.
    pub resolved: Value,
}

/// The stack's alignment run: the master frame that every frame's [`Alignment`] is relative to, and the alignment
/// settings it ran with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlignmentRun {
    pub master: FrameId,
    /// florastack-align's settings.
    pub settings: Value,
}

/// What an output was made from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Input {
    Frame(FrameId),
    /// Another output, such as a slab of a slabbed stack.
    Output(OutputId),
}

/// A layer of retouch strokes on an output. The strokes are florastack-retouch's records, kept in the order they were
/// painted, one per undo step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetouchLayer {
    pub(crate) id: LayerId,
    pub name: String,
    pub visible: bool,
    #[serde(default)]
    pub strokes: Vec<Value>,
}

impl RetouchLayer {
    pub fn id(&self) -> LayerId {
        self.id
    }
}

/// A result: a stack, a slab or a retouched copy of one, with how it was made and where its pixels are.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub(crate) id: OutputId,
    pub name: String,
    /// The stacking method's name in the command registry, such as `pyramid` or `depth-map`.
    pub method: String,
    /// The method's parameters (florastack-stack's).
    pub parameters: Value,
    created: Timestamp,
    /// The frames and outputs it was made from, in order.
    pub inputs: Vec<Input>,
    /// Its pixels, losslessly, in the cache.
    pub image: CacheRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<CacheRef>,
    /// Its other maps, such as `depth` and `confidence`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub maps: Maps,
    /// Retouch layers, bottom first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retouch: Vec<RetouchLayer>,
}

impl Output {
    pub fn id(&self) -> OutputId {
        self.id
    }

    pub fn created(&self) -> Timestamp {
        self.created
    }

    pub fn layer(&self, id: LayerId) -> Option<&RetouchLayer> {
        self.retouch.iter().find(|l| l.id == id)
    }

    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut RetouchLayer> {
        self.retouch.iter_mut().find(|l| l.id == id)
    }
}

/// What makes a new output (its id and creation time are given by [`Project::add_output`]).
#[derive(Clone, Debug, PartialEq)]
pub struct NewOutput {
    pub name: String,
    pub method: String,
    pub parameters: Value,
    pub inputs: Vec<Input>,
    pub image: CacheRef,
    pub preview: Option<CacheRef>,
    pub maps: Maps,
}

/// A focus-stacking project. See the crate documentation for how it is stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    format: FormatTag,
    version: VersionTag,
    /// The software that last saved the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    software: Option<String>,
    /// The name the user gave the project; `None` for a temporary project that hasn't been named.
    pub(crate) name: Option<String>,
    created: Timestamp,
    modified: Timestamp,
    /// The next id to give out; ids are never reused.
    pub(crate) next_id: u64,
    /// The input frames, in the user's order.
    #[serde(default)]
    pub frames: Vec<Frame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub develop: Option<Develop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<AlignmentRun>,
    /// Every processing preference, by group: what presets set.
    #[serde(default)]
    pub preferences: Preferences,
    /// Maps of the whole stack, such as the dust mask (in sensor coordinates) or the coverage mask.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub maps: Maps,
    /// The results, oldest first.
    #[serde(default)]
    pub outputs: Vec<Output>,
}

/// The longest project name, in characters.
pub const MAX_NAME_CHARS: usize = 200;
/// The longest stored path, in bytes.
const MAX_PATH_BYTES: usize = 32 * 1024;

/// Checks a name for a project, an output or a layer.
pub(crate) fn check_name(what: &'static str, name: &str) -> Result<(), Problem> {
    if name.trim().is_empty() {
        return Err(Problem::Text { what, reason: "is empty" });
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(Problem::Text { what, reason: "is longer than 200 characters" });
    }
    if name.chars().any(char::is_control) {
        return Err(Problem::Text { what, reason: "holds control characters" });
    }
    Ok(())
}

impl Project {
    /// A new, empty project; an unnamed one is temporary.
    pub fn new(name: Option<&str>) -> Result<Project, ProjectError> {
        if let Some(name) = name {
            check_name("project name", name).map_err(|p| ProjectError::Name { reason: name_reason(&p) })?;
        }
        let now = Timestamp::now();
        Ok(Project {
            format: FormatTag,
            version: VersionTag,
            software: None,
            name: name.map(str::to_owned),
            created: now,
            modified: now,
            next_id: 1,
            frames: Vec::new(),
            develop: None,
            alignment: None,
            preferences: Preferences::default(),
            maps: Maps::new(),
            outputs: Vec::new(),
        })
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Names the project, which makes it permanent: orphan cleanup only ever takes unnamed projects.
    pub fn set_name(&mut self, name: &str) -> Result<(), ProjectError> {
        check_name("project name", name).map_err(|p| ProjectError::Name { reason: name_reason(&p) })?;
        self.name = Some(name.to_owned());
        Ok(())
    }

    /// Whether the project is a temporary one, which the user hasn't named.
    pub fn is_temporary(&self) -> bool {
        self.name.is_none()
    }

    pub fn created(&self) -> Timestamp {
        self.created
    }

    /// When the project was last saved.
    pub fn modified(&self) -> Timestamp {
        self.modified
    }

    /// The software that last saved the project, such as `FloraStack 0.1.0`.
    pub fn software(&self) -> Option<&str> {
        self.software.as_deref()
    }

    pub(crate) fn mark_saved(&mut self, at: Timestamp, software: String) {
        self.modified = at;
        self.software = Some(software);
    }

    fn allocate(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    /// Adds a frame at the end of the list, enabled.
    pub fn add_frame(&mut self, file: FrameFile, exif: Option<Value>) -> FrameId {
        let id = FrameId(self.allocate());
        self.frames.push(Frame {
            id,
            path: file.path,
            size: file.size,
            modified: file.modified,
            hash: file.hash,
            exif,
            enabled: true,
            alignment: None,
            aligned: None,
            preview: None,
            maps: Maps::new(),
        });
        id
    }

    pub fn frame(&self, id: FrameId) -> Option<&Frame> {
        self.frames.iter().find(|f| f.id == id)
    }

    pub fn frame_mut(&mut self, id: FrameId) -> Option<&mut Frame> {
        self.frames.iter_mut().find(|f| f.id == id)
    }

    /// Removes a frame and returns it. What refers to it (outputs, the alignment run) keeps the reference.
    pub fn remove_frame(&mut self, id: FrameId) -> Option<Frame> {
        let index = self.frames.iter().position(|f| f.id == id)?;
        Some(self.frames.remove(index))
    }

    /// Adds an output, created now.
    pub fn add_output(&mut self, new: NewOutput) -> OutputId {
        let id = OutputId(self.allocate());
        self.outputs.push(Output {
            id,
            name: new.name,
            method: new.method,
            parameters: new.parameters,
            created: Timestamp::now(),
            inputs: new.inputs,
            image: new.image,
            preview: new.preview,
            maps: new.maps,
            retouch: Vec::new(),
        });
        id
    }

    pub fn output(&self, id: OutputId) -> Option<&Output> {
        self.outputs.iter().find(|o| o.id == id)
    }

    pub fn output_mut(&mut self, id: OutputId) -> Option<&mut Output> {
        self.outputs.iter_mut().find(|o| o.id == id)
    }

    pub fn remove_output(&mut self, id: OutputId) -> Option<Output> {
        let index = self.outputs.iter().position(|o| o.id == id)?;
        Some(self.outputs.remove(index))
    }

    /// Adds an empty, visible retouch layer on top of an output's; `None` if there is no such output.
    pub fn add_layer(&mut self, output: OutputId, name: &str) -> Option<LayerId> {
        let id = LayerId(self.next_id);
        let output = self.outputs.iter_mut().find(|o| o.id == output)?;
        output.retouch.push(RetouchLayer { id, name: name.to_owned(), visible: true, strokes: Vec::new() });
        self.next_id = self.next_id.saturating_add(1);
        Some(id)
    }

    /// Every cache file the project refers to.
    pub fn cache_refs(&self) -> Vec<&CacheRef> {
        let mut refs: Vec<&CacheRef> = Vec::new();
        for f in &self.frames {
            refs.extend(f.aligned.iter().chain(&f.preview).chain(f.maps.values()));
        }
        refs.extend(self.maps.values());
        for o in &self.outputs {
            refs.extend(std::iter::once(&o.image).chain(&o.preview).chain(o.maps.values()));
        }
        refs
    }

    /// Checks what serde can't: ids, names and numbers. Run before every save and after every load.
    pub fn validate(&self) -> Result<(), Problem> {
        if let Some(name) = &self.name {
            check_name("project name", name)?;
        }
        let mut seen = BTreeSet::new();
        let mut id = |id: u64| -> Result<(), Problem> {
            if id == 0 {
                return Err(Problem::ZeroId);
            }
            if id >= self.next_id {
                return Err(Problem::IdNotAllocated { id, next: self.next_id });
            }
            if !seen.insert(id) {
                return Err(Problem::DuplicateId { id });
            }
            Ok(())
        };
        for f in &self.frames {
            id(f.id.0)?;
            check_frame_path(&f.path)?;
            if let Some(a) = &f.alignment {
                let finite = a.transform.m.iter().flatten().all(|v| v.is_finite()) && a.brightness_gain.is_finite() && a.quality.is_finite();
                if !finite {
                    return Err(Problem::NotFinite { what: "frame alignment" });
                }
            }
        }
        for o in &self.outputs {
            id(o.id.0)?;
            check_name("output name", &o.name)?;
            let method_ok = (1..=64).contains(&o.method.len())
                && o.method.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.');
            if !method_ok {
                return Err(Problem::Text { what: "output method", reason: "must be 1 to 64 lower-case letters, digits, '-' and '.'" });
            }
            for input in &o.inputs {
                if let Input::Output(other) = input
                    && other.0 >= o.id.0
                {
                    return Err(Problem::OutputOrder { output: o.id.0, input: other.0 });
                }
            }
            for layer in &o.retouch {
                id(layer.id.0)?;
                check_name("retouch layer name", &layer.name)?;
            }
        }
        self.preferences.validate()
    }
}

/// Checks a frame's stored path.
pub(crate) fn check_frame_path(path: &str) -> Result<(), Problem> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
        return Err(Problem::Text { what: "frame path", reason: "is empty, too long or holds a NUL character" });
    }
    Ok(())
}

fn name_reason(problem: &Problem) -> &'static str {
    match problem {
        Problem::Text { reason, .. } => reason,
        _ => "it isn't valid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(n: u8) -> FrameFile {
        FrameFile { path: format!("/photos/{n}.tif"), size: 10, modified: None, hash: ContentHash::from_bytes([n; 32]) }
    }

    #[test]
    fn ids_are_never_reused() {
        let mut p = Project::new(None).unwrap();
        let a = p.add_frame(file(1), None);
        let b = p.add_frame(file(2), None);
        assert_eq!(p.remove_frame(a).map(|f| f.id()), Some(a));
        let c = p.add_frame(file(3), None);
        assert!(a < b && b < c);
        assert_eq!(p.frames.iter().map(Frame::id).collect::<Vec<_>>(), [b, c]);
        assert_eq!(p.add_layer(OutputId(99), "Layer"), None, "no such output");
        p.validate().unwrap();
        assert_eq!(c.to_string(), "frame 3");
    }

    #[test]
    fn names_are_checked() {
        assert!(Project::new(Some("Bee head, 2026")).is_ok());
        for bad in ["", "   ", "tab\there", &"x".repeat(201)] {
            assert!(matches!(Project::new(Some(bad)), Err(ProjectError::Name { .. })), "{bad:?}");
        }
        let mut p = Project::new(None).unwrap();
        assert!(p.is_temporary());
        p.set_name("Coin").unwrap();
        assert!(!p.is_temporary() && p.name() == Some("Coin"));
        assert!(MapName::new("depth").is_ok() && MapName::new("dust-mask2").is_ok());
        for bad in ["", "Depth", "-x", "a/b", &"d".repeat(33)] {
            assert!(MapName::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn validation_catches_what_serde_cannot() {
        let mut p = Project::new(None).unwrap();
        let a = p.add_frame(file(1), None);
        p.frames.push(p.frames[0].clone());
        assert_eq!(p.validate(), Err(Problem::DuplicateId { id: a.get() }));
        p.frames.pop();
        p.frame_mut(a).unwrap().alignment = Some(Alignment { transform: Affine::IDENTITY, brightness_gain: f64::NAN, quality: 1.0 });
        assert_eq!(p.validate(), Err(Problem::NotFinite { what: "frame alignment" }));
        p.frame_mut(a).unwrap().alignment = None;
        p.next_id = 1;
        assert_eq!(p.validate(), Err(Problem::IdNotAllocated { id: 1, next: 1 }));
    }
}
