//! Alignment on the real stacks of the corpus (`corpus/README.md`): the downloaded ones in `corpus/stacks/` and the
//! local ones in `corpus/local/` that have their `stack.toml`. Real frames have no ground truth, so the test checks
//! what must hold whatever the scene, and one estimate against another:
//!
//! - every frame is aligned: none rejected, none at a limit, none without a match;
//! - focus breathing is even: put in order of magnification, no frame lies far from its neighbors. The files' own
//!   order needn't be the order of focus: a camera's focus stacking mode seems to shoot the frame at the focus point
//!   first, then from in front of it to behind it, going by the first frame of three of the local stacks;
//! - two chains agree: aligned again with every other frame left out, each remaining frame lands where the chain
//!   through all of them put it. Neighbors two steps of focus apart still share most of their detail, the two chains
//!   share no link, and a link that slipped or errors that add up along the chain show as a difference. Where the
//!   sparser chain rejects a frame, as it may on a stack whose frames are far apart in focus, that frame isn't
//!   compared;
//! - movement is the exception: at most half the frames are flagged as moved. A subject can move in every frame, but
//!   the corpus's stacks are of still subjects, and the change of blur from frame to frame passed for movement on
//!   nearly every frame of them before movement was counted only where some frame has detail in focus (2.3);
//! - detected dust is dust: a mask of at most [`MAX_DEFECTS`] of the frame. The scene's own texture, which the
//!   frames show at other sensor pixels as they move, must not pass for it.
//!
//! Aligning each frame directly against the master (`refine`) is no such check on stacks this deep: the frames
//! focused four steps from the master land up to 0.9 thousandths of the frame (4 pixels of 5184) from where the
//! chain puts them, growing evenly with the distance in focus on both sides, as a match that defocus biases does.
//!
//! Without a stack it records a skip. `cargo test -p florastack-engine --test corpus -- --nocapture` prints each
//! stack's table.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{TempDir, config, path_list, run};
use florastack_core::Affine;
use florastack_core::testutil::transform_error;
use florastack_engine::{Engine, Value};

/// The files the engine reads as frames, by extension.
const FRAMES: [&str; 14] = ["jpg", "jpeg", "tif", "tiff", "png", "dng", "orf", "cr2", "cr3", "nef", "arw", "raf", "rw2", "pef"];

/// The largest gap in magnification between frames next to each other in order of magnification, relative to the
/// stack's mean gap, and the floor under that mean: a stack that barely breathes has gaps of noise alone.
const MAX_GAP_RATIO: f64 = 4.0;
const MIN_MEAN_GAP: f64 = 2e-4;

/// How far apart the two chains may put a frame, in thousandths of the frame's longer side: a link that slipped
/// moves a frame by several. Measured on the local stacks (20 MP camera JPEGs, aligned at 2048 pixels): 0.11 and 0.13
/// on the two of 8 frames, 0.39 on the 23 frames of the wasp (2 pixels of 5184 after 11 links), and 0.89 on the
/// ladybug (4.6 pixels after 8 links), where the chains differ by 2 thousandths in magnification, steadily more the
/// further from the master. Which chain is nearer the truth these frames can't say; the roadmap keeps the question
/// (2.1), with the defocus model that would answer it.
const MAX_DISAGREEMENT: f64 = 1.0;

/// The most of the frame a detected defect mask may cover: real dust covers a few hundred spots of a few pixels at
/// most, far less than a thousandth of the frame.
const MAX_DEFECTS: f64 = 0.001;

/// A stack's frames, in name order: the image files of `dir` but its `reference`, a trusted result that `stack.toml`
/// may name.
fn frames(dir: &Path) -> Vec<PathBuf> {
    let described = std::fs::read_to_string(dir.join("stack.toml")).unwrap_or_default();
    let reference = described.split("reference").nth(1).and_then(|rest| rest.split('"').nth(1)).map(str::to_owned);
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| FRAMES.contains(&e.to_ascii_lowercase().as_str())))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned) != reference)
        .collect();
    files.sort();
    files
}

