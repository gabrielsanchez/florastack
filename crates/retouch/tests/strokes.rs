//! Strokes painted and replayed: replay is deterministic (twice, tile by tile, live as drawn, over a region), undo and
//! redo round-trip, a Details stroke from an identical source changes nothing, and each brush does what it is for.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_core::{ColorSpace, Image, Rect};
use florastack_project::FrameId;
use florastack_retouch::{Brush, Fields, LiveStroke, Painted, Source, Stroke, apply, render, replay};
use proptest::prelude::*;

const W: usize = 96;
const H: usize = 72;

fn image(f: impl Fn(usize, usize) -> [f32; 3]) -> Image<f32> {
    let f = &f;
    let data = (0..H).flat_map(|y| (0..W).flat_map(move |x| f(x, y))).collect();
    Image::from_vec(W, H, 3, ColorSpace::LINEAR, data).unwrap()
}

/// A stacked result: texture with soft detail.
fn output() -> Image<f32> {
    image(|x, y| {
        let t = ((x * 7 + y * 13 + (x * y) % 29) % 60) as f32 / 300.0;
        [0.25 + t, 0.3 + t * 0.8, 0.2 + t * 0.6]
    })
}

/// A frame: the same scene, brighter and tinted, with fine detail in a band.
fn frame() -> Image<f32> {
    image(|x, y| {
        let t = ((x * 7 + y * 13 + (x * y) % 29) % 60) as f32 / 300.0;
        let detail = if (30..60).contains(&x) { ((x + y) % 2) as f32 * 0.2 } else { 0.0 };
        [(0.25 + t + detail) * 1.3, (0.3 + t * 0.8 + detail) * 1.25, (0.2 + t * 0.6 + detail) * 1.4]
    })
}

fn frame_id(n: u64) -> FrameId {
    serde_json::from_value(serde_json::json!(n)).unwrap()
}

fn stroke(brush: Brush, samples: &[[f64; 3]]) -> Stroke {
    Stroke {
        version: 1,
        brush,
        source: brush.needs_source().then(|| Source::Frame(frame_id(1))),
        size: 18.0,
        hardness: 0.3,
        opacity: 0.9,
        samples: samples.to_vec(),
    }
}

fn whole() -> Rect {
    Rect::new(0, 0, W, H)
}

/// Three strokes crossing one another, by three brushes.
fn strokes() -> Vec<Stroke> {
    vec![
        stroke(Brush::Details, &[[10.0, 10.0, 1.0], [60.0, 30.0, 0.8], [80.0, 60.0, 0.5]]),
        stroke(Brush::Pixels, &[[20.0, 50.0, 1.0], [50.0, 20.0, 1.0]]),
        stroke(Brush::Darken, &[[5.0, 40.0, 1.0], [90.0, 45.0, 1.0]]),
    ]
}

fn painted<'a>(strokes: &'a [Stroke], fields: Fields<'a>) -> Vec<Painted<'a>> {
    strokes.iter().map(|stroke| Painted { stroke, fields }).collect()
}

fn same_bits(a: &Image<f32>, b: &Image<f32>) -> bool {
    a.as_raw().iter().zip(b.as_raw()).all(|(x, y)| x.to_bits() == y.to_bits())
}

#[test]
fn replay_is_deterministic() {
    let (base, source) = (output(), frame());
    let fields = Fields { base: &base, source: Some(&source), coverage: None };
    let strokes = strokes();
    let first = render(&base, &painted(&strokes, fields)).unwrap();
    let second = render(&base, &painted(&strokes, fields)).unwrap();
    assert!(same_bits(&first, &second), "twice");
    assert!(!same_bits(&first, &base), "the strokes painted something");
    // Tile by tile: every stroke over each tile in turn.
    let mut tiled = base.clone();
    for ty in (0..H).step_by(17) {
        for tx in (0..W).step_by(23) {
            let tile = Rect::new(tx, ty, 23.min(W - tx), 17.min(H - ty));
            for p in painted(&strokes, fields) {
                apply(&mut tiled, p, tile).unwrap();
            }
        }
    }
    assert!(same_bits(&first, &tiled), "tile by tile");
    // Live, a sample at a time.
    let mut live = base.clone();
    for s in &strokes {
        let mut drawing = LiveStroke::new(Stroke { samples: vec![s.samples[0]], ..s.clone() }).unwrap();
        drawing.extend(&mut live, &fields, &[]).unwrap();
        for sample in &s.samples[1..] {
            drawing.extend(&mut live, &fields, &[*sample]).unwrap();
        }
        assert_eq!(drawing.stroke(), s);
    }
    assert!(same_bits(&first, &live), "live, as drawn");
}

