//! AI selections (`adjust.model`, `adjust.setModel`, `adjust.select`, `adjust.describe`): Meta's SAM 3 model run on the
//! output open for adjusting, from clicks or a description, its selection kept with the mask as one undo step
//! (florastack-edit's `segment`). Not in the browser build.
//!
//! The model isn't FloraStack's: the user downloads its files (the `facebook/sam3` checkpoint on Hugging Face, under
//! Meta's SAM License) and names their folder once (`adjust.setModel`, which checks the weights' size and SHA-256);
//! the folder is remembered in the user's settings (`models.json`, beside the calibration profiles). The model loads
//! the first time a selection needs it, several gigabytes of memory, and stays loaded while an output is open for
//! adjusting (`adjust.close` lets it go). Rendering and exporting a selection never need it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use florastack_core::Monitor;
use florastack_edit::segment::{Prompt, Segmenter, WEIGHTS_SHA256, WEIGHTS_SIZE, check_model};
use florastack_edit::{Click, Component, Op, Shape};
use florastack_project::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
use crate::registry::{CommandSpec, Meta, NoParams, Params};
use crate::session::Session;

/// The model's license, which the user accepts where they download it.
pub const LICENSE_URL: &str = "https://github.com/facebookresearch/sam3/blob/main/LICENSE";
/// Where the model's files are.
pub const DOWNLOAD_URL: &str = "https://huggingface.co/facebook/sam3";

