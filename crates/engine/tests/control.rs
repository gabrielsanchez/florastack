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
                let result = automation::request(&mut engine, &pending.method, pending.params.clone()).map_err(|e| e.info());
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
    assert!(automation::request(&mut engine, "resources.read", json!({"uri": "file:///secret"})).is_err());
}
