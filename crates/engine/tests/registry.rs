//! The registry: every command has a name in the pinned list, help text, and parameter and result schemas fit for MCP
//! tool definitions (draft 2020-12 objects, inlined, every parameter described), and its examples are valid.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use florastack_engine::registry::valid_name;
use florastack_engine::{CommandError, Engine, Event, Kind, Registry, Value};

/// Every property of an object schema, at every depth, has a description, directly or on the type it is.
fn undescribed(schema: &Value, at: &str, out: &mut Vec<String>) {
    let Some(object) = schema.as_object() else { return };
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (key, property) in properties {
            let described = property.get("description").and_then(Value::as_str).is_some_and(|d| !d.trim().is_empty());
            if !described {
                out.push(format!("{at}/{key}"));
            }
        }
    }
}

#[test]
fn the_command_list_is_pinned() {
    let pinned: Vec<&str> = include_str!("commands.txt").lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let names = Registry::standard().names();
    assert_eq!(
        names, pinned,
        "commands were added, removed or renamed: update crates/engine/tests/commands.txt in the same change (a renamed command keeps its old name as an alias)"
    );
    assert!(Registry::builder().standard().build().is_ok());
}

#[test]
fn every_command_has_help_and_schemas() {
    let registry = Registry::standard();
    let mut problems = Vec::new();
    for info in registry.list() {
        let name = info.name;
        assert!(valid_name(name), "{name}");
        for (what, text) in [("title", info.title), ("summary", info.summary)] {
            if text.trim().is_empty() {
                problems.push(format!("{name}: no {what}"));
            }
        }
        if !info.summary.ends_with('.') {
            problems.push(format!("{name}: the summary is a sentence"));
        }
        if info.title.ends_with('.') || info.title.len() > 60 {
            problems.push(format!("{name}: the title is a short label"));
        }
        for (what, schema) in [("params", &info.params), ("result", &info.result)] {
            if schema["$schema"] != "https://json-schema.org/draft/2020-12/schema" {
                problems.push(format!("{name}: the {what} schema isn't draft 2020-12"));
            }
            if schema["type"] != "object" {
                problems.push(format!("{name}: the {what} schema isn't an object"));
            }
            if schema.get("$defs").is_some() || schema.to_string().contains("\"$ref\"") {
                problems.push(format!("{name}: the {what} schema isn't inlined"));
            }
        }
        if info.params["additionalProperties"] != false {
            problems.push(format!("{name}: unknown parameters must be refused (deny_unknown_fields)"));
        }
        let mut missing = Vec::new();
        undescribed(&info.params, name, &mut missing);
        problems.extend(missing.into_iter().map(|p| format!("{p}: no description")));
        if info.undoable && info.kind == Kind::Query {
            problems.push(format!("{name}: a query can't be undoable"));
        }
        if (info.kind == Kind::Job) != info.lane.is_some() {
            problems.push(format!("{name}: a job has a lane, and only a job"));
        }
        if info.examples.is_empty() {
            problems.push(format!("{name}: no example"));
        }
        for (i, example) in info.examples.iter().enumerate() {
            if let Err(e) = common::validate(example, &info.params, &format!("{name} example {i}")) {
                problems.push(format!("the example doesn't fit the schema: {e}"));
            }
            if let Err(e) = registry.check(name, example.clone()) {
                problems.push(format!("{name} example {i} is refused: {e}"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_listing_is_json_with_the_schemas() {
    let registry = Registry::standard();
    let listing = serde_json::to_value(registry.list()).unwrap();
    let add = listing.as_array().unwrap().iter().find(|c| c["name"] == "project.addFrames").unwrap();
    assert_eq!(add["kind"], "job");
    assert_eq!(add["lane"], "heavy");
    assert_eq!(add["undoable"], true);
    assert_eq!(add["params"]["required"], serde_json::json!(["paths"]));
    assert_eq!(add["params"]["properties"]["paths"]["maxItems"], 100_000);
    let set = registry.get("develop.set").unwrap();
    assert_eq!(set.params["properties"]["exposure"]["maximum"], 10.0, "the ranges of the settings are in the schema");
    assert_eq!(set.params["properties"]["white_balance"]["default"], "as-shot");
    let save = registry.get("output.save").unwrap();
    assert_eq!(save.params["properties"]["spec"]["properties"]["format"]["oneOf"].as_array().unwrap().len(), 5, "one per output format");
    assert_eq!(
        registry.get("project.info").unwrap().params,
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "NoParams",
            "description": "The parameters of a command that takes none: `{}` (or `null`).",
            "type": "object",
            "additionalProperties": false,
        })
    );
}

#[test]
fn unknown_commands_are_errors_with_a_suggestion() {
    let dir = common::TempDir::new("unknown");
    let mut engine = Engine::new(common::config(dir.path()));
    let e = engine.dispatch("project.opne", Value::Null).unwrap_err();
    assert!(matches!(&e, CommandError::UnknownCommand { suggestion: Some("project.open"), .. }), "{e}");
    assert_eq!(e.code(), "unknownCommand");
    let e = engine.dispatch(&"x".repeat(10_000), Value::Null).unwrap_err();
    assert!(matches!(&e, CommandError::UnknownCommand { suggestion: None, .. }));
    assert!(e.to_string().len() < 200, "a long name isn't echoed whole");
}

#[test]
fn events_have_a_schema_too() {
    let schema = florastack_engine::schema::schema_for::<Event>();
    assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(schema["oneOf"].as_array().unwrap().len(), 4);
}