/// The model, loaded once and shared by the selection jobs; `None` until a selection needs it.
pub(crate) type SharedModel = Arc<Mutex<Option<Segmenter>>>;

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("adjust.model", "AI model", "Whether Meta's SAM 3 model, which AI selections need, is installed, and where.").help(
                "FloraStack doesn't ship the model: its files (`model.safetensors`, `vocab.json`, `merges.txt`) are \
                     the `facebook/sam3` checkpoint on Hugging Face, under Meta's SAM License, which you accept there \
                     before downloading them. Then `adjust.setModel` names their folder.",
            ),
            model,
        ),
        CommandSpec::job(
            Meta::new("adjust.setModel", "Set the AI model's folder", "Checks a folder of SAM 3's files and remembers it for AI selections.")
                .help(
                    "Checks that the folder holds `model.safetensors` (the official checkpoint: its size and SHA-256, \
                     which takes a while over 3.4 GB), `vocab.json` and `merges.txt`, then remembers it in the user's \
                     settings. The files stay where they are.",
                )
                .examples(&[r#"{ "folder": "C:/Users/me/Downloads/sam3" }"#]),
            Lane::Preview,
            set_model,
        ),
        CommandSpec::job(
            Meta::new(
                "adjust.select",
                "AI select",
                "Selects an object of the output open for adjusting from clicks on it, with the AI model, into a mask, as one undo step.",
            )
            .help(
                "The clicks (`at`: fractions of the output's width and height) are on the object, or, with `include` \
                     false, on what to leave out of it; send them all each time. The selection goes into the mask's \
                     first `object` shape, or a new one. The first selection on an output loads the model and reads \
                     the image, which takes tens of seconds on a CPU; later ones about a second. Needs the model \
                     (`adjust.model`).",
            )
            .undoable()
            .examples(&[
                r#"{ "mask": 1, "clicks": [ { "at": [0.48, 0.52] } ] }"#,
                r#"{ "mask": 1, "clicks": [ { "at": [0.48, 0.52] }, { "at": [0.7, 0.2], "include": false } ] }"#,
            ]),
            Lane::Heavy,
            select,
        ),
        CommandSpec::job(
            Meta::new(
                "adjust.describe",
                "AI describe",
                "Selects everything a description names in the output open for adjusting, with the AI model, into a mask, as one undo step.",
            )
            .help(
                "A short noun phrase works best: `insect`, `flower`, `the red berries`. The selection goes into the \
                     mask's first `described` shape, or a new one. Fails, changing nothing, when nothing matches. Needs \
                     the model (`adjust.model`).",
            )
            .undoable()
            .examples(&[r#"{ "mask": 1, "text": "insect" }"#]),
            Lane::Heavy,
            describe,
        ),
    ]
}

/// The model folder remembered in the user's settings, if any.
pub(crate) fn model_folder(session: &Session) -> Option<PathBuf> {
    let path = session.models_path.as_ref()?;
    if !florastack_core::fs::path_exists(path) {
        return None;
    }
    let bytes = super::calibration::read_capped(path, 64 << 10).ok()?;
    let settings: ModelSettings = serde_json::from_slice(&bytes).ok()?;
    settings.sam3.filter(|p| !p.as_os_str().is_empty())
}

/// The user's model settings (`models.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
struct ModelSettings {
    /// The SAM 3 model's folder.
    #[serde(default)]
    sam3: Option<PathBuf>,
}

/// Whether the model is installed, and where.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ModelInfo {
    /// Whether the folder named holds the model's files (their sizes; `adjust.setModel` checked their hash).
    installed: bool,
    /// The folder named, if any.
    folder: Option<String>,
    /// Why the folder named doesn't hold the model, when it doesn't.
    problem: Option<String>,
    /// Where the files come from, and their license.
    download: &'static str,
    license: &'static str,
    /// The files, and the weights' size in bytes.
    files: [&'static str; 3],
    bytes: u64,
}

fn model(session: &Session, _: NoParams) -> Result<ModelInfo, CommandError> {
    let folder = model_folder(session);
    let problem = folder.as_deref().map(check_model).and_then(Result::err).map(|e| e.to_string());
    Ok(ModelInfo {
        installed: folder.is_some() && problem.is_none(),
        folder: folder.map(|f| f.display().to_string()),
        problem,
        download: DOWNLOAD_URL,
        license: LICENSE_URL,
        files: ["model.safetensors", "vocab.json", "merges.txt"],
        bytes: WEIGHTS_SIZE,
    })
}

/// A model folder.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FolderParams {
    /// The folder holding `model.safetensors`, `vocab.json` and `merges.txt`.
    folder: String,
}

impl Params for FolderParams {
    fn check(&self) -> Result<(), Invalid> {
        super::check_path("folder", &self.folder)
    }
}

/// The model's folder, checked and remembered.
#[derive(Debug, Serialize, JsonSchema)]
struct ModelSet {
    folder: String,
}

fn set_model(session: &Session, p: FolderParams) -> Result<JobPlan<ModelSet>, CommandError> {
    let path = session.models_path.clone().ok_or_else(|| session.unavailable("the host has no user settings folder"))?;
    let folder = super::absolute(&p.folder)?;
    let job = JobPlan::new(move |ctx| {
        check_model(&folder)?;
        ctx.checkpoint()?;
        let weights = folder.join(florastack_edit::segment::WEIGHTS_FILE);
        let file = std::fs::File::open(&weights).map_err(|source| florastack_project::ProjectError::Read { path: weights.clone(), source })?;
        let (hash, _) = ContentHash::of_reader(Checked { inner: file, ctx })
            .map_err(|source| florastack_project::ProjectError::Read { path: weights.clone(), source })?;
        if hash.to_string() != format!("sha256:{WEIGHTS_SHA256}") {
            return Err(CommandError::Unavailable {
                command: "adjust.setModel".to_owned(),
                reason: format!("{} isn't the facebook/sam3 checkpoint (its SHA-256 is {hash}): download it again", weights.display()),
            });
        }
        if let Some(parent) = path.parent() {
            florastack_core::fs::create_dir_all(parent)
                .map_err(|source| florastack_project::ProjectError::Write { path: parent.to_path_buf(), source })?;
        }
        let bytes = serde_json::to_vec_pretty(&ModelSettings { sam3: Some(folder.clone()) })
            .map_err(|source| florastack_project::ProjectError::Data { what: "model settings", source })?;
        super::output::write_atomically(&path, &bytes)?;
        let set = ModelSet { folder: folder.display().to_string() };
        Ok(commit(move |session: &mut Session| {
            // A model loaded from another folder goes.
            *session.model.lock().unwrap_or_else(PoisonError::into_inner) = None;
            Ok(set)
        }))
    });
    Ok(job.stages(&[("Checking the model", 1.0)]))
}

/// A reader that stops when its job is cancelled: hashing 3.4 GB takes a while.
struct Checked<'a, R> {
    inner: R,
    ctx: &'a crate::jobs::JobContext,
}

impl<R: std::io::Read> std::io::Read for Checked<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.ctx.is_cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        self.inner.read(buf)
    }
}

/// Clicks on an object, into a mask.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SelectParams {
    /// The mask's id, as `adjust.get` lists it.
    mask: u32,
    /// The clicks, all of them, at most 64: on the object, or with `include` false on what to leave out of it.
    clicks: Vec<Click>,
}

impl Params for SelectParams {
    fn check(&self) -> Result<(), Invalid> {
        let shape = Shape::Object { clicks: self.clicks.clone(), selection: None };
        check_shape(&shape)?;
        if !self.clicks.iter().any(|c| c.include) {
            return Err(Invalid::new("clicks", "needs a click on what to select"));
        }
        Ok(())
    }
}

/// A description, into a mask.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DescribeParams {
    /// The mask's id, as `adjust.get` lists it.
    mask: u32,
    /// What to select, at most 200 characters: a short noun phrase.
    text: String,
}

