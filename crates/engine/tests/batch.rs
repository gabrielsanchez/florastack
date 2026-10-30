//! Batch/capture acceptance tests through the registry, using synthetic frame files and real slow writes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use florastack_engine::{DevelopSettings, Dispatched, Engine, Event, ReadOptions, branding, json};

fn preset(root: &Path) -> String {
    let path = root.join("preset.toml");
    std::fs::write(&path, "[stack.depth_map]\nthreshold = 'percentile'\npercentile = 42\n").unwrap();
    path.display().to_string()
}

#[test]
fn three_synthetic_stacks_have_projects_outputs_and_csv() {
    let dir = common::TempDir::new("batch-three");
    let input = dir.path().join("input");
    for (i, name) in ["bee", "flower", "wasp"].iter().enumerate() {
        common::synthetic_frames(&input, name, 80, 64, &common::breathing(3, 0.005, None, i as u64));
    }
    let mut engine = Engine::new(common::config(dir.path()));
    let result = engine
        .run(
            "batch.run",
            json!({"input": input, "output": dir.path().join("out"), "preset": preset(dir.path()), "method": "depth-map",
        "template": "{stack}-{method}-{range}-{date}-{params}.tif"}),
        )
        .unwrap();
    assert_eq!(result["failed"], 0, "{result}");
    assert_eq!(result["stacks"].as_array().unwrap().len(), 3);
    let csv = std::fs::read_to_string(result["summary"].as_str().unwrap()).unwrap();
    assert_eq!(csv.lines().count(), 4);
    assert!(csv.starts_with("stack,frames,method,time,warnings"));
    for entry in result["stacks"].as_array().unwrap() {
        assert_eq!(entry["frames"], 3);
        assert_eq!(entry["method"], "depth-map");
        assert!(entry["time"].as_f64().unwrap() > 0.0);
        let output = Path::new(entry["output"].as_str().unwrap());
        assert!(output.is_file());
        assert!(output.file_name().unwrap().to_str().unwrap().contains("percentile-42"));
        let mut reader = florastack_engine::frames::FrameReader::new(DevelopSettings::default(), ReadOptions::default());
        let image = reader.read(&std::fs::read(output).unwrap(), florastack_engine::Quality::Output).unwrap().image;
        assert!(image.as_raw().iter().all(|v| v.is_finite()));
        engine.run("project.open", json!({"path": entry["project"]})).unwrap();
        assert_eq!(engine.project().unwrap().frames.len(), 3);
        assert_eq!(engine.run("stack.get", json!({})).unwrap()["settings"]["depth_map"]["percentile"], 42.0);
        // Relocating the staged project must preserve references to input frames and cached results.
        engine.run("align.run", json!({})).unwrap();
        engine.run("project.close", json!({"discardChanges": true})).unwrap();
    }
    let before = std::fs::read(result["summary"].as_str().unwrap()).unwrap();
    assert!(engine.run("batch.run", json!({"input": input, "output": dir.path().join("out"), "preset": preset(dir.path())})).is_err());
    assert_eq!(std::fs::read(result["summary"].as_str().unwrap()).unwrap(), before);
}

#[test]
fn flat_folder_auto_splits_and_a_failed_entry_does_not_stop_the_queue() {
    let dir = common::TempDir::new("batch-split");
    let flat = dir.path().join("flat");
    common::synthetic_frames(dir.path(), "flat", 72, 56, &common::breathing(3, 0.0, None, 1));
    // A dimension change is an unambiguous boundary.
    let second = common::synthetic_frames(dir.path(), "second", 80, 64, &common::breathing(3, 0.0, None, 2));
    for (i, path) in second.iter().enumerate() {
        std::fs::copy(path, flat.join(format!("next-{i}.tif"))).unwrap();
    }
    let mut engine = Engine::new(common::config(dir.path()));
    let result = engine
        .run(
            "batch.run",
            json!({"input": flat, "output": dir.path().join("split"), "preset": preset(dir.path()),
        "split": {"minSimilarity": -1, "maxScaleStep": 50, "maxShift": 50}}),
        )
        .unwrap();
    assert_eq!(result["failed"], 0, "{result}");
    assert_eq!(result["stacks"].as_array().unwrap().len(), 2, "{result}");

    let input = dir.path().join("folders");
    common::synthetic_frames(&input, "a,good", 72, 56, &common::breathing(2, 0.0, None, 3));
    std::fs::create_dir(input.join("b-empty")).unwrap();
    common::synthetic_frames(&input, "c-good", 72, 56, &common::breathing(2, 0.0, None, 4));
    let result = engine.run("batch.run", json!({"input": input, "output": dir.path().join("partial"), "preset": preset(dir.path())})).unwrap();
    assert_eq!(result["failed"], 1);
    assert!(result["stacks"][0]["output"].is_string());
    assert!(result["stacks"][1]["error"].as_str().unwrap().contains("no frames"));
    assert!(result["stacks"][2]["output"].is_string());
}

