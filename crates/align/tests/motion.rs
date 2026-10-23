//! Subject movement on synthetic stacks (roadmap 2.3): stacks whose content only breathes, shifts, turns and changes
//! focus flag no frame as moved, and a patch of the subject moved in some frames is flagged where and when it moved,
//! where the frame is in focus: movement in a frame focused elsewhere doesn't reach the stack.
//! Each test prints what it measured (`cargo test -p florastack-align --test motion -- --nocapture`).
//!
//! Dust stays on the sensor while the scene shifts, so that against an aligned neighbor it moves as far as the frames
//! do: the stacks here are noisy but dust-free, or have their dust inpainted first, as the engine does.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_align::{AlignSettings, DefectMask, Flag, StackAligner, StackAlignment, align_frames, inpaint};
use florastack_core::ops::{Border, Interpolation, warp};
use florastack_core::testutil::{StackParams, generate, scene};
use florastack_core::{Affine, ColorSpace, Image, Unmonitored};

const WIDTH: usize = 320;
const HEIGHT: usize = 240;
const FRAMES: usize = 16;
const MASTER: usize = 8;

fn frames(params: &StackParams) -> Vec<Image<f32>> {
    let (sharp, depth) = scene::textured(WIDTH, HEIGHT, params.seed).unwrap();
    generate(&sharp, &depth, params).unwrap().frames
}

/// The frames of the same stack whose scene has the content of a disc of `PATCH_RADIUS` pixels around `PATCH` moved by
/// `by` pixels, with its depth: the subject as it is after it moved. Every other parameter, the frames' transforms and
/// noise included, is the stack's. With `background`, only the background plane moves (depth 0.55 and beyond).
fn moved_frames(params: &StackParams, by: (f64, f64), background: bool) -> Vec<Image<f32>> {
    let (sharp, depth) = scene::textured(WIDTH, HEIGHT, params.seed).unwrap();
    let shift = |image: &Image<f32>| warp(image, &Affine::translation(-by.0, -by.1), Interpolation::Bicubic, Border::Clamp).unwrap();
    let (shifted_sharp, shifted_depth) = (shift(&sharp), shift(&depth));
    let in_patch = |x: usize, y: usize| {
        let inside = (x as f64 - PATCH.0).hypot(y as f64 - PATCH.1) <= PATCH_RADIUS;
        inside && (!background || depth.get(x, y, 0).unwrap().min(shifted_depth.get(x, y, 0).unwrap()) >= 0.55)
    };
    let with_patch = |image: &Image<f32>, shifted: &Image<f32>| {
        Image::from_fn(WIDTH, HEIGHT, image.channels(), ColorSpace::LINEAR, |x, y, c| {
            if in_patch(x, y) { shifted.get(x, y, c).unwrap() } else { image.get(x, y, c).unwrap() }
        })
        .unwrap()
    };
    generate(&with_patch(&sharp, &shifted_sharp), &with_patch(&depth, &shifted_depth), params).unwrap().frames
}

fn align(frames: &[Image<f32>]) -> StackAlignment {
    align_frames(frames, MASTER, &AlignSettings::default(), &Unmonitored).unwrap()
}

/// Noisy frames without dust.
fn noisy(seed: u64) -> StackParams {
    StackParams { dust: Vec::new(), ..StackParams::noisy_dusty(FRAMES, WIDTH, HEIGHT, seed) }
}

/// A cell's index from its fractional coordinate.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A cell of a small map, inside it.
fn cell(v: f64) -> usize {
    v.round().max(0.0) as usize
}

fn moved(alignment: &StackAlignment) -> Vec<usize> {
    (0..alignment.frames.len()).filter(|&i| alignment.frames[i].flags.contains(&Flag::Moved)).collect()
}

#[test]
fn stacks_without_movement_flag_nothing() {
    for seed in 1..=3 {
        let dusty = StackParams::noisy_dusty(FRAMES, WIDTH, HEIGHT, seed);
        // The dust inpainted under a mask of where it darkens by 1% or more, as a user would draw it.
        let mask =
            DefectMask::from_fn(WIDTH, HEIGHT, |x, y| 1.0 - dusty.dust.iter().map(|s| s.transmission(x as f64, y as f64)).product::<f64>() >= 0.01)
                .unwrap();
        let mut inpainted = frames(&dusty);
        for frame in &mut inpainted {
            inpaint(frame, &mask).unwrap();
        }
        let stacks = [
            ("clean", frames(&StackParams::clean(FRAMES, seed)), None),
            ("noisy", frames(&noisy(seed)), None),
            ("dusty, inpainted", inpainted, Some(mask)),
        ];
        for (name, frames, mask) in stacks {
            let mut aligner = StackAligner::new(&AlignSettings::default(), FRAMES, MASTER).unwrap();
            aligner.set_defects(mask);
            while let Some(i) = aligner.next_frame() {
                aligner.add(&frames[i], &Unmonitored).unwrap();
            }
            let alignment = aligner.finish().unwrap();
            let motions: Vec<_> = alignment.frames.iter().filter_map(|f| f.motion.as_ref()).collect();
            let peak = motions.iter().map(|m| m.peak).fold(0.0, f64::max);
            let score = motions.iter().map(|m| m.score).fold(0.0, f64::max);
            let cells = motions.iter().flat_map(|m| &m.regions).map(|r| r.cells).max().unwrap_or(0);
            println!("seed {seed}, {name}: largest residual motion {peak:.2} px, largest score {score:.4}, largest region {cells} cells");
            assert_eq!(motions.len(), FRAMES - 1, "seed {seed}, {name}: every frame but the master has a motion map");
            assert_eq!(moved(&alignment), Vec::<usize>::new(), "seed {seed}, {name}");
        }
    }
}

