//! The job engine: one heavy job at a time, previews beside it, newer previews cancelling older ones; cancellation at
//! the next checkpoint leaves the project exactly as it was; failures and panics change nothing; what a running job
//! depends on can't be edited; progress reaches the events. The jobs here are gated (they wait for the test at each
//! step), so that every test decides when each step happens.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::{Gate, TempDir, job_of, open_fixture, run, state, test_registry, until};
use florastack_engine::{CommandError, Engine, Event, InlineExecutor, JobState, Registry, json};
use florastack_project::{Area, ProjectFolder};

fn state_of(engine: &Engine, job: florastack_engine::JobId) -> JobState {
    engine.job(job).unwrap().state
}

#[test]
fn heavy_jobs_run_one_at_a_time_in_order() {
    let dir = TempDir::new("heavy");
    let gate = Gate::new();
    let (mut engine, _) = open_fixture(dir.path(), test_registry(&gate));
    let jobs: Vec<_> = (0..3).map(|i| job_of(engine.dispatch("test.gate", json!({ "steps": 2, "name": format!("job {i}") })).unwrap())).collect();
    assert_eq!(state_of(&engine, jobs[0]), JobState::Running);
    assert_eq!((state_of(&engine, jobs[1]), state_of(&engine, jobs[2])), (JobState::Queued, JobState::Queued));
    gate.open(6);
    for &job in &jobs {
        engine.wait(job).unwrap();
    }
    assert_eq!(gate.most.load(Ordering::SeqCst), 1, "never two heavy jobs at once");
    assert_eq!(engine.project().unwrap().name(), Some("job 2"), "committed in order");
    let history = run(&mut engine, "edit.history", "{}");
    assert_eq!(history["undo"].as_array().unwrap().len(), 3, "each commit is one step: {history}");
}

#[test]
fn previews_run_beside_a_heavy_job() {
    let dir = TempDir::new("previews");
    let gate = Gate::new();
    let (mut engine, _) = open_fixture(dir.path(), test_registry(&gate));
    let heavy = job_of(engine.dispatch("test.gate", json!({ "steps": 1 })).unwrap());
    let preview = job_of(engine.dispatch("test.preview", json!({ "steps": 1 })).unwrap());
    assert_eq!((state_of(&engine, heavy), state_of(&engine, preview)), (JobState::Running, JobState::Running));
    until(&mut engine, "both jobs at the gate", |_| gate.running.load(Ordering::SeqCst) == 2);
    gate.open(2);
    engine.wait(heavy).unwrap();
    engine.wait(preview).unwrap();
    assert_eq!(gate.most.load(Ordering::SeqCst), 2);
}

#[test]
fn a_newer_preview_cancels_the_older_one() {
    let dir = TempDir::new("supersede");
    let gate = Gate::new();
    let (mut engine, _) = open_fixture(dir.path(), test_registry(&gate));
    let older = job_of(engine.dispatch("test.preview", json!({ "steps": 3, "name": "older" })).unwrap());
    until(&mut engine, "the older preview at the gate", |_| gate.running.load(Ordering::SeqCst) == 1);
    let newer = job_of(engine.dispatch("test.preview", json!({ "steps": 1, "name": "newer" })).unwrap());
    assert!(engine.job(older).unwrap().cancel_requested);
    gate.open(4);
    assert!(matches!(engine.wait(older), Err(CommandError::Cancelled)));
    engine.wait(newer).unwrap();
    assert_eq!(engine.project().unwrap().name(), Some("newer"));
}

