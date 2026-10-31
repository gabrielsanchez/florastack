//! Preference presets: TOML files that hold some of the project's preferences, applied on top of them, saved from
//! them, and read; and the built-in ones shipped with FloraStack ([`BUILT_IN`], `preset.list`).
//!
//! A preset's groups are checked with the types that own them before they are applied: `develop` with
//! florastack-raw's `DevelopSettings`, `align` with florastack-align's `AlignSettings`, `stack` with florastack-stack's
//! `StackSettings`, and `output` with the engine's `OutputSettings` (`commands::export`). A preset may hold any of the
//! groups (a partial preset), and `groups` applies or saves only some of them.

use florastack_project::{Edit, Group, Preferences, Preset, Value};

/// The built-in presets: each one's id and TOML file, which tests check like any preset.
pub const BUILT_IN: [(&str, &str); 4] = [
    ("insect-macro", include_str!("../../presets/insect-macro.toml")),
    ("product-shot", include_str!("../../presets/product-shot.toml")),
    ("landscape", include_str!("../../presets/landscape.toml")),
    ("microscope", include_str!("../../presets/microscope.toml")),
];

/// The prefix that names a built-in preset where a preset's path is expected (`batch.run`, `watch.run`).
pub const BUILT_IN_PREFIX: &str = "builtin:";

/// The preset `reference` names: a built-in one (`builtin:<id>`), or a file.
pub(crate) fn load(reference: &str) -> Result<Preset, CommandError> {
    match reference.strip_prefix(BUILT_IN_PREFIX) {
        Some(id) => built_in(id),
        None => Ok(Preset::load(&absolute(reference)?)?),
    }
}

