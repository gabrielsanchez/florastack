//! Authentication, numeric loopback validation, JSON framing and registry dispatch across a real socket.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use florastack_engine::{
    Compute, Engine, EngineConfig, automation,
    control::{self, Client, Server},
    json,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "control-test-token-0123456789abcdef";

#[test]
fn control_addresses_and_token_files_are_validated() {
    for text in ["0.0.0.0:1234", "192.168.1.2:1234", "example.com:80", "localhost:1234", "[::]:1234"] {
        assert!(control::address(text).is_err(), "{text}");
    }
    for text in ["127.0.0.1:1234", "[::1]:1234"] {
        assert!(control::address(text).is_ok());
    }
    assert!(Client::new("192.168.0.1:1234".parse().unwrap(), TOKEN.into()).is_err());
    assert!(Server::start(0, "short".into(), Arc::new(|| {})).is_err());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("token");
    for token in ["short".to_owned(), "x".repeat(259), format!("{}\r\nextra", "x".repeat(256)), format!("{TOKEN} ")] {
        std::fs::write(&file, token).unwrap();
        assert!(control::read_token(&file).is_err());
    }
    std::fs::write(&file, format!("{TOKEN}\r\n")).unwrap();
    assert_eq!(control::read_token(&file).unwrap(), TOKEN);
}

#[tokio::test(flavor = "current_thread")]
async fn authentication_precedes_dispatch_and_valid_requests_use_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(0, TOKEN.into(), Arc::new(|| {})).unwrap();
    assert!(server.address.ip().is_loopback());
    let wrong = Client::new(server.address, "wrong-control-token-0123456789abcdef".into()).unwrap();
    assert_eq!(wrong.request("command.dispatch", json!({"command": "project.new"})).await.unwrap().unwrap_err().code, "unauthorized");
    assert!(server.try_recv().is_none(), "unauthenticated requests never reach the host");
    let mut malformed = tokio::net::TcpStream::connect(server.address).await.unwrap();
    malformed.write_all(b"{broken}\n").await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), malformed.read_to_end(&mut bytes)).await.unwrap().unwrap();
    assert_eq!(bytes, [] as [u8; 0]);
    let client = Client::new(server.address, TOKEN.into()).unwrap();
    let task = tokio::spawn(async move { client.request("command.dispatch", json!({"command": "project.new", "params": {}})).await });
    let mut engine =
        Engine::new(EngineConfig { temporary_projects: Some(dir.path().join("projects")), compute: Compute::CPU, ..EngineConfig::default() });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(pending) = server.try_recv() {
                let result =
                    automation::request(&mut engine, automation::Access::Full, &pending.method, pending.params.clone()).map_err(|e| e.info());
                pending.respond(result);
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    assert!(task.await.unwrap().unwrap().unwrap()["result"]["path"].is_string());
    assert!(engine.project().is_some());
    assert!(automation::request(&mut engine, automation::Access::Full, "resources.read", json!({"uri": "file:///secret"})).is_err());
}

