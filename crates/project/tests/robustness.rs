//! Never crash: a damaged, hostile or newer project file gives an error the user can act on, never a panic, and
//! nothing that reads it can be made to leave the project folder or to allocate without bound.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::TempDir;
use florastack_project::{ProjectError, ProjectFolder, SchemaError, schema};
use proptest::prelude::*;

/// A small valid project file, in the current version.
fn valid() -> String {
    let dir = TempDir::new("valid");
    let (folder, _) = ProjectFolder::create(dir.path(), "Valid").unwrap();
    std::fs::read_to_string(folder.project_file()).unwrap()
}

/// Opens a project folder holding `bytes` as its project file.
fn open(bytes: &[u8]) -> ProjectError {
    let dir = TempDir::new("hostile");
    let root = dir.path().join("hostile.florastack");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("project.json"), bytes).unwrap();
    match ProjectFolder::open(&root) {
        Ok(_) => panic!("opened {:?}", String::from_utf8_lossy(bytes)),
        Err(e) => e,
    }
}

const HASH: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn with(field: &str) -> String {
    format!(
        r#"{{"format": "florastack-project", "version": 3, "name": null, "created": "2026-10-21T00:00:00Z",
            "modified": "2026-10-21T00:00:00Z", "next_id": 5, {field}}}"#
    )
}