#[test]
fn a_cancelled_job_leaves_the_project_as_it_was() {
    let dir = TempDir::new("cancel");
    let gate = Gate::new();
    let (mut engine, fixture) = open_fixture(dir.path(), test_registry(&gate));
    let events = engine.subscribe();
    let before = state(&engine);
    let job = job_of(engine.dispatch("test.gate", json!({ "steps": 5, "writeCache": true, "lockFrames": true, "name": "changed" })).unwrap());
    gate.open(2);
    until(&mut engine, "two steps done", |e| e.job(job).unwrap().progress.done >= 2);
    assert!(engine.cancel(job));
    gate.open(3);
    assert!(matches!(engine.wait(job), Err(CommandError::Cancelled)));
    let info = engine.job(job).unwrap();
    assert_eq!(info.state, JobState::Cancelled);
    assert!(info.cancel_requested && info.result.is_none());
    // Nothing changed: the project, its history, its saved state.
    assert_eq!(state(&engine), before);
    assert!(!engine.is_dirty());
    assert_eq!(run(&mut engine, "edit.history", "{}")["undo"], json!([]));
    let got: Vec<Event> = events.try_iter().collect();
    assert!(!got.iter().any(|e| matches!(e, Event::Changed { .. } | Event::History { .. })), "no change was reported: {got:?}");
    // The cache files the job wrote are referenced by nothing, and go when the project is closed.
    let cache = fixture.path.join(Area::Cache.dir());
    let written = std::fs::read_dir(&cache).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("gated-")).count();
    assert!(written >= 2, "the job wrote cache files");
    let closed = run(&mut engine, "project.close", "{}");
    assert!(closed["cleaned"].as_u64().unwrap() >= 2, "{closed}");
    let (folder, project) = ProjectFolder::open(&fixture.path).unwrap();
    assert_eq!(folder.unreferenced(&project).unwrap(), Vec::<std::path::PathBuf>::new());
    assert!(folder.check_cache(&project, true).is_empty(), "what the project refers to is intact");
    project.validate().unwrap();
}

#[test]
fn a_queued_job_is_cancelled_at_once() {
    let dir = TempDir::new("queued");
    let gate = Gate::new();
    let (mut engine, fixture) = open_fixture(dir.path(), test_registry(&gate));
    let running = job_of(engine.dispatch("test.gate", json!({ "steps": 1 })).unwrap());
    let file = dir.path().join("out.tif");
    let spec = json!({ "format": { "format": "tiff", "compression": "none" }, "depth": "u8" });
    let save = job_of(engine.dispatch("output.save", json!({ "output": fixture.output.get(), "path": file, "spec": spec })).unwrap());
    assert_eq!(state_of(&engine, save), JobState::Queued);
    assert!(engine.cancel(save));
    assert_eq!(state_of(&engine, save), JobState::Cancelled);
    gate.open(1);
    engine.wait(running).unwrap();
    engine.poll();
    assert!(!file.exists(), "a cancelled save writes nothing");
    assert!(!engine.cancel(save), "it has ended");
}