/// A host holds each connection to an access level: its clients see and may run only the commands of that level, by
/// what the commands declare they change.
#[tokio::test(flavor = "current_thread")]
async fn clients_are_held_to_the_hosts_access_level() {
    use automation::Access;
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(0, TOKEN.into(), Arc::new(|| {})).unwrap();
    let mut engine =
        Engine::new(EngineConfig { temporary_projects: Some(dir.path().join("projects")), compute: Compute::CPU, ..EngineConfig::default() });
    // One request over the socket, answered by the host at `access`.
    let mut ask = async |access: Access, method: &'static str, params: florastack_engine::Value| {
        let client = Client::new(server.address, TOKEN.into()).unwrap();
        let task = tokio::spawn(async move { client.request(method, params).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(pending) = server.try_recv() {
                    let result = automation::request(&mut engine, access, &pending.method, pending.params.clone()).map_err(|e| e.info());
                    pending.respond(result);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        task.await.unwrap().unwrap()
    };
    let names = |listed: &florastack_engine::Value| -> Vec<String> {
        listed.as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_owned()).collect()
    };
    let dispatch = |command: &str, params: florastack_engine::Value| json!({"command": command, "params": params});

    // What each level lists: queries; then what changes the project; then everything.
    let read = names(&ask(Access::Read, "commands.list", json!({})).await.unwrap());
    let project = names(&ask(Access::Project, "commands.list", json!({})).await.unwrap());
    let full = names(&ask(Access::Full, "commands.list", json!({})).await.unwrap());
    let has = |list: &[String], name: &str| list.iter().any(|n| n == name);
    assert!(read.len() < project.len() && project.len() < full.len(), "{} {} {}", read.len(), project.len(), full.len());
    assert_eq!(full.len(), florastack_engine::Registry::standard().list().len());
    for (name, levels) in [
        ("project.info", [true, true, true]),
        ("project.open", [false, true, true]),
        ("stack.pyramid", [false, true, true]),
        ("stack.preview", [false, true, true]),
        ("project.new", [false, false, true]),
        ("export.run", [false, false, true]),
        ("output.save", [false, false, true]),
        ("preset.save", [false, false, true]),
        ("project.trashOrphans", [false, false, true]),
    ] {
        assert_eq!([has(&read, name), has(&project, name), has(&full, name)], levels, "{name}");
    }

    // A command beyond the level is refused before it runs, and says what it needs.
    let e = ask(Access::Project, "command.dispatch", dispatch("project.new", json!({}))).await.unwrap_err();
    assert_eq!(e.code, "forbidden", "{e:?}");
    assert!(e.message.contains("`full`") && e.message.contains("project.new"), "{}", e.message);
    assert!(!dir.path().join("projects").exists(), "nothing was made");
    let made = ask(Access::Full, "command.dispatch", dispatch("project.new", json!({}))).await.unwrap();
    let path = made["result"]["path"].as_str().unwrap().to_owned();
    // A reader reads, and changes nothing: not the project, not through an old name of a command either.
    assert_eq!(ask(Access::Read, "command.dispatch", dispatch("project.info", json!({}))).await.unwrap()["result"]["path"], path);
    for (command, params) in [("project.rename", json!({"name": "Mine"})), ("project.close", json!({})), ("job.cancel", json!({"job": 1}))] {
        let e = ask(Access::Read, "command.dispatch", dispatch(command, params)).await.unwrap_err();
        assert_eq!(e.code, "forbidden", "{command}: {e:?}");
    }
    assert_eq!(
        ask(Access::Read, "resources.read", json!({"uri": automation::PROJECT_URI})).await.unwrap()["contents"][0]["uri"],
        automation::PROJECT_URI
    );
    // The project's level changes the project, and writes nothing outside it.
    ask(Access::Project, "command.dispatch", dispatch("project.rename", json!({"name": "Mine"}))).await.unwrap();
    let elsewhere = dir.path().join("elsewhere");
    for (command, params) in [("project.saveAs", json!({"folder": elsewhere})), ("preset.save", json!({"path": elsewhere.join("p.toml")}))] {
        let e = ask(Access::Project, "command.dispatch", dispatch(command, params)).await.unwrap_err();
        assert_eq!(e.code, "forbidden", "{command}: {e:?}");
    }
    assert!(!elsewhere.exists());
    // A name that is no command is still told so, at any level.
    assert_eq!(ask(Access::Read, "command.dispatch", dispatch("project.explode", json!({}))).await.unwrap_err().code, "unknownCommand");
    for (text, level) in [("read", Access::Read), ("project", Access::Project), ("full", Access::Full)] {
        assert_eq!((text.parse::<Access>().unwrap(), level.to_string().as_str()), (level, text));
    }
    assert!("admin".parse::<Access>().is_err());
}