#[test]
fn a_newer_project_file_asks_for_an_update() {
    let e = open(br#"{"format": "florastack-project", "version": 4, "name": "Future", "frames": {"in": "a new shape"}}"#);
    assert!(matches!(e, ProjectError::Newer { found: 4, supported: 3, .. }), "{e:?}");
    assert!(e.to_string().contains("update FloraStack"), "{e}");
    let e = open(br#"{"format": "florastack-project", "version": 18446744073709551615}"#);
    assert!(matches!(e, ProjectError::Newer { .. }), "{e:?}");
}

#[test]
fn damaged_and_hostile_files_are_errors() {
    let damaged = |bytes: &[u8]| match open(bytes) {
        ProjectError::Damaged { source, .. } => source,
        other => panic!("{:?}: {other:?}", String::from_utf8_lossy(bytes)),
    };
    assert!(matches!(damaged(b""), SchemaError::Syntax(_)));
    assert!(matches!(damaged(b"\xff\xfe{}"), SchemaError::Syntax(_)));
    assert!(matches!(damaged(br#"{"format": "florastack-project", "version": 1"#), SchemaError::Syntax(_)), "truncated");
    assert!(matches!(damaged(b"[]"), SchemaError::NotAProject));
    assert!(matches!(damaged(br#"{"format": "photoshop", "version": 1}"#), SchemaError::NotAProject));
    assert!(matches!(damaged(br#"{"format": "florastack-project", "version": "1"}"#), SchemaError::NoVersion));
    assert!(matches!(damaged(br#"{"format": "florastack-project"}"#), SchemaError::NoVersion));
    // Unknown, missing or ill-typed fields name what is wrong and where.
    let SchemaError::Content { source, .. } = damaged(with(r#""frams": []"#).as_bytes()) else { panic!() };
    assert!(source.to_string().contains("frams") && source.to_string().contains("line"), "{source}");
    assert!(matches!(damaged(br#"{"format": "florastack-project", "version": 3, "name": null}"#), SchemaError::Content { .. }));
    assert!(matches!(damaged(with(r#""frames": "none""#).as_bytes()), SchemaError::Content { .. }));
    // Cache paths that would leave the project folder, and malformed hashes and times.
    let output = |path: &str, hash: &str, created: &str| {
        with(&format!(
            r#""outputs": [{{"id": 1, "name": "x", "method": "pyramid", "parameters": null, "created": "{created}", "inputs": [],
                "image": {{"path": "{path}", "size": 1, "hash": "{hash}"}}}}]"#
        ))
    };
    for path in ["cache/../../../etc/passwd", "../outside.tiff", "/tmp/x.tiff", "C:/x.tiff", "cache/a/b.tiff", "cache/CON.tiff"] {
        assert!(matches!(damaged(output(path, HASH, "2026-10-21T00:00:00Z").as_bytes()), SchemaError::Content { .. }), "{path}");
    }
    assert!(matches!(damaged(output("cache/x.tiff", "md5:00", "2026-10-21T00:00:00Z").as_bytes()), SchemaError::Content { .. }));
    assert!(matches!(damaged(output("cache/x.tiff", HASH, "yesterday").as_bytes()), SchemaError::Content { .. }));
    assert!(matches!(damaged(with(r#""frames": [], "x": 1e999"#).as_bytes()), SchemaError::Syntax(_) | SchemaError::Content { .. }));
    // Nesting deep enough to overflow a recursive parser's stack.
    let deep = with(&format!(
        r#""frames": [{{"id": 1, "path": "a", "size": 1, "hash": "{HASH}", "enabled": true, "exif": {}1{}}}]"#,
        "[".repeat(100_000),
        "]".repeat(100_000)
    ));
    assert!(matches!(damaged(deep.as_bytes()), SchemaError::Syntax(_) | SchemaError::Content { .. }));
}

#[test]
fn inconsistent_projects_are_refused() {
    let frame = |id: u64| format!(r#"{{"id": {id}, "path": "/a.tif", "size": 1, "hash": "{HASH}", "enabled": true}}"#);
    for (field, what) in [
        (format!(r#""frames": [{}, {}]"#, frame(1), frame(1)), "a duplicate id"),
        (format!(r#""frames": [{}]"#, frame(5)), "an id that was never given out"),
        (format!(r#""frames": [{}]"#, frame(0)), "id 0"),
        (r#""preferences": {"align": {"rotation": null}}"#.to_owned(), "a null preference"),
        (r#""name": "tab\there""#.to_owned(), "a name with a control character"),
    ] {
        let text = with(&field).replacen(r#""name": null, "#, if field.starts_with(r#""name""#) { "" } else { r#""name": null, "# }, 1);
        let e = open(text.as_bytes());
        assert!(matches!(e, ProjectError::Invalid { .. }), "{what}: {e:?}");
    }
    let unknown_group = with(r#""preferences": {"devlop": {}}"#);
    assert!(matches!(open(unknown_group.as_bytes()), ProjectError::Damaged { .. }));
}

#[test]
fn an_oversized_file_is_refused_before_it_is_read() {
    let dir = TempDir::new("huge");
    let root = dir.path().join("huge.florastack");
    std::fs::create_dir(&root).unwrap();
    let file = std::fs::File::create(root.join("project.json")).unwrap();
    // Sparse: no disk space is used, and nothing is read.
    file.set_len(schema::MAX_FILE_BYTES + 1).unwrap();
    drop(file);
    assert!(matches!(ProjectFolder::open(&root), Err(ProjectError::TooLarge { .. })));
    // A folder named project.json is no project file either.
    let odd = dir.path().join("odd.florastack");
    std::fs::create_dir_all(odd.join("project.json")).unwrap();
    assert!(matches!(ProjectFolder::open(&odd), Err(ProjectError::NotAProject { .. })));
}

#[test]
fn a_byte_order_mark_is_allowed() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(valid().as_bytes());
    assert!(schema::parse(&bytes).is_ok());
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = schema::parse(&bytes);
        let _ = schema::header(&bytes);
    }

    #[test]
    fn damaged_project_files_never_panic(
        edits in proptest::collection::vec((any::<proptest::sample::Index>(), any::<u8>()), 1..8),
        cut in any::<proptest::sample::Index>(),
    ) {
        let text = valid_with_content();
        let mut bytes = text.into_bytes();
        for (at, byte) in edits {
            let i = at.index(bytes.len());
            bytes[i] = byte;
        }
        let _ = schema::parse(&bytes);
        let end = cut.index(bytes.len() + 1);
        let _ = schema::parse(&bytes[..end]);
    }
}

/// A valid project file with frames, an output and a layer, for the mutation test.
fn valid_with_content() -> String {
    with(&format!(
        r#""frames": [{{"id": 1, "path": "/a.tif", "size": 1, "hash": "{HASH}", "enabled": true,
             "alignment": {{"transform": [[1, 0, 0.5], [0, 1, -0.25]], "brightness_gain": 1.0, "quality": 0.9}}}}],
           "preferences": {{"align": {{"rotation": false, "limits": {{"max_shift": 0.1}}}}}},
           "outputs": [{{"id": 2, "name": "Pyramid", "method": "pyramid", "parameters": {{"grit": 0.5}}, "created": "2026-10-21T00:00:00Z",
             "inputs": [{{"frame": 1}}], "image": {{"path": "cache/output-2-0011223344556677.tiff", "size": 9, "hash": "{HASH}"}},
             "retouch": [{{"id": 3, "name": "Layer", "visible": true, "strokes": [{{"brush": "pixels"}}]}}]}}]"#
    ))
}

#[test]
fn the_mutation_test_starts_from_a_valid_file() {
    schema::parse(valid_with_content().as_bytes()).unwrap();
    schema::parse(valid().as_bytes()).unwrap();
}
