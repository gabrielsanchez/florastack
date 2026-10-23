//! `project.json`: the project file's format, its versions and the migrations between them.
//!
//! # The file
//!
//! A JSON object that starts with `"format": "florastack-project"` and `"version"`, the format version, followed by
//! the [`Project`] (see its fields):
//!
//! ```json
//! {
//!   "format": "florastack-project",
//!   "version": 2,
//!   "software": "FloraStack 0.1.0",
//!   "name": "Bee head",
//!   "created": "2026-10-21T13:02:11.250Z",
//!   "modified": "2026-10-21T13:40:05Z",
//!   "next_id": 6,
//!   "frames": [ { "id": 1, "path": "...", "size": 25165824, "hash": "sha256:...", "enabled": true, ... } ],
//!   "develop": { "reference": 1, "settings": { ... }, "resolved": { ... } },
//!   "alignment": { "master": 1, "settings": { ... }, "report": { ... } },
//!   "preferences": { "develop": { ... }, "align": { ... }, "stack": { ... }, "output": { ... } },
//!   "maps": { "dust": { "path": "cache/dust-....tiff", "size": 1234, "hash": "sha256:..." } },
//!   "outputs": [ { "id": 4, "name": "...", "method": "pyramid", "parameters": { ... }, "created": "...",
//!                  "inputs": [ { "frame": 1 } ], "image": { ... }, "retouch": [ ... ] } ]
//! }
//! ```
//!
//! `format`, `version` and `name` keep their meaning in every version, so that any version of FloraStack can tell
//! a project file, its version and whether it is a named project (which orphan cleanup needs) from any other.
//!
//! # Versions and migrations
//!
//! [`VERSION`] is the format this build writes. Every change to the file's shape, a new field included, raises it and
//! adds a migration, a function from the JSON of one version to the next ([`MIGRATIONS`]); reading an older file runs
//! the chain from its version up, on the JSON tree, before the typed read. Because the chain works on JSON, it can
//! also rewrite the data that other crates own (develop settings, stroke records) when their shape changes: the
//! engine's tests pin those shapes, so that such a change can't go unnoticed. A file from a newer version is refused
//! with an error that says to update FloraStack, never read in part and saved back without what this build doesn't
//! know. An upgraded file is written in the current version at the next save, and the original is kept beside it
//! (`project.v0.json`) the first time.
//!
//! Version 0 is the draft layout the model was designed from (camelCase-free but flat: `file`, `sha256`, a `skip`
//! flag, a six-number `matrix`, Unix-second times and a single stroke list per output), never released; its
//! migration exists so that the chain is exercised, by a committed fixture, from the first release on. Version 2
//! adds what alignment reports beyond its transforms, an optional `report` on each frame and on the alignment run.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::error::{MigrationError, SchemaError};
use crate::hash::ContentHash;
use crate::model::Project;
use crate::time::Timestamp;

/// The `format` of every project file.
pub const FORMAT: &str = "florastack-project";
/// The format version this build writes, and the newest it reads.
pub const VERSION: u32 = 2;
/// The project file's name in the project folder.
pub const FILE_NAME: &str = "project.json";
/// The largest project file. Bulk data (pixels, focus statistics) lives in the cache; a project file this large is
/// not one.
pub const MAX_FILE_BYTES: u64 = 64 << 20;

/// A migration: the JSON of one version (the index in [`MIGRATIONS`]) to that of the next.
type Migration = fn(Map<String, Value>) -> Result<Map<String, Value>, MigrationError>;

/// The migrations, from version `i` to `i + 1`.
pub const MIGRATIONS: [Migration; VERSION as usize] = [v0::to_v1, v1::to_v2];

/// The `format` field: always [`FORMAT`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FormatTag;

impl Serialize for FormatTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(FORMAT)
    }
}

impl<'de> Deserialize<'de> for FormatTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<FormatTag, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text == FORMAT { Ok(FormatTag) } else { Err(serde::de::Error::custom(format!("the format is {text:?}, not {FORMAT:?}"))) }
    }
}

