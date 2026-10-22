//! The RAW develop settings: one [`DevelopSettings`] per stack, a preference group of the project.

use std::borrow::Cow;

use florastack_project::{Edit, FrameId, Group, Preferences, Table};
use florastack_raw::{Chromaticity, DevelopSettings};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::CommandError;
use crate::registry::{CommandSpec, Meta, NoParams, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("develop.get", "Develop settings", "The develop settings of the stack's RAW frames, and how they were resolved.")
                .help(
                    "`settings` are the preferences with every key they don't set at its default. `resolved` is the \
                     development worked out on a reference frame (one white balance and one chromatic-aberration \
                     correction for every frame), when the project has one; `current` says whether it was resolved \
                     with these settings.",
                )
                .requires(Requires::Project),
            get,
        ),
        CommandSpec::instant(
            Meta::new("develop.set", "Set develop settings", "Changes some of the develop settings of the stack's RAW frames.")
                .help(
                    "Only the keys given change, as when a preset is applied: `{\"exposure\": 0.5}` changes the \
                     exposure and keeps the rest, and a nested table (`noise_reduction`) merges key by key. The \
                     result is checked whole. A misspelled key is an error. They apply to camera RAW and DNG frames; \
                     TIFF and other image files are imported unchanged.",
                )
                .undoable()
                .examples(&[
                    r#"{ "exposure": 0.5 }"#,
                    r#"{ "white_balance": { "custom": { "temperature": 5200, "tint": 3 } }, "noise_reduction": { "color": 0.3 } }"#,
                    r#"{ "white_balance": "auto", "demosaic": "fast", "lens_distortion": true }"#,
                ]),
            set,
        ),
        CommandSpec::instant(
            Meta::new("develop.reset", "Reset develop settings", "Puts every develop setting back at its default.").undoable(),
            reset,
        ),
    ]
}

/// The stack's development as resolved on its reference frame.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ResolvedInfo {
    /// The frame it was resolved on.
    reference: FrameId,
    /// Whether it was resolved with the settings in force now; if not, it is resolved again when frames are next read.
    current: bool,
    /// The white every frame is balanced for.
    white: Chromaticity,
    /// The red and blue planes' radial scales relative to green.
    chromatic_aberration: [f32; 2],
}

/// The develop settings.
#[derive(Debug, Serialize, JsonSchema)]
struct Develop {
    /// Every setting, with the defaults filled in.
    settings: DevelopSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved: Option<ResolvedInfo>,
}

fn get(session: &Session, _: NoParams) -> Result<Develop, CommandError> {
    let project = session.project()?;
    let settings = crate::project::develop_settings(project)?;
    let resolved = match &project.develop {
        None => None,
        Some(stored) => {
            let resolved: florastack_raw::Resolved = florastack_project::from_data("resolved development", &stored.resolved)?;
            Some(ResolvedInfo {
                reference: stored.reference,
                current: crate::project::develop(project)?.is_some(),
                white: resolved.white,
                chromatic_aberration: resolved.chromatic_aberration,
            })
        }
    };
    Ok(Develop { settings, resolved })
}

/// Some develop settings: any of [`DevelopSettings`]' keys, checked as such.
#[derive(Debug)]
pub(crate) struct DevelopPatch(Table);

impl<'de> Deserialize<'de> for DevelopPatch {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<DevelopPatch, D::Error> {
        let table = Table::deserialize(deserializer)?;
        // Every key, and the type of every value, is checked by the settings type itself.
        DevelopSettings::deserialize(serde_json::Value::Object(table.clone())).map_err(serde::de::Error::custom)?;
        Ok(DevelopPatch(table))
    }
}

impl JsonSchema for DevelopPatch {
    fn schema_name() -> Cow<'static, str> {
        DevelopSettings::schema_name()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        DevelopSettings::json_schema(generator)
    }
}

impl Params for DevelopPatch {}

/// The develop settings now.
#[derive(Debug, Serialize, JsonSchema)]
struct Settings {
    /// Every setting, with the defaults filled in.
    settings: DevelopSettings,
}

/// Sets the develop preferences to `table`, after checking the settings it makes; no step when nothing changes.
fn set_table(session: &mut Session, table: Table) -> Result<Settings, CommandError> {
    let mut preferences = Preferences::new();
    preferences.set(Group::Develop, &table)?;
    let settings: DevelopSettings = preferences.get(Group::Develop)?;
    settings.validate().map_err(|e| session.invalid("settings", e.to_string()))?;
    if session.project()?.preferences.group(Group::Develop) != preferences.group(Group::Develop) {
        session.apply(Edit::SetPreferences { group: Group::Develop, table: preferences.group(Group::Develop).clone() })?;
    }
    Ok(Settings { settings })
}

fn set(session: &mut Session, patch: DevelopPatch) -> Result<Settings, CommandError> {
    let mut top = Preferences::new();
    top.set(Group::Develop, &patch.0)?;
    let mut merged = session.project()?.preferences.only(&[Group::Develop]);
    merged.layer(&top);
    set_table(session, merged.group(Group::Develop).clone())
}

fn reset(session: &mut Session, _: NoParams) -> Result<Settings, CommandError> {
    set_table(session, Table::new())
}
