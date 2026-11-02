//! Relative-depth geometry, camera direction and actual exported containers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use florastack_core::{ColorSpace, Image, Unmonitored};
use florastack_engine::{
    Engine, json,
    parallax::{Scene, Settings, Stereo, Surface},
};

fn settings() -> Settings {
    Settings { max_side: 128, mesh_side: 64, confidence: 0.1, smoothing: 0, max_depth_jump: 1.0, ..Default::default() }
}
fn surface(reverse: bool) -> Surface {
    let image = Image::from_fn(128, 64, 3, ColorSpace::LINEAR, |x, _, _| if x == 40 || x == 88 { 1.0 } else { 0.0 }).unwrap();
    let depth = Image::from_fn(128, 64, 1, ColorSpace::LINEAR, |x, _, _| if x < 64 { 0.0 } else { 1.0 }).unwrap();
    let confidence = Image::from_fn(128, 64, 1, ColorSpace::LINEAR, |_, _, _| 1.0).unwrap();
    Surface::new(&image, &depth, &confidence, &Settings { reverse_depth: reverse, ..settings() }, &Unmonitored).unwrap()
}
fn peaks(image: &Image<u8>) -> Vec<usize> {
    image.row(30).unwrap().as_chunks::<4>().0.iter().enumerate().filter_map(|(x, p)| (p[0] > 240 && p[3] > 0).then_some(x)).collect()
}

#[test]
fn nearer_features_move_right_in_left_eye_and_reverse_depth_swaps_direction() {
    let surface = surface(false);
    let left = surface.view(-0.5, 0.125, &Unmonitored).unwrap();
    let right = surface.view(0.5, 0.125, &Unmonitored).unwrap();
    assert_eq!(peaks(&left), [44, 84]);
    assert_eq!(peaks(&right), [36, 92]);
    assert_eq!(peaks(&super_surface_view(true, -0.5)), peaks(&right));
    assert_eq!(surface.view(0.0, 0.0, &Unmonitored).unwrap(), surface.view(0.5, 0.0, &Unmonitored).unwrap());
    let parallel = florastack_engine::parallax::stereo(&left, &right, Stereo::Parallel).unwrap();
    let cross = florastack_engine::parallax::stereo(&left, &right, Stereo::CrossEyed).unwrap();
    assert_eq!(&parallel.row(30).unwrap()[..128 * 4], left.row(30).unwrap());
    assert_eq!(&cross.row(30).unwrap()[..128 * 4], right.row(30).unwrap());
    let anaglyph = florastack_engine::parallax::stereo(&left, &right, Stereo::Anaglyph).unwrap();
    assert_eq!(anaglyph.width(), 128);
    assert!(anaglyph.row(30).unwrap()[44 * 4] > 240);
    assert!(anaglyph.row(30).unwrap()[36 * 4 + 1] > 240);
}
fn super_surface_view(reverse: bool, camera: f32) -> Image<u8> {
    surface(reverse).view(camera, 0.125, &Unmonitored).unwrap()
}

#[test]
fn smoothing_preserves_confidence_holes_and_reduces_depth_noise() {
    let image = Image::from_fn(32, 24, 3, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
    let depth = Image::from_fn(32, 24, 1, ColorSpace::LINEAR, |x, y, _| if x == 16 && y == 12 { 1.0 } else { 0.2 }).unwrap();
    let confidence = Image::from_fn(32, 24, 1, ColorSpace::LINEAR, |x, _, _| if x == 4 { 0.0 } else { 1.0 }).unwrap();
    let s = Settings { mesh_side: 32, smoothing: 2, ..settings() };
    let surface = Surface::new(&image, &depth, &confidence, &s, &Unmonitored).unwrap();
    let scene = surface.mesh(&s, &Unmonitored).unwrap();
    assert!(scene.vertices.iter().all(|v| v[4].is_finite() && (v[3] - 4.0 / 31.0).abs() > 1e-6));
    let z = scene.vertices.iter().find(|v| (v[3] - 16.0 / 31.0).abs() < 1e-6 && (v[4] - 12.0 / 23.0).abs() < 1e-6).unwrap()[2];
    assert!((z - (0.5 - 0.232) * s.relief).abs() < 1e-5, "{z}");
    let view = surface.view(0.0, 0.0, &Unmonitored).unwrap();
    assert_eq!(view.row(10).unwrap()[4 * 4 + 3], 0);
    for tri in scene.indices.as_chunks::<3>().0 {
        let [a, b, c] = [scene.vertices[tri[0] as usize], scene.vertices[tri[1] as usize], scene.vertices[tri[2] as usize]];
        assert!((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]) > 0.0, "front winding faces positive Z");
    }
}

