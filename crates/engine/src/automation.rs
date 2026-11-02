//! Shared read-only resources and dispatch for automation hosts. Project mutations still go through the registry.

use base64::Engine as _;
use serde::Deserialize;

use crate::{CommandError, Engine, Invalid, JobId, Value, json};

/// Resource URIs are scoped to the currently open project, never arbitrary file paths.
pub const PROJECT_URI: &str = "florastack://project";

/// Handles the engine portion of the control protocol. Hosts handle `ui.*` themselves.
pub fn request(engine: &mut Engine, method: &str, params: Value) -> Result<Value, CommandError> {
    engine.poll();
    match method {
        "commands.list" => encode(engine.registry().list()),
        "command.dispatch" => {
            let p: Dispatch = decode(method, params)?;
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
