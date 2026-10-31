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
}

/// An output, as the Export tab shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputRow {
    pub id: u64,
    pub name: String,
    pub method: String,
    /// Its image, in the project's cache.
    pub image: PathBuf,
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
    /// The output a stacking job just made, for the viewer to show.
    pub made: Option<u64>,
    /// What a `project.findStacks` job just found, for the app to offer.
    pub stacks_found: Option<Value>,
    /// Projects opened or saved elsewhere since last asked, for the recent projects.
    pub opened: Vec<PathBuf>,
    /// The engine's compute device (`compute.info`), once asked.
    compute: Option<Value>,
    /// Whether the lists need reading again.
    stale: bool,
    /// Whether `filmstrip.update` was asked for the project open now.
    previews_asked: bool,
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
            made: None,
            stacks_found: None,
            opened: Vec::new(),
            compute: None,
            stale: true,
            previews_asked: false,
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

    /// Dispatches `name` with `params`: an instant command's or a query's result, or `None` for a job (which the top
    /// bar then follows) or a failure (which is shown).
    pub fn run(&mut self, name: &str, params: Value) -> Option<Value> {
        self.run_with(name, params, None)
    }

    /// [`Shell::run`] with calls of the same `coalesce` key, one after another, as one undo step (a slider's drag).
    pub fn run_with(&mut self, name: &str, params: Value, coalesce: Option<String>) -> Option<Value> {
        match self.engine.dispatch_with(name, params, DispatchOptions { coalesce }) {
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

    /// Brings the jobs' progress and results in and folds the engine's events; reads the lists again when the project
    /// changed. Returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = self.engine.poll() > 0;
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
                        self.previews_asked = false;
                    }
                    self.stale = true;
                }
                Event::Changed { .. } => self.stale = true,
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
        if job.state.is_finished() {
            match job.state {
                JobState::Failed => {
                    let message = job.error.as_ref().map_or_else(|| job.title.clone(), |e| e.message.clone());
                    self.notify(message, true);
                }
                JobState::Done if job.command == "stack.preview" => {
                    self.preview = job.result.as_ref().and_then(|r| r["path"].as_str()).map(PathBuf::from);
                }
                JobState::Done if job.command == "project.findStacks" => self.stacks_found.clone_from(&job.result),
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
            return;
        };
        let root = self.engine.project_path().map(std::path::Path::to_path_buf).unwrap_or_default();
        self.outputs = project
            .outputs
            .iter()
            .map(|o| OutputRow { id: o.id().get(), name: o.name.clone(), method: o.method.clone(), image: o.image.path.resolve(&root) })
            .collect();
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
                })
            })
            .collect();
        let stale = strip["stale"].as_u64().unwrap_or(0);
        if stale > 0 && !self.previews_asked {
            self.previews_asked = true;
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
