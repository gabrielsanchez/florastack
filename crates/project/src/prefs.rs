//! Processing preferences ([`Preferences`]) and the TOML preset files that hold any subset of them ([`Preset`]).
//!
//! Preferences come in groups ([`Group`]): `develop` (florastack-raw's `DevelopSettings`), `align` (alignment, order,
//! framing, dust and movement), `stack` (the method and its parameters, slabbing) and `output` (format, depth, profile,
//! metadata, file names). Each group is a table whose keys belong to the crate that defines it; this crate checks only
//! that the values are ones a TOML file can hold, and the engine reads each group with its crate's types, which give
//! every missing key its default and reject unknown ones.
//!
//! A preset may set only some keys, so presets layer: [`Preferences::layer`] puts one on top of another, tables
//! merging key by key at every depth and any other value replacing what was below. So a preset for alignment and one
//! for output can be applied one after the other, and a preset that sets `stack.pyramid.grit` leaves the other
//! pyramid parameters as they were. A preset can't remove a key; to go back to a default, it sets the default.
//!
//! A preset file is TOML, with an optional `[preset]` table for its name, description and format version:
//!
//! ```toml
//! [preset]
//! name = "Rail, steady light"
//! description = "Shift and scale only: no rotation, no brightness changes."
//! format = 1
//!
//! [align]
//! rotation = false
//! brightness = false
//! ```

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{PresetError, Problem, ProjectError};

/// A group of preferences, named in presets, project files and command parameters as [`Group::name`] says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Group {
    /// How RAW frames are developed (florastack-raw's `DevelopSettings`).
    Develop,
    /// Alignment, processing order, framing, dust and movement (florastack-align).
    Align,
    /// The stacking method, its parameters and slabbing (florastack-stack).
    Stack,
    /// What outputs are written as (format, depth, profile, metadata, file names).
    Output,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Develop, Group::Align, Group::Stack, Group::Output];

    /// The group's table name in presets and project files.
    pub fn name(self) -> &'static str {
        match self {
            Group::Develop => "develop",
            Group::Align => "align",
            Group::Stack => "stack",
            Group::Output => "output",
        }
    }
}

/// A group's table: keys and values as their crate serializes its settings.
pub type Table = Map<String, Value>;

/// The deepest nesting of tables and arrays in a preference value.
const MAX_DEPTH: usize = 32;
/// The longest key.
const MAX_KEY_CHARS: usize = 128;

/// Every processing preference, by group. See the module documentation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    #[serde(skip_serializing_if = "Map::is_empty")]
    develop: Table,
    #[serde(skip_serializing_if = "Map::is_empty")]
    align: Table,
    #[serde(skip_serializing_if = "Map::is_empty")]
    stack: Table,
    #[serde(skip_serializing_if = "Map::is_empty")]
    output: Table,
}

impl Preferences {
    pub fn new() -> Preferences {
        Preferences::default()
    }

    /// A group's table (empty when nothing in it is set).
    pub fn group(&self, group: Group) -> &Table {
        match group {
            Group::Develop => &self.develop,
            Group::Align => &self.align,
            Group::Stack => &self.stack,
            Group::Output => &self.output,
        }
    }

    /// Replaces a group's table, after checking that its values can be preferences, and returns the old one.
    pub(crate) fn replace_group(&mut self, group: Group, table: Table) -> Result<Table, Problem> {
        check_table(group.name(), &table, 0).map_err(|(key, reason)| Problem::Preference { key, reason })?;
        Ok(std::mem::replace(self.group_mut(group), table))
    }

    fn group_mut(&mut self, group: Group) -> &mut Table {
        match group {
            Group::Develop => &mut self.develop,
            Group::Align => &mut self.align,
            Group::Stack => &mut self.stack,
            Group::Output => &mut self.output,
        }
    }

    /// Whether no preference is set.
    pub fn is_empty(&self) -> bool {
        Group::ALL.iter().all(|&g| self.group(g).is_empty())
    }

    /// A group read as its crate's settings type, whose serde defaults fill the keys that aren't set.
    pub fn get<T: DeserializeOwned>(&self, group: Group) -> Result<T, ProjectError> {
        serde_json::from_value(Value::Object(self.group(group).clone())).map_err(|source| ProjectError::Data { what: group.name(), source })
    }

    /// Replaces a group with `settings`, which must serialize to a table. Keys whose value is `null` (unset options)
    /// are left out, so that reading the group back gives them their defaults.
    pub fn set<T: Serialize>(&mut self, group: Group, settings: &T) -> Result<(), ProjectError> {
        let value = serde_json::to_value(settings).map_err(|source| ProjectError::Data { what: group.name(), source })?;
        let Value::Object(mut table) = value else {
            return Err(ProjectError::Data { what: group.name(), source: serde::ser::Error::custom("the settings aren't a table") });
        };
        strip_nulls(&mut table, 0);
        check_table(group.name(), &table, 0).map_err(|(key, reason)| ProjectError::Data {
            what: group.name(),
            source: serde::ser::Error::custom(format!("the preference {key} {reason}")),
        })?;
        *self.group_mut(group) = table;
        Ok(())
    }