#[test]
fn watch_retries_slow_files_and_publishes_incremental_previews() {
    let dir = common::TempDir::new("watch-slow");
    let source = common::synthetic_frames(dir.path(), "source", 80, 64, &common::breathing(3, 0.005, None, 8));
    let input = dir.path().join("capture");
    std::fs::create_dir(&input).unwrap();
    let bytes: Vec<_> = source.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let first = input.join("frame-0.tif");
    // Leave an incomplete TIFF unchanged longer than settleMs: stability alone must never admit it.
    std::fs::write(&first, bytes[0].get(..16).unwrap()).unwrap();
    let mut engine = Engine::new(common::config(dir.path()));
    let events = engine.subscribe();
    let Dispatched::Started(id) = engine
        .dispatch_json(
            "watch.run",
            &json!({"input": input, "output": dir.path().join("out"), "preset": preset(dir.path()),
        "settleMs": 80, "pollMs": 20, "idleSeconds": 3, "method": "depth-map"})
            .to_string(),
        )
        .unwrap()
    else {
        panic!("expected job")
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        engine.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(engine.job(id).unwrap().preview.is_none(), "truncated file must not enter a preview");
    let (preview_tx, preview_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        for (i, bytes) in bytes.iter().enumerate() {
            let path = input.join(format!("frame-{i}.tif"));
            let mut file = std::fs::File::create(&path).unwrap();
            for chunk in bytes.chunks(bytes.len().div_ceil(4)) {
                file.write_all(chunk).unwrap();
                file.flush().unwrap();
                std::thread::sleep(Duration::from_millis(45));
            }
            drop(file);
            // Synchronize the next capture with the observed preview, rather than assuming machine speed.
            assert_eq!(preview_rx.recv_timeout(Duration::from_secs(15)).unwrap(), i + 1);
        }
        std::fs::write(input.join("broken.tif"), b"not a complete image").unwrap();
        std::fs::write(input.join("ignored.part"), b"temporary camera file").unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut previews = Vec::new();
    let mut acknowledged = 0;
    while !engine.job(id).unwrap().state.is_finished() && Instant::now() < deadline {
        engine.poll();
        for event in events.try_iter() {
            if let Event::Job { job } = event
                && let Some(preview) = job.preview
            {
                previews.push(preview.frames);
                if preview.frames > acknowledged {
                    preview_tx.send(preview.frames).unwrap();
                    acknowledged = preview.frames;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if !engine.job(id).unwrap().state.is_finished() {
        engine.cancel(id);
        panic!("watch timed out");
    }
    writer.join().unwrap();
    let result = engine.wait(id).unwrap();
    assert_eq!(result["stacks"][0]["frames"], 3, "{result}");
    assert_eq!(result["failed"], 0, "{result}");
    assert!(previews.contains(&1) && previews.contains(&2) && previews.contains(&3), "{previews:?}");
    assert!(engine.job(id).unwrap().preview.as_ref().is_some_and(|p| Path::new(&p.path).is_file()));
    assert!(result["stacks"][0]["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("broken.tif")));
    engine.run("project.open", json!({"path": result["stacks"][0]["project"]})).unwrap();
    assert_eq!(engine.project().unwrap().frames.len(), 3);
    assert_eq!(engine.run("stack.get", json!({})).unwrap()["settings"]["depth_map"]["percentile"], 42.0);
    engine.run("project.close", json!({})).unwrap();
}

#[test]
fn watch_can_be_cancelled_before_the_first_frame_without_publishing() {
    let dir = common::TempDir::new("watch-cancel");
    let input = dir.path().join("capture");
    let output = dir.path().join("out");
    std::fs::create_dir(&input).unwrap();
    let mut engine = Engine::new(common::config(dir.path()));
    let Dispatched::Started(id) = engine.dispatch("watch.run", json!({"input": input, "output": output, "preset": preset(dir.path())})).unwrap()
    else {
        panic!("expected job")
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(engine.cancel(id));
    assert!(matches!(engine.wait(id), Err(florastack_engine::CommandError::Cancelled)));
    assert!(!output.join("summary.csv").exists());
    if output.exists() {
        assert!(!std::fs::read_dir(&output).unwrap().flatten().any(|e| e.path().is_dir()));
    }
    assert!(!output.join(format!("capture-capture{}", branding::PROJECT_EXTENSION)).exists());
}

#[test]
fn output_cannot_feed_back_into_the_capture() {
    let dir = common::TempDir::new("watch-recursion");
    let input = dir.path().join("capture");
    std::fs::create_dir(&input).unwrap();
    let mut engine = Engine::new(common::config(dir.path()));
    let error = engine.run("watch.run", json!({"input": input, "output": input.join("out"), "preset": preset(dir.path())})).unwrap_err();
    assert!(error.info().message.contains("outside"));
}
