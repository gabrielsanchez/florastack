//! White balance on synthetic stacks whose frames change each color channel's gain by known amounts
//! (florastack-core's `testutil`, `color_jitter`): each frame's measured gains against the true ones, balanced as the
//! aligner balances them; on stacks whose frames differ only in focus, gains of 1, so that focus never passes for a
//! change of color; nothing measured unless asked for; and the gains rebased with the master. Each test prints its
//! errors (`cargo test -p florastack-align --test white_balance -- --nocapture`).
//!
//! The generator doesn't conserve light where it blurs a bright near object over its background (`accuracy.rs`), so
//! frames focused away from it come out up to 0.8% brighter or darker there than their gains say, by about as much in
//! every channel, which the balance mostly cancels. Measured: at most 0.15% (textured) and 0.06% (layered) with the
//! white balance changing, 0.23% and 0.04% with focus alone; the targets are 0.5%.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_align::{AlignSettings, StackAlignment, align_frames};
use florastack_core::Unmonitored;
use florastack_core::testutil::{Backdrop, StackParams, SyntheticStack, generate, generate_layered, scene};

const WIDTH: usize = 320;
const HEIGHT: usize = 240;
const FRAMES: usize = 10;
const MASTER: usize = 4;
const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

fn textured(params: &StackParams) -> SyntheticStack {
    let (sharp, depth) = scene::textured(WIDTH, HEIGHT, params.seed).unwrap();
    generate(&sharp, &depth, params).unwrap()
}

fn layered(params: &StackParams) -> SyntheticStack {
    let scene = scene::layered(WIDTH, HEIGHT, 1.0, Backdrop::Textured, params.seed).unwrap();
    generate_layered(&scene, params).unwrap()
}

fn with_white_balance() -> AlignSettings {
    AlignSettings { white_balance: true, ..AlignSettings::default() }
}

/// Each aligned frame's largest relative error over its three gains, against the truth relative to the master: the
/// master's channel gains over the frame's, balanced to a luminance-weighted mean of 1.
fn errors(stack: &SyntheticStack, alignment: &StackAlignment) -> Vec<f64> {
    let master = stack.truth.frames[alignment.master].channel_gains;
    let mut out = Vec::new();
    for (i, (truth, found)) in stack.truth.frames.iter().zip(&alignment.frames).enumerate() {
        if i == alignment.master {
            continue;
        }
        let gains = found.white_balance.unwrap_or_else(|| panic!("frame {i} has no white balance"));
        let ratio = [0, 1, 2].map(|c| f64::from(master[c]) / f64::from(truth.channel_gains[c]));
        let mean: f64 = ratio.iter().zip(LUMA).map(|(r, w)| r * w).sum();
        let worst = (0..3).map(|c| (gains[c] - ratio[c] / mean).abs() / (ratio[c] / mean)).fold(0.0, f64::max);
        out.push(worst);
    }
    out
}

fn show(name: &str, errors: &[f64]) -> f64 {
    let max = errors.iter().copied().fold(0.0, f64::max);
    let mean = errors.iter().sum::<f64>() / errors.len() as f64;
    println!("{name:<40} mean {:.3}%  max {:.3}%", 100.0 * mean, 100.0 * max);
    max
}

#[test]
fn gains_follow_the_frames_white_balance() {
    // Moved, breathing, defocused, brighter and darker, noisy: as a real stack is, with the white balance changing too.
    let params = StackParams { color_jitter: 0.06, brightness_jitter: 0.04, read_noise: 0.005, shot_noise: 0.01, ..StackParams::clean(FRAMES, 21) };
    for (name, stack) in [("textured", textured(&params)), ("layered", layered(&params))] {
        let alignment = align_frames(&stack.frames, MASTER, &with_white_balance(), &Unmonitored).unwrap();
        let max = show(&format!("white balance, {name}"), &errors(&stack, &alignment));
        assert!(max < 0.005, "{name}: {max}");
    }
}

#[test]
fn focus_alone_changes_no_color() {
    let params = StackParams { read_noise: 0.005, shot_noise: 0.01, ..StackParams::clean(FRAMES, 22) };
    for (name, stack) in [("textured", textured(&params)), ("layered", layered(&params))] {
        let alignment = align_frames(&stack.frames, MASTER, &with_white_balance(), &Unmonitored).unwrap();
        let max = show(&format!("focus only, {name}"), &errors(&stack, &alignment));
        assert!(max < 0.005, "{name}: {max}");
    }
}

#[test]
fn nothing_is_measured_unless_asked_and_gains_follow_a_new_master() {
    let params = StackParams { color_jitter: 0.06, ..StackParams::clean(FRAMES, 23) };
    let stack = textured(&params);
    let plain = align_frames(&stack.frames, MASTER, &AlignSettings::default(), &Unmonitored).unwrap();
    assert!(plain.frames.iter().all(|f| f.white_balance.is_none()));
    let alignment = align_frames(&stack.frames, MASTER, &with_white_balance(), &Unmonitored).unwrap();
    assert!(alignment.frames[MASTER].white_balance.is_none(), "the master changes nothing");
    let rebased = alignment.rebased(7).unwrap();
    assert!(rebased.frames[7].white_balance.is_none());
    let max = show("white balance, rebased", &errors(&stack, &rebased));
    assert!(max < 0.005, "{max}");
    // Gray frames have no color to balance.
    let gray: Vec<_> = stack
        .frames
        .iter()
        .map(|f| {
            let values = f.as_raw().as_chunks::<3>().0.iter().map(|p| p.iter().sum::<f32>() / 3.0).collect();
            florastack_core::Image::from_vec(f.width(), f.height(), 1, f.color_space().clone(), values).unwrap()
        })
        .collect();
    let aligned = align_frames(&gray, MASTER, &with_white_balance(), &Unmonitored).unwrap();
    assert!(aligned.frames.iter().all(|f| f.white_balance.is_none()));
}
