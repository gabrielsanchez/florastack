//! Shared read-only resources and dispatch for automation hosts. Project mutations still go through the registry.
//!
//! A host gives each connection an [`Access`] level, and the engine holds its clients to it by what every command
//! declares it may change ([`crate::registry::Effects`]): a client sees, in `commands.list`, and may dispatch only
//! the commands of its level. The level is the host's to choose (a flag of the desktop app's control channel and of
//! the MCP server), never the client's.

use base64::Engine as _;
use serde::Deserialize;

use crate::registry::Effects;
use crate::{CommandError, Engine, Invalid, JobId, Value, json};

/// How much an automation client may do, from least to most.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    /// Queries only: nothing changes, in the project or outside it.
    Read,
    /// Also the commands that change the open project or the session, and write in the project's own folder.
    Project,
    /// Every command, those that write files outside the project folder too: exports, presets, new projects, the
    /// trash.
    #[default]
    Full,
}

impl Access {
    pub const ALL: [Access; 3] = [Access::Read, Access::Project, Access::Full];

    /// The level's name, as the hosts' flags take it: `read`, `project` or `full`.
    pub fn name(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Project => "project",
            Access::Full => "full",
        }
    }

    /// The least level that may run a command with these effects.
    pub fn needed(effects: Effects) -> Access {
        match effects {
            Effects::None => Access::Read,
            Effects::Project => Access::Project,
            Effects::Files => Access::Full,
        }
    }

    /// Whether a client of this level may run a command with these effects.
    pub fn permits(self, effects: Effects) -> bool {
        self >= Access::needed(effects)
    }
}

impl std::str::FromStr for Access {
    type Err = String;

    fn from_str(text: &str) -> Result<Access, String> {
        Access::ALL.into_iter().find(|a| a.name() == text).ok_or_else(|| format!("`{text}` is no access level: use read, project or full"))
    }
}

impl std::fmt::Display for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Refuses a command that `access` doesn't allow. A name that is no command passes, for dispatch to say so.
fn permit(engine: &Engine, access: Access, command: &str) -> Result<(), CommandError> {
    let Some(info) = engine.registry().get(command) else { return Ok(()) };
    if access.permits(info.effects) {
        return Ok(());
    }
    let changes = match info.effects {
        Effects::None => "nothing",
        Effects::Project => "the open project",
        Effects::Files => "files outside the project",
    };
    Err(CommandError::Forbidden { command: info.name.to_owned(), changes, access: access.name(), needs: Access::needed(info.effects).name() })
}

/// Resource URIs are scoped to the currently open project, never arbitrary file paths.
pub const PROJECT_URI: &str = "florastack://project";

/// Handles the engine portion of the control protocol for a client of `access`. Hosts handle `ui.*` themselves.
pub fn request(engine: &mut Engine, access: Access, method: &str, params: Value) -> Result<Value, CommandError> {
    engine.poll();
    match method {
        "commands.list" => encode(engine.registry().list().into_iter().filter(|info| access.permits(info.effects)).collect::<Vec<_>>()),
        "command.dispatch" => {
            let p: Dispatch = decode(method, params)?;
            permit(engine, access, &p.command)?;
            engine.dispatch(&p.command, p.params).map(|d| match d {
                crate::Dispatched::Done(result) => json!({"result": result}),
                crate::Dispatched::Started(job) => json!({"job": job}),
            })
        }
        "job.get" => {
            let p: Job = decode(method, params)?;
            encode(engine.job(p.job).ok_or_else(|| CommandError::NotFound { what: p.job.to_string() })?)
        }
        "resources.list" => {
            let mut resources = vec![json!({"uri": PROJECT_URI, "name": "Project state", "mimeType": "application/json"})];
            if let Some(project) = engine.project() {
                resources.extend(
                    project
                        .outputs
                        .iter()
                        .filter(|o| o.preview.is_some())
                        .map(|o| json!({"uri": thumbnail_uri(o.id().get()), "name": format!("{} thumbnail", o.name), "mimeType": "image/jpeg"})),
                );
            }
            Ok(json!({"resources": resources}))
        }
        "resources.read" => {
            let p: ReadResource = decode(method, params)?;
            read_resource(engine, &p.uri)
        }
        _ => Err(CommandError::NotFound { what: format!("automation method `{method}`") }),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dispatch {
    command: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    job: JobId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadResource {
    uri: String,
}

/// Checks protocol-specific parameters with the same errors as registry commands.
pub fn decode<T: serde::de::DeserializeOwned>(method: &str, params: Value) -> Result<T, CommandError> {
    serde_json::from_value(params).map_err(|source| CommandError::Params { command: method.to_owned(), source })
}

fn encode(value: impl serde::Serialize) -> Result<Value, CommandError> {
    serde_json::to_value(value).map_err(|_| CommandError::Internal { command: "automation".into() })
}

fn thumbnail_uri(id: u64) -> String {
    format!("florastack://outputs/{id}/thumbnail")
}

fn read_resource(engine: &Engine, uri: &str) -> Result<Value, CommandError> {
    if uri == PROJECT_URI {
        let state = json!({"project": engine.project(), "path": engine.project_path(), "dirty": engine.is_dirty(), "jobs": engine.jobs()});
        return Ok(json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": state.to_string()}]}));
    }
    let not_found = || CommandError::NotFound { what: format!("resource `{uri}`") };
    let project = engine.project().ok_or_else(not_found)?;
    let output = project.outputs.iter().find(|o| thumbnail_uri(o.id().get()) == uri).ok_or_else(not_found)?;
    let preview = output.preview.as_ref().ok_or_else(not_found)?;
    // CacheDir verifies the reference and its hash; a URI cannot escape the cache.
    let folder = engine.session().folder()?;
    if preview.size > 4 << 20 {
        return Err(CommandError::Invalid { command: "resources.read".into(), source: Invalid::new("thumbnail", "exceeds 4 MiB") });
    }
    let bytes = folder.read(preview)?;
    let blob = base64::prelude::BASE64_STANDARD.encode(bytes);
    Ok(json!({"contents": [{"uri": uri, "mimeType": "image/jpeg", "blob": blob}]}))
}
