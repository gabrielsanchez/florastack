//! Exercise the real executable against a small synthetic focus sweep and saved projects.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use florastack_engine::{DevelopSettings, Quality, ReadOptions, Registry, Value, branding, json};
use tempfile::TempDir;

fn cli(dir: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!());
    command.current_dir(dir).args(["--temporary-projects"]).arg(dir.join("temporary projects"));
    command
}

fn json_output(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn lines(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes).lines().map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"))).collect()
}

fn png_file(path: &Path, width: u32, height: u32, channels: png::ColorType, pixels: &[u8]) {
    let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), width, height);
    encoder.set_color(channels);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    encoder.write_header().unwrap().write_image_data(pixels).unwrap();
}

/// Four depth bands with a common low-frequency texture, and fine detail that comes into focus one band at a time.
fn frames(dir: &Path) -> Vec<PathBuf> {
    let folder = dir.join("input photos");
    std::fs::create_dir(&folder).unwrap();
    (0..4)
        .map(|focus| {
            let mut pixels = Vec::new();
            for y in 0..72_u32 {
                for x in 0..96_u32 {
                    let texture = (x * 7 + y * 13 + (x * y) % 29) % 60;
                    let detail = if x / 24 == focus { (x + y) % 2 * 65 } else { 32 };
                    let sample = u8::try_from(60 + texture + detail).unwrap();
                    pixels.extend_from_slice(&[sample, sample.saturating_add(7), sample.saturating_sub(9)]);
                }
            }
            let path = folder.join(format!("focus {focus}.png"));
            png_file(&path, 96, 72, png::ColorType::Rgb, &pixels);
            path
        })
        .collect()
}

fn check_image(path: &Path) {
    let mut reader = florastack_engine::frames::FrameReader::new(DevelopSettings::default(), ReadOptions::default());
    let frame = reader.read(&std::fs::read(path).unwrap(), Quality::Output).unwrap();
    assert!(frame.image.width() >= 64 && frame.image.height() >= 48);
    assert!(frame.image.as_raw().iter().all(|v| v.is_finite()));
    assert!(frame.image.as_raw().iter().any(|&v| v > 0.1));
}

fn check_depth(path: &Path, float: bool) {
    let mut decoder = tiff::decoder::Decoder::new(std::fs::File::open(path).unwrap()).unwrap();
    match decoder.read_image().unwrap() {
        tiff::decoder::DecodingResult::F32(_) => assert!(float),
        tiff::decoder::DecodingResult::U16(_) => assert!(!float),
        _ => panic!("expected 16-bit or 32-bit float TIFF samples"),
    }
}