/// The corpus's stacks that are here, as `(name, frames)`: every downloaded one, and the local ones with a
/// `stack.toml`.
fn stacks() -> Vec<(String, Vec<PathBuf>)> {
    let mut found = Vec::new();
    for (kind, described) in [("stacks", false), ("local", true)] {
        let Ok(entries) = std::fs::read_dir(florastack_testkit::corpus_dir().join(kind)) else { continue };
        let mut dirs: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        for dir in dirs {
            let files = frames(&dir);
            if files.len() >= 3 && (!described || dir.join("stack.toml").is_file()) {
                found.push((format!("{kind}/{}", dir.file_name().unwrap().to_string_lossy()), files));
            }
        }
    }
    found
}

/// A frame of `align.info`'s run.
struct Aligned {
    id: u64,
    transform: Affine,
    scale: f64,
    score: f64,
    status: String,
    flags: Vec<String>,
    /// Whether it is the frame the aligner aligned the others to, which the stored run needn't be relative to.
    master: bool,
}

fn aligned(info: &Value) -> Vec<Aligned> {
    let number = |v: &Value| v.as_f64().unwrap();
    info["run"]["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let rows: Vec<Vec<f64>> = f["transform"].as_array().unwrap().iter().map(|r| r.as_array().unwrap().iter().map(number).collect()).collect();
            Aligned {
                id: f["frame"].as_u64().unwrap(),
                transform: Affine { m: [[rows[0][0], rows[0][1], rows[0][2]], [rows[1][0], rows[1][1], rows[1][2]]] },
                scale: number(&f["scale"]),
                score: number(&f["score"]),
                status: f["status"].as_str().unwrap().to_owned(),
                flags: f["flags"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
                master: f.get("reference").is_none_or(Value::is_null),
            }
        })
        .collect()
}

/// A similarity's magnification.
fn magnification(transform: &Affine) -> f64 {
    let m = transform.m;
    (m[0][0] * m[1][1] - m[0][1] * m[1][0]).abs().sqrt()
}

/// Checks that every frame of a run was aligned: a frame may be flagged for its focus, for what moved in it or for a
/// repeating pattern, which aren't about its alignment.
fn check(name: &str, frames: &[Aligned]) {
    for frame in frames {
        let trusted = frame.flags.iter().all(|f| ["out-of-focus", "moved", "repeating-pattern"].contains(&f.as_str()));
        assert!(frame.status != "rejected" && trusted, "{name}, frame {}: {} {:?}", frame.id, frame.status, frame.flags);
    }
}

