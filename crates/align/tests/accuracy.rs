//! Alignment accuracy on synthetic stacks with known transforms and gains (florastack-core's `testutil`), scored as
//! the roadmap asks: for each stack, the mean over its frames of `transform_error`, the mean distance between where
//! the estimated transform and the true one put the points of an 8x8 grid spread over the frame. Each test prints its
//! table (`cargo test -p florastack-align --test accuracy -- --nocapture`).
//!
//! # Defocus moves the image, too
//!
//! The synthetic frames are defocused as well as moved, and a change of focus shifts image content where nothing
//! moved:
//!
//! - **Uneven blur**: a surface that recedes is blurred more at one end than at the other, and when the focus moves,
//!   its blur changes by different amounts across it. Two such frames no longer correlate symmetrically about the true
//!   shift, so anything that matches image content (correlation, ECC, Lucas-Kanade, whatever its features) finds a
//!   shift and stretch that the scene didn't make. The test scenes' background recedes by 0.4 units of depth over the
//!   frame, at 8 pixels of blur per unit: about 0.1 px over a stack.
//! - **Depth edges**: the generator blurs each pixel with its own depth's blur, so an edge between a near object and
//!   the background blurs differently on each side, and its profile, with its apparent position, changes with focus.
//!
//! Real lenses do both, by amounts that depend on the scene. On frames that differ only in focus (every transform
//! off), the aligner finds 0.06 to 0.12 px of such apparent motion per stack on these scenes; the band-pass features,
//! the robust weights and the defocus term of the solver (see `ecc.rs` and `prepare.rs`) are what keep it that small.
//! The roadmap's first target, 0.1 px on the clean set, lies below it, so the tests hold three targets:
//!
//! - frames moved but not defocused, which measure the aligner's own precision: 0.05 px;
//! - the clean set, defocus included: 0.15 px;
//! - the noisy, dusty set: 0.3 px.
//!
//! The same goes for brightness: the generator doesn't conserve light where it blurs a bright near object over its
//! background, so frames focused away from it come out up to 0.8% brighter or darker than their gain says, and the
//! estimated gain follows. Without defocus, the gain is right to within 0.1%.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_align::{AlignSettings, Flag, StackAlignment, Status, align_frames};
use florastack_core::Unmonitored;
use florastack_core::testutil::{StackParams, SyntheticStack, generate, scene, transform_error};

const WIDTH: usize = 320;
const HEIGHT: usize = 240;
const FRAMES: usize = 16;

/// A synthetic stack of the test scene of `seed`.
fn stack(params: &StackParams, width: usize, height: usize, seed: u64) -> SyntheticStack {
    let (sharp, depth) = scene::textured(width, height, seed).unwrap();
    generate(&sharp, &depth, params).unwrap()
}

/// What an alignment got right and wrong: each frame's transform error and its gain error (relative to the true
/// gain).
struct Score {
    errors: Vec<f64>,
    gain_errors: Vec<f64>,
}

impl Score {
    fn of(result: &StackAlignment, stack: &SyntheticStack) -> Score {
        let truth = &stack.truth;
        let (w, h) = (truth.sharp.width(), truth.sharp.height());
        let master = result.master;
        let errors =
            result.frames.iter().enumerate().map(|(i, f)| transform_error(&f.transform, &truth.relative(i, master).unwrap(), w, h)).collect();
        let gain_errors =
            result.frames.iter().zip(&truth.frames).map(|(f, t)| (f.gain / f64::from(truth.frames[master].gain / t.gain) - 1.0).abs()).collect();
        Score { errors, gain_errors }
    }

    fn mean(&self) -> f64 {
        self.errors.iter().sum::<f64>() / self.errors.len() as f64
    }

    fn worst(&self) -> f64 {
        self.errors.iter().copied().fold(0.0, f64::max)
    }

    fn worst_gain(&self) -> f64 {
        self.gain_errors.iter().copied().fold(0.0, f64::max)
    }
}