#[test]
fn malformed_maps_settings_and_previews_return_errors() {
    let image = Image::new(8, 8, 3, ColorSpace::LINEAR).unwrap();
    let map = Image::from_fn(8, 8, 1, ColorSpace::LINEAR, |_, _, _| 1.0).unwrap();
    let bad = Image::from_fn(8, 8, 1, ColorSpace::LINEAR, |_, _, _| f32::NAN).unwrap();
    assert!(Surface::new(&image, &bad, &map, &settings(), &Unmonitored).is_err());
    assert!(Surface::new(&image, &map, &bad, &settings(), &Unmonitored).is_err());
    assert!(Surface::new(&image, &map, &map, &Settings { max_side: usize::MAX, ..settings() }, &Unmonitored).is_err());
    assert!(Scene { width: usize::MAX, height: 2, vertices: vec![], indices: vec![], rgba: vec![] }.validate().is_err());
    assert!(surface(false).view(f32::NAN, 0.1, &Unmonitored).is_err());
}

#[test]
fn cancellation_stops_surface_preparation() {
    struct Cancel;
    impl florastack_core::Monitor for Cancel {
        fn is_cancelled(&self) -> bool {
            true
        }
        fn progress(&self, _: u64, _: u64) {}
    }
    let image = Image::new(8, 8, 3, ColorSpace::LINEAR).unwrap();
    let map = Image::from_fn(8, 8, 1, ColorSpace::LINEAR, |_, _, _| 1.0).unwrap();
    assert!(matches!(Surface::new(&image, &map, &map, &settings(), &Cancel), Err(florastack_engine::CommandError::Cancelled)));
}

