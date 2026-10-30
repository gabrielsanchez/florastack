//! Dispatch and presentation only: all image work and project edits belong to registry commands.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::Duration;

use florastack_engine::{CommandError, Dispatched, Engine, EngineConfig, ErrorInfo, Event, JobState, Value, json};
use serde::Serialize;

pub struct Failure {
    pub exit: u8,
    pub info: ErrorInfo,
}

impl Failure {
    pub fn new(exit: u8, code: &str, message: impl Into<String>) -> Self {
        Self { exit, info: ErrorInfo { code: code.into(), command: None, message: message.into() } }
    }
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(2, "params", message)
    }
}

impl From<CommandError> for Failure {
    fn from(error: CommandError) -> Self {
        let exit = match error.code() {
            "params" | "unknownCommand" => 2,
            "cancelled" => 130,
            _ => 1,
        };
        Self { exit, info: error.info() }
    }
}
impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::new(1, "io", error.to_string())
    }
}
impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::new(1, "json", error.to_string())
    }
}

pub fn path_text(path: &Path) -> Result<&str, Failure> {
    path.to_str().ok_or_else(|| Failure::usage("paths must be valid Unicode"))
}

pub fn print_json(value: &impl Serialize) -> Result<(), Failure> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

pub fn report_error(failure: &Failure, json_progress: bool) {
    let mut err = io::stderr().lock();
    if json_progress {
        let _ = serde_json::to_writer(&mut err, &json!({ "event": "error", "error": failure.info }));
        let _ = writeln!(err);
    } else {
        let _ = writeln!(err, "error: {}", failure.info.message);
    }
}

pub struct Client {
    pub engine: Engine,
    events: Receiver<Event>,
    wake: Receiver<()>,
    pub reporter: Reporter,
}

impl Client {
    pub fn new(temporary_projects: std::path::PathBuf, json_progress: bool) -> Self {
        let (tx, wake) = sync_channel(1);
        let config = EngineConfig {
            temporary_projects: Some(temporary_projects),
            waker: Some(Arc::new(move || {
                let _ = tx.try_send(());
            })),
            ..EngineConfig::default()
        };
        let mut engine = Engine::new(config);
        let events = engine.subscribe();
        Self { engine, events, wake, reporter: Reporter::new(json_progress) }
    }
    pub fn run(&mut self, name: &str, params: Value) -> Result<Value, Failure> {
        let dispatched = self.engine.dispatch(name, params);
        self.drain()?;
        self.finish(dispatched?)
    }
    pub fn run_json(&mut self, name: &str, params: &str) -> Result<Value, Failure> {
        let dispatched = self.engine.dispatch_json(name, params);
        self.drain()?;
        self.finish(dispatched?)
    }
    fn finish(&mut self, dispatched: Dispatched) -> Result<Value, Failure> {
        let id = match dispatched {
            Dispatched::Done(value) => return Ok(value),
            Dispatched::Started(id) => id,
        };
        loop {
            self.engine.poll();
            self.drain()?;
            if self.engine.job(id).is_none_or(|j| j.state.is_finished()) {
                let result = self.engine.wait(id);
                self.drain()?;
                return result.map_err(Into::into);
            }
            let _ = self.wake.recv_timeout(Duration::from_millis(50));
        }
    }
    fn drain(&mut self) -> Result<(), Failure> {
        for event in self.events.try_iter() {
            self.reporter.event(&event)?;
        }
        Ok(())
    }
}

pub struct Reporter {
    json: bool,
    terminal: bool,
    last_stage: Option<(u64, JobState, Option<String>)>,
    bar_visible: bool,
    line_width: usize,
}

impl Reporter {
    fn new(json: bool) -> Self {
        Self { json, terminal: io::stderr().is_terminal(), last_stage: None, bar_visible: false, line_width: 0 }
    }
    fn event(&mut self, event: &Event) -> Result<(), Failure> {
        let mut err = io::stderr().lock();
        if self.json {
            serde_json::to_writer(&mut err, event)?;
            writeln!(err)?;
            return Ok(());
        }
        let Event::Job { job } = event else { return Ok(()) };
        let stage = job.progress.stage.as_deref().unwrap_or("Working");
        if self.terminal {
            let fraction = job.progress.fraction.clamp(0.0, 1.0);
            let bar: String = (0..24).map(|i| if (i as f32) < fraction * 24.0 { '=' } else { ' ' }).collect();
            let eta = job.progress.eta_ms.map(|ms| format!(", ETA {}s", ms.div_ceil(1000))).unwrap_or_default();
            let ending = if job.state.is_finished() { format!(" ({:?})", job.state) } else { String::new() };
            let line = format!("[{bar}] {:3.0}% {}: {stage}{eta}{ending}", fraction * 100.0, job.title);
            // Carriage return and padding also work in Windows consoles without ANSI processing enabled.
            self.line_width = self.line_width.max(line.chars().count());
            write!(err, "\r{line:width$}", width = self.line_width)?;
            self.bar_visible = true;
            if job.state.is_finished() {
                writeln!(err)?;
                self.bar_visible = false;
                self.line_width = 0;
            }
        } else {
            let current = (job.id.0, job.state, job.progress.stage.clone());
            if self.last_stage.as_ref() != Some(&current) {
                writeln!(err, "{}: {} ({:?})", job.title, stage, job.state)?;
                self.last_stage = Some(current);
            }
        }
        err.flush()?;
        Ok(())
    }
    pub fn warning(&mut self, message: &str) -> Result<(), Failure> {
        self.clear_bar()?;
        let mut err = io::stderr().lock();
        if self.json {
            serde_json::to_writer(&mut err, &json!({ "event": "warning", "message": message }))?;
            writeln!(err)?;
        } else {
            writeln!(err, "warning: {message}")?;
        }
        Ok(())
    }
    pub fn clear_bar(&mut self) -> Result<(), Failure> {
        if self.bar_visible {
            writeln!(io::stderr().lock())?;
            self.bar_visible = false;
            self.line_width = 0;
        }
        Ok(())
    }
}