fn stack_method(method: &str) {
    let dir = TempDir::new().unwrap();
    let files = frames(dir.path());
    let mut command = cli(dir.path());
    // Use explicit files for average, the engine's folder ingestion for the other methods.
    command.arg("stack");
    if method == "average" {
        command.args(&files);
    } else {
        command.arg(files[0].parent().unwrap());
    }
    let output = command.args(["--method", method, "--out", "result.tif", "--json-progress"]).assert().success().get_output().clone();
    let result = json_output(&output);
    let outputs = result["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), if method == "both" { 2 } else { 1 });
    for entry in outputs {
        check_image(Path::new(entry["file"]["path"].as_str().unwrap()));
        check_depth(Path::new(entry["file"]["path"].as_str().unwrap()), false);
        assert!(entry["stack"]["frames"].as_array().unwrap().len() >= 2);
    }
    if method == "both" {
        assert!(dir.path().join("result-pyramid.tif").is_file());
        assert!(dir.path().join("result-depthmap.tif").is_file());
        assert!(!dir.path().join("result.tif").exists());
    }
    let events = lines(&output.stderr);
    for event in &events {
        assert!(event["event"].is_string());
    }
    for command in ["project.addFrames", "align.run", "output.save"] {
        let jobs: Vec<_> = events.iter().filter(|e| e["event"] == "job" && e["job"]["command"] == command).collect();
        assert!(jobs.iter().any(|e| e["job"]["state"] == "running"));
        assert!(jobs.iter().any(|e| e["job"]["state"] == "done"));
        for event in jobs {
            let fraction = event["job"]["progress"]["fraction"].as_f64().unwrap();
            assert!((0.0..=1.0).contains(&fraction));
        }
    }
    assert!(Path::new(result["project"].as_str().unwrap()).join("project.json").is_file());
}

#[test]
fn pyramid() {
    stack_method("pyramid");
}
#[test]
fn depthmap() {
    stack_method("depthmap");
}
#[test]
fn average() {
    stack_method("average");
}
#[test]
fn both() {
    stack_method("both");
}

#[test]
fn scientific_sidecar_reproduces_all_artifacts_without_the_original_project_or_cache() {
    let dir = TempDir::new().unwrap();
    let files = frames(dir.path());
    let mask_path = dir.path().join("defects.png");
    let mut mask = vec![255; 96 * 72];
    mask[12 * 96 + 12] = 0;
    png_file(&mask_path, 96, 72, png::ColorType::Grayscale, &mask);
    let stacked = cli(dir.path())
        .args(["--device", "cpu", "stack"])
        .arg(files[0].parent().unwrap())
        .args(["--method", "depthmap", "--out", "stack.tif"])
        .arg("--dust-mask")
        .arg(&mask_path)
        .assert()
        .success()
        .get_output()
        .clone();
    let result = json_output(&stacked);
    let project = PathBuf::from(result["project"].as_str().unwrap());
    let output = result["outputs"][0]["stack"]["output"].clone();
    let source_frame = result["outputs"][0]["stack"]["frames"][0].clone();
    let output_settings = json!({"template":"science","metadata":false,"depth_map":true,"confidence_map":true,"aligned_frames":true,"contribution_maps":true,"alignment_csv":true,"calibrated_depth":true,"z_step_micrometers":2.5,"pixels_per_micrometer":2.0,"calibration_name":"10x"});
    let layers = json!({"output":output,"layers":[
        {"name":"Scale","visible":true,"white":true,"textSize":8,"annotation":{"kind":"scale-bar","style":"bracket","position":"bottom-right"}},
        {"name":"Specimen","visible":true,"white":false,"textSize":8,"annotation":{"kind":"text","text":"A","position":[0.05,0.05]}}
    ]});
    let exported_dir = dir.path().join("scientific");
    // Settings changed after stacking must not change the immutable input state used by replay.
    let exported = cli(dir.path())
        .arg("run")
        .arg(&project)
        .args(["--cmd", "develop.set", "--params", &json!({"exposure":1.0}).to_string()])
        .args(["--cmd", "export.set", "--params", &output_settings.to_string()])
        .args(["--cmd", "annotation.set", "--params", &layers.to_string()])
        .args(["--cmd", "retouch.open", "--params", &json!({"output":output}).to_string()])
        .args(["--cmd", "retouch.setSource", "--params", &json!({"frame":source_frame}).to_string()])
        .args([
            "--cmd",
            "retouch.stroke",
            "--params",
            &json!({"stroke":{"brush":"pixels","source":{"frame":source_frame},"size":12,"hardness":0.5,"opacity":0.8,"samples":[[20,20,1]]}})
                .to_string(),
        ])
        .args(["--cmd", "export.run", "--params", &json!({"output":output,"folder":exported_dir}).to_string()])
        .assert()
        .success()
        .get_output()
        .clone();
    let events = lines(&exported.stdout);
    let artifacts = events.last().unwrap()["result"]["files"].as_array().unwrap();
    let sidecar = artifacts.iter().find(|f| f["what"] == "sidecar").unwrap()["path"].as_str().unwrap();
    let manifest: Value = serde_json::from_slice(&std::fs::read(sidecar).unwrap()).unwrap();
    assert!(manifest["project"]["frames"][0]["hash"].as_str().unwrap().starts_with("sha256:"));
    std::fs::remove_dir_all(&project).unwrap();
    std::fs::remove_file(dir.path().join("stack.tif")).unwrap();
    let reproduced_dir = dir.path().join("reproduced");
    cli(dir.path()).arg("reproduce").arg(sidecar).arg("--out").arg(&reproduced_dir).assert().success();
    for file in artifacts {
        let source = Path::new(file["path"].as_str().unwrap());
        let reproduced = reproduced_dir.join(source.strip_prefix(&exported_dir).unwrap());
        assert!(reproduced.is_file(), "{}", reproduced.display());
        match file["what"].as_str().unwrap() {
            "sidecar" => {}
            "alignment" => assert_eq!(std::fs::read(source).unwrap(), std::fs::read(&reproduced).unwrap()),
            _ => {
                let read = |path: &Path| {
                    let mut decoder = tiff::decoder::Decoder::new(std::fs::File::open(path).unwrap()).unwrap();
                    decoder.read_image().unwrap()
                };
                match (read(source), read(&reproduced)) {
                    (tiff::decoder::DecodingResult::U16(a), tiff::decoder::DecodingResult::U16(b)) => assert_eq!(a, b, "{}", source.display()),
                    (tiff::decoder::DecodingResult::F32(a), tiff::decoder::DecodingResult::F32(b)) => assert_eq!(a, b, "{}", source.display()),
                    _ => panic!("replay changed the sample format"),
                }
            }
        }
    }
    // The sidecar is no authority to accept changed inputs or to publish incomplete results.
    let mut changed = std::fs::read(&files[0]).unwrap();
    changed.push(0);
    std::fs::write(&files[0], changed).unwrap();
    let refused = dir.path().join("refused");
    cli(dir.path()).arg("reproduce").arg(sidecar).arg("--out").arg(&refused).assert().failure();
    assert!(!refused.exists());
}

#[test]
fn plain_exports_replay_every_stacking_method_and_slab_dependencies() {
    let dir = TempDir::new().unwrap();
    let files = frames(dir.path());
    for (index, method, slab) in [(0, "average", false), (1, "pyramid", false), (2, "pyramid", true)] {
        let path = dir.path().join(format!("result-{index}.tif"));
        let mut command = cli(dir.path());
        command.args(["--device", "cpu", "stack"]).arg(files[0].parent().unwrap()).arg("--method").arg(method).arg("--out").arg(&path);
        if slab {
            command.args(["--slab", "2:1"]);
        }
        let original = command.assert().success().get_output().clone();
        let original = json_output(&original);
        let sidecar = original["outputs"][0]["file"]["sidecar"].as_str().unwrap();
        let target = dir.path().join(format!("replay-{index}"));
        cli(dir.path()).arg("reproduce").arg(sidecar).arg("--out").arg(&target).assert().success();
        let read = |p: &Path| {
            let mut decoder = tiff::decoder::Decoder::new(std::fs::File::open(p).unwrap()).unwrap();
            let tiff::decoder::DecodingResult::U16(samples) = decoder.read_image().unwrap() else { panic!("16-bit TIFF") };
            samples
        };
        assert_eq!(read(&path), read(&target.join(path.file_name().unwrap())), "{method}, slab={slab}");
    }
}

#[test]
fn a_built_in_preset_is_named_by_its_id() {
    let dir = TempDir::new().unwrap();
    let files = frames(dir.path());
    let output = cli(dir.path())
        .arg("stack")
        .arg(files[0].parent().unwrap())
        .args(["--preset", "builtin:product-shot", "--out", "result.tif"])
        .assert()
        .success()
        .get_output()
        .clone();
    let result = json_output(&output);
    assert_eq!(result["outputs"][0]["stack"]["settings"]["grit"], 0.25, "the product shot's grit: {result}");
    cli(dir.path()).arg("stack").arg(files[0].parent().unwrap()).args(["--preset", "builtin:underwater", "--out", "other.tif"]).assert().failure();
}

#[test]
fn slab_preset_and_explicit_options_reach_the_engine() {
    let dir = TempDir::new().unwrap();
    frames(dir.path());
    std::fs::write(
        dir.path().join("preset.toml"),
        "[align]\nrotation = true\nframing = 'wide'\n[develop]\nwhite_balance = 'auto'\n[stack.average]\npower = 2.0\n",
    )
    .unwrap();
    let mask = vec![255; 96 * 72];
    png_file(&dir.path().join("dust.png"), 96, 72, png::ColorType::Grayscale, &mask);
    let output = cli(dir.path())
        .args([
            "stack",
            "input photos",
            "--method",
            "average",
            "--out",
            "slabs.tif",
            "--slab",
            "2:1",
            "--preset",
            "preset.toml",
            "--align",
            "shift,brightness",
            "--crop",
            "common",
            "--raw-wb",
            "asshot",
            "--depth",
            "32f",
            "--dust-mask",
            "dust.png",
            "--json-progress",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let result = json_output(&output);
    let stacked = &result["outputs"][0]["stack"];
    assert_eq!(stacked["slabSize"], 2);
    assert_eq!(stacked["overlap"], 1);
    assert_eq!(stacked["framing"], "common");
    assert!(stacked["slabs"].as_array().unwrap().len() > 1);
    check_image(&dir.path().join("slabs.tif"));
    check_depth(&dir.path().join("slabs.tif"), true);
    let project = result["project"].as_str().unwrap();
    let response = cli(dir.path())
        .args(["run", project, "--cmd", "align.get", "--cmd", "develop.get", "--cmd", "stack.get", "--cmd", "defects.info"])
        .assert()
        .success()
        .get_output()
        .clone();
    let queries = lines(&response.stdout);
    assert_eq!(queries[0]["result"]["settings"]["rotation"], false);
    assert_eq!(queries[0]["result"]["settings"]["scale"], false);
    assert_eq!(queries[0]["result"]["settings"]["brightness"], true);
    assert_eq!(queries[1]["result"]["settings"]["white_balance"], "as-shot");
    assert_eq!(queries[2]["result"]["settings"]["average"]["power"], 2.0);
    assert!(queries[3]["result"]["summary"].is_object());
}

#[test]
fn run_commands_are_ordered_and_changes_are_saved() {
    let dir = TempDir::new().unwrap();
    cli(dir.path()).args(["project", "new", "Bee"]).assert().success();
    let project = format!("Bee{}", branding::PROJECT_EXTENSION);
    let output = cli(dir.path())
        .args(["run", &project, "--cmd", "project.rename", "--params", r#"{"name":"Flower"}"#, "--cmd", "project.info", "--params", "{}"])
        .assert()
        .success()
        .get_output()
        .clone();
    let results = lines(&output.stdout);
    assert_eq!(results[0]["command"], "project.rename");
    assert_eq!(results[1]["result"]["name"], "Flower");
    let info = cli(dir.path()).args(["project", "info", &project]).assert().success().get_output().clone();
    assert_eq!(json_output(&info)["name"], "Flower");
    assert_eq!(json_output(&info)["readOnly"], true);
}

#[test]
fn commands_include_every_schema_without_a_project() {
    let dir = TempDir::new().unwrap();
    let output = cli(dir.path()).arg("commands").assert().success().get_output().clone();
    assert_eq!(json_output(&output), serde_json::to_value(Registry::standard().list()).unwrap());
    assert_eq!(output.stderr, b"");
    assert!(!dir.path().join("temporary projects").exists());
}

#[test]
fn every_help_page_has_examples_and_exit_codes() {
    let dir = TempDir::new().unwrap();
    for args in [
        vec![],
        vec!["stack"],
        vec!["batch"],
        vec!["watch"],
        vec!["run"],
        vec!["reproduce"],
        vec!["commands"],
        vec!["mcp"],
        vec!["project"],
        vec!["project", "new"],
        vec!["project", "info"],
        vec!["project", "clean-orphans"],
    ] {
        let output = cli(dir.path()).args(args).arg("--help").assert().success().get_output().clone();
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Examples:"), "{help}");
        assert!(help.contains("Exit codes:"), "{help}");
    }
}

#[test]
fn file_only_invocation_handles_spaces_and_writes_beside_the_first_file() {
    let dir = TempDir::new().unwrap();
    let files = frames(dir.path());
    // No CLI flags: this is exactly the argument shape Explorer supplies on Windows.
    let output = Command::new(assert_cmd::cargo::cargo_bin!())
        .current_dir(dir.path())
        .env("TMPDIR", dir.path())
        .env("TEMP", dir.path())
        .env("TMP", dir.path())
        .args(&files)
        .assert()
        .success()
        .get_output()
        .clone();
    let result = json_output(&output);
    assert!(Path::new(result["project"].as_str().unwrap()).join("project.json").is_file());
    check_image(&files[0].with_file_name("focus 0-stacked.tif"));
}

#[test]
fn clean_orphans_can_preview_without_touching_the_system_trash() {
    let dir = TempDir::new().unwrap();
    let created = cli(dir.path()).args(["project", "new"]).assert().success().get_output().clone();
    let project = json_output(&created)["path"].as_str().unwrap().to_owned();
    let preview = cli(dir.path()).args(["project", "clean-orphans", "--days", "0", "--dry-run"]).assert().success().get_output().clone();
    assert!(json_output(&preview)["orphans"].as_array().unwrap().iter().any(|o| o["path"] == project));
    assert!(Path::new(&project).exists());
    let empty = cli(dir.path()).args(["project", "clean-orphans"]).assert().success().get_output().clone();
    assert_eq!(json_output(&empty), json!({ "trashed": [], "failed": [] }));
}

#[test]
fn invalid_inputs_and_parameters_have_documented_exit_codes() {
    let dir = TempDir::new().unwrap();
    for args in [
        vec!["stack", "missing.png", "--out", "result.tif", "--slab", "2:2"],
        vec!["stack", "missing.png", "--out", "result.tif", "--align", "shfit"],
        vec!["stack", "missing.png", "--out", "result.tif", "--depth", "8"],
    ] {
        let output = cli(dir.path()).args(args).arg("--json-progress").assert().code(2).get_output().clone();
        assert_eq!(lines(&output.stderr).last().unwrap()["event"], "error");
    }
    cli(dir.path()).args(["project", "new", "Bee"]).assert().success();
    let project = format!("Bee{}", branding::PROJECT_EXTENSION);
    for (command, params) in [("missing.command", "{}"), ("align.set", "{"), ("align.set", r#"{"max_shift":999}"#)] {
        let output =
            cli(dir.path()).args(["run", &project, "--cmd", command, "--params", params, "--json-progress"]).assert().code(2).get_output().clone();
        assert_eq!(lines(&output.stderr).last().unwrap()["event"], "error");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
    cli(dir.path()).args(["run", &project, "--cmd", "project.info", "--cmd", "align.get", "--params", "{}"]).assert().code(2);
    cli(dir.path()).args(["stack", "missing.png", "--out", "result.tif"]).assert().code(1);
    assert!(!dir.path().join("result.tif").exists());
}

#[test]
fn an_existing_output_is_preserved_and_empty_folders_fail() {
    let dir = TempDir::new().unwrap();
    frames(dir.path());
    std::fs::write(dir.path().join("result.tif"), b"existing output").unwrap();
    cli(dir.path()).args(["stack", "input photos", "--out", "result.tif"]).assert().code(1);
    assert_eq!(std::fs::read(dir.path().join("result.tif")).unwrap(), b"existing output");
    std::fs::create_dir(dir.path().join("empty")).unwrap();
    cli(dir.path()).args(["stack", "empty", "--out", "empty.tif"]).assert().code(1);
    assert!(!dir.path().join("empty.tif").exists());
}

#[test]
fn batch_processes_three_stacks_and_records_the_preset() {
    let dir = TempDir::new().unwrap();
    let source = frames(dir.path());
    for name in ["bee", "flower", "wasp"] {
        let folder = dir.path().join("stacks").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        for frame in &source {
            std::fs::copy(frame, folder.join(frame.file_name().unwrap())).unwrap();
        }
    }
    std::fs::write(dir.path().join("preset.toml"), "[stack.depth_map]\nthreshold = 'percentile'\npercentile = 42\n").unwrap();
    let output = cli(dir.path())
        .args([
            "batch",
            "--in",
            "stacks",
            "--out",
            "results",
            "--preset",
            "preset.toml",
            "--method",
            "depthmap",
            "--template",
            "{stack}-{method}-{first}-{last}-{date}.tif",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let result = json_output(&output);
    assert_eq!(result["failed"], 0, "{result}");
    assert_eq!(result["stacks"].as_array().unwrap().len(), 3);
    assert_eq!(std::fs::read_to_string(result["summary"].as_str().unwrap()).unwrap().lines().count(), 4);
    for entry in result["stacks"].as_array().unwrap() {
        assert_eq!(entry["frames"], 4);
        check_image(Path::new(entry["output"].as_str().unwrap()));
        let info = cli(dir.path()).args(["run", entry["project"].as_str().unwrap(), "--cmd", "stack.get"]).assert().success().get_output().clone();
        assert_eq!(json_output(&info)["result"]["settings"]["depth_map"]["percentile"], 42.0);
    }
    cli(dir.path()).args(["batch", "--in", "stacks", "--out", "results", "--preset", "preset.toml"]).assert().code(1);
}

#[test]
fn watch_cli_accepts_slowly_written_frames_and_emits_live_previews() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::time::Duration;
    let dir = TempDir::new().unwrap();
    let source = frames(dir.path());
    let input = dir.path().join("capture");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(dir.path().join("preset.toml"), "[stack.average]\npower = 2\n").unwrap();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin!())
        .current_dir(dir.path())
        .args([
            "watch",
            "capture",
            "--preset",
            "preset.toml",
            "--method",
            "average",
            "--idle",
            "5",
            "--settle-ms",
            "60",
            "--poll-ms",
            "20",
            "--json-progress",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (preview_tx, preview_rx) = std::sync::mpsc::channel();
    let progress = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut acknowledged = 0;
        for line in BufReader::new(stderr).lines() {
            let line = line.unwrap();
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
            let event: Value = serde_json::from_str(&line).unwrap();
            if let Some(frames) = event["job"]["preview"]["frames"].as_u64()
                && frames > acknowledged
            {
                let _ = preview_tx.send(frames);
                acknowledged = frames;
            }
        }
        bytes
    });
    let writer = std::thread::spawn(move || {
        for (i, path) in source.iter().enumerate() {
            let bytes = std::fs::read(path).unwrap();
            let partial = input.join(format!("frame-{i}.part"));
            let mut file = std::fs::File::create(&partial).unwrap();
            for chunk in bytes.chunks(bytes.len().div_ceil(4)) {
                file.write_all(chunk).unwrap();
                file.flush().unwrap();
                std::thread::sleep(Duration::from_millis(35));
            }
            drop(file);
            std::fs::rename(&partial, input.join(format!("frame-{i}.png"))).unwrap();
            // Wait for each live preview: fixed delays race the idle timeout and can batch all arrivals on a busy runner.
            let frames = preview_rx.recv_timeout(Duration::from_secs(15)).unwrap_or_else(|error| panic!("preview for frame {i}: {error}"));
            assert_eq!(frames, i as u64 + 1);
        }
    });
    let mut output = child.wait_with_output().unwrap();
    output.stderr = progress.join().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    writer.join().unwrap();
    let result = json_output(&output);
    assert_eq!(result["stacks"][0]["frames"], 4, "{result}");
    check_image(Path::new(result["stacks"][0]["output"].as_str().unwrap()));
    assert!(dir.path().join("capture-stacked").join("summary.csv").is_file());
    let previews: Vec<_> = lines(&output.stderr).into_iter().filter_map(|e| e["job"]["preview"]["frames"].as_u64()).collect();
    assert!((1..=4).all(|n| previews.contains(&n)), "{previews:?}");
}

#[test]
fn batch_failures_have_csv_and_a_processing_exit_code() {
    let dir = TempDir::new().unwrap();
    frames(dir.path());
    std::fs::create_dir(dir.path().join("empty")).unwrap();
    std::fs::write(dir.path().join("preset.toml"), "").unwrap();
    let output = cli(dir.path())
        .args(["batch", "--in", ".", "--out", "../unused", "--preset", "preset.toml", "--template", "../escape.tif"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    assert!(!output.stdout.is_empty() || !output.stderr.is_empty());
    let input = dir.path().join("queue");
    std::fs::create_dir_all(input.join("empty")).unwrap();
    let output =
        cli(dir.path()).args(["batch", "--in", "queue", "--out", "results", "--preset", "preset.toml"]).assert().code(1).get_output().clone();
    let result = json_output(&output);
    assert_eq!(result["failed"], 1);
    assert!(dir.path().join("results/summary.csv").is_file());
}
