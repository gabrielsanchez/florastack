//! Scientific data values, immutable input state, annotation layers and calibration persistence through JSON commands.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{TempDir, config, run};
use florastack_codecs::ReadOptions;
use florastack_core::{AnyImage, ColorSpace, Image};
use florastack_engine::{Engine, Value, json};
use florastack_project::{Input, MapName, OutputId, ProjectFolder};

fn exported(engine: &mut Engine, output: OutputId, folder: &std::path::Path, settings: &Value) -> Value {
    run(engine, "export.run", &json!({"output":output,"folder":folder,"settings":settings}).to_string())
}

#[test]
fn calibrated_tiff_matches_known_synthetic_depths_in_recorded_capture_order() {
    let dir = TempDir::new("calibrated-depth");
    let fixture = common::fixture_project(dir.path());
    let (mut folder, mut project) = ProjectFolder::open(&fixture.path).unwrap();
    // A known synthetic depth ramp, composited in reverse order with the middle capture omitted.
    let depth = Image::from_fn(12, 8, 1, ColorSpace::LINEAR, |x, _, _| x as f32 / 11.0).unwrap();
    let raw = florastack_engine::project::write_image(&folder, "depth-frames", &AnyImage::F32(depth.clone()), None).unwrap();
    let integer = florastack_engine::project::write_image(&folder, "depth", &AnyImage::U16(depth.convert().unwrap()), None).unwrap();
    let mut source = project.clone();
    source.outputs.clear();
    let output = project.output_mut(fixture.output).unwrap();
    output.method = "depth-map".into();
    output.inputs = vec![Input::Frame(fixture.frames[2]), Input::Frame(fixture.frames[0])];
    output.maps.insert(MapName::new("depth-frames").unwrap(), raw);
    output.maps.insert(MapName::new("depth").unwrap(), integer);
    output.source_state = Some(std::sync::Arc::new(source));
    // Later reordering must not turn two physical steps into one, or reverse the physical Z.
    project.frames.reverse();
    folder.save(&mut project).unwrap();
    drop(folder);
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.open", &json!({"path":fixture.path}).to_string());
    let result = exported(
        &mut engine,
        fixture.output,
        &dir.path().join("export"),
        &json!({"calibrated_depth":true,"depth_map":true,"z_step_micrometers":-2.5,"z_origin_micrometers":10}),
    );
    let files = result["files"].as_array().unwrap();
    let calibrated = files.iter().find(|f| f["what"] == "calibrated-depth").unwrap();
    let decoded = florastack_codecs::read_file(std::path::Path::new(calibrated["path"].as_str().unwrap()), &ReadOptions::default()).unwrap();
    assert_eq!((decoded.info.bits_per_sample, decoded.info.float, decoded.info.channels), (32, true, 1));
    assert!(decoded.profile.is_none());
    let AnyImage::F32(image) = decoded.image else { panic!("physical depths need float samples") };
    for (row, depths) in image.rows().zip(depth.rows()) {
        for (&z, &d) in row.iter().zip(depths) {
            // Capture 2 lies at 5 µm, capture 0 at 10 µm. Float export must retain physical values above 1.
            assert!((z - (5.0 + d * 5.0)).abs() < 1e-6, "{z} at {d}");
        }
    }
    let integer = files.iter().find(|f| f["what"] == "depth").unwrap();
    let decoded = florastack_codecs::read_file(std::path::Path::new(integer["path"].as_str().unwrap()), &ReadOptions::default()).unwrap();
    assert_eq!(decoded.info.bits_per_sample, 16);
    let sidecar = files.iter().find(|f| f["what"] == "sidecar").unwrap();
    let manifest: Value = serde_json::from_slice(&std::fs::read(sidecar["path"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(manifest["project"]["outputs"][0]["source_state"]["frames"][0]["id"], fixture.frames[0].get());
    assert_eq!(manifest["softwareVersion"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn calibration_profiles_persist_outside_projects_and_null_clears_a_measurement() {
    let dir = TempDir::new("calibration-profiles");
    let mut engine = Engine::new(config(dir.path()));
    let profiles = json!({"profiles":[{"name":"10x","pixelsPerMicrometer":2.5},{"name":"40x","pixelsPerMicrometer":10.0}]});
    run(&mut engine, "calibration.set", &profiles.to_string());
    drop(engine);
    let mut engine = Engine::new(config(dir.path()));
    assert_eq!(run(&mut engine, "calibration.get", "{}"), profiles);
    run(&mut engine, "project.new", "{}");
    run(&mut engine, "export.set", r#"{"pixels_per_micrometer":2.5,"z_step_micrometers":1.5}"#);
    let cleared = run(&mut engine, "export.set", r#"{"pixels_per_micrometer":null,"z_step_micrometers":null}"#);
    assert!(cleared["settings"]["pixels_per_micrometer"].is_null());
    assert!(cleared["settings"]["z_step_micrometers"].is_null());
}

#[test]
fn input_hashes_describe_the_bytes_actually_used_for_processing() {
    let dir = TempDir::new("scientific-input-hashes");
    let files = common::synthetic_frames(dir.path(), "frames", 32, 24, &florastack_core::testutil::StackParams::identity(3));
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.new", "{}");
    run(&mut engine, "project.addFrames", &json!({"paths":files}).to_string());
    let file = &files[0];
    let mut bytes = std::fs::read(file).unwrap();
    bytes.push(0); // Still a decodable TIFF of a valid alignment size; only its fingerprint changes.
    std::fs::write(file, bytes).unwrap();
    let error = engine.run("align.run", json!({})).unwrap_err();
    assert!(error.info().message.contains("changed since it was added"), "{error}");
}

#[test]
fn annotation_layers_save_undo_hide_and_render_only_on_the_export_copy() {
    let dir = TempDir::new("annotation-layers");
    let fixture = common::fixture_project(dir.path());
    let mut engine = Engine::new(config(dir.path()));
    run(&mut engine, "project.open", &json!({"path":fixture.path}).to_string());
    let base = engine.project().unwrap().output(fixture.output).unwrap().image.clone();
    let layers = json!([{"name":"Specimen A","visible":true,"white":true,"textSize":8,"annotation":{"kind":"text","text":"A","position":[0.0,0.0]}}]);
    run(&mut engine, "annotation.set", &json!({"output":fixture.output,"layers":layers}).to_string());
    run(&mut engine, "edit.undo", "{}");
    assert_eq!(engine.project().unwrap().output(fixture.output).unwrap().annotations, Vec::<Value>::new());
    run(&mut engine, "edit.redo", "{}");
    run(&mut engine, "project.save", "{}");
    run(&mut engine, "project.close", "{}");
    run(&mut engine, "project.open", &json!({"path":fixture.path}).to_string());
    assert_eq!(run(&mut engine, "annotation.get", &json!({"output":fixture.output}).to_string())["layers"], layers);
    let settings = json!({"format":"tiff","depth":"f32","profile":"untagged","metadata":false});
    let annotated = exported(&mut engine, fixture.output, &dir.path().join("annotated"), &settings);
    let mut plain = settings;
    plain["annotations"] = json!(false);
    let unannotated = exported(&mut engine, fixture.output, &dir.path().join("plain"), &plain);
    let read = |result: &Value| {
        florastack_codecs::read_file(std::path::Path::new(result["files"][0]["path"].as_str().unwrap()), &ReadOptions::default()).unwrap().image
    };
    assert_ne!(read(&annotated), read(&unannotated));
    assert_eq!(engine.project().unwrap().output(fixture.output).unwrap().image, base);
    let mut hidden = layers;
    hidden[0]["visible"] = json!(false);
    run(&mut engine, "annotation.set", &json!({"output":fixture.output,"layers":hidden}).to_string());
    let hidden = exported(
        &mut engine,
        fixture.output,
        &dir.path().join("hidden"),
        &json!({"format":"tiff","depth":"f32","profile":"untagged","metadata":false}),
    );
    assert_eq!(read(&hidden), read(&unannotated));
}
