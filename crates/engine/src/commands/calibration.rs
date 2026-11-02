//! Objective/magnification calibrations in the user's settings folder, shared by their projects.

use std::io::Read;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{CommandError, Invalid};
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params};
use crate::session::Session;

/// A measured lateral calibration in output pixels per micrometer, for a named objective/magnification.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Profile {
    /// Objective or magnification name (up to 200 characters).
    pub name: String,
    /// Pixels per micrometer in the master image used by the output canvas, from 1e-9 to 1e9.
    #[schemars(range(min = 1e-9, max = 1e9))]
    pub pixels_per_micrometer: f64,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Profiles {
    /// The full library, at most 64 uniquely named calibrations. An empty list removes the library.
    profiles: Vec<Profile>,
}

impl Params for Profiles {
    fn check(&self) -> Result<(), Invalid> {
        if self.profiles.len() > 64 {
            return Err(Invalid::new("profiles", "has more than 64 profiles"));
        }
        let mut names = std::collections::BTreeSet::new();
        for p in &self.profiles {
            super::check_name("name", &p.name)?;
            if !p.pixels_per_micrometer.is_finite() || !(1e-9..=1e9).contains(&p.pixels_per_micrometer) {
                return Err(Invalid::new("pixelsPerMicrometer", "must be finite and from 1e-9 to 1e9"));
            }
            if !names.insert(&p.name) {
                return Err(Invalid::new("profiles", "has duplicate names"));
            }
        }
        Ok(())
    }
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(Meta::new("calibration.get", "Calibration profiles", "Lists the user's objective and magnification calibrations."), get),
        CommandSpec::instant(
            Meta::new("calibration.set", "Save calibration profiles", "Replaces the user's library of calibration profiles.")
                .help("The library is saved as calibrations.json beside the host's temporary-projects folder, independently of projects and presets. Select a profile in the Export tab to copy its value into that project's export settings. Editing the library then leaves existing measurements reproducible.")
                .effects(Effects::Files).examples(&[r#"{"profiles": [{"name": "10x", "pixelsPerMicrometer": 2.5}]}"#]),
            set,
        ),
    ]
}

pub(crate) fn read_capped(path: &Path, maximum: u64) -> Result<Vec<u8>, CommandError> {
    let error = |source| florastack_project::ProjectError::Read { path: path.to_path_buf(), source };
    let file = std::fs::File::open(path).map_err(error)?;
    if file.metadata().map_err(error)?.len() > maximum {
        return Err(super::scientific::unavailable(format!("{} exceeds the {maximum}-byte limit", path.display())));
    }
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1)).read_to_end(&mut bytes).map_err(error)?;
    if bytes.len() as u64 > maximum {
        return Err(super::scientific::unavailable("settings file is too large"));
    }
    Ok(bytes)
}

fn get(session: &Session, _: NoParams) -> Result<Profiles, CommandError> {
    let Some(path) = &session.calibration_path else { return Ok(Profiles::default()) };
    if !path.exists() {
        return Ok(Profiles::default());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&read_capped(path, 1 << 20)?).map_err(|source| session.invalid("profiles", source.to_string()))?;
    let profiles: Profiles = florastack_project::from_data("calibration profiles", &value)?;
    profiles.check().map_err(|e| session.invalid(e.field, e.reason))?;
    Ok(profiles)
}

fn set(session: &mut Session, p: Profiles) -> Result<Profiles, CommandError> {
    let path = session.calibration_path.as_ref().ok_or_else(|| session.unavailable("the host has no user settings folder"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| florastack_project::ProjectError::Write { path: parent.to_path_buf(), source })?;
    }
    let bytes = serde_json::to_vec_pretty(&p).map_err(|e| session.invalid("profiles", e.to_string()))?;
    super::output::write_atomically(path, &bytes)?;
    Ok(p)
}
