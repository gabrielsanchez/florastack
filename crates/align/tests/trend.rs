//! Frames off the stack's trend (roadmap 2.4): on a stack whose scale changes steadily, as focus breathing changes it,
//! a frame knocked out of line is flagged and no other; stacks whose frames wander at random have no trend to be off.
//! Prints what it measured (`cargo test -p florastack-align --test trend -- --nocapture`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_align::{AlignSettings, Flag, StackAlignment, align_frames};
use florastack_core::testutil::{StackParams, generate, scene};
use florastack_core::{Image, Rect, Unmonitored};

/// The margin, in pixels, the frames are generated with on every side, which a knocked frame is cropped into.
const MARGIN: usize = 8;

/// Frames of `width` x `height`, cropped from a stack generated `MARGIN` pixels larger, frame `knocked` (if any)
/// cropped `by` pixels to the right and down, as if the camera had been knocked while it was shot.
fn frames(width: usize, height: usize, params: &StackParams, knocked: Option<usize>, by: usize) -> Vec<Image<f32>> {
    let (sharp, depth) = scene::textured(width + 2 * MARGIN, height + 2 * MARGIN, params.seed).unwrap();
    let stack = generate(&sharp, &depth, params).unwrap();
    stack
        .frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let at = if Some(i) == knocked { MARGIN + by } else { MARGIN };
            f.crop(Rect::new(at, at, width, height)).unwrap()
        })
        .collect()
}

fn off_trend(alignment: &StackAlignment) -> Vec<usize> {
    alignment.frames.iter().enumerate().filter(|(_, f)| f.flags.contains(&Flag::OffTrend)).map(|(i, _)| i).collect()
}

#[test]
fn a_frame_knocked_out_of_a_breathing_stack_is_flagged_alone() {
    for (seed, knocked) in [(1, 6), (2, 0), (3, 13)] {
        // Each frame 0.4% larger than the one before, in focus from front to back, otherwise in register.
        let params = StackParams { breathing: 0.06, ..StackParams::clean(14, seed) };
        let params = StackParams { max_shift: 0.0, max_rotation_deg: 0.0, ..params };
        let steady = frames(240, 180, &params, None, 0);
        let alignment = align_frames(&steady, 7, &AlignSettings::default(), &Unmonitored).unwrap();
        assert_eq!(off_trend(&alignment), Vec::<usize>::new(), "seed {seed}: in register");
        let knocked_frames = frames(240, 180, &params, Some(knocked), 3);
        let alignment = align_frames(&knocked_frames, 7, &AlignSettings::default(), &Unmonitored).unwrap();
        println!("seed {seed}, frame {knocked} knocked by 3 px: flagged {:?}", off_trend(&alignment));
        assert_eq!(off_trend(&alignment), [knocked], "seed {seed}");
    }
}

#[test]
fn frames_that_wander_at_random_have_no_trend_to_be_off() {
    for seed in 1..=3 {
        // The standard clean stacks: each frame shifted by up to 1.5 px and turned by up to 0.3 degrees at random.
        let params = StackParams { breathing: 0.06, ..StackParams::clean(14, seed) };
        let wandering = frames(240, 180, &params, None, 0);
        let alignment = align_frames(&wandering, 7, &AlignSettings::default(), &Unmonitored).unwrap();
        println!("seed {seed}: flagged {:?}", off_trend(&alignment));
        assert_eq!(off_trend(&alignment), Vec::<usize>::new(), "seed {seed}");
    }
}