/// Aligns the stacks of seeds 1 to 3, each to its first frame and to its middle one, checks that no frame's alignment
/// was flagged, prints a table and returns the scores. A frame may be flagged out of focus or moved, which isn't about
/// its alignment: the test scenes leave a gap in depth between their bristles and their discs, and a frame focused in
/// it has next to nothing in focus; and the dust of the noisy, dusty stacks stays on the sensor while the frames shift
/// by up to 1.5 pixels, so that against an aligned neighbor it moves (inpainted first, as the engine does, it doesn't:
/// `tests/motion.rs`).
fn run(name: &str, settings: &AlignSettings, params: impl Fn(u64) -> StackParams) -> Vec<Score> {
    println!("{name}: mean and worst frame error (px), worst gain error (%)");
    let mut scores = Vec::new();
    for seed in 1..=3 {
        let stack = stack(&params(seed), WIDTH, HEIGHT, seed);
        for master in [0, FRAMES / 2] {
            let result = align_frames(&stack.frames, master, settings, &Unmonitored).unwrap();
            for (i, frame) in result.frames.iter().enumerate() {
                let aligned = frame.flags.iter().all(|f| matches!(f, Flag::OutOfFocus | Flag::Moved)) && frame.status != Status::Rejected;
                assert!(aligned, "seed {seed}, master {master}, frame {i}: {frame:?}");
            }
            let score = Score::of(&result, &stack);
            println!("  seed {seed}, master {master:2}: {:.4} {:.4} {:.3}%", score.mean(), score.worst(), 100.0 * score.worst_gain());
            scores.push(score);
        }
    }
    scores
}

/// [`StackParams::clean`] or [`StackParams::noisy_dusty`] without defocus.
fn sharp(params: StackParams) -> StackParams {
    StackParams { blur_per_depth: 0.0, max_blur: 0.0, ..params }
}

#[test]
fn moved_frames_without_defocus_are_aligned_within_0_05_px() {
    let clean = run("moved, not defocused", &AlignSettings::default(), |seed| sharp(StackParams::clean(FRAMES, seed)));
    let noisy =
        run("moved, noisy and dusty, not defocused", &AlignSettings::default(), |seed| sharp(StackParams::noisy_dusty(FRAMES, WIDTH, HEIGHT, seed)));
    for score in clean.iter().chain(&noisy) {
        assert!(score.mean() <= 0.05, "mean error {} px", score.mean());
        assert!(score.worst_gain() <= 0.001, "gain error {}", score.worst_gain());
    }
}

#[test]
fn clean_stacks_are_aligned_within_0_15_px() {
    for score in run("clean", &AlignSettings::default(), |seed| StackParams::clean(FRAMES, seed)) {
        assert!(score.mean() <= 0.15, "mean error {} px", score.mean());
    }
}

#[test]
fn noisy_dusty_stacks_are_aligned_within_0_3_px() {
    for score in run("noisy and dusty", &AlignSettings::default(), |seed| StackParams::noisy_dusty(FRAMES, WIDTH, HEIGHT, seed)) {
        assert!(score.mean() <= 0.3, "mean error {} px", score.mean());
        // Up to 0.8% of it is the generator's: see the module documentation.
        assert!(score.worst_gain() <= 0.01, "gain error {}", score.worst_gain());
    }
}

#[test]
fn a_frame_larger_than_the_working_resolution_keeps_its_accuracy() {
    // 640x480 frames aligned at 320 pixels: one working pixel is two frame pixels, and the errors are in frame pixels.
    let (w, h) = (640, 480);
    let settings = AlignSettings { resolution: 320, ..AlignSettings::default() };
    let stack = stack(&sharp(StackParams { max_shift: 3.0, ..StackParams::clean(8, 5) }), w, h, 5);
    let result = align_frames(&stack.frames, 0, &settings, &Unmonitored).unwrap();
    let score = Score::of(&result, &stack);
    println!("640x480 at 320: {:.4} {:.4}", score.mean(), score.worst());
    assert!(score.mean() <= 0.05, "mean error {} px", score.mean());
}

#[test]
fn refining_against_the_master_suits_shallow_stacks() {
    // Frames that differ by little more than their motion share their detail with the master, and refining each
    // against it keeps the chain's small errors from adding up.
    let settings = AlignSettings { refine: true, ..AlignSettings::default() };
    for score in run("refined, not defocused", &settings, |seed| sharp(StackParams::noisy_dusty(FRAMES, WIDTH, HEIGHT, seed))) {
        assert!(score.mean() <= 0.05, "mean error {} px", score.mean());
    }
}