#[test]
fn real_stacks_are_aligned_whole_evenly_and_the_same_by_two_chains() {
    let stacks = stacks();
    if stacks.is_empty() {
        florastack_testkit::skip!(Corpus, "the corpus has no stack: add one under corpus/local/, or to corpus/manifest.toml (corpus/README.md)");
        return;
    }
    // The stacks whose sparser chain held, for the two to be compared.
    let mut checked = 0;
    for (name, files) in stacks {
        let dir = TempDir::new("corpus-align");
        let mut engine = Engine::new(config(dir.path()));
        let folder = serde_json::to_string(&dir.path().display().to_string()).unwrap();
        run(&mut engine, "project.new", &format!(r#"{{ "name": "Stack", "folder": {folder} }}"#));
        let added = run(&mut engine, "project.addFrames", &format!(r#"{{ "paths": [{}] }}"#, path_list(&files)));
        let ids: Vec<u64> = added["added"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();

        let ran = run(&mut engine, "align.run", "{}");
        let info = run(&mut engine, "align.info", "{}");
        let all = aligned(&info);
        let side = |name: &str| usize::try_from(info["run"][name].as_u64().unwrap()).unwrap();
        let (width, height) = (side("width"), side("height"));
        println!("{name}: {} frames of {width}x{height}", all.len());
        assert_eq!((all.len(), ran["rejected"].as_array().map(Vec::len)), (files.len(), Some(0)), "{name}: {ran}");
        check(&name, &all);

        // Breathing: the gaps between the frames' magnifications, in their order.
        let mut scales: Vec<f64> = all.iter().map(|f| f.scale).collect();
        scales.sort_by(f64::total_cmp);
        let gaps: Vec<f64> = scales.windows(2).map(|w| w[1] / w[0] - 1.0).collect();
        let mean = (gaps.iter().sum::<f64>() / gaps.len() as f64).max(MIN_MEAN_GAP);
        let largest = gaps.iter().copied().fold(0.0, f64::max);
        println!("  magnification over {:.4}, in gaps of {mean:.5} on average and {largest:.5} at most", scales[scales.len() - 1] / scales[0]);
        assert!(largest <= MAX_GAP_RATIO * mean, "{name}: a gap of {largest} in magnification, {mean} on average");

        // Movement: flagged on a few frames at most.
        let moved: Vec<u64> = all.iter().filter(|f| f.flags.iter().any(|g| g == "moved")).map(|f| f.id).collect();
        println!("  moved: {moved:?}");
        assert!(moved.len() * 2 <= all.len(), "{name}: {} of {} frames flagged as moved: {moved:?}", moved.len(), all.len());

        // Every other frame, chained by itself.
        let odd: Vec<String> = ids.iter().skip(1).step_by(2).map(ToString::to_string).collect();
        run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": [{}], "enabled": false }}"#, odd.join(", ")));
        run(&mut engine, "align.run", "{}");
        let half = aligned(&run(&mut engine, "align.info", "{}"));
        // Both runs from the pixels of the second one's master.
        let master = half.iter().find(|f| f.master).unwrap();
        let from = |frames: &[Aligned], id: u64| {
            let of = |id: u64| frames.iter().find(|f| f.id == id).unwrap().transform;
            of(master.id).inverse().unwrap().then(&of(id))
        };
        let unit = width.max(height) as f64 / 1000.0;
        let (mut worst, mut compared): (f64, usize) = (0.0, 0);
        for frame in &all {
            // A stack whose frames are far apart in focus may be too deep for the sparser chain: what that rejects
            // isn't compared.
            let in_both = half.iter().any(|f| f.id == frame.id && f.status != "rejected");
            let apart = in_both.then(|| transform_error(&from(&all, frame.id), &from(&half, frame.id), width, height) / unit);
            // How much of it is magnification: the two scales' ratio, in thousandths too, which moves the frame's
            // corners by half as much of its longer side.
            let scales = in_both.then(|| 1000.0 * (magnification(&from(&half, frame.id)) / magnification(&from(&all, frame.id)) - 1.0));
            compared += usize::from(in_both && frame.id != master.id);
            println!(
                "  frame {:3}: score {:.3}, scale {:.5}{}",
                frame.id,
                frame.score,
                frame.scale,
                apart.zip(scales).map_or_else(String::new, |(a, s)| format!(", {a:.3} apart, {s:+.3} in magnification"))
            );
            worst = worst.max(apart.unwrap_or(0.0));
        }
        println!("  the two chains are at most {worst:.3} thousandths of the frame apart, on {compared} frames");
        checked += usize::from(compared >= 2);
        assert!(worst <= MAX_DISAGREEMENT, "{name}: the two chains put a frame {worst} thousandths of the frame apart");

        // Dust and hot pixels, detected on every frame.
        run(&mut engine, "project.setFramesEnabled", &format!(r#"{{ "frames": [{}], "enabled": true }}"#, odd.join(", ")));
        let detected = run(&mut engine, "defects.detect", "{}");
        let summary = &detected["summary"];
        let share = summary["pixels"].as_f64().unwrap() / (width * height) as f64;
        println!("  defects: {} spots, {:.5} of the frame", summary["spots"], share);
        assert!(share <= MAX_DEFECTS, "{name}: the detected mask covers {share} of the frame: {detected}");
    }
    assert!(checked > 0, "no stack's frames could be chained two apart");
}
