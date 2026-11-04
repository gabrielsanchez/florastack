//! The book's engine-command reference (`book/src/reference/commands.md`) is generated from the registry: this test
//! renders it from every command's name, texts, flags, parameter and result schemas and examples (as
//! `florastack-cli commands` prints them), and compares it with the committed page, so that the page lists every
//! command as it is. After adding or changing a command, write the page again with
//! `cargo xtask book --update-reference`, which runs this test with `FLORASTACK_UPDATE_REFERENCE` set.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use florastack_engine::{Registry, Value};

/// Set to write the page instead of checking it.
const UPDATE: &str = "FLORASTACK_UPDATE_REFERENCE";

fn page_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../book/src/reference/commands.md")
}

/// One line of text from a description or help: whitespace runs (line continuations in the source) become one space,
/// and table cells keep their pipes escaped.
fn text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cell(s: &str) -> String {
    text(s).replace('|', "\\|")
}

fn json(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

/// The alternatives of a schema that isn't `null` itself, through `anyOf` and `oneOf`.
fn alternatives(schema: &Value) -> Vec<&Value> {
    let list = schema.get("anyOf").or_else(|| schema.get("oneOf")).and_then(Value::as_array);
    match list {
        Some(list) => list.iter().filter(|s| s.get("type") != Some(&Value::from("null"))).flat_map(alternatives).collect(),
        None => vec![schema],
    }
}

/// A schema's type, as the reference shows it: `string`, `integer, from 0 to 100`, `` `"auto"` | `"cpu"` ``,
/// `array of string`, `object`.
fn type_of(schema: &Value) -> String {
    if let Some(value) = schema.get("const") {
        return format!("`{}`", json(value));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values.iter().map(|v| format!("`{}`", json(v))).collect::<Vec<_>>().join(" \\| ");
    }
    let options = alternatives(schema);
    if options.len() != 1 || !std::ptr::eq(options[0], schema) {
        let mut shown: Vec<String> = options.into_iter().map(type_of).collect();
        shown.dedup();
        return shown.join(" \\| ");
    }
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).filter(|t| *t != "null").collect(),
        _ => Vec::new(),
    };
    let mut shown = types
        .iter()
        .map(|t| match *t {
            "array" => match (schema.get("items"), schema.get("prefixItems").and_then(Value::as_array)) {
                (_, Some(tuple)) => format!("[{}]", tuple.iter().map(type_of).collect::<Vec<_>>().join(", ")),
                (Some(items), None) => format!("array of {}", type_of(items)),
                (None, None) => "array".to_owned(),
            },
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" or ");
    if shown.is_empty() {
        "any".clone_into(&mut shown);
    }
    let number = |key: &str| schema.get(key).filter(|v| v.is_number()).map(json);
    match (number("minimum"), number("maximum")) {
        (Some(min), Some(max)) => write!(shown, ", from {min} to {max}").unwrap(),
        (Some(min), None) if min != "0" => write!(shown, ", at least {min}").unwrap(),
        (None, Some(max)) => write!(shown, ", at most {max}").unwrap(),
        _ => {}
    }
    shown
}

/// The table rows of an object schema's properties, nested objects' properties under `parent.child` (`depth` levels
/// at most), and for parameters (`params`) the required ones marked.
fn rows(schema: &Value, prefix: &str, depth: usize, params: bool, out: &mut Vec<[String; 3]>) {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else { return };
    let required: Vec<&str> =
        schema.get("required").and_then(Value::as_array).map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    for (name, property) in properties {
        let path = format!("{prefix}{name}");
        let inner = alternatives(property);
        let description =
            property.get("description").or_else(|| inner.iter().find_map(|s| s.get("description"))).and_then(Value::as_str).unwrap_or_default();
        let mut notes = cell(description);
        if params && required.contains(&name.as_str()) {
            notes = format!("**Required.** {notes}");
        }
        if let Some(default) = property.get("default").filter(|d| !d.is_null()) {
            write!(notes, " Default: `{}`.", json(default).replace('|', "\\|")).unwrap();
        }
        out.push([format!("`{path}`"), type_of(property), notes.trim().to_owned()]);
        let objects: Vec<&&Value> = inner.iter().filter(|s| s.get("properties").is_some()).collect();
        if depth > 0
            && let [object] = objects.as_slice()
        {
            rows(object, &format!("{path}."), depth - 1, params, out);
        }
    }
}

/// A table of a parameter (`params`) or result schema's properties: parameters with their nested objects' too.
fn table(out: &mut String, title: &str, schema: &Value, params: bool) {
    let mut list = Vec::new();
    rows(schema, "", if params { 3 } else { 0 }, params, &mut list);
    if list.is_empty() {
        writeln!(out, "{title}: none.\n").unwrap();
        return;
    }
    writeln!(out, "{title}:\n\n| Name | Type | Description |\n|---|---|---|").unwrap();
    for [name, kind, notes] in list {
        writeln!(out, "| {name} | {kind} | {notes} |").unwrap();
    }
    out.push('\n');
}

