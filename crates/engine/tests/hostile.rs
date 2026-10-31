//! Hostile parameters (AGENTS.md, Never crash): whatever JSON a client sends, every command answers with a result or
//! an error that says what is wrong, never a panic, and the project stays consistent.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::cell::RefCell;

use common::{TempDir, open_fixture};
use florastack_engine::{Dispatched, Engine, Registry, Value, json};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

#[test]
fn parameters_must_be_an_object() {
    let dir = TempDir::new("objects");
    let (mut engine, _) = open_fixture(dir.path(), Registry::standard());
    for name in Registry::standard().names() {
        for bad in [json!([]), json!(1), json!("x"), json!(true)] {
            let e = engine.dispatch(name, bad.clone()).unwrap_err();
            assert_eq!(e.code(), "params", "{name} {bad}: {e}");
        }
    }
}

#[test]
fn unknown_keys_are_refused_by_every_command() {
    let dir = TempDir::new("unknown-keys");
    let (mut engine, _) = open_fixture(dir.path(), Registry::standard());
    for name in Registry::standard().names() {
        let e = engine.dispatch(name, json!({ "bogus": 1 })).unwrap_err();
        assert_eq!(e.code(), "params", "{name}: {e}");
        assert!(e.info().message.contains("bogus"), "{name}: the error names the key: {}", e.info().message);
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One list of hostile parameters, command by command.
fn hostile_values_are_refused_with_what_is_wrong() {
    let dir = TempDir::new("values");
    let (mut engine, fixture) = open_fixture(dir.path(), Registry::standard());
    let frame = fixture.frames[0].get();
    let many: Vec<u64> = (0..100_001).collect();
    let long = "x".repeat(40 * 1024);
    let cases = [
        ("job.cancel", json!({ "job": -1 })),
        ("compute.set", json!({ "device": "fastest" })),
        ("compute.set", json!({ "device": 1 })),
        ("compute.set", json!({ "budgetMb": 1 })),
        ("compute.set", json!({ "budgetMb": -5 })),
        ("compute.set", json!({ "budgetMb": 1e30 })),
        ("batch.run", json!({ "input": "x", "output": "y", "preset": "p", "template": "../{stack}.tif" })),
        ("batch.run", json!({ "input": "x", "output": "y", "preset": "p", "template": "{unknown}.tif" })),
        ("batch.run", json!({ "input": "x", "output": "y", "preset": "p", "split": {"gap": -1} })),
        ("watch.run", json!({ "input": "x", "output": "y", "preset": "p", "idleSeconds": -1 })),
        ("watch.run", json!({ "input": "x", "output": "y", "preset": "p", "idleSeconds": 1e300 })),
        ("watch.run", json!({ "input": "x", "output": "y", "preset": "p", "settleMs": 0 })),
        ("watch.run", json!({ "input": "x", "output": "y", "preset": "p", "pollMs": 0 })),
        ("watch.run", json!({ "input": "x\u{0}", "output": "y", "preset": "p" })),
        ("job.cancel", json!({ "job": 1.5 })),
        ("job.cancel", json!({ "job": 1e30 })),
        ("project.moveFrames", json!({ "frames": [frame], "to": -1 })),
        ("project.moveFrames", json!({ "frames": [frame], "to": 1e30 })),
        ("project.removeFrames", json!({ "frames": [] })),
        ("project.removeFrames", json!({ "frames": [frame, frame] })),
        ("project.removeFrames", json!({ "frames": many })),
        ("project.rename", json!({ "name": "" })),
        ("project.rename", json!({ "name": "tab\there" })),
        ("project.rename", json!({ "name": "n".repeat(201) })),
        ("project.open", json!({ "path": "a\u{0}b" })),
        ("project.open", json!({ "path": long })),
        ("project.addFrames", json!({ "paths": [] })),
        ("project.findOrphans", json!({ "days": 99_999 })),
        ("preset.apply", json!({ "path": " " })),
        ("preset.apply", json!({ "path": "p.toml", "groups": ["colour"] })),
        ("develop.set", json!({ "exposure": 1e300 })),
        ("develop.set", json!({ "exposure": null })),
        ("align.set", json!({ "max_shift": 500 })),
        ("align.set", json!({ "framing": "widest" })),
        ("align.set", json!({ "min_sharp_fraction": 2 })),
        ("align.set", json!({ "resolution": 1e30 })),
        ("align.set", json!({ "reverse_order": "yes" })),
        ("align.set", json!({ "max_rotation": null })),
        ("align.set", json!({ "min_motion": 0 })),
        ("align.set", json!({ "min_motion": 1e9 })),
        ("align.set", json!({ "detect_defects": "yes" })),
        ("stack.set", json!({ "average": { "radius": -1 } })),
        ("stack.set", json!({ "average": { "smoothing": 1e30 } })),
        ("stack.set", json!({ "average": { "power": 9 } })),
        ("stack.set", json!({ "average": { "radius": "wide" } })),
        ("stack.set", json!({ "average": null })),
        ("stack.set", json!({ "pyramid": { "grit": 0.1 } })),
        ("stack.set", json!({ "pyramid": { "levels": 1e9 } })),
        ("stack.set", json!({ "pyramid": { "tone": "squash" } })),
        ("stack.set", json!({ "pyramid": { "coarse_limit": -3 } })),
        ("stack.set", json!({ "depthmap": {} })),
        ("stack.set", json!({ "depth_map": { "level": -1e30 } })),
        ("stack.set", json!({ "depth_map": { "threshold": "high" } })),
        ("stack.set", json!({ "depth_map": { "threshold": { "level": 1 } } })),
        ("stack.set", json!({ "depth_map": { "radius": 1e9 } })),
        ("stack.depthMap", json!({ "smoothing": 1e30 })),
        ("stack.depthMap", json!({ "percentile": 1e300 })),
        ("stack.depthMap", json!({ "threshold": "otsu" })),
        ("stack.depthMap", json!({ "name": "" })),
        ("stack.preview", json!({ "method": "depth-map", "radius": -1 })),
        ("stack.pyramid", json!({ "grit": -1 })),
        ("stack.pyramid", json!({ "rangeLimit": 1e30 })),
        ("stack.pyramid", json!({ "coarseLimit": 1e12 })),
        ("stack.pyramid", json!({ "levels": -1 })),
        ("stack.pyramid", json!({ "decisions": "hue" })),
        ("stack.pyramid", json!({ "name": "" })),
        ("stack.preview", json!({ "method": "pyramid", "grit": 0.1 })),
        ("stack.preview", json!({ "method": "pyramid", "smoothing": 2 })),
        ("stack.average", json!({ "radius": 1e9 })),
        ("stack.average", json!({ "smoothing": -0.5 })),
        ("stack.average", json!({ "power": 0 })),
        ("stack.average", json!({ "name": "" })),
        ("stack.average", json!({ "name": "a\nb" })),
        ("stack.preview", json!({ "radius": 65 })),
        ("stack.preview", json!({ "power": "high" })),
        ("stack.preview", json!({ "name": "x" })),
        ("stack.slabbed", json!({ "slabSize": 1 })),
        ("stack.slabbed", json!({ "slabSize": 1e12 })),
        ("stack.slabbed", json!({ "slabSize": 4, "overlap": 4 })),
        ("stack.slabbed", json!({ "overlap": -1 })),
        ("stack.slabbed", json!({ "overlap": 200_000 })),
        ("stack.slabbed", json!({ "slabs": { "method": "pyramid", "grit": 0.1 } })),
        ("stack.slabbed", json!({ "slabs": { "smoothing": 2 } })),
        ("stack.slabbed", json!({ "final": { "power": 2 } })),
        ("stack.slabbed", json!({ "final": { "method": "median" } })),
        ("stack.slabbed", json!({ "final": { "percentile": 1e300 } })),
        ("stack.slabbed", json!({ "slabs": null })),
        ("stack.slabbed", json!({ "reuse": "yes" })),
        ("stack.slabbed", json!({ "name": "" })),
        ("project.findStacks", json!({ "paths": [] })),
        ("project.findStacks", json!({ "paths": ["a\u{0}b"] })),
        ("project.findStacks", json!({ "paths": ["x"], "split": { "minScore": 2 } })),
        ("project.findStacks", json!({ "paths": ["x"], "split": { "gap": -5 } })),
        ("project.findStacks", json!({ "paths": ["x"], "split": { "maxShift": 1e300 } })),
        ("project.findStacks", json!({ "paths": ["x"], "split": { "minSimilarity": -2 } })),
        ("project.findStacks", json!({ "paths": ["x"], "split": { "max_shift": 5 } })),
        ("defects.import", json!({ "path": "" })),
        ("defects.import", json!({ "path": "a\u{0}b" })),
        ("defects.export", json!({ "path": " " })),
        ("defects.export", json!({ "path": "x.png", "overwrite": "yes" })),
        (
            "output.save",
            json!({ "output": 1, "path": "x.jpg", "spec": { "format": { "format": "jpeg", "quality": 0, "subsampling": "s444" }, "depth": "u8" } }),
        ),
        ("output.save", json!({ "output": 1, "path": "x.png", "spec": { "format": { "format": "png" }, "depth": "u12" } })),
        ("output.save", json!({ "output": 1, "path": "x.png", "spec": { "format": { "format": "gif" }, "depth": "u8" } })),
        (
            "output.save",
            json!({ "output": 1, "map": "../depth", "path": "x.tif", "spec": { "format": { "format": "tiff", "compression": "deflate" }, "depth": "u16", "profile": "untagged" } }),
        ),
        (
            "output.save",
            json!({ "output": 1, "map": "depth", "path": "x.tif", "spec": { "format": { "format": "tiff", "compression": "deflate" }, "depth": "u16" } }),
        ),
    ];
    for (name, params) in cases {
        let e = engine.dispatch(name, params.clone()).unwrap_err();
        assert_eq!(e.code(), "params", "{name} {}: {e}", params.to_string().chars().take(100).collect::<String>());
    }
    for (name, params) in [
        ("project.removeFrames", json!({ "frames": [frame, 9999] })),
        ("output.rename", json!({ "output": 9999, "name": "x" })),
        ("project.moveFrames", json!({ "frames": [9999], "to": 0 })),
    ] {
        assert_eq!(engine.dispatch(name, params).unwrap_err().code(), "notFound", "{name}");
    }
    assert_eq!(engine.project().unwrap().frames.len(), 3, "nothing changed");
}

#[test]
fn deep_or_huge_json_is_refused() {
    let dir = TempDir::new("deep");
    let mut engine = Engine::new(common::config(dir.path()));
    let deep = "[".repeat(100_000);
    let e = engine.dispatch_json("project.findOrphans", &deep).unwrap_err();
    assert_eq!(e.code(), "params", "{e}");
    let huge = format!(r#"{{ "days": 1, "x": "{}" }}"#, "a".repeat(17 << 20));
    let e = engine.dispatch_json("project.findOrphans", &huge).unwrap_err();
    assert!(e.to_string().contains("parameters"), "{e}");
    assert!(e.info().message.contains("16 MiB"), "{}", e.info().message);
    assert_eq!(engine.dispatch_json("project.findOrphans", "{ not json").unwrap_err().code(), "params");
}

/// Commands left out of the arbitrary-JSON test: they create, move or write files outside the test's folders when
/// their parameters happen to be valid paths, such as `"."`.
const WRITES_FILES: [&str; 7] = ["project.new", "project.trashOrphans", "preset.save", "output.save", "defects.export", "batch.run", "watch.run"];

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_filter("JSON numbers are finite", |f| f.is_finite()).prop_map(Value::from),
        "[a-z0-9 ./\\-]{0,12}".prop_map(Value::from),
    ]
}

fn value(keys: Vec<String>) -> impl Strategy<Value = Value> {
    leaf().prop_recursive(3, 32, 4, move |inner| {
        let key = prop_oneof![proptest::sample::select(keys.clone()), "[a-z]{1,8}"];
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::btree_map(key, inner, 0..5).prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

#[test]
fn arbitrary_json_never_crashes_a_command() {
    let dir = TempDir::new("arbitrary");
    let (engine, _) = open_fixture(dir.path(), Registry::standard());
    let engine = RefCell::new(engine);
    let registry = Registry::standard();
    let names: Vec<&'static str> = registry.names().into_iter().filter(|n| !WRITES_FILES.contains(n)).collect();
    // The parameter names of every command, so that the objects generated often look like real parameters.
    let mut keys: Vec<String> = registry
        .list()
        .iter()
        .flat_map(|c| c.params["properties"].as_object().map(|p| p.keys().cloned().collect::<Vec<_>>()).unwrap_or_default())
        .collect();
    keys.sort();
    keys.dedup();
    let mut runner = TestRunner::new(Config { cases: 300, ..Config::default() });
    let strategy = (
        proptest::sample::select(names),
        prop_oneof![
            value(keys.clone()),
            proptest::collection::btree_map(proptest::sample::select(keys), leaf(), 0..4).prop_map(|m| Value::Object(m.into_iter().collect()))
        ],
    );
    runner
        .run(&strategy, |(name, params)| {
            let mut engine = engine.borrow_mut();
            let outcome = match engine.dispatch(name, params.clone()) {
                Ok(Dispatched::Done(v)) => Ok(v),
                Ok(Dispatched::Started(job)) => engine.wait(job),
                Err(e) => Err(e),
            };
            if let Err(e) = &outcome {
                prop_assert_ne!(e.code(), "internal", "{} {}: {}", name, params, e);
            }
            if let Some(project) = engine.project() {
                prop_assert!(project.validate().is_ok(), "{} {} left the project inconsistent", name, params);
            }
            Ok(())
        })
        .unwrap();
}
