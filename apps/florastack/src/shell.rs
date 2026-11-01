//! The app's side of the engine ([`Shell`]): it owns the [`Engine`] on the UI thread, dispatches the commands the UI
//! asks for, folds the engine's events into what the panels show (the jobs, the frame list, the outputs) and keeps
//! the messages to show the user. It decides nothing about the project: every change is a command.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use florastack_engine::{
    CommandError, DispatchOptions, Dispatched, Engine, EngineConfig, Event, JobId, JobInfo, JobState, ProjectEvent, Value, json,
};

/// A frame, as the frame list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameRow {
    pub id: u64,
    /// Its file's name.
    pub name: String,
    /// Its file, where the project looks for it.
    pub file: PathBuf,
    pub enabled: bool,
    /// What alignment flagged about it, such as `moved` or `out-of-focus` (and `limit` for any term's limit).
    pub flags: Vec<String>,
    /// Its preview as shot, a small JPEG in the project's cache, once made.
    pub preview: Option<PathBuf>,
    /// Its preview aligned onto the output's canvas, once aligned and made.
    pub aligned_preview: Option<PathBuf>,
    /// How its alignment went, once aligned (`align.info`).
    pub alignment: Option<Value>,
}

/// An output, as the Export tab shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputRow {
    pub id: u64,
    pub name: String,
    pub method: String,
    /// Its image, in the project's cache.
    pub image: PathBuf,
    /// The method's parameters it was made with.
    pub parameters: Value,
}

