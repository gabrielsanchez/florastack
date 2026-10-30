//! Splitting batches (roadmap 3.4) on synthetic stacks: neighbors of one stack link as one stack, however blurred, and
//! a batch of three stacks shot one after another (one scene, the same scene framed anew, another scene) is cut
//! exactly where each starts. Each test prints what it measured (`cargo test -p florastack-align --test split --
//! --nocapture`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_align::{Link, Neighbors, Reason, SplitSettings, boundaries, link};
use florastack_core::ops::resize;
use florastack_core::testutil::{StackParams, generate, scene};
use florastack_core::{Image, Rect, Unmonitored};

const WIDTH: usize = 240;
const HEIGHT: usize = 180;

/// A stack of `frames` of the textured scene of `seed`, seen through a window `zoom` times smaller than the frame
/// (1 for none) centred `offset` pixels from the scene's centre, focused from the scene's near end to its far end.
fn stack(frames: usize, seed: u64, zoom: f64, offset: (usize, usize), noisy: bool) -> Vec<Image<f32>> {
    let (sharp, depth) = scene::textured(WIDTH * 2, HEIGHT * 2, seed).unwrap();
    let base = if noisy { StackParams::noisy_dusty(frames, WIDTH * 2, HEIGHT * 2, seed) } else { StackParams::clean(frames, seed) };
    let params = StackParams { dust: Vec::new(), blur_per_depth: 10.0, max_blur: 8.0, ..base };
    let generated = generate(&sharp, &depth, &params).unwrap();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (w, h) = ((WIDTH as f64 / zoom).round() as usize, (HEIGHT as f64 / zoom).round() as usize);
    let (x, y) = (WIDTH - w / 2 + offset.0, HEIGHT - h / 2 + offset.1);
    generated
        .frames
        .iter()
        .map(|f| {
            let window = f.crop(Rect::new(x, y, w, h)).unwrap();
            if (w, h) == (WIDTH, HEIGHT) { window } else { resize(&window, WIDTH, HEIGHT).unwrap() }
        })
        .collect()
}

fn links(frames: &[Image<f32>]) -> Vec<Link> {
    frames.windows(2).map(|pair| link(&pair[0], &pair[1], &Unmonitored).unwrap()).collect()
}

fn show(what: &str, links: &[Link]) {
    let fmt = |l: &Link| format!("{}{:.2}/{:.2} s{:.1} t{:.1}", if l.matched { "" } else { "!" }, l.score, l.similarity, l.scale_step, l.shift);
    println!("{what}: {}", links.iter().map(fmt).collect::<Vec<_>>().join("  "));
}

#[test]
fn neighbors_of_one_stack_stay_together() {
    let settings = SplitSettings::default();
    for (name, frames) in [("clean", stack(10, 41, 1.0, (0, 0), false)), ("noisy", stack(10, 42, 1.0, (0, 0), true))] {
        let measured = links(&frames);
        show(name, &measured);
        let pairs: Vec<Neighbors> = measured.iter().map(|&l| Neighbors { gap: None, link: Some(l) }).collect();
        assert!(boundaries(&pairs, &settings).is_empty(), "{name}: {:?}", boundaries(&pairs, &settings));
    }
}

#[test]
fn three_concatenated_stacks_are_cut_where_each_starts() {
    let first = stack(8, 51, 1.0, (0, 0), false);
    // The same scene, framed anew: 25% larger.
    let reframed = stack(8, 51, 1.25, (0, 0), false);
    let other = stack(8, 52, 1.0, (0, 0), false);
    let batch: Vec<Image<f32>> = first.iter().chain(&reframed).chain(&other).cloned().collect();
    let measured = links(&batch);
    show("batch", &measured);
    let pairs: Vec<Neighbors> = measured.iter().map(|&l| Neighbors { gap: None, link: Some(l) }).collect();
    let cuts = boundaries(&pairs, &SplitSettings::default());
    println!("{cuts:?}");
    assert_eq!(cuts.iter().map(|b| b.index).collect::<Vec<_>>(), [8, 16]);
    assert!(cuts.iter().all(|c| matches!(c.reasons[..], [Reason::Scene { .. }])), "{cuts:?}");
}

#[test]
fn a_new_framing_of_the_same_scene_is_cut_by_its_jump() {
    let first = stack(4, 61, 1.0, (0, 0), false);
    let settings = SplitSettings::default();
    for (name, next, expected) in
        [("12% larger", stack(4, 61, 1.12, (0, 0), false), "scale"), ("moved a fifth", stack(4, 61, 1.0, (48, 36), false), "shift")]
    {
        let l = link(&first[3], &next[0], &Unmonitored).unwrap();
        show(name, &[l]);
        let cuts = boundaries(&[Neighbors { gap: None, link: Some(l) }], &settings);
        println!("{cuts:?}");
        let reason = match cuts.first().map(|c| c.reasons.as_slice()) {
            Some([Reason::Scale { .. }, ..]) => "scale",
            Some([Reason::Shift { .. }, ..]) => "shift",
            Some([Reason::Scene { .. }]) => "scene",
            _ => "none",
        };
        assert_eq!(reason, expected, "{name}: {cuts:?}");
    }
}