#[test]
fn undo_and_redo_round_trip() {
    let (base, source) = (output(), frame());
    let fields = Fields { base: &base, source: Some(&source), coverage: None };
    let strokes = strokes();
    let all = render(&base, &painted(&strokes, fields)).unwrap();
    let without_last = render(&base, &painted(&strokes[..2], fields)).unwrap();
    // Undo: the last stroke's region is replayed with the strokes before it.
    let mut canvas = all.clone();
    let reach = Stroke::reach(&strokes[2].dabs(), 1.0, W, H).unwrap();
    replay(&mut canvas, &base, &painted(&strokes[..2], fields), reach).unwrap();
    assert!(same_bits(&canvas, &without_last), "undone");
    // Redo: the stroke painted again.
    apply(&mut canvas, Painted { stroke: &strokes[2], fields }, whole()).unwrap();
    assert!(same_bits(&canvas, &all), "redone");
    // Undoing the middle stroke, which the last one crosses (a layer hidden, say), replays over its region alone.
    let mut canvas = all.clone();
    let reach = Stroke::reach(&strokes[1].dabs(), 1.0, W, H).unwrap();
    let kept = [strokes[0].clone(), strokes[2].clone()];
    replay(&mut canvas, &base, &painted(&kept, fields), reach).unwrap();
    assert!(same_bits(&canvas, &render(&base, &painted(&kept, fields)).unwrap()));
}

#[test]
fn a_details_stroke_from_an_identical_source_changes_nothing() {
    let base = output();
    let fields = Fields { base: &base, source: Some(&base), coverage: None };
    let mut canvas = base.clone();
    let mut s = stroke(Brush::Details, &[[5.0, 5.0, 1.0], [90.0, 65.0, 1.0], [10.0, 60.0, 0.4]]);
    s.size = 60.0;
    s.opacity = 1.0;
    let changed = apply(&mut canvas, Painted { stroke: &s, fields }, whole()).unwrap();
    assert!(changed.is_some(), "the stroke reached pixels");
    assert!(same_bits(&canvas, &base));
}

#[test]
fn details_matches_a_brighter_source_and_keeps_its_detail() {
    let (base, source) = (output(), frame());
    let fields = Fields { base: &base, source: Some(&source), coverage: None };
    let s = stroke(Brush::Details, &[[48.0, 36.0, 1.0]]);
    let mut details = base.clone();
    apply(&mut details, Painted { stroke: &s, fields }, whole()).unwrap();
    let mut pixels = base.clone();
    apply(&mut pixels, Painted { stroke: &Stroke { brush: Brush::Pixels, ..s.clone() }, fields }, whole()).unwrap();
    // At the dab's center, away from the detail's peaks, Details stays near the output's brightness where Pixels
    // brings the frame's 30% more.
    let mean = |img: &Image<f32>| {
        let mut sum = 0.0;
        for y in 32..40 {
            for x in 44..52 {
                sum += img.get(x, y, 1).unwrap();
            }
        }
        sum / 64.0
    };
    let (b, d, p) = (mean(&base), mean(&details), mean(&pixels));
    assert!((d - b).abs() < 0.03 * b, "details {d}, output {b}");
    assert!(p > b * 1.15, "pixels {p}, output {b}");
    // The frame's fine detail comes through: neighbors differ as in the frame.
    let contrast = |img: &Image<f32>| (img.get(48, 36, 1).unwrap() - img.get(49, 36, 1).unwrap()).abs();
    assert!(contrast(&details) > 0.1, "{}", contrast(&details));
}

#[test]
fn the_eraser_brings_back_the_output_and_darken_never_lightens() {
    let (base, source) = (output(), frame());
    let fields = Fields { base: &base, source: Some(&source), coverage: None };
    let painted_over = render(&base, &painted(&strokes(), fields)).unwrap();
    let mut restored = painted_over.clone();
    let mut eraser = stroke(Brush::Eraser, &[[0.0, 36.0, 1.0], [96.0, 36.0, 1.0]]);
    (eraser.size, eraser.hardness, eraser.opacity) = (200.0, 1.0, 1.0);
    apply(&mut restored, Painted { stroke: &eraser, fields }, whole()).unwrap();
    assert!(same_bits(&restored, &base), "a hard, full eraser over everything restores the output");
    let mut darkened = base.clone();
    let mut darken = stroke(Brush::Darken, &[[48.0, 36.0, 1.0]]);
    darken.size = 80.0;
    apply(&mut darkened, Painted { stroke: &darken, fields }, whole()).unwrap();
    let luma = |img: &Image<f32>, x: usize, y: usize| {
        let p = img.pixel(x, y).unwrap();
        0.2627 * p[0] + 0.678 * p[1] + 0.0593 * p[2]
    };
    for y in 0..H {
        for x in 0..W {
            assert!(luma(&darkened, x, y) <= luma(&base, x, y) + 1e-6, "({x}, {y}) got lighter");
        }
    }
}

