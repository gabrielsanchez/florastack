//! Preference presets: TOML files that hold some of the project's preferences, applied on top of them, saved from
//! them, and read.
//!
//! A preset's groups are checked with the types of the crates that own them before they are applied. The groups that
//! no crate has settings for yet (`align`, `stack` and `output`, until their phases) can't be checked, so a preset that
//! sets them is refused rather than stored unchecked: the error names the group, and `groups` can leave it out.

use florastack_project::{Edit, Group, Preferences, Preset, Value};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{absolute, check_path, display};
use crate::error::{CommandError, Invalid};
use crate::registry::{CommandSpec, Effects, Meta, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::instant(
            Meta::new("preset.apply", "Apply preset", "Applies a preset file on top of the project's preferences.")
                .help(
                    "Tables merge key by key and other values replace what was there, so presets layer: one for \
                     develop settings, another for output. `groups` applies only some of the preset's groups. A group \
                     that this version can't check yet (align, stack and output have no settings yet) is refused.",
                )
                .undoable()
                .examples(&[r#"{ "path": "C:/Presets/rail.toml" }"#, r#"{ "path": "C:/Presets/insect.toml", "groups": ["develop"] }"#]),
            apply,
        ),
        CommandSpec::instant(
            Meta::new("preset.save", "Save preset", "Saves the project's preferences, or some groups of them, as a preset file.")
                .help("The file is TOML, written atomically. An existing file is replaced only with `overwrite`.")
                .requires(Requires::Project)
                .effects(Effects::Files)
                .examples(&[r#"{ "path": "C:/Presets/bee.toml", "name": "Bee", "groups": ["develop"] }"#]),
            save,
        ),
        CommandSpec::query(
            Meta::new("preset.read", "Read preset", "Reads a preset file: its name, description and preferences.")
                .examples(&[r#"{ "path": "C:/Presets/rail.toml" }"#]),
            read,
        ),
    ]
}

/// The groups whose settings this version can check: the ones some crate defines a type for.
fn check_group(session: &Session, group: Group, preferences: &Preferences) -> Result<(), CommandError> {
    match group {
        Group::Develop => {
            let settings: florastack_raw::DevelopSettings = preferences.get(Group::Develop)?;
            settings.validate().map_err(|e| session.invalid("develop", e.to_string()))
        }
        Group::Align | Group::Stack | Group::Output => Err(session.invalid(
            group.name(),
            format!(
                "can't be checked: this version of FloraStack has no {} settings yet, so a preset can't set them; apply its other groups with `groups`",
                group.name()
            ),
        )),
    }
}

/// Applies a preset.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ApplyParams {
    /// The preset file (TOML).
    path: String,
    /// The groups to apply; all of the preset's when left out.
    #[serde(default)]
    groups: Option<Vec<Group>>,
}

impl Params for ApplyParams {
    fn check(&self) -> Result<(), Invalid> {
        check_path("path", &self.path)
    }
}

/// The preset applied.
#[derive(Debug, Serialize, JsonSchema)]
struct Applied {
    /// Its name, if it has one.
    name: Option<String>,
    /// The groups it set.
    groups: Vec<Group>,
}

fn apply(session: &mut Session, p: ApplyParams) -> Result<Applied, CommandError> {
    let preset = Preset::load(&absolute(&p.path)?)?;
    let chosen = p.groups.unwrap_or_else(|| Group::ALL.to_vec());
    let top = preset.preferences.only(&chosen);
    let groups: Vec<Group> = Group::ALL.into_iter().filter(|&g| !top.group(g).is_empty()).collect();
    let mut merged = session.project()?.preferences.clone();
    merged.layer(&top);
    for &group in &groups {
        check_group(session, group, &merged)?;
    }
    let current = &session.project()?.preferences;
    let edits: Vec<Edit> = groups
        .iter()
        .filter(|&&g| current.group(g) != merged.group(g))
        .map(|&group| Edit::SetPreferences { group, table: merged.group(group).clone() })
        .collect();
    session.apply(Edit::Batch(edits))?;
    Ok(Applied { name: preset.name, groups })
}

/// Saves a preset.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SaveParams {
    /// The preset file to write (TOML).
    path: String,
    /// The groups to save; all the project sets when left out.
    #[serde(default)]
    groups: Option<Vec<Group>>,
    /// The preset's name, for the presets list.
    #[serde(default)]
    name: Option<String>,
    /// What it is for.
    #[serde(default)]
    description: Option<String>,
    /// Replace the file if it exists.
    #[serde(default)]
    overwrite: bool,
}

impl Params for SaveParams {
    fn check(&self) -> Result<(), Invalid> {
        check_path("path", &self.path)?;
        if let Some(name) = &self.name {
            super::check_name("name", name)?;
        }
        if self.description.as_ref().is_some_and(|d| d.chars().count() > 2000 || d.contains('\0')) {
            return Err(Invalid::new("description", "is longer than 2000 characters or holds a NUL character"));
        }
        Ok(())
    }
}

/// The preset written.
#[derive(Debug, Serialize, JsonSchema)]
struct Written {
    path: String,
    /// The groups it holds.
    groups: Vec<Group>,
}

fn save(session: &mut Session, p: SaveParams) -> Result<Written, CommandError> {
    let path = absolute(&p.path)?;
    if !p.overwrite && path.exists() {
        return Err(session.unavailable(format!("{} exists: pass `overwrite` to replace it", path.display())));
    }
    let preferences = session.project()?.preferences.only(&p.groups.unwrap_or_else(|| Group::ALL.to_vec()));
    let groups = Group::ALL.into_iter().filter(|&g| !preferences.group(g).is_empty()).collect();
    Preset { name: p.name, description: p.description, preferences }.save(&path)?;
    Ok(Written { path: display(&path), groups })
}

/// Reads a preset.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReadParams {
    /// The preset file (TOML).
    path: String,
}

impl Params for ReadParams {
    fn check(&self) -> Result<(), Invalid> {
        check_path("path", &self.path)
    }
}

/// A preset's contents.
#[derive(Debug, Serialize, JsonSchema)]
struct PresetInfo {
    name: Option<String>,
    description: Option<String>,
    /// Its preferences, by group, as the file has them.
    preferences: serde_json::Map<String, Value>,
}

fn read(_: &Session, p: ReadParams) -> Result<PresetInfo, CommandError> {
    let preset = Preset::load(&absolute(&p.path)?)?;
    let preferences = Group::ALL
        .into_iter()
        .filter(|&g| !preset.preferences.group(g).is_empty())
        .map(|g| (g.name().to_owned(), Value::Object(preset.preferences.group(g).clone())))
        .collect();
    Ok(PresetInfo { name: preset.name, description: preset.description, preferences })
}