/// Something to tell the user: a result, or what went wrong.
#[derive(Clone, Debug)]
pub struct Notice {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

/// See the module documentation.
pub struct Shell {
    engine: Engine,
    events: Receiver<Event>,
    /// Jobs queued, running, or ended since they were last shown.
    pub jobs: BTreeMap<JobId, JobInfo>,
    pub frames: Vec<FrameRow>,
    pub outputs: Vec<OutputRow>,
    pub notices: Vec<Notice>,
    /// The latest preview stack (`stack.preview`'s file), until a project is opened or closed.
    pub preview: Option<PathBuf>,
    /// Advances when a running capture replaces the contents at its preview path.
    pub preview_revision: u64,
    /// The output a stacking job just made, for the viewer to show.
    pub made: Option<u64>,
    /// What a `project.findStacks` job just found, for the app to offer.
    pub stacks_found: Option<Value>,
    /// What the latest `stack.threshold` job showed: its picture, level and share below.
    pub threshold: Option<Value>,
    /// What an `export.run` job just wrote, for the app to tell.
    pub exported: Option<Value>,
    /// What the latest `compute.test` found, until the Preferences dialog opens again.
    pub tested: Option<Value>,
    /// The stacking settings (`stack.get`), the alignment settings (`align.get`) and the alignment (`align.info`), read
    /// with the lists.
    pub stack_settings: Value,
    pub align_settings: Value,
    pub alignment: Value,
    /// The export settings (`export.get`), read with the lists.
    pub export_settings: Value,
    /// What changed in the retouch session since the Retouch tab last drew it.
    pub retouch: Option<florastack_engine::retouch::RetouchChanges>,
    /// Projects opened or saved elsewhere since last asked, for the recent projects.
    pub opened: Vec<PathBuf>,
    /// The engine's compute device (`compute.info`), once asked.
    compute: Option<Value>,
    /// Whether the lists need reading again.
    stale: bool,
    /// Counts the project's changes, and the count when `filmstrip.update` was last asked for, which is asked for again
    /// only after the project changed (a failing update isn't asked for over and over).
    changes: u64,
    previews_asked: Option<u64>,
    /// Counts the times the lists were read again, so that what the panels keep from the project can tell it is old.
    pub revision: u64,
}

impl Shell {
    pub fn new(mut config: EngineConfig, waker: Option<florastack_engine::Waker>) -> Shell {
        config.waker = waker;
        let mut engine = Engine::new(config);
        let events = engine.subscribe();
        Shell {
            engine,
            events,
            jobs: BTreeMap::new(),
            frames: Vec::new(),
            outputs: Vec::new(),
            notices: Vec::new(),
            preview: None,
            preview_revision: 0,
            made: None,
            stacks_found: None,
            threshold: None,
            exported: None,
            tested: None,
            retouch: None,
            stack_settings: Value::Null,
            align_settings: Value::Null,
            alignment: Value::Null,
            export_settings: Value::Null,
            opened: Vec::new(),
            compute: None,
            stale: true,
            changes: 0,
            previews_asked: None,
            revision: 0,
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Tells the user something.
    pub fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.notices.push(Notice { text: text.into(), error, at: Instant::now() });
    }

    fn failed(&mut self, command: &str, e: &CommandError) {
        tracing::info!("{command} failed: {e}");
        self.notify(e.info().message, true);
    }

    /// Folds what changed in the retouch session into [`Shell::retouch`].
    fn take_retouch(&mut self) {
        if let Some(new) = self.engine.take_retouch_changes() {
            let old = self.retouch.take().unwrap_or_default();
            self.retouch = Some(florastack_engine::retouch::RetouchChanges {
                region: florastack_engine::retouch::union(old.region, new.region),
                source: old.source || new.source,
                opened: old.opened || new.opened,
                closed: (old.closed && !new.opened) || new.closed,
            });
        }
    }

    /// Dispatches `name` with `params`: an instant command's or a query's result, or `None` for a job (which the top
    /// bar then follows) or a failure (which is shown).
    pub fn run(&mut self, name: &str, params: Value) -> Option<Value> {
        self.run_with(name, params, None)
    }

    /// [`Shell::run`] with calls of the same `coalesce` key, one after another, as one undo step (a slider's drag).
    pub fn run_with(&mut self, name: &str, params: Value, coalesce: Option<String>) -> Option<Value> {
        let done = self.engine.dispatch_with(name, params, DispatchOptions { coalesce });
        self.take_retouch();
        match done {
            Ok(Dispatched::Done(value)) => Some(value),
            Ok(Dispatched::Started(job)) => {
                if let Some(info) = self.engine.job(job) {
                    self.jobs.insert(job, info);
                }
                None
            }
            Err(e) => {
                self.failed(name, &e);
                None
            }
        }
    }

    /// Whether a job of one of `commands` is queued or running.
    pub fn running(&self, commands: &[&str]) -> bool {
        self.jobs.values().any(|j| !j.state.is_finished() && commands.contains(&j.command.as_str()))
    }

    /// Whether a job is queued or running.
    pub fn busy(&self) -> bool {
        self.jobs.values().any(|j| !j.state.is_finished())
    }

    /// The compute device, asked of the engine the first time (which probes the GPU once per process).
    pub fn compute(&mut self) -> &Value {
        if self.compute.is_none() {
            self.compute = Some(self.engine.run("compute.info", json!({})).unwrap_or(Value::Null));
        }
        self.compute.as_ref().unwrap_or(&Value::Null)
    }

    /// Asks the engine for its compute device again next time: its preference changed.
    pub fn forget_compute(&mut self) {
        self.compute = None;
    }

    /// Brings the jobs' progress and results in and folds the engine's events; reads the lists again when the project
    /// changed. Returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = self.engine.poll() > 0;
        self.take_retouch();
        while let Ok(event) = self.events.try_recv() {
            changed = true;
            match event {
                Event::Job { job } => self.job_event(*job),
                Event::Project { state, path } => {
                    if matches!(state, ProjectEvent::Opened | ProjectEvent::Moved) {
                        self.opened.push(PathBuf::from(path));
                    }
                    if matches!(state, ProjectEvent::Opened | ProjectEvent::Closed) {
                        self.preview = None;
                        self.threshold = None;
                    }
                    self.changes += 1;
                    self.stale = true;
                }
                Event::Changed { .. } => {
                    self.changes += 1;
                    self.stale = true;
                }
                Event::History { .. } => {}
            }
        }
        if self.stale {
            self.refresh();
            self.stale = false;
            changed = true;
        }
        changed
    }

    fn job_event(&mut self, job: JobInfo) {
        if matches!(job.command.as_str(), "watch.run" | "capture.run")
            && let Some(preview) = &job.preview
        {
            self.preview = Some(PathBuf::from(&preview.path));
            if self.jobs.get(&job.id).and_then(|previous| previous.preview.as_ref()) != Some(preview) {
                self.preview_revision = self.preview_revision.saturating_add(1);
            }
        }
        if job.state.is_finished() {
            match job.state {
                JobState::Failed => {
                    let message = job.error.as_ref().map_or_else(|| job.title.clone(), |e| e.message.clone());
                    self.notify(message, true);
                }
                JobState::Done if job.command == "stack.preview" => {
                    self.preview = job.result.as_ref().and_then(|r| r["path"].as_str()).map(PathBuf::from);
                }
                JobState::Done if job.command == "capture.camera" => {
                    if let Some(path) = job.result.as_ref().and_then(|r| r["path"].as_str()) {
                        self.preview = Some(PathBuf::from(path));
                    }
                }
                JobState::Done if job.command == "project.findStacks" => self.stacks_found.clone_from(&job.result),
                JobState::Done if job.command == "stack.threshold" => self.threshold.clone_from(&job.result),
                JobState::Done if job.command == "export.run" => self.exported.clone_from(&job.result),
                JobState::Done if job.command == "compute.test" => {
                    self.tested.clone_from(&job.result);
                    self.compute = None;
                }
                JobState::Done if job.command.starts_with("stack.") => {
                    self.made = job.result.as_ref().and_then(|r| r["output"].as_u64());
                }
                _ => {}
            }
        }
        self.jobs.insert(job.id, job);
        // Ended jobs stay listed only while others run, so that the top bar shows the batch through to its end.
        if !self.busy() {
            self.jobs.retain(|_, j| !j.state.is_finished());
        }
    }

