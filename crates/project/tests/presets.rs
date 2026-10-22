//! Partial presets: each holds only some preferences, and they layer, later ones on top, tables merging key by key.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::TempDir;
use florastack_project::{Group, Preferences, Preset, PresetError, ProjectError, Value};
use serde_json::json;

/// Everything set, as a full preset would.
const BASE: &str = r#"
[preset]
name = "Defaults"

[develop]
white_balance = "as-shot"
exposure = 0.0

[align]
shift = true
scale = true
rotation = true
brightness = true
limits = { max_shift = 0.1, max_rotation = 2.0, max_scale = 0.05 }

[stack]
method = "pyramid"
pyramid = { grit = 0.2, levels = 7, decide_on = "luminance" }

[output]
format = "tiff"
depth = "u16"
profile = "input"
"#;

/// Alignment only, for a rail with steady light.
const RAIL: &str = r#"
[preset]
name = "Rail, steady light"
description = "Shift and scale only: no rotation, no brightness changes."
format = 1

[align]
rotation = false
brightness = false

[align.limits]
max_shift = 0.02
"#;

/// Output only.
const PNG_8: &str = "[output]\nformat = \"png\"\ndepth = \"u8\"\n";

/// One nested key of the stacking parameters.
const GRIT: &str = "[stack.pyramid]\ngrit = 0.8\n";

fn parse(text: &str) -> Preferences {
    Preset::parse(text).unwrap().preferences
}

fn group(p: &Preferences, g: Group) -> Value {
    Value::Object(p.group(g).clone())
}

#[test]
fn partial_presets_layer_in_order() {
    let layered = Preferences::layered([&parse(BASE), &parse(RAIL), &parse(PNG_8), &parse(GRIT)]);
    assert_eq!(
        group(&layered, Group::Align),
        json!({
            "shift": true, "scale": true, "rotation": false, "brightness": false,
            "limits": { "max_shift": 0.02, "max_rotation": 2.0, "max_scale": 0.05 },
        }),
        "the rail preset changes three keys, one of them nested, and leaves the rest"
    );
    assert_eq!(group(&layered, Group::Stack), json!({ "method": "pyramid", "pyramid": { "grit": 0.8, "levels": 7, "decide_on": "luminance" } }));
    assert_eq!(group(&layered, Group::Output), json!({ "format": "png", "depth": "u8", "profile": "input" }));
    assert_eq!(group(&layered, Group::Develop), json!({ "white_balance": "as-shot", "exposure": 0.0 }), "untouched");

    // The last layer wins.
    let reversed = Preferences::layered([&parse(RAIL), &parse(BASE)]);
    assert_eq!(reversed.group(Group::Align)["rotation"], json!(true));
    assert_eq!(reversed.group(Group::Align)["limits"]["max_shift"], json!(0.1));
    // A partial preset on nothing holds only its own keys.
    let alone = Preferences::layered([&parse(RAIL)]);
    assert!(alone.group(Group::Stack).is_empty() && alone.group(Group::Output).is_empty());
    assert_eq!(alone.group(Group::Align).len(), 3);
}

#[test]
fn values_other_than_tables_replace_what_is_below() {
    let below = parse("[stack]\nframes = [1, 2, 3]\nslab = { size = 15, overlap = 5 }\nmode = \"wide\"\n");
    let above = parse("[stack]\nframes = [9]\nslab = \"off\"\nmode = { crop = \"common\" }\n");
    let mut layered = below.clone();
    layered.layer(&above);
    assert_eq!(group(&layered, Group::Stack), json!({ "frames": [9], "slab": "off", "mode": { "crop": "common" } }));
    // An enum's table merges with the same variant's: one field changes, the other stays.
    let wb = parse("[develop.white_balance.custom]\ntemperature = 5000.0\ntint = 4.0\n");
    let warmer = parse("[develop.white_balance.custom]\ntemperature = 4300.0\n");
    let layered = Preferences::layered([&wb, &warmer]);
    assert_eq!(layered.group(Group::Develop)["white_balance"], json!({ "custom": { "temperature": 4300.0, "tint": 4.0 } }));
}

