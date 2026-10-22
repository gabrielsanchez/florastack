//! Orphan cleanup finds only old, unnamed, closed temporary projects, and moves them to a trash, never deleting them.
//! Every test here uses `FolderTrash`, a folder of the test's own: none may touch this machine's real trash.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::TempDir;
use florastack_project::{FolderTrash, Orphans, ProjectError, ProjectFolder, Trash, TrashError};

const DAY: Duration = Duration::from_hours(24);

/// An unnamed temporary project in `root`, closed (its lock released).
fn temporary(root: &Path) -> PathBuf {
    let (folder, _) = ProjectFolder::create_temporary(root).unwrap();
    folder.path().to_path_buf()
}

/// A trash that fails, to check that a failure leaves the project where it was.
struct Broken;

impl Trash for Broken {
    fn trash(&self, _: &Path) -> Result<(), TrashError> {
        Err(std::io::Error::other("the trash is full").into())
    }
}

#[test]
fn only_old_unnamed_closed_temporary_projects_are_orphans() {
    let dir = TempDir::new("orphans");
    let root = dir.path().join("temporary");
    let old = temporary(&root);
    assert!(old.file_name().unwrap().to_str().unwrap().starts_with("untitled-"));
    // Named, so kept whatever its age.
    let (mut folder, mut named) = ProjectFolder::create_temporary(&root).unwrap();
    named.set_name("Kept").unwrap();
    folder.save(&mut named).unwrap();
    drop(folder);
    // Open in FloraStack: its lock is held.
    let (open, _) = ProjectFolder::create_temporary(&root).unwrap();
    // Not a project, or not one that can be read.
    let damaged = root.join("damaged.florastack");
    std::fs::create_dir(&damaged).unwrap();
    std::fs::write(damaged.join("project.json"), b"{ not json").unwrap();
    let other = root.join("photos");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("project.json"), br#"{"format": "florastack-project", "version": 1, "name": null}"#).unwrap();
    std::fs::write(root.join("stray.florastack"), b"a file").unwrap();
    // Unnamed, from a newer FloraStack: its header is all that is needed.
    let newer = root.join("newer.florastack");
    std::fs::create_dir(&newer).unwrap();
    std::fs::write(newer.join("project.json"), br#"{"format": "florastack-project", "version": 99, "name": null, "new": []}"#).unwrap();

    let orphans = Orphans::new(&root, 30);
    assert!(orphans.find().unwrap().is_empty(), "nothing is 30 days old yet");
    let later = SystemTime::now() + 31 * DAY;
    let found: Vec<PathBuf> = orphans.find_at(later).unwrap().into_iter().map(|o| o.path).collect();
    let mut expected = vec![old.clone(), newer.clone()];
    expected.sort();
    let mut sorted = found.clone();
    sorted.sort();
    assert_eq!(sorted, expected);
    let orphan = orphans.find_at(later).unwrap().into_iter().find(|o| o.path == old).unwrap();
    assert!(orphan.bytes > 0, "its size is reported");
    drop(open);
    assert_eq!(Orphans::new(&root, 30).find_at(later).unwrap().len(), 3, "closed, the third one is an orphan too");
    assert_eq!(Orphans::new(dir.path().join("missing"), 30).find().unwrap(), Vec::new());
}

#[test]
fn orphans_go_to_the_trash_and_nothing_is_deleted() {
    let dir = TempDir::new("orphans-trash");
    let root = dir.path().join("temporary");
    let path = temporary(&root);
    let (folder, _) = ProjectFolder::open(&path).unwrap();
    let image = folder.store(florastack_project::Area::Cache, "output-1", "tiff", b"pixels", None).unwrap();
    drop(folder);
    let bin = FolderTrash::new(dir.path().join("bin"));
    let orphans = Orphans::new(&root, 7);
    let later = SystemTime::now() + 8 * DAY;
    let found = orphans.find_at(later).unwrap();
    assert_eq!(found.len(), 1);

    // A trash that fails leaves the project where it is.
    let e = orphans.trash_at(&found[0], &Broken, later).unwrap_err();
    assert!(matches!(e, ProjectError::Trash { .. }), "{e}");
    assert!(path.join("project.json").is_file());

    orphans.trash_at(&found[0], &bin, later).unwrap();
    assert!(!path.exists());
    let moved = bin.path().join(path.file_name().unwrap());
    assert!(moved.join("project.json").is_file(), "everything is in the trash");
    assert_eq!(std::fs::read(image.path.resolve(&moved)).unwrap(), b"pixels");
    assert_eq!(orphans.find_at(later).unwrap(), Vec::new());
}

#[test]
fn an_orphan_is_checked_again_before_it_goes() {
    let dir = TempDir::new("orphans-recheck");
    let root = dir.path().join("temporary");
    let path = temporary(&root);
    let bin = FolderTrash::new(dir.path().join("bin"));
    let orphans = Orphans::new(&root, 1);
    let later = SystemTime::now() + 2 * DAY;
    let orphan = orphans.find_at(later).unwrap().remove(0);

    // Opened since: it is in use.
    let (mut folder, mut project) = ProjectFolder::open(&path).unwrap();
    let e = orphans.trash_at(&orphan, &bin, later).unwrap_err();
    assert!(matches!(e, ProjectError::NotOrphan { reason: "it is open in FloraStack", .. }), "{e}");
    // Named since: it is no longer temporary.
    project.set_name("Saved after all").unwrap();
    folder.save(&mut project).unwrap();
    drop(folder);
    let e = orphans.trash_at(&orphan, &bin, later).unwrap_err();
    assert!(matches!(e, ProjectError::NotOrphan { .. }), "{e}");
    // Used recently, by the clock of the check.
    let mut unnamed = temporary(&root);
    let orphan = orphans.find_at(later).unwrap().into_iter().find(|o| o.path == unnamed).unwrap();
    assert!(matches!(orphans.trash_at(&orphan, &bin, SystemTime::now()), Err(ProjectError::NotOrphan { reason: "it was used recently", .. })));
    // Not in the temporary-projects folder, whatever the caller says.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let moved = elsewhere.join(unnamed.file_name().unwrap());
    std::fs::rename(&unnamed, &moved).unwrap();
    unnamed = moved;
    let outside = florastack_project::Orphan { path: unnamed.clone(), ..orphan };
    assert!(matches!(orphans.trash_at(&outside, &bin, later), Err(ProjectError::NotOrphan { .. })));
    assert!(unnamed.is_dir() && path.is_dir(), "nothing was moved");
    assert!(!bin.path().exists(), "the trash was never used");
}