fn stacked(dir: &std::path::Path) -> (Engine, u64) {
    let params = florastack_core::testutil::StackParams { blur_per_depth: 8.0, max_blur: 8.0, ..florastack_core::testutil::StackParams::identity(6) };
    let paths = common::synthetic_frames(dir, "frames", 64, 48, &params);
    let mut engine = Engine::new(common::config(dir));
    common::run(&mut engine, "project.new", "{}");
    common::run(&mut engine, "project.addFrames", &json!({"paths":paths}).to_string());
    common::run(&mut engine, "align.run", "{}");
    let output = common::run(&mut engine, "stack.depthMap", r#"{"threshold":"level","level":0}"#)["output"].as_u64().unwrap();
    (engine, output)
}

#[test]
fn exported_glb_passes_the_official_khronos_validator() {
    let package = std::env::temp_dir().join("florastack-gltf-validator-2.0.0-dev.3.10/node_modules/gltf-validator");
    if !package.join("index.js").is_file() {
        florastack_testkit::skip!(Corpus, "Khronos validator fixture missing: run cargo xtask gltf-validator (requires Node.js)");
        return;
    }
    let dir = common::TempDir::new("gltf-validator");
    let (mut engine, output) = stacked(dir.path());
    let exported = common::run(&mut engine, "parallax.mesh", &json!({"output":output,"folder":dir.path(),"settings":{"confidence":0.0}}).to_string());
    let path = exported["files"][0].as_str().unwrap();
    let result = std::process::Command::new("node")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/validate-glb.cjs"))
        .arg(package)
        .arg(path)
        .output()
        .expect("Node.js runs the official validator");
    assert!(result.status.success(), "{}\n{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
}

#[test]
fn stereo_pair_from_a_synthetic_stack_has_the_expected_disparity() {
    let dir = common::TempDir::new("stack-stereo");
    let (mut engine, output) = stacked(dir.path());
    let preview =
        common::run(&mut engine, "parallax.preview", &json!({"output":output,"settings":{"confidence":0,"smoothing":0,"meshSide":512}}).to_string());
    let scene = florastack_engine::parallax::load_scene(std::path::Path::new(preview["path"].as_str().unwrap())).unwrap();
    let result = common::run(
        &mut engine,
        "parallax.stereo",
        &json!({"output":output,"folder":dir.path(),"mode":"parallel","settings":{"confidence":0,"smoothing":0,"baseline":0.25}}).to_string(),
    );
    let pair = florastack_codecs::read_file(std::path::Path::new(result["files"][0].as_str().unwrap()), &florastack_codecs::ReadOptions::default())
        .unwrap()
        .image;
    let florastack_core::AnyImage::U8(pair) = pair else { panic!("8-bit stereo PNG") };
    let mut near = 0;
    let mut far = 0;
    for y in 0..scene.height {
        let row = pair.row(y).unwrap();
        let (left, right) = row.split_at(scene.width * 4);
        let source = scene.rgba[y * scene.width * 4..(y + 1) * scene.width * 4].as_chunks::<4>().0;
        for (src, pixel) in source.iter().enumerate().filter(|(_, p)| p[3] > 0) {
            if source.iter().filter(|p| *p == pixel).count() != 1 {
                continue;
            }
            if let (Some(lx), Some(rx)) =
                (left.as_chunks::<4>().0.iter().position(|p| p == pixel), right.as_chunks::<4>().0.iter().position(|p| p == pixel))
            {
                let (u, v) = (src as f32 / (scene.width - 1) as f32, y as f32 / (scene.height - 1) as f32);
                let vertex = scene.vertices.iter().find(|p| (p[3] - u).abs() < 1e-6 && (p[4] - v).abs() < 1e-6).unwrap();
                let d = 0.5 - vertex[2] / 0.2;
                if d < 0.2 {
                    assert!(lx > rx, "near feature: left {lx}, right {rx}, depth {d}");
                    near += 1;
                } else if d > 0.8 {
                    assert!(lx < rx, "far feature: left {lx}, right {rx}, depth {d}");
                    far += 1;
                }
            }
        }
    }
    assert!(near > 0 && far > 0, "synthetic scene exercises both sides of the convergence plane: {near}, {far}");
}

#[test]
fn animation_is_a_decodable_loop_with_the_requested_frame_count() {
    let bytes = florastack_engine::parallax::animation(&surface(false), &settings(), 8, 16, &Unmonitored).unwrap();
    let mut decoder = gif::DecodeOptions::new().read_info(std::io::Cursor::new(bytes)).unwrap();
    let mut frames = 0;
    let mut time = 0;
    while let Some(frame) = decoder.read_next_frame().unwrap() {
        frames += 1;
        time += frame.delay;
    }
    assert_eq!((frames, time), (8, 50));
    assert_eq!(florastack_engine::parallax::rocking_offset(0, 8), 0.0);
    assert!((florastack_engine::parallax::rocking_offset(2, 8) - 0.5).abs() < 1e-6);
    assert!((florastack_engine::parallax::rocking_offset(6, 8) + 0.5).abs() < 1e-6);
}

#[test]
fn obj_companions_stay_together_and_existing_files_are_refused() {
    let dir = common::TempDir::new("obj-companions");
    let (mut engine, output) = stacked(dir.path());
    let params = json!({"output":output,"folder":dir.path(),"format":"obj","settings":{"confidence":0.0}});
    let result = common::run(&mut engine, "parallax.mesh", &params.to_string());
    assert_eq!(result["files"].as_array().unwrap().len(), 3);
    let before = std::fs::read(dir.path().join("model.obj")).unwrap();
    let text = String::from_utf8(before.clone()).unwrap();
    assert!(text.contains("mtllib material.mtl\nusemtl composite\n"));
    assert!(text.lines().any(|l| l.starts_with("vt ") && l.ends_with(" 1")), "top row maps to OBJ V=1");
    let material = std::fs::read_to_string(dir.path().join("material.mtl")).unwrap();
    assert!(material.contains("map_Kd texture.png"));
    assert!(florastack_codecs::read_file(&dir.path().join("texture.png"), &florastack_codecs::ReadOptions::default()).is_ok());
    assert!(engine.run("parallax.mesh", params.clone()).is_err());
    assert_eq!(std::fs::read(dir.path().join("model.obj")).unwrap(), before);
    let mut replace = params;
    replace["overwrite"] = json!(true);
    common::run(&mut engine, "parallax.mesh", &replace.to_string());
}

#[test]
fn overlapping_projected_samples_keep_the_foreground() {
    let image = Image::from_fn(128, 32, 3, ColorSpace::LINEAR, |x, _, c| if (x == 60 && c == 0) || (x == 68 && c == 2) { 1.0 } else { 0.0 }).unwrap();
    let depth = Image::from_fn(128, 32, 1, ColorSpace::LINEAR, |x, _, _| if x < 64 { 0.0 } else { 1.0 }).unwrap();
    let confidence = Image::from_fn(128, 32, 1, ColorSpace::LINEAR, |_, _, _| 1.0).unwrap();
    let surface = Surface::new(&image, &depth, &confidence, &settings(), &Unmonitored).unwrap();
    let view = surface.view(-0.5, 0.125, &Unmonitored).unwrap();
    assert_eq!(&view.row(16).unwrap()[64 * 4..64 * 4 + 4], &[255, 0, 0, 255]);
}