/// The `version` field: always [`VERSION`] once read (older files are migrated first).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct VersionTag;

impl Serialize for VersionTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(VERSION)
    }
}

impl<'de> Deserialize<'de> for VersionTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<VersionTag, D::Error> {
        let version = u64::deserialize(deserializer)?;
        if version == u64::from(VERSION) { Ok(VersionTag) } else { Err(serde::de::Error::custom(format!("the version is {version}, not {VERSION}"))) }
    }
}

/// What every version of the file says the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u64,
    pub name: Option<String>,
}

/// The fields of a project file that every version keeps; the others are skipped.
#[derive(Deserialize)]
struct RawHeader {
    #[serde(default)]
    format: Option<Value>,
    #[serde(default)]
    version: Option<Value>,
    #[serde(default)]
    name: Option<Value>,
}

/// The bytes without a UTF-8 byte-order mark, which some editors add.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// Reads the header of a project file of any version, newer ones included.
pub fn header(bytes: &[u8]) -> Result<Header, SchemaError> {
    let bytes = strip_bom(bytes);
    let raw: RawHeader = serde_json::from_slice(bytes).map_err(|e| match e.classify() {
        serde_json::error::Category::Data => SchemaError::NotAProject,
        _ => SchemaError::Syntax(e),
    })?;
    if raw.format.as_ref().and_then(Value::as_str) != Some(FORMAT) {
        return Err(SchemaError::NotAProject);
    }
    let version = raw.version.as_ref().and_then(Value::as_u64).ok_or(SchemaError::NoVersion)?;
    let name = match raw.name {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name),
        Some(_) => return Err(SchemaError::NotAProject),
    };
    Ok(Header { version, name })
}

/// A project read from its file, with the version it was upgraded from, if it was.
pub(crate) struct Decoded {
    pub project: Project,
    pub migrated_from: Option<u32>,
}

/// Reads a project file's bytes: checks its header, upgrades it to [`VERSION`] if it is older, reads it and checks
/// the result ([`Project::validate`]).
pub fn parse(bytes: &[u8]) -> Result<Project, SchemaError> {
    decode(bytes).map(|d| d.project)
}

/// [`parse`], also saying which version the file was upgraded from.
pub(crate) fn decode(bytes: &[u8]) -> Result<Decoded, SchemaError> {
    let bytes = strip_bom(bytes);
    let header = header(bytes)?;
    if header.version > u64::from(VERSION) {
        return Err(SchemaError::Newer { found: header.version, supported: VERSION });
    }
    let from = u32::try_from(header.version).map_err(|_| SchemaError::NoVersion)?;
    if from == VERSION {
        // Straight from the text, so that an error says where it is.
        let project: Project = serde_json::from_slice(bytes).map_err(|source| SchemaError::Content { version: VERSION, source })?;
        project.validate().map_err(SchemaError::Invalid)?;
        return Ok(Decoded { project, migrated_from: None });
    }
    let Value::Object(mut doc) = serde_json::from_slice(bytes).map_err(SchemaError::Syntax)? else {
        return Err(SchemaError::NotAProject);
    };
    for (version, migrate) in (from..VERSION).zip(MIGRATIONS.iter().skip(usize::try_from(from).unwrap_or(usize::MAX))) {
        doc = migrate(doc).map_err(|source| SchemaError::Migration { from: version, source })?;
    }
    let project: Project = serde_json::from_value(Value::Object(doc)).map_err(|source| SchemaError::Content { version: VERSION, source })?;
    project.validate().map_err(SchemaError::Invalid)?;
    Ok(Decoded { project, migrated_from: Some(from) })
}

