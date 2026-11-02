//! Control requests drive the real app's UI and screenshot its full offscreen GPU render.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use florastack_app::{FloraStackApp, Options, offscreen, prefs::Prefs, viewer::GpuViewer};
use florastack_engine::{Compute, EngineConfig, Value, control::Client, json};

fn request(client: Client, method: &str, params: Value, pump: &mut impl FnMut()) -> Result<Value, florastack_engine::ErrorInfo> {
    let method = method.to_owned();
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        tx.send(runtime.block_on(client.request(&method, params)).unwrap()).unwrap();
    });
    let start = Instant::now();
    let value = loop {
        pump();
        if let Ok(value) = rx.try_recv() {
            break value;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(2));
    };
    worker.join().unwrap();
    value
}

#[test]
fn authenticated_control_changes_view_and_renders_an_offscreen_screenshot() {
    let mut renderer = match offscreen::Renderer::new() {
        Ok(renderer) => renderer,
        Err(reason) => {
            assert!(std::env::var_os("FLORASTACK_REQUIRE_GPU").is_none_or(|v| v.is_empty()), "{reason}");
            florastack_testkit::skip!(Gpu, "no offscreen renderer: {reason}");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let gpu = GpuViewer::install(&renderer.state).unwrap();
    let options = Options {
        engine: EngineConfig { temporary_projects: Some(dir.path().join("projects")), compute: Compute::CPU, ..EngineConfig::default() },
        files: Vec::new(),
    };
    let mut app = FloraStackApp::with_context(&ctx, options, Prefs::default(), Some(gpu));
    let token = "app-control-test-token-0123456789abcdef";
    let address = app.enable_control(&ctx, 0, token.into()).unwrap();
    let client = Client::new(address, token.into()).unwrap();
    let mut pump = || renderer.frame(&mut app, &ctx, [1000, 650]).unwrap();
    request(client.clone(), "ui.setView", json!({"tab": "export", "zoom": 2.0}), &mut pump).unwrap();
    let state = request(client.clone(), "ui.state", json!({}), &mut pump).unwrap();
    assert_eq!(state["tab"], "export");
    assert!(request(client.clone(), "ui.setView", json!({"zoom": 1e30}), &mut pump).is_err());
    request(client.clone(), "command.dispatch", json!({"command": "project.new", "params": {}}), &mut pump).unwrap();
    let files = common::frames(&dir.path().join("photos"));
    request(client.clone(), "command.dispatch", json!({"command": "project.addFrames", "params": {"paths": files}}), &mut pump).unwrap();
    for _ in 0..100 {
        let state = request(client.clone(), "ui.state", json!({}), &mut pump).unwrap();
        if state["frames"] == 4 && state["ready"] == true {
            break;
        }
    }
    let screenshot = dir.path().join("ui.png");
    let result = request(client.clone(), "ui.screenshot", json!({"path": screenshot}), &mut pump).unwrap();
    assert_eq!((result["width"].as_u64(), result["height"].as_u64()), (Some(1000), Some(650)));
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&screenshot).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!((info.width, info.height), (1000, 650));
    assert!(bytes.windows(4).any(|p| p != &bytes[..4]), "the image contains rendered UI");
    assert!(request(client.clone(), "ui.screenshot", json!({"path": screenshot}), &mut pump).is_err());
    assert!(request(client, "ui.close", json!({}), &mut pump).is_err(), "dirty projects require explicit discard");
    assert_eq!(app.shell.frames.len(), 4);
}