/// The built-in preset `id`.
fn built_in(id: &str) -> Result<Preset, CommandError> {
    let (_, text) =
        BUILT_IN.iter().find(|(name, _)| *name == id).ok_or_else(|| CommandError::NotFound { what: format!("the built-in preset {id}") })?;
    Preset::parse(text).map_err(|source| {
        florastack_project::ProjectError::Preset { path: std::path::PathBuf::from(format!("{BUILT_IN_PREFIX}{id}")), source }.into()
    })
}
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{absolute, check_path, display};
use crate::error::{CommandError, Invalid};
use crate::registry::{CommandSpec, Effects, Meta, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::instant(
            Meta::new("preset.apply", "Apply preset", "Applies a preset, a file or a built-in one, on top of the project's preferences.")
                .help(
                    "Tables merge key by key and other values replace what was there, so presets layer: one for \
                     develop settings, another for output. `groups` applies only some of the preset's groups (a partial \
                     preset). Give `path` (a TOML file) or `builtin` (an id from `preset.list`). Every group is checked \
                     before anything changes.",
                )
                .undoable()
                .examples(&[
                    r#"{ "path": "C:/Presets/rail.toml" }"#,
                    r#"{ "path": "C:/Presets/insect.toml", "groups": ["develop"] }"#,
                    r#"{ "builtin": "insect-macro" }"#,
                ]),
            apply,
        ),
        CommandSpec::instant(
            Meta::new("preset.save", "Save preset", "Saves the project's preferences, or some groups of them, as a preset file.")
                .help(
                    "The file is TOML, written atomically. An existing file is replaced only with `overwrite`. By                      default it holds only the settings changed from their defaults, so that it layers over what a                      project has; with `complete`, every setting of its groups, the defaults included, so that                      applying it gives these settings whatever the project had.",
                )
                .requires(Requires::Project)
                .effects(Effects::Files)
                .examples(&[
                    r#"{ "path": "C:/Presets/bee.toml", "name": "Bee", "groups": ["develop"] }"#,
                    r#"{ "path": "C:/Presets/rail.toml", "name": "Rail", "complete": true }"#,
                ]),
            save,
        ),
        CommandSpec::query(
            Meta::new("preset.read", "Read preset", "Reads a preset file: its name, description and preferences.")
                .examples(&[r#"{ "path": "C:/Presets/rail.toml" }"#]),
            read,
        ),
        CommandSpec::query(
            Meta::new("preset.list", "Built-in presets", "The presets shipped with FloraStack: each one's id, name, description and groups.").help(
                "Apply one with `preset.apply` and its `builtin` id; in `batch.run` and `watch.run`, name it as the \
                 preset `builtin:<id>`.",
            ),
            list,
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
        Group::Align => {
            let settings: florastack_align::AlignSettings = preferences.get(Group::Align)?;
            settings.validate().map_err(|e| session.invalid("align", e.to_string()))
        }
        Group::Stack => {
            let settings: florastack_stack::StackSettings = preferences.get(Group::Stack)?;
            settings.validate().map_err(|e| session.invalid("stack", e.to_string()))
        }
        Group::Output => {
            let settings: super::export::OutputSettings = preferences.get(Group::Output)?;
            settings.validate().map_err(|e| session.invalid("output", e))
        }
    }
}

/// Applies a preset.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ApplyParams {
    /// The preset file (TOML).
    #[serde(default)]
    path: Option<String>,
    /// A built-in preset's id, as `preset.list` lists them, in place of `path`.
    #[serde(default)]
    builtin: Option<String>,
    /// The groups to apply; all of the preset's when left out.
    #[serde(default)]
    groups: Option<Vec<Group>>,
}

impl Params for ApplyParams {
    fn check(&self) -> Result<(), Invalid> {
        match (&self.path, &self.builtin) {
            (Some(path), None) => check_path("path", path),
            (None, Some(id)) if BUILT_IN.iter().any(|(name, _)| name == id) => Ok(()),
            (None, Some(_)) => Err(Invalid::new("builtin", format!("is none of {}", BUILT_IN.map(|(id, _)| id).join(", ")))),
            _ => Err(Invalid::new("path", "give either `path` or `builtin`")),
        }
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
    let preset = match (&p.path, &p.builtin) {
        (Some(path), _) => Preset::load(&absolute(path)?)?,
        (None, Some(id)) => built_in(id)?,
        (None, None) => return Err(session.invalid("path", "give either `path` or `builtin`")),
    };
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
    /// Save every setting of the groups, the defaults included, rather than only those changed from them.
    #[serde(default)]
    complete: bool,
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
    let chosen = p.groups.unwrap_or_else(|| Group::ALL.to_vec());
    let mut preferences = session.project()?.preferences.only(&chosen);
    if p.complete {
        preferences = complete(&preferences, &chosen)?;
    }
    let groups = Group::ALL.into_iter().filter(|&g| !preferences.group(g).is_empty()).collect();
    Preset { name: p.name, description: p.description, preferences }.save(&path)?;
    Ok(Written { path: display(&path), groups })
}

/// `groups` of `preferences` with every setting, the defaults filled in by the types that own them.
fn complete(preferences: &Preferences, groups: &[Group]) -> Result<Preferences, CommandError> {
    let mut full = Preferences::new();
    for &group in groups {
        match group {
            Group::Develop => full.set(group, &preferences.get::<florastack_raw::DevelopSettings>(group)?)?,
            Group::Align => full.set(group, &preferences.get::<florastack_align::AlignSettings>(group)?)?,
            Group::Stack => full.set(group, &preferences.get::<florastack_stack::StackSettings>(group)?)?,
            Group::Output => full.set(group, &preferences.get::<super::export::OutputSettings>(group)?)?,
        }
    }
    Ok(full)
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

/// A built-in preset.
#[derive(Debug, Serialize, JsonSchema)]
struct BuiltIn {
    /// Its id, for `preset.apply`'s `builtin`.
    id: &'static str,
    name: Option<String>,
    description: Option<String>,
    /// The groups it sets.
    groups: Vec<Group>,
}

/// The built-in presets.
#[derive(Debug, Serialize, JsonSchema)]
struct BuiltIns {
    presets: Vec<BuiltIn>,
}

fn list(_: &Session, _: crate::registry::NoParams) -> Result<BuiltIns, CommandError> {
    let presets = BUILT_IN
        .iter()
        .map(|(id, _)| {
            let preset = built_in(id)?;
            let groups = Group::ALL.into_iter().filter(|&g| !preset.preferences.group(g).is_empty()).collect();
            Ok(BuiltIn { id, name: preset.name, description: preset.description, groups })
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    Ok(BuiltIns { presets })
}