    /// Puts `top` on top of these preferences: tables merge key by key at every depth, and every other value of `top`
    /// replaces the one below.
    pub fn layer(&mut self, top: &Preferences) {
        for group in Group::ALL {
            merge(self.group_mut(group), top.group(group), 0);
        }
    }

    /// `layers` put on top of each other in order, the last on top.
    pub fn layered<'a>(layers: impl IntoIterator<Item = &'a Preferences>) -> Preferences {
        let mut out = Preferences::default();
        for layer in layers {
            out.layer(layer);
        }
        out
    }

    /// Only the given groups, for a partial preset (alignment only, output only).
    #[must_use]
    pub fn only(&self, groups: &[Group]) -> Preferences {
        let mut out = Preferences::default();
        for &group in groups {
            out.group_mut(group).clone_from(self.group(group));
        }
        out
    }

    /// Checks that every value can be written to a preset: no nulls (TOML has none, and a `nan` read from TOML turns
    /// into one), whole numbers that TOML's 64-bit integers hold, and no deeper nesting than 32.
    pub fn validate(&self) -> Result<(), Problem> {
        for group in Group::ALL {
            check_table(group.name(), self.group(group), 0).map_err(|(key, reason)| Problem::Preference { key, reason })?;
        }
        Ok(())
    }
}

fn strip_nulls(table: &mut Table, depth: usize) {
    table.retain(|_, v| !v.is_null());
    if depth < MAX_DEPTH {
        for v in table.values_mut() {
            if let Value::Object(inner) = v {
                strip_nulls(inner, depth + 1);
            }
        }
    }
}

fn merge(base: &mut Table, top: &Table, depth: usize) {
    for (key, value) in top {
        match (base.get_mut(key), value) {
            (Some(Value::Object(below)), Value::Object(above)) if depth < MAX_DEPTH => merge(below, above, depth + 1),
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Why a value at `path` can't be a preference, as (the key's dotted path, the reason).
fn check_value(path: &str, value: &Value, depth: usize) -> Result<(), (String, &'static str)> {
    if depth > MAX_DEPTH {
        return Err((path.to_owned(), "is nested too deeply"));
    }
    match value {
        Value::Null => Err((path.to_owned(), "has no value (TOML has no null, and nan or inf isn't a usable setting)")),
        // JSON numbers are finite; only whole numbers beyond i64 don't fit TOML.
        Value::Number(n) if n.is_u64() && n.as_i64().is_none() => Err((path.to_owned(), "is a whole number too large for a preset")),
        Value::Array(items) => items.iter().try_for_each(|item| check_value(path, item, depth + 1)),
        Value::Object(table) => check_table(path, table, depth + 1),
        _ => Ok(()),
    }
}

fn check_table(path: &str, table: &Table, depth: usize) -> Result<(), (String, &'static str)> {
    for (key, value) in table {
        let full = format!("{path}.{key}");
        if key.is_empty() || key.chars().count() > MAX_KEY_CHARS || key.chars().any(char::is_control) {
            return Err((full, "has an empty, overlong or unprintable name"));
        }
        // How the toml crate hands a date and time to serde: no preference is a date.
        if key.starts_with("$__toml_private") {
            return Err((path.to_owned(), "is a date, which no preference is"));
        }
        check_value(&full, value, depth)?;
    }
    Ok(())
}

/// The newest preset format this version reads and writes.
pub const PRESET_FORMAT: u32 = 1;
/// The largest preset file.
pub const MAX_PRESET_BYTES: u64 = 1 << 20;

/// A preset: some preferences, with a name and a description for the presets list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preset {
    pub name: Option<String>,
    pub description: Option<String>,
    pub preferences: Preferences,
}

/// A preset file as TOML has it.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetFile {
    #[serde(default)]
    preset: PresetInfo,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    develop: Table,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    align: Table,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    stack: Table,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    output: Table,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    format: Option<i64>,
}