#[test]
fn failed_and_panicking_jobs_change_nothing() {
    let dir = TempDir::new("failures");
    let gate = Gate::new();
    let (mut engine, _) = open_fixture(dir.path(), test_registry(&gate));
    let before = state(&engine);
    gate.open(10);
    let failed = job_of(engine.dispatch("test.gate", json!({ "steps": 3, "failAt": 1, "name": "x" })).unwrap());
    let e = engine.wait(failed).unwrap_err();
    assert!(matches!(e, CommandError::NotFound { .. }), "the job's own error: {e}");
    let info = engine.job(failed).unwrap();
    assert_eq!(info.state, JobState::Failed);
    assert_eq!(info.error.unwrap().code, "notFound");
    let panicked = job_of(engine.dispatch("test.gate", json!({ "steps": 3, "panicAt": 0, "name": "x" })).unwrap());
    let e = engine.wait(panicked).unwrap_err();
    assert_eq!(e.code(), "internal", "{e}");
    assert_eq!(state(&engine), before);
    assert!(!engine.is_dirty());
    // The engine goes on.
    run(&mut engine, "project.rename", r#"{ "name": "Still working" }"#);
}

#[test]
fn what_a_running_job_depends_on_cant_be_edited() {
    let dir = TempDir::new("conflict");
    let gate = Gate::new();
    let (mut engine, fixture) = open_fixture(dir.path(), test_registry(&gate));
    run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": [{}], "enabled": false }}"#, fixture.frames[0].get()));
    let job = job_of(engine.dispatch("test.gate", json!({ "steps": 1, "lockFrames": true })).unwrap());
    let before = state(&engine);
    let e = engine.dispatch("project.removeFrames", json!({ "frames": [fixture.frames[1].get()] })).unwrap_err();
    assert!(matches!(&e, CommandError::Conflict { job, .. } if job == "Gated job"), "{e}");
    assert_eq!(e.code(), "conflict");
    assert!(e.to_string().contains("frames"), "{e}");
    let e = engine.dispatch("edit.undo", json!({})).unwrap_err();
    assert_eq!(e.code(), "conflict", "undoing a frame edit is refused too: {e}");
    assert_eq!(state(&engine), before);
    // What the job doesn't depend on can be changed meanwhile.
    run(&mut engine, "develop.set", r#"{ "exposure": 1.0 }"#);
    run(&mut engine, "edit.undo", "{}");
    gate.open(1);
    engine.wait(job).unwrap();
    run(&mut engine, "project.removeFrames", &format!(r#"{{ "frames": [{}] }}"#, fixture.frames[1].get()));
}

#[test]
fn progress_reaches_the_events() {
    let dir = TempDir::new("progress");
    let gate = Gate::new();
    let (mut engine, _) = open_fixture(dir.path(), test_registry(&gate));
    let events = engine.subscribe();
    let job = job_of(engine.dispatch("test.gate", json!({ "steps": 4 })).unwrap());
    for step in 1..=4 {
        gate.open(1);
        until(&mut engine, "the next step", |e| e.job(job).unwrap().progress.done >= step);
    }
    engine.wait(job).unwrap();
    let progress: Vec<(f32, Option<String>, JobState)> = events
        .try_iter()
        .filter_map(|e| if let Event::Job { job } = e { Some((job.progress.fraction, job.progress.stage.clone(), job.state)) } else { None })
        .collect();
    assert!(progress.len() >= 5, "{progress:?}");
    assert!(progress.windows(2).all(|w| w[0].0 <= w[1].0), "the fraction never goes back: {progress:?}");
    let last = progress.last().unwrap();
    assert_eq!((last.0, last.1.as_deref(), last.2), (1.0, Some("Waiting"), JobState::Done));
    let json = serde_json::to_value(engine.job(job).unwrap()).unwrap();
    assert_eq!(json["progress"]["stages"], 1);
    assert_eq!(json["result"], json!({ "steps": 4 }));
}

#[test]
fn closing_a_project_stops_its_jobs_first() {
    let dir = TempDir::new("close");
    let gate = Gate::new();
    let (mut engine, _) = open_fixture(dir.path(), test_registry(&gate));
    let job = job_of(engine.dispatch("test.gate", json!({ "steps": 3, "writeCache": true, "name": "never" })).unwrap());
    // Enough permits for the job to reach its next checkpoint, where it sees the cancel.
    gate.open(3);
    run(&mut engine, "project.close", r#"{ "discardChanges": true }"#);
    assert!(engine.project().is_none());
    assert_eq!(state_of(&engine, job), JobState::Cancelled);
}

#[test]
fn the_inline_executor_runs_jobs_on_the_callers_thread() {
    let dir = TempDir::new("inline");
    let files = common::frame_files(dir.path(), 2);
    let mut engine = Engine::with_registry(common::config_with(dir.path(), Arc::new(InlineExecutor)), Registry::standard());
    run(&mut engine, "project.new", &format!(r#"{{ "name": "Inline", "folder": {} }}"#, serde_json::to_string(dir.path()).unwrap()));
    let job = job_of(engine.dispatch("project.addFrames", json!({ "paths": files })).unwrap());
    assert!(engine.poll() >= 3, "its messages were waiting: its stages, its progress, its end");
    assert_eq!(state_of(&engine, job), JobState::Done);
    assert_eq!(engine.project().unwrap().frames.len(), 2);
}