fn render(registry: &Registry) -> String {
    let commands: Vec<Value> = registry.list().into_iter().map(|info| serde_json::to_value(info).unwrap()).collect();
    let mut areas: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for command in &commands {
        let name = command["name"].as_str().unwrap();
        let area = name.split_once('.').map_or(name, |(area, _)| area);
        areas.entry(area.to_owned()).or_default().push(command);
    }
    let mut out = String::from(
        "<!-- Generated from the engine's command registry by crates/engine/tests/reference.rs. Don't edit it by hand: \
         after changing a command, run `cargo xtask book --update-reference`. -->\n\n# Engine commands\n\n",
    );
    out.push_str(
        "Everything FloraStack does is one of these commands: the desktop app's buttons and menus, the command-line \
         tool's subcommands, the command palette (Ctrl+K), MCP tools, the desktop control channel and the Python \
         package's `Project.run` all call them. Each takes its parameters as a JSON object and returns a JSON object. \
         `florastack-cli run` runs them on a project, and `florastack-cli commands` prints this list as JSON, with \
         the complete schemas. [Automation, MCP and Python](../user/automation.md) shows how to call them.\n\n\
         - A **query** reads and returns at once; an **instant** command changes the session and returns; a **job** \
         runs in the background, on one of the engine's lanes, and returns its result when it ends.\n\
         - An **undoable** command's changes to the project are one undo step.\n\
         - Parameters left out keep their defaults; unknown parameters are refused.\n\n",
    );
    out.push_str("| Area | Commands |\n|---|---|\n");
    for (area, list) in &areas {
        let names: Vec<String> =
            list.iter().map(|c| format!("[`{0}`](#{1})", c["name"].as_str().unwrap(), anchor(c["name"].as_str().unwrap()))).collect();
        writeln!(out, "| [{area}](#{area}) | {} |", names.join(", ")).unwrap();
    }
    out.push('\n');
    for (area, list) in &areas {
        writeln!(out, "## {area}\n").unwrap();
        for command in list {
            command_section(&mut out, command);
        }
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// The heading id that mdBook and GitHub give `` ### `align.get` ``: the name without its dot.
fn anchor(name: &str) -> String {
    name.to_lowercase().replace('.', "")
}

fn command_section(out: &mut String, command: &Value) {
    let s = |key: &str| command[key].as_str().unwrap_or_default();
    writeln!(out, "### `{}`\n\n**{}.** {}\n", s("name"), text(s("title")), text(s("summary"))).unwrap();
    if !s("help").trim().is_empty() {
        writeln!(out, "{}\n", text(s("help"))).unwrap();
    }
    let runs = match (s("kind"), s("lane")) {
        ("job", lane) => format!("Job, on the {lane} lane"),
        ("instant", _) => "Instant".to_owned(),
        _ => "Query".to_owned(),
    };
    let requires = match s("requires") {
        "project" => "An open project",
        "writableProject" => "A project open for editing",
        _ => "Nothing",
    };
    let changes = match s("effects") {
        "project" => "The project or the session",
        "files" => "Files outside the project",
        _ => "Nothing",
    };
    let undoable = if command["undoable"].as_bool() == Some(true) { "Yes" } else { "No" };
    writeln!(out, "| Runs | Undoable | Requires | Changes |\n|---|---|---|---|\n| {runs} | {undoable} | {requires} | {changes} |\n").unwrap();
    table(out, "Parameters", &command["params"], true);
    table(out, "Result", &command["result"], false);
    let examples: Vec<String> = command["examples"].as_array().map(|e| e.iter().map(json).collect()).unwrap_or_default();
    if !examples.is_empty() {
        let label = if examples.len() == 1 { "Example" } else { "Examples" };
        writeln!(out, "{label}:\n\n```json\n{}\n```\n", examples.join("\n")).unwrap();
    }
}

#[test]
fn the_command_reference_is_the_registry() {
    let registry = Registry::standard();
    let page = render(&registry);
    let path = page_path();
    if std::env::var_os(UPDATE).is_some() {
        std::fs::write(&path, &page).unwrap();
        return;
    }
    // A Windows checkout may have CRLF line endings (core.autocrlf).
    let committed = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
    let missing: Vec<&str> = registry.names().into_iter().filter(|name| !committed.contains(&format!("### `{name}`"))).collect();
    assert!(missing.is_empty(), "{} lacks these commands: {missing:?}; run `cargo xtask book --update-reference`", path.display());
    if committed != page {
        let line =
            committed.lines().zip(page.lines()).position(|(a, b)| a != b).unwrap_or_else(|| committed.lines().count().min(page.lines().count()));
        panic!(
            "{} is out of date from line {}: run `cargo xtask book --update-reference` and commit the page\n  committed: {:?}\n  generated: {:?}",
            path.display(),
            line + 1,
            committed.lines().nth(line).unwrap_or("(end)"),
            page.lines().nth(line).unwrap_or("(end)")
        );
    }
}

#[test]
fn types_render_as_the_reference_shows_them() {
    let schema: Value = serde_json::json!({ "type": ["integer", "null"], "minimum": 64, "maximum": 1024 });
    assert_eq!(type_of(&schema), "integer, from 64 to 1024");
    let choice = serde_json::json!({ "anyOf": [{ "oneOf": [{ "const": "auto" }, { "const": "cpu" }] }, { "type": "null" }] });
    assert_eq!(type_of(&choice), "`\"auto\"` \\| `\"cpu\"`");
    let list = serde_json::json!({ "type": "array", "items": { "type": "string" } });
    assert_eq!(type_of(&list), "array of string");
    let pair = serde_json::json!({ "type": "array", "prefixItems": [{ "type": "number" }, { "type": "number" }] });
    assert_eq!(type_of(&pair), "[number, number]");
    assert_eq!(type_of(&serde_json::json!({ "type": "integer", "minimum": 0 })), "integer", "unsigned integers' 0 isn't shown");
    assert_eq!(anchor("stack.depthMap"), "stackdepthmap");
    assert_eq!(cell("a |  b\n c"), "a \\| b c");
}
