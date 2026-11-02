//! The real executable over MCP stdio, both headless and bridged to a control host.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use florastack_engine::{Compute, Engine, EngineConfig, Registry, Value, automation, control, json};
use rmcp::{
    ServiceExt,
    model::*,
    service::{Peer, RoleClient},
    transport::TokioChildProcess,
};

async fn tool(peer: &Peer<RoleClient>, name: &str, params: Value) -> Value {
    let request = CallToolRequestParams::new(name.to_owned()).with_arguments(params.as_object().unwrap().clone());
    let result = peer.call_tool(request).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{name}: {result:?}");
    result.structured_content.unwrap()
}

fn frames(dir: &Path) -> Vec<std::path::PathBuf> {
    (0..4_u32)
        .map(|focus| {
            let pixels: Vec<u8> = (0..72_u32)
                .flat_map(|y| {
                    (0..96_u32).flat_map(move |x| {
                        let detail = if x / 24 == focus { (x + y) % 2 * 65 } else { 32 };
                        let value = u8::try_from(60 + (x * 7 + y * 13 + x * y % 29) % 60 + detail).unwrap();
                        [value, value + 7, value - 9]
                    })
                })
                .collect();
            let path = dir.join(format!("focus {focus}.png"));
            let mut png = png::Encoder::new(std::fs::File::create(&path).unwrap(), 96, 72);
            png.set_color(png::ColorType::Rgb);
            png.set_depth(png::BitDepth::Eight);
            png.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            png.write_header().unwrap().write_image_data(&pixels).unwrap();
            path
        })
        .collect()
}

async fn exercise(bridged: bool) {
    let dir = tempfile::tempdir().unwrap();
    let files = frames(dir.path());
    let mut command = tokio::process::Command::new(assert_cmd::cargo::cargo_bin!());
    command.args(["--device", "cpu", "--temporary-projects"]).arg(dir.path().join("projects")).arg("mcp");
    command.kill_on_drop(true);
    let stop = Arc::new(AtomicBool::new(false));
    let mut host = None;
    if bridged {
        let token = "integration-test-token-0123456789abcdef";
        let listener = control::Server::start(0, token.into(), Arc::new(|| {})).unwrap();
        let token_file = dir.path().join("token");
        std::fs::write(&token_file, token).unwrap();
        command.arg("--connect").arg(listener.address.to_string()).arg("--token-file").arg(token_file);
        let stopping = stop.clone();
        let projects = dir.path().join("desktop-projects");
        // Emulate the desktop's owning thread. This is its engine, not the CLI's otherwise unused engine.
        let (address_tx, address_rx) = std::sync::mpsc::sync_channel(1);
        host = Some(std::thread::spawn(move || {
            let mut engine = Engine::new(EngineConfig { temporary_projects: Some(projects), compute: Compute::CPU, ..EngineConfig::default() });
            address_tx.send(()).unwrap();
            while !stopping.load(Ordering::Relaxed) {
                engine.poll();
                if let Some(pending) = listener.try_recv() {
                    let result = automation::request(&mut engine, &pending.method, pending.params.clone()).map_err(|e| e.info());
                    pending.respond(result);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            engine.project().map_or(0, |p| p.outputs.len())
        }));
        address_rx.recv().unwrap();
    }
    let transport = TokioChildProcess::new(command).unwrap();
    let client = ().serve(transport).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();
    let registry = Registry::standard();
    assert_eq!(tools.len(), registry.list().len());
    for command in registry.list() {
        let listed = tools.iter().find(|t| t.name == command.name).unwrap();
        assert_eq!(json!(listed.input_schema), command.params);
        assert_eq!(json!(listed.output_schema), command.result);
    }
    tool(&client, "project.new", json!({})).await;
    let missing = client.call_tool(CallToolRequestParams::new("stack.pyramid")).await.unwrap();
    assert_eq!(missing.is_error, Some(true), "engine errors are tool results, not a transport failure");
    tool(&client, "project.addFrames", json!({"paths": files})).await;
    tool(&client, "align.run", json!({})).await;
    let output = tool(&client, "stack.pyramid", json!({})).await;
    assert!(output["output"].is_number());
    let resources = client.list_all_resources().await.unwrap();
    assert_eq!(resources.len(), 2);
    let project = client.read_resource(ReadResourceRequestParams::new(automation::PROJECT_URI)).await.unwrap();
    let contents = json!(project);
    let project: Value = serde_json::from_str(contents["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(project["project"]["outputs"].as_array().unwrap().len(), 1);
    let thumbnail = resources.iter().find(|r| r.mime_type.as_deref() == Some("image/jpeg")).unwrap();
    let contents = client.read_resource(ReadResourceRequestParams::new(thumbnail.uri.clone())).await.unwrap();
    assert!(json!(contents)["contents"][0]["blob"].as_str().unwrap().len() > 100);
    assert!(client.read_resource(ReadResourceRequestParams::new("florastack://outputs/../../secret/thumbnail")).await.is_err());
    client.cancel().await.unwrap();
    stop.store(true, Ordering::Relaxed);
    if let Some(host) = host {
        assert_eq!(host.join().unwrap(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stdio_lists_registry_tools_and_stacks_headless() {
    tokio::time::timeout(Duration::from_secs(60), exercise(false)).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn stdio_bridge_uses_the_hosts_project_and_outputs() {
    tokio::time::timeout(Duration::from_secs(60), exercise(true)).await.unwrap();
}