    /// Reads the frame list and the outputs again, and asks for the frames' missing previews once per project.
    fn refresh(&mut self) {
        self.revision += 1;
        let Some(project) = self.engine.project() else {
            self.frames.clear();
            self.outputs.clear();
            (self.stack_settings, self.align_settings, self.alignment) = (Value::Null, Value::Null, Value::Null);
            self.export_settings = Value::Null;
            return;
        };
        let root = self.engine.project_path().map(std::path::Path::to_path_buf).unwrap_or_default();
        self.outputs = project
            .outputs
            .iter()
            .map(|o| OutputRow {
                id: o.id().get(),
                name: o.name.clone(),
                method: o.method.clone(),
                image: o.image.path.resolve(&root),
                parameters: o.parameters.clone(),
            })
            .collect();
        self.stack_settings = self.engine.run("stack.get", json!({})).map_or(Value::Null, |v| v["settings"].clone());
        self.align_settings = self.engine.run("align.get", json!({})).map_or(Value::Null, |v| v["settings"].clone());
        self.alignment = self.engine.run("align.info", json!({})).unwrap_or(Value::Null);
        self.export_settings = self.engine.run("export.get", json!({})).map_or(Value::Null, |v| v["settings"].clone());
        let aligned: BTreeMap<u64, &Value> =
            self.alignment["run"]["frames"].as_array().into_iter().flatten().filter_map(|f| Some((f["frame"].as_u64()?, f))).collect();
        let listed = self.engine.run("project.frames", json!({})).unwrap_or(Value::Null);
        let strip = self.engine.run("filmstrip.list", json!({})).unwrap_or(Value::Null);
        let previews: BTreeMap<u64, &Value> =
            strip["frames"].as_array().into_iter().flatten().filter_map(|f| Some((f["frame"].as_u64()?, f))).collect();
        self.frames = listed["frames"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| {
                let id = f["id"].as_u64()?;
                let file = PathBuf::from(f["file"].as_str()?);
                let strip = previews.get(&id);
                Some(FrameRow {
                    id,
                    name: file.file_name().map_or_else(|| file.display().to_string(), |n| n.to_string_lossy().into_owned()),
                    file,
                    enabled: f["enabled"].as_bool().unwrap_or(true),
                    flags: f["flags"].as_array().into_iter().flatten().filter_map(flag_name).collect(),
                    preview: strip.and_then(|s| s["preview"].as_str()).map(PathBuf::from),
                    aligned_preview: strip.and_then(|s| s["alignedPreview"].as_str()).map(PathBuf::from),
                    alignment: aligned.get(&id).map(|&a| a.clone()),
                })
            })
            .collect();
        let stale = strip["stale"].as_u64().unwrap_or(0);
        let updating = self.jobs.values().any(|j| j.command == "filmstrip.update" && !j.state.is_finished());
        if stale > 0 && !updating && self.previews_asked != Some(self.changes) {
            self.previews_asked = Some(self.changes);
            self.run("filmstrip.update", json!({}));
        }
    }
}

/// A flag's name, as `project.frames` lists it: a string, or `{"limit": term}` for a term that reached its limit.
fn flag_name(flag: &Value) -> Option<String> {
    match flag {
        Value::String(name) => Some(name.clone()),
        Value::Object(map) => map.keys().next().cloned(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_named_whatever_their_shape() {
        assert_eq!(flag_name(&json!("out-of-focus")).as_deref(), Some("out-of-focus"));
        assert_eq!(flag_name(&json!({ "limit": "shift" })).as_deref(), Some("limit"));
        assert_eq!(flag_name(&json!(3)), None);
    }
}