/// Writes a project file, indented, in [`VERSION`].
pub(crate) fn encode(project: &Project) -> Result<Vec<u8>, SchemaError> {
    let mut bytes = serde_json::to_vec_pretty(project).map_err(SchemaError::Encode)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Taking a version's JSON apart, with errors that say where.
struct Fields {
    map: Map<String, Value>,
    at: String,
}

impl Fields {
    fn new(value: Value, at: String) -> Result<Fields, MigrationError> {
        match value {
            Value::Object(map) => Ok(Fields { map, at }),
            _ => Err(MigrationError { at, expected: "an object" }),
        }
    }

    fn error(&self, key: &str, expected: &'static str) -> MigrationError {
        MigrationError { at: format!("{}/{key}", self.at), expected }
    }

    fn take(&mut self, key: &str) -> Option<Value> {
        self.map.remove(key).filter(|v| !v.is_null())
    }

    fn required(&mut self, key: &str, expected: &'static str) -> Result<Value, MigrationError> {
        self.take(key).ok_or_else(|| self.error(key, expected))
    }

    fn string(&mut self, key: &str) -> Result<String, MigrationError> {
        match self.take(key) {
            Some(Value::String(s)) => Ok(s),
            _ => Err(self.error(key, "a string")),
        }
    }

    fn number(&mut self, key: &str) -> Result<Value, MigrationError> {
        match self.take(key) {
            Some(n @ Value::Number(_)) => Ok(n),
            _ => Err(self.error(key, "a number")),
        }
    }

    fn u64(&mut self, key: &str) -> Result<u64, MigrationError> {
        self.take(key).and_then(|v| v.as_u64()).ok_or_else(|| self.error(key, "a whole number"))
    }

    /// Unix seconds as a timestamp's text.
    fn seconds(&mut self, key: &str) -> Result<String, MigrationError> {
        let seconds = self.take(key).and_then(|v| v.as_i64()).ok_or_else(|| self.error(key, "a time in Unix seconds"))?;
        let ts = seconds.checked_mul(1000).and_then(Timestamp::from_unix_millis).ok_or_else(|| self.error(key, "a time in Unix seconds"))?;
        Ok(ts.to_string())
    }

    /// 64 hexadecimal digits as a content hash's text.
    fn sha256(&mut self, key: &str) -> Result<String, MigrationError> {
        let hex = self.string(key)?;
        let text = format!("sha256:{}", hex.to_ascii_lowercase());
        text.parse::<ContentHash>().map(|h| h.to_string()).map_err(|_| self.error(key, "64 hexadecimal digits"))
    }

    fn bool_or(&mut self, key: &str, default: bool) -> Result<bool, MigrationError> {
        match self.take(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(b),
            Some(_) => Err(self.error(key, "true or false")),
        }
    }

    fn array(&mut self, key: &str) -> Result<Vec<Value>, MigrationError> {
        match self.take(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => Ok(items),
            Some(_) => Err(self.error(key, "an array")),
        }
    }

    /// Fails if a key is left that the version doesn't have.
    fn finish(self) -> Result<(), MigrationError> {
        match self.map.keys().next() {
            Some(key) => Err(MigrationError { at: format!("{}/{key}", self.at), expected: "no such key in this format version" }),
            None => Ok(()),
        }
    }
}

/// Version 0 to version 1.
mod v0 {
    use serde_json::{Map, Value, json};

    use super::{FORMAT, Fields};
    use crate::error::MigrationError;

    /// Version 0, field by field:
    ///
    /// - top level: `format`, `version` (0), `name` (a string or null), `created` (Unix seconds), `develop` (the
    ///   develop settings), `frames`, `outputs`;
    /// - a frame: `file`, `bytes`, `sha256` (64 hex digits), `exif`, `skip` (default false) and `align`, with
    ///   `matrix` (six numbers, row by row), `gain` and `score`;
    /// - an output: `method`, `params`, `created` (Unix seconds), `file`, `bytes`, `sha256` (its pixels in the cache),
    ///   and `strokes`, its retouch strokes.
    ///
    /// Version 1 gives ids (frames first, in order, then outputs and their layers), a `next_id`, `enabled` for
    /// `skip`, an affine `transform` of two rows, timestamps as text, the develop settings under
    /// `preferences.develop`, a name for each output (`Output <n>`), its inputs (the frames that weren't skipped,
    /// which version 0 always stacked), its image as a cache reference, and its strokes as one retouch layer.
    pub(super) fn to_v1(doc: Map<String, Value>) -> Result<Map<String, Value>, MigrationError> {
        let mut top = Fields { map: doc, at: String::new() };
        top.take("format");
        top.take("version");
        let name = top.take("name");
        if name.as_ref().is_some_and(|n| !n.is_string()) {
            return Err(top.error("name", "a string or null"));
        }
        let created = top.seconds("created")?;
        let develop = top.take("develop");
        if develop.as_ref().is_some_and(|d| !d.is_object()) {
            return Err(top.error("develop", "an object"));
        }
        let mut next_id: u64 = 1;
        let mut frames = Vec::new();
        let mut enabled = Vec::new();
        for (i, frame) in top.array("frames")?.into_iter().enumerate() {
            let mut f = Fields::new(frame, format!("/frames/{i}"))?;
            let id = next_id;
            next_id += 1;
            let path = f.string("file")?;
            let size = f.u64("bytes")?;
            let hash = f.sha256("sha256")?;
            let exif = f.take("exif");
            let skip = f.bool_or("skip", false)?;
            let mut out = json!({ "id": id, "path": path, "size": size, "hash": hash, "enabled": !skip });
            if let Some(exif) = exif {
                out["exif"] = exif;
            }
            if let Some(align) = f.take("align") {
                let mut a = Fields::new(align, format!("/frames/{i}/align"))?;
                let matrix = a.required("matrix", "six numbers")?;
                let rows = matrix
                    .as_array()
                    .filter(|m| m.len() == 6 && m.iter().all(Value::is_number))
                    .and_then(|m| Some([m.get(..3)?.to_vec(), m.get(3..)?.to_vec()]))
                    .ok_or_else(|| a.error("matrix", "six numbers"))?;
                let transform = json!(rows);
                let gain = a.number("gain")?;
                let score = a.number("score")?;
                a.finish()?;
                out["alignment"] = json!({ "transform": transform, "brightness_gain": gain, "quality": score });
            }
            f.finish()?;
            if !skip {
                enabled.push(json!({ "frame": id }));
            }
            frames.push(out);
        }
        let mut outputs = Vec::new();
        for (j, output) in top.array("outputs")?.into_iter().enumerate() {
            let mut o = Fields::new(output, format!("/outputs/{j}"))?;
            let id = next_id;
            next_id += 1;
            let method = o.string("method")?;
            let parameters = o.take("params").unwrap_or_else(|| json!({}));
            let created = o.seconds("created")?;
            let image = json!({ "path": o.string("file")?, "size": o.u64("bytes")?, "hash": o.sha256("sha256")? });
            let strokes = o.array("strokes")?;
            o.finish()?;
            let mut out = json!({
                "id": id,
                "name": format!("Output {}", j + 1),
                "method": method,
                "parameters": parameters,
                "created": created,
                "inputs": enabled,
                "image": image,
            });
            if !strokes.is_empty() {
                out["retouch"] = json!([{ "id": next_id, "name": "Retouching", "visible": true, "strokes": strokes }]);
                next_id += 1;
            }
            outputs.push(out);
        }
        top.finish()?;
        let mut preferences = Map::new();
        if let Some(develop) = develop {
            preferences.insert("develop".into(), develop);
        }
        let out = json!({
            "format": FORMAT,
            "version": 1,
            "name": name,
            "created": created,
            "modified": created,
            "next_id": next_id,
            "frames": frames,
            "preferences": preferences,
            "outputs": outputs,
        });
        match out {
            Value::Object(map) => Ok(map),
            _ => Err(MigrationError { at: String::new(), expected: "an object" }),
        }
    }
}

/// Version 1 to version 2.
mod v1 {
    use serde_json::{Map, Value};

    use crate::error::MigrationError;

    /// Version 2 adds what alignment reports beyond its transforms: an optional `report` on each frame (its flags,
    /// such as out of focus, and its sharp-content fraction) and on the alignment run (the frames' size, the narrow end
    /// and the processing order), as the engine stores them. A version 1 file has neither, so only its version
    /// changes.
    #[allow(clippy::unnecessary_wraps)] // Every migration has the same signature, `Migration`.
    pub(super) fn to_v2(mut doc: Map<String, Value>) -> Result<Map<String, Value>, MigrationError> {
        doc.insert("version".into(), Value::from(2));
        Ok(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_1_file_is_upgraded_unchanged_but_for_its_version() {
        let v1 = br#"{"format": "florastack-project", "version": 1, "name": "Coin", "created": "2026-10-21T00:00:00Z",
            "modified": "2026-10-21T00:00:00Z", "next_id": 2,
            "frames": [{"id": 1, "path": "/a.tif", "size": 1, "hash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                        "enabled": true, "alignment": {"transform": [[1, 0, 0], [0, 1, 0]], "brightness_gain": 1.0, "quality": 1.0}}],
            "alignment": {"master": 1, "settings": {}}}"#;
        let decoded = decode(v1).unwrap();
        assert_eq!(decoded.migrated_from, Some(1));
        let p = decoded.project;
        assert_eq!((p.name(), p.frames.len(), p.frames[0].report.clone()), (Some("Coin"), 1, None));
        assert_eq!(p.alignment.unwrap().report, None);
    }

    #[test]
    fn headers_are_read_from_any_version() {
        let h = header(br#"{"format": "florastack-project", "version": 7, "name": "Coin", "frames": "whatever"}"#).unwrap();
        assert_eq!(h, Header { version: 7, name: Some("Coin".into()) });
        assert_eq!(header(br#"{"format": "florastack-project", "version": 0}"#).unwrap().name, None);
        assert!(matches!(header(b"[1, 2]"), Err(SchemaError::NotAProject)));
        assert!(matches!(header(br#"{"format": "other", "version": 1}"#), Err(SchemaError::NotAProject)));
        assert!(matches!(header(br#"{"format": "florastack-project", "version": -1}"#), Err(SchemaError::NoVersion)));
        assert!(matches!(header(br#"{"format": "florastack-project", "version": 1.5}"#), Err(SchemaError::NoVersion)));
        assert!(matches!(header(br#"{"format": "florastack-project", "version": 1, "name": 3}"#), Err(SchemaError::NotAProject)));
        assert!(matches!(header(br#"{"format": "florastack-project""#), Err(SchemaError::Syntax(_))));
    }

    #[test]
    fn a_migration_reports_where_the_old_file_is_wrong() {
        let doc = |text: &str| match serde_json::from_str::<Value>(text).unwrap() {
            Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        };
        let e = v0::to_v1(doc(r#"{"created": 0, "frames": [{"file": "a", "bytes": 1, "sha256": "xyz"}]}"#)).unwrap_err();
        assert_eq!(e, MigrationError { at: "/frames/0/sha256".into(), expected: "64 hexadecimal digits" });
        let e = v0::to_v1(doc(r#"{"created": 0, "extra": 1}"#)).unwrap_err();
        assert_eq!(e.at, "/extra");
        let e = v0::to_v1(doc(r#"{"created": "yesterday"}"#)).unwrap_err();
        assert_eq!(e.to_string(), "at /created: expected a time in Unix seconds");
        let hash = "ab".repeat(32);
        let frame = format!(r#"{{"file": "a", "bytes": 1, "sha256": "{hash}", "align": {{"matrix": [1, 0, 0, 0, 1], "gain": 1, "score": 1}}}}"#);
        let e = v0::to_v1(doc(&format!(r#"{{"created": 0, "frames": [{frame}]}}"#))).unwrap_err();
        assert_eq!(e.at, "/frames/0/align/matrix");
    }
}