#[test]
fn presets_round_trip_through_toml_files() {
    let dir = TempDir::new("presets");
    let rail = Preset::parse(RAIL).unwrap();
    assert_eq!(rail.name.as_deref(), Some("Rail, steady light"));
    assert!(rail.description.as_deref().unwrap().starts_with("Shift and scale"));
    let path = dir.path().join("rail.toml");
    rail.save(&path).unwrap();
    assert_eq!(Preset::load(&path).unwrap(), rail);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("format = 1"), "{text}");

    // A partial preset saved from full preferences: only the chosen groups.
    let full = parse(BASE);
    let output_only = Preset { name: Some("Output".into()), description: None, preferences: full.only(&[Group::Output]) };
    let path = dir.path().join("output.toml");
    output_only.save(&path).unwrap();
    let back = Preset::load(&path).unwrap();
    assert_eq!(back.preferences.group(Group::Output), full.group(Group::Output));
    assert!(back.preferences.group(Group::Align).is_empty() && back.preferences.group(Group::Develop).is_empty());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("[align]") && text.contains("[output]"), "{text}");
    // Saving replaces the file whole.
    rail.save(&path).unwrap();
    assert_eq!(Preset::load(&path).unwrap(), rail);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "no temporary file is left");
}

#[test]
fn a_bad_preset_says_what_is_wrong() {
    let syntax = |text: &str| match Preset::parse(text) {
        Err(PresetError::Syntax(e)) => e.to_string(),
        other => panic!("{text:?}: {other:?}"),
    };
    let e = syntax("[align]\nrotation = false\n\n[devlop]\nexposure = 1.0\n");
    assert!(e.contains("devlop") && e.contains("line 4"), "{e}");
    assert!(syntax("[preset]\nnam = \"x\"\n").contains("nam"));
    assert!(syntax("[align\n").contains("line 1"));
    assert!(syntax("align = 3\n").contains("align"), "a group must be a table");
    assert!(matches!(Preset::parse("[preset]\nformat = 2\n"), Err(PresetError::Newer { found: 2, supported: 1 })));
    assert!(matches!(Preset::parse("[preset]\nformat = 0\n"), Err(PresetError::Format { found: 0 })));
    assert!(matches!(Preset::parse("[align]\nmax_shift = inf\n"), Err(PresetError::Value { key, .. }) if key == "align.max_shift"));
    assert!(matches!(Preset::parse(&format!("# {}\n", "x".repeat(1 << 20))), Err(PresetError::TooLarge { .. })));
    let newer = Preset::parse("[preset]\nformat = 2\n").unwrap_err().to_string();
    assert!(newer.contains("update FloraStack"), "{newer}");

    let dir = TempDir::new("bad-presets");
    let path = dir.path().join("latin1.toml");
    std::fs::write(&path, b"[preset]\nname = \"caf\xe9\"\n").unwrap();
    assert!(matches!(Preset::load(&path), Err(ProjectError::Preset { .. })));
    assert!(matches!(Preset::load(&dir.path().join("missing.toml")), Err(ProjectError::Read { .. })));
}

#[test]
fn a_deeply_nested_preset_is_an_error_not_a_crash() {
    for depth in [40, 10_000, 200_000] {
        let arrays = format!("[stack]\nx = {}1{}\n", "[".repeat(depth), "]".repeat(depth));
        assert!(Preset::parse(&arrays).is_err(), "{depth} arrays deep");
        let tables = format!("[stack]\nx = {}1{}\n", "{ a = ".repeat(depth), " }".repeat(depth));
        assert!(Preset::parse(&tables).is_err(), "{depth} inline tables deep");
    }
    let dotted = format!("[stack.{}]\nx = 1\n", vec!["a"; 100].join("."));
    assert!(Preset::parse(&dotted).is_err(), "100 dotted tables deep");
}