#[test]
fn a_frame_paints_only_where_it_covers_and_a_cancelled_stroke_leaves_nothing() {
    let (base, source) = (output(), frame());
    let coverage = Image::from_vec(W, H, 1, ColorSpace::LINEAR, (0..W * H).map(|i| if i % W < 48 { 0.0 } else { 1.0 }).collect()).unwrap();
    let fields = Fields { base: &base, source: Some(&source), coverage: Some(&coverage) };
    let mut s = stroke(Brush::Pixels, &[[10.0, 36.0, 1.0], [90.0, 36.0, 1.0]]);
    s.size = 40.0;
    let mut canvas = base.clone();
    apply(&mut canvas, Painted { stroke: &s, fields }, whole()).unwrap();
    assert_eq!(canvas.pixel(20, 36), base.pixel(20, 36), "uncovered");
    assert_ne!(canvas.pixel(70, 36), base.pixel(70, 36), "covered");
    let mut live = base.clone();
    let mut drawing = LiveStroke::new(s).unwrap();
    drawing.extend(&mut live, &fields, &[]).unwrap();
    assert!(!same_bits(&live, &base));
    drawing.cancel(&mut live);
    assert!(same_bits(&live, &base));
}

#[test]
fn images_that_dont_fit_are_refused() {
    let base = output();
    let small = Image::from_vec(10, 10, 3, ColorSpace::LINEAR, vec![0.5; 300]).unwrap();
    let fields = Fields { base: &base, source: Some(&small), coverage: None };
    let mut canvas = base.clone();
    let s = stroke(Brush::Pixels, &[[5.0, 5.0, 1.0]]);
    assert!(apply(&mut canvas, Painted { stroke: &s, fields }, whole()).is_err());
    let gray = Image::from_vec(W, H, 1, ColorSpace::LINEAR, vec![0.5; W * H]).unwrap();
    let fields = Fields { base: &base, source: Some(&gray), coverage: None };
    assert!(apply(&mut canvas, Painted { stroke: &s, fields }, whole()).is_err(), "gray onto color");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Any stroke: painted whole, live in pieces and over a split of the image give the same bits.
    #[test]
    fn any_stroke_paints_the_same_however_it_is_painted(
        brush in prop::sample::select(vec![Brush::Details, Brush::Pixels, Brush::Darken, Brush::Lighten, Brush::Eraser]),
        size in 1.0f64..60.0,
        hardness in 0.0f64..=1.0,
        opacity in 0.0f64..=1.0,
        samples in prop::collection::vec((0.0f64..96.0, 0.0f64..72.0, 0.0f64..=1.0), 1..12),
        cut in 1usize..95,
    ) {
        let (base, source) = (output(), frame());
        let fields = Fields { base: &base, source: Some(&source), coverage: None };
        let s = Stroke {
            size,
            hardness,
            opacity,
            samples: samples.iter().map(|&(x, y, p)| [x, y, p]).collect(),
            ..stroke(brush, &[[0.0, 0.0, 1.0]])
        }
        .quantized();
        let mut whole_canvas = base.clone();
        apply(&mut whole_canvas, Painted { stroke: &s, fields }, whole()).unwrap();
        let mut split = base.clone();
        apply(&mut split, Painted { stroke: &s, fields }, Rect::new(0, 0, cut, H)).unwrap();
        apply(&mut split, Painted { stroke: &s, fields }, Rect::new(cut, 0, W - cut, H)).unwrap();
        prop_assert!(same_bits(&whole_canvas, &split));
        let mut live = base.clone();
        let mut drawing = LiveStroke::new(Stroke { samples: vec![s.samples[0]], ..s.clone() }).unwrap();
        drawing.extend(&mut live, &fields, &[]).unwrap();
        for chunk in s.samples[1..].chunks(3) {
            drawing.extend(&mut live, &fields, chunk).unwrap();
        }
        prop_assert!(same_bits(&whole_canvas, &live));
    }
}