impl Params for DescribeParams {
    fn check(&self) -> Result<(), Invalid> {
        check_shape(&Shape::Described { text: self.text.clone(), selection: None })
    }
}

fn check_shape(shape: &Shape) -> Result<(), Invalid> {
    let mask = florastack_edit::Mask { components: vec![Component { op: Op::Add, invert: false, shape: shape.clone() }], ..Default::default() };
    florastack_edit::Adjustments { masks: vec![mask], ..Default::default() }.validate().map_err(|e| match e {
        florastack_edit::EditError::Invalid { reason } => Invalid::new("adjustments", reason),
        other => Invalid::new("adjustments", other.to_string()),
    })
}

/// What was selected.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Selected {
    /// The mask, and the index of the shape the selection went into.
    mask: u32,
    component: usize,
    /// Whether the model had to load or read the image first (the slow part).
    loaded: bool,
}

/// Runs the model on the output open for adjusting for `prompt` and keeps its selection in mask `mask`, in the shape
/// `place` picks or adds.
fn run_model(session: &Session, command: &'static str, mask: u32, prompt: Prompted) -> Result<JobPlan<Selected>, CommandError> {
    let open = session.adjust.as_ref().ok_or_else(|| session.unavailable("no output is open for adjusting: open one with `adjust.open`"))?;
    let (output, renderer) = (open.output, Arc::clone(&open.renderer));
    let adjustments = crate::adjust::adjustments(session.project()?.output(output).ok_or(CommandError::NotFound { what: output.to_string() })?)?;
    if adjustments.mask(mask).is_none() {
        return Err(CommandError::NotFound { what: format!("mask {mask} on output {output}") });
    }
    let folder = model_folder(session).ok_or_else(|| {
        session.unavailable(format!(
            "AI selections need Meta's SAM 3 model, which FloraStack doesn't ship: download model.safetensors, vocab.json and merges.txt from {DOWNLOAD_URL} (accepting its license there), then name their folder with `adjust.setModel`"
        ))
    })?;
    let model = Arc::clone(&session.model);
    let job = JobPlan::new(move |ctx| {
        let mut guard = model.lock().unwrap_or_else(PoisonError::into_inner);
        let loaded = guard.is_none() || !guard.as_ref().is_some_and(|m| m.has_read(&renderer));
        if guard.is_none() {
            *guard = Some(Segmenter::load(&folder)?);
        }
        ctx.checkpoint()?;
        ctx.stage(1);
        let Some(segmenter) = guard.as_mut() else { return Err(CommandError::Internal { command: command.to_owned() }) };
        segmenter.read(&renderer)?;
        ctx.checkpoint()?;
        ctx.stage(2);
        let selection = match &prompt {
            Prompted::Clicks(clicks) => segmenter.select(&renderer, Prompt::Clicks(clicks))?,
            Prompted::Text(text) => segmenter.select(&renderer, Prompt::Text(text))?,
        };
        drop(guard);
        let Some(selection) = selection else {
            return Err(CommandError::Unavailable {
                command: command.to_owned(),
                reason: "nothing in the output matches the description".to_owned(),
            });
        };
        Ok(commit(move |session: &mut Session| {
            let mut component = 0;
            super::adjust::change_mask(session, output, mask, |m| {
                let (shape, fits): (Shape, fn(&Shape) -> bool) = match prompt {
                    Prompted::Clicks(clicks) => (Shape::Object { clicks, selection: Some(selection) }, |s| matches!(s, Shape::Object { .. })),
                    Prompted::Text(text) => (Shape::Described { text, selection: Some(selection) }, |s| matches!(s, Shape::Described { .. })),
                };
                component = if let Some(i) = m.components.iter().position(|c| fits(&c.shape)) {
                    i
                } else {
                    m.components.push(Component { op: Op::Add, invert: false, shape: Shape::Whole });
                    m.components.len() - 1
                };
                if let Some(c) = m.components.get_mut(component) {
                    c.shape = shape;
                }
            })?;
            Ok(Selected { mask, component, loaded })
        }))
    });
    Ok(job.stages(&[("Loading the AI model", 0.6), ("Reading the image", 0.35), ("Selecting", 0.05)]))
}

/// What the model is asked.
enum Prompted {
    Clicks(Vec<Click>),
    Text(String),
}

fn select(session: &Session, p: SelectParams) -> Result<JobPlan<Selected>, CommandError> {
    run_model(session, "adjust.select", p.mask, Prompted::Clicks(p.clicks))
}

fn describe(session: &Session, p: DescribeParams) -> Result<JobPlan<Selected>, CommandError> {
    run_model(session, "adjust.describe", p.mask, Prompted::Text(p.text.trim().to_owned()))
}