const PATCH: (f64, f64) = (160.0, 120.0);
const PATCH_RADIUS: f64 = 40.0;
const BY: (f64, f64) = (3.0, -2.0);

#[test]
fn a_patch_moved_in_one_frame_flags_it_and_the_next() {
    for seed in 1..=3 {
        let mut frames = frames(&noisy(seed));
        // Frame 11 is aligned to frame 10, and frame 12 to frame 11: the patch moves there and back.
        frames[11] = moved_frames(&noisy(seed), BY, false).swap_remove(11);
        let alignment = align(&frames);
        let flagged = moved(&alignment);
        let motion = alignment.frames[11].motion.as_ref().unwrap();
        let region = motion.regions[0];
        println!("seed {seed}: frames {flagged:?} flagged; frame 11's score {:.3}, largest region {region:?}", motion.score);
        assert_eq!(flagged, [11, 12], "seed {seed}");
        // The region lies on the patch, give or take a cell (8 pixels): on the part of it that frame 11 has in focus.
        let (x0, y0) = (PATCH.0 - PATCH_RADIUS - 12.0, PATCH.1 - PATCH_RADIUS - 12.0);
        let (x1, y1) = (PATCH.0 + PATCH_RADIUS + 12.0, PATCH.1 + PATCH_RADIUS + 12.0);
        assert!(region.cells >= 8, "seed {seed}: {region:?}");
        assert!(region.x as f64 >= x0 && region.y as f64 >= y0, "seed {seed}: {region:?}");
        assert!((region.x + region.width) as f64 <= x1 && (region.y + region.height) as f64 <= y1, "seed {seed}: {region:?}");
        // Inside the patch, the map holds the patch's displacement.
        let (cx, cy) = motion.map.to_frame().inverse().unwrap().apply(PATCH.0, PATCH.1);
        let d = motion.map.displacement(cell(cx), cell(cy)).unwrap();
        assert!((d.0 - BY.0).abs() < 0.3 && (d.1 - BY.1).abs() < 0.3, "seed {seed}: {d:?}");
        let back = alignment.frames[12].motion.as_ref().unwrap();
        let (cx, cy) = back.map.to_frame().inverse().unwrap().apply(PATCH.0, PATCH.1);
        let d = back.map.displacement(cell(cx), cell(cy)).unwrap();
        assert!((d.0 + BY.0).abs() < 0.3 && (d.1 + BY.1).abs() < 0.3, "seed {seed}: frame 12 moves it back: {d:?}");
    }
}

#[test]
fn a_subject_that_moves_once_flags_one_frame() {
    for seed in 1..=3 {
        let mut frames = frames(&noisy(seed));
        // From frame 11 on, the patch stays where it moved.
        let after = moved_frames(&noisy(seed), BY, false);
        frames.splice(11.., after[11..].iter().cloned());
        let alignment = align(&frames);
        println!("seed {seed}: frames {:?} flagged", moved(&alignment));
        assert_eq!(moved(&alignment), [11], "seed {seed}");
    }
}

#[test]
fn movement_where_a_frame_is_out_of_focus_is_not_flagged() {
    for seed in 1..=3 {
        // Frames 0 and 1, focused on the bristles in front, see the background behind the patch moved; frame 1 is
        // aligned to frame 2, which sees it where it was. Frame 12, focused on the background, sees it moved too.
        let mut frames = frames(&noisy(seed));
        let before = moved_frames(&noisy(seed), BY, true);
        frames.splice(..2, before[..2].iter().cloned());
        frames[12] = before[12].clone();
        let alignment = align(&frames);
        let measured = alignment.frames[1].motion.as_ref().unwrap();
        println!("seed {seed}: frames {:?} flagged; frame 1's largest displacement {:.2} px", moved(&alignment), measured.peak);
        assert_eq!(moved(&alignment), [12, 13], "seed {seed}: only where the background is in focus");
    }
}

#[test]
fn a_small_movement_below_the_threshold_is_not_flagged() {
    let mut frames = frames(&noisy(1));
    frames[11] = moved_frames(&noisy(1), BY, false).swap_remove(11);
    let settings = AlignSettings { min_motion: 5.0, ..AlignSettings::default() };
    let alignment = align_frames(&frames, MASTER, &settings, &Unmonitored).unwrap();
    let motion = alignment.frames[11].motion.as_ref().unwrap();
    println!("3.6 pixels against a threshold of 5: frames {:?} flagged, frame 11's peak {:.2} px", moved(&alignment), motion.peak);
    assert_eq!(moved(&alignment), Vec::<usize>::new(), "3.6 pixels is below 5");
}