impl Preset {
    /// Reads a preset from TOML text. An unknown group or key in `[preset]` is an error that names it and its line.
    pub fn parse(text: &str) -> Result<Preset, PresetError> {
        if text.len() as u64 > MAX_PRESET_BYTES {
            return Err(PresetError::TooLarge { size: text.len() as u64, max: MAX_PRESET_BYTES });
        }
        let file: PresetFile = toml::from_str(text).map_err(PresetError::Syntax)?;
        if let Some(found) = file.preset.format {
            if found < 1 {
                return Err(PresetError::Format { found });
            }
            if found > i64::from(PRESET_FORMAT) {
                return Err(PresetError::Newer { found, supported: PRESET_FORMAT });
            }
        }
        let preferences = Preferences { develop: file.develop, align: file.align, stack: file.stack, output: file.output };
        preferences.validate().map_err(|p| match p {
            Problem::Preference { key, reason } => PresetError::Value { key, reason },
            _ => PresetError::Value { key: String::new(), reason: "is not valid" },
        })?;
        Ok(Preset { name: file.preset.name, description: file.preset.description, preferences })
    }

    /// The preset as TOML text, with `format` set.
    pub fn to_toml(&self) -> Result<String, PresetError> {
        let p = &self.preferences;
        let file = PresetFile {
            preset: PresetInfo { name: self.name.clone(), description: self.description.clone(), format: Some(i64::from(PRESET_FORMAT)) },
            develop: p.develop.clone(),
            align: p.align.clone(),
            stack: p.stack.clone(),
            output: p.output.clone(),
        };
        toml::to_string(&file).map_err(PresetError::Encode)
    }

    /// Reads a preset file.
    pub fn load(path: &Path) -> Result<Preset, ProjectError> {
        let bytes = crate::fsutil::read_capped(path, MAX_PRESET_BYTES)?;
        let preset_error = |source| ProjectError::Preset { path: path.to_path_buf(), source };
        let text = String::from_utf8(bytes).map_err(|_| preset_error(PresetError::Value { key: String::new(), reason: "is not UTF-8 text" }))?;
        Preset::parse(&text).map_err(preset_error)
    }

    /// Writes the preset to `path`, replacing it whole: a failed write leaves the old file as it was.
    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        self.preferences.validate().map_err(|p| ProjectError::Preset {
            path: path.to_path_buf(),
            source: match p {
                Problem::Preference { key, reason } => PresetError::Value { key, reason },
                _ => PresetError::Value { key: String::new(), reason: "is not valid" },
            },
        })?;
        let text = self.to_toml().map_err(|source| ProjectError::Preset { path: path.to_path_buf(), source })?;
        crate::fsutil::write_atomic(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(text: &str) -> Preferences {
        Preset::parse(text).unwrap().preferences
    }

    #[test]
    fn groups_are_named_alike_everywhere() {
        for group in Group::ALL {
            assert_eq!(serde_json::to_value(group).unwrap(), Value::from(group.name()));
            assert_eq!(serde_json::from_value::<Group>(Value::from(group.name())).unwrap(), group);
        }
        assert!(serde_json::from_value::<Group>(Value::from("Develop")).is_err());
    }

    #[test]
    fn values_toml_cannot_hold_are_refused() {
        let mut p = Preferences::new();
        p.align.insert("limit".into(), Value::from(u64::MAX));
        assert!(matches!(p.validate(), Err(Problem::Preference { key, .. }) if key == "align.limit"));
        let mut p = Preferences::new();
        p.stack.insert("radius".into(), Value::Null);
        assert!(p.validate().is_err());
        let mut nested = Value::from(1);
        for _ in 0..40 {
            nested = Value::Array(vec![nested]);
        }
        let mut p = Preferences::new();
        p.output.insert("deep".into(), nested);
        assert!(p.validate().is_err());
        assert!(matches!(Preset::parse("[align]\nmax_shift = nan\n"), Err(PresetError::Value { key, .. }) if key == "align.max_shift"));
        assert!(matches!(Preset::parse("[stack]\nwhen = 1979-05-27T07:32:00Z\n"), Err(PresetError::Value { .. })));
    }

    #[test]
    fn set_and_get_use_the_owners_types() {
        #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct Align {
            rotation: bool,
            max_shift: f64,
            master: Option<u32>,
        }
        let mut p = prefs("[align]\nrotation = true\n");
        assert_eq!(p.get::<Align>(Group::Align).unwrap(), Align { rotation: true, ..Align::default() });
        p.set(Group::Align, &Align { rotation: false, max_shift: 0.1, master: None }).unwrap();
        assert!(!p.group(Group::Align).contains_key("master"), "unset options are left out");
        assert_eq!(p.get::<Align>(Group::Align).unwrap(), Align { rotation: false, max_shift: 0.1, master: None });
        let typo = prefs("[align]\nrotaton = false\n");
        assert!(matches!(typo.get::<Align>(Group::Align), Err(ProjectError::Data { what: "align", .. })), "a misspelled key is an error");
        assert!(p.set(Group::Stack, &3).is_err(), "settings must be a table");
    }
}
