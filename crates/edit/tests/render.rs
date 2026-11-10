#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Test scenes and slider values, all small: f32 holds them as exactly as the checks need.
#![allow(clippy::cast_possible_truncation)]
//! Rendering adjustments: neutral settings change nothing, each slider moves the image the way it says, masks apply
//! where they select, and renders repeat exactly.

use florastack_core::testutil::scene;
use florastack_core::{ColorSpace, Image};
use florastack_edit::{Adjustments, Component, EditError, Global, Local, Mask, Op, Renderer, Request, Shape, Size, Stroke};

const W: usize = 192;
const H: usize = 128;

/// The textured test scene (linear RGB) and its depth map.
fn scene() -> (Image<f32>, Image<f32>) {
    scene::textured(W, H, 7).unwrap()
}

fn renderer() -> Renderer {
    Renderer::new(&scene().0).unwrap()
}

fn render(r: &Renderer, a: &Adjustments) -> Image<f32> {
    r.render(a, &Request::FULL).unwrap()
}

fn luma(p: &[f32]) -> f32 {
    0.2627 * p[0] + 0.678 * p[1] + 0.0593 * p[2]
}

/// The mean luminance of the pixels `keep` selects.
fn mean_luma(image: &Image<f32>, keep: impl Fn(usize, usize) -> bool) -> f32 {
    let (mut sum, mut n) = (0.0, 0.0);
    for y in 0..image.height() {
        for x in 0..image.width() {
            if keep(x, y) {
                sum += luma(image.pixel(x, y).unwrap());
                n += 1.0;
            }
        }
    }
    sum / n
}

fn max_difference(a: &Image<f32>, b: &Image<f32>) -> f32 {
    assert!(a.same_shape(b));
    a.as_raw().iter().zip(b.as_raw()).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn masked(shape: Shape, adjust: Local) -> Adjustments {
    Adjustments {
        masks: vec![Mask { id: 1, components: vec![Component { op: Op::Add, invert: false, shape }], adjust, ..Mask::default() }],
        ..Adjustments::default()
    }
}

fn brighter() -> Local {
    Local { exposure: 1.0, ..Local::default() }
}

#[test]
fn neutral_adjustments_give_the_image_back_exactly() {
    let (image, _) = scene();
    let r = Renderer::new(&image).unwrap();
    let out = render(&r, &Adjustments::default());
    assert_eq!(out.as_raw(), image.as_raw());
    assert_eq!(out.color_space(), image.color_space());
}

#[test]
fn the_pipeline_changes_nothing_where_nothing_is_adjusted() {
    // A brightening mask that selects nothing sends the image through every stage of the pipeline.
    let (image, _) = scene();
    let r = Renderer::new(&image).unwrap();
    let nothing = masked(Shape::Linear { start: [0.0, -2.0], end: [0.0, -1.0] }, brighter());
    let out = render(&r, &nothing);
    let d = max_difference(&out, &image);
    // The pipeline computes in f32 through color conversions and back: within a 16-bit step.
    assert!(d < 1.0 / 65535.0, "{d}");
}

/// Renders `g` on the scene: the scene, and the result.
fn global(g: Global) -> (Image<f32>, Image<f32>) {
    let (image, _) = scene();
    let out = render(&Renderer::new(&image).unwrap(), &Adjustments { global: g, ..Adjustments::default() });
    (image, out)
}

/// The standard deviation of luminance.
fn luma_spread(image: &Image<f32>) -> f32 {
    let l: Vec<f32> = image.as_raw().as_chunks::<3>().0.iter().map(|p| luma(p)).collect();
    let mean = l.iter().sum::<f32>() / l.len() as f32;
    (l.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / l.len() as f32).sqrt()
}

/// The mean of each channel.
fn channel_means(image: &Image<f32>) -> [f32; 3] {
    let channels = image.channels();
    let mut sum = [0.0f32; 3];
    for p in image.as_raw().chunks_exact(channels) {
        for (s, v) in sum.iter_mut().zip(p) {
            *s += v;
        }
    }
    sum.map(|s| s / image.pixel_count() as f32)
}

/// The mean distance of each pixel's channels from their mean: how colorful the image is.
fn colorfulness(image: &Image<f32>) -> f32 {
    let mut sum = 0.0;
    for p in image.as_raw().as_chunks::<3>().0 {
        let m = (p[0] + p[1] + p[2]) / 3.0;
        sum += p.iter().map(|v| (v - m).abs()).sum::<f32>();
    }
    sum / image.pixel_count() as f32
}

/// The mean squared difference between each pixel's luminance and its four neighbors': fine detail.
fn detail(image: &Image<f32>) -> f32 {
    let l = |x: usize, y: usize| luma(image.pixel(x, y).unwrap());
    let mut sum = 0.0;
    for y in 1..image.height() - 1 {
        for x in 1..image.width() - 1 {
            let lap = 4.0 * l(x, y) - l(x - 1, y) - l(x + 1, y) - l(x, y - 1) - l(x, y + 1);
            sum += lap * lap;
        }
    }
    sum / image.pixel_count() as f32
}

#[test]
fn every_slider_moves_the_image_its_way() {
    let all = |_, _| true;
    let (image, out) = global(Global { exposure: 1.0, ..Global::default() });
    let (a, b) = (mean_luma(&image, all), mean_luma(&out, all));
    assert!(b > 1.6 * a, "exposure +1: {a} -> {b}");

    let (image, out) = global(Global { contrast: 60.0, ..Global::default() });
    assert!(luma_spread(&out) > 1.05 * luma_spread(&image), "contrast");

    let (image, out) = global(Global { saturation: -100.0, ..Global::default() });
    assert!(colorfulness(&out) < 0.05 * colorfulness(&image), "saturation -100: {} -> {}", colorfulness(&image), colorfulness(&out));
    let (image, out) = global(Global { saturation: 50.0, ..Global::default() });
    assert!(colorfulness(&out) > 1.1 * colorfulness(&image), "saturation +50");
    let (image, out) = global(Global { vibrance: 60.0, ..Global::default() });
    assert!(colorfulness(&out) > 1.05 * colorfulness(&image), "vibrance");

    let (image, out) = global(Global { temperature: 50.0, ..Global::default() });
    let (a, b) = (channel_means(&image), channel_means(&out));
    assert!(b[0] / b[2] > 1.1 * a[0] / a[2], "warmer: {a:?} -> {b:?}");
    let (image, out) = global(Global { tint: 50.0, ..Global::default() });
    let (a, b) = (channel_means(&image), channel_means(&out));
    assert!(b[1] / (b[0] + b[2]) < 0.97 * a[1] / (a[0] + a[2]), "magenta: {a:?} -> {b:?}");

    let (image, out) = global(Global { shadows: 80.0, ..Global::default() });
    let dark = |img: &Image<f32>| mean_luma(img, |x, y| luma(image.pixel(x, y).unwrap()) < 0.05);
    assert!(dark(&out) > 1.1 * dark(&image), "shadows: {} -> {}", dark(&image), dark(&out));
    let (image, out) = global(Global { highlights: -80.0, ..Global::default() });
    let bright = |img: &Image<f32>| mean_luma(img, |x, y| luma(image.pixel(x, y).unwrap()) > 0.5);
    assert!(bright(&out) < 0.97 * bright(&image), "highlights: {} -> {}", bright(&image), bright(&out));

    for g in [
        Global { clarity: 80.0, ..Global::default() },
        Global { texture: 80.0, ..Global::default() },
        Global { sharpening: 120.0, ..Global::default() },
    ] {
        let (image, out) = global(g);
        assert!(detail(&out) > 1.1 * detail(&image), "{g:?}: {} -> {}", detail(&image), detail(&out));
    }
    let (image, out) = global(Global { noise_reduction: 80.0, ..Global::default() });
    assert!(detail(&out) < 0.95 * detail(&image), "noise reduction: {} -> {}", detail(&image), detail(&out));
    let (image, out) = global(Global { dehaze: 60.0, ..Global::default() });
    assert!(luma_spread(&out) > 1.02 * luma_spread(&image), "dehaze");
}

#[test]
fn renders_repeat_exactly() {
    let r = renderer();
    let a = Adjustments {
        global: Global { exposure: 0.4, clarity: 30.0, sharpening: 50.0, noise_reduction: 20.0, ..Global::default() },
        masks: masked(Shape::Radial { center: [0.4, 0.5], radius_x: 0.2, radius_y: 0.1, angle: 20.0, feather: 50.0 }, brighter()).masks,
    };
    let first = render(&r, &a);
    // Again from the cache, and from a renderer that has none.
    assert_eq!(render(&r, &a).as_raw(), first.as_raw());
    assert_eq!(render(&renderer(), &a).as_raw(), first.as_raw());
}

#[test]
fn a_reduced_render_fits_and_keeps_the_aspect_ratio() {
    let r = renderer();
    let a = Adjustments { global: Global { exposure: 0.5, ..Global::default() }, ..Adjustments::default() };
    let out = r.render(&a, &Request { size: Size::Fit { width: 96, height: 96 }, draft: true, overlay: None }).unwrap();
    assert_eq!((out.width(), out.height()), (96, 64));
    // Never enlarged.
    let out = r.render(&a, &Request { size: Size::Fit { width: 1000, height: 1000 }, draft: false, overlay: None }).unwrap();
    assert_eq!((out.width(), out.height()), (W, H));
}

#[test]
fn coverage_is_kept() {
    let (image, _) = scene();
    let data: Vec<f32> =
        image.as_raw().as_chunks::<3>().0.iter().enumerate().flat_map(|(i, p)| [p[0], p[1], p[2], if i % W < 20 { 0.0 } else { 1.0 }]).collect();
    let rgba = Image::from_vec(W, H, 4, ColorSpace::LINEAR, data).unwrap();
    let r = Renderer::new(&rgba).unwrap();
    let out = render(&r, &Adjustments { global: Global { exposure: 1.0, ..Global::default() }, ..Adjustments::default() });
    assert_eq!(out.channels(), 4);
    for (i, p) in out.as_raw().as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(p[3], if i % W < 20 { 0.0 } else { 1.0 });
    }
    let small = r.render(&Adjustments::default(), &Request { size: Size::Fit { width: 96, height: 64 }, draft: false, overlay: None }).unwrap();
    assert_eq!(small.channels(), 4);
}

#[test]
fn a_whole_image_mask_adjusts_every_pixel() {
    let (image, _) = scene();
    let out = render(&Renderer::new(&image).unwrap(), &masked(Shape::Whole, brighter()));
    let all = |_, _| true;
    assert!(mean_luma(&out, all) > 1.6 * mean_luma(&image, all));
    for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1)] {
        assert!(luma(out.pixel(x, y).unwrap()) > 1.5 * luma(image.pixel(x, y).unwrap()), "({x}, {y})");
    }
}

#[test]
fn a_linear_mask_fades_from_start_to_end() {
    let (image, _) = scene();
    let out = render(&Renderer::new(&image).unwrap(), &masked(Shape::Linear { start: [0.0, 0.2], end: [0.0, 0.5] }, brighter()));
    let ratio = |y0: usize, y1: usize| mean_luma(&out, |_, y| (y0..y1).contains(&y)) / mean_luma(&image, |_, y| (y0..y1).contains(&y));
    assert!(ratio(0, H / 5) > 1.6, "full before the start: {}", ratio(0, H / 5));
    assert!((ratio(H * 6 / 10, H) - 1.0).abs() < 0.01, "nothing beyond the end: {}", ratio(H * 6 / 10, H));
}

#[test]
fn a_radial_mask_selects_inside_its_ellipse() {
    let (image, _) = scene();
    let shape = Shape::Radial { center: [0.5, 0.5], radius_x: 0.15, radius_y: 0.15, angle: 0.0, feather: 0.0 };
    let r = Renderer::new(&image).unwrap();
    let out = render(&r, &masked(shape.clone(), brighter()));
    let distance = |x: usize, y: usize| (x as f32 - W as f32 / 2.0).hypot(y as f32 - H as f32 / 2.0);
    let inside = |x, y| distance(x, y) < 0.1 * W as f32;
    let outside = |x, y| distance(x, y) > 0.2 * W as f32;
    assert!(mean_luma(&out, inside) > 1.6 * mean_luma(&image, inside));
    assert!((mean_luma(&out, outside) / mean_luma(&image, outside) - 1.0).abs() < 0.01);
    // Inverted, the other way round.
    let mut a = masked(shape, brighter());
    a.masks[0].invert = true;
    let out = render(&r, &a);
    assert!((mean_luma(&out, inside) / mean_luma(&image, inside) - 1.0).abs() < 0.01);
    assert!(mean_luma(&out, outside) > 1.6 * mean_luma(&image, outside));
}

#[test]
fn components_add_subtract_and_intersect() {
    let (image, _) = scene();
    let r = Renderer::new(&image).unwrap();
    // The left two fifths, and the top two fifths.
    let left = Shape::Linear { start: [0.4, 0.0], end: [0.45, 0.0] };
    let top = Shape::Linear { start: [0.0, 0.4], end: [0.0, 0.45] };
    let quadrant = |out: &Image<f32>, left: bool, top: bool| {
        let pick = |x: usize, y: usize| (x < W / 3 && left || x > W / 2 && !left) && (y < H / 3 && top || y > H / 2 && !top);
        mean_luma(out, pick) / mean_luma(&image, pick)
    };
    let mask = |op| Adjustments {
        masks: vec![Mask {
            id: 1,
            components: vec![Component { op: Op::Add, invert: false, shape: left.clone() }, Component { op, invert: false, shape: top.clone() }],
            adjust: brighter(),
            ..Mask::default()
        }],
        ..Adjustments::default()
    };
    let unchanged = |v: f32| (v - 1.0).abs() < 0.01;
    let out = render(&r, &mask(Op::Add));
    assert!(quadrant(&out, true, false) > 1.6 && quadrant(&out, false, true) > 1.6 && unchanged(quadrant(&out, false, false)));
    let out = render(&r, &mask(Op::Intersect));
    assert!(quadrant(&out, true, true) > 1.6 && unchanged(quadrant(&out, true, false)) && unchanged(quadrant(&out, false, true)));
    let out = render(&r, &mask(Op::Subtract));
    assert!(quadrant(&out, true, false) > 1.6 && unchanged(quadrant(&out, true, true)) && unchanged(quadrant(&out, false, true)));
}

#[test]
fn a_brush_selects_along_its_strokes() {
    let (image, _) = scene();
    let stroke = Stroke { points: vec![[0.1, 0.5], [0.9, 0.5]], size: 0.05, feather: 0.0, ..Stroke::default() };
    let out = render(&Renderer::new(&image).unwrap(), &masked(Shape::Brush { strokes: vec![stroke] }, brighter()));
    let on = |x: usize, y: usize| x > W / 5 && x < W * 4 / 5 && y.abs_diff(H / 2) < 3;
    let off = |_, y: usize| y.abs_diff(H / 2) > H / 4;
    assert!(mean_luma(&out, on) > 1.6 * mean_luma(&image, on));
    assert!((mean_luma(&out, off) / mean_luma(&image, off) - 1.0).abs() < 0.01);
}

#[test]
fn a_luminance_range_selects_by_brightness() {
    let (image, _) = scene();
    let shape = Shape::Luminance { low: 0.6, high: 1.0, low_feather: 0.05, high_feather: 0.0 };
    let out = render(&Renderer::new(&image).unwrap(), &masked(shape, Local { exposure: -1.0, ..Local::default() }));
    let l = |x, y| luma(image.pixel(x, y).unwrap());
    let (bright, dark) = (|x, y| l(x, y) > 0.6, |x, y| l(x, y) < 0.1);
    assert!(mean_luma(&out, bright) < 0.8 * mean_luma(&image, bright), "{} -> {}", mean_luma(&image, bright), mean_luma(&out, bright));
    assert!((mean_luma(&out, dark) / mean_luma(&image, dark) - 1.0).abs() < 0.05, "{}", mean_luma(&out, dark) / mean_luma(&image, dark));
}

/// The scene with fine detail everywhere, a 2-pixel checker of 20% over it, so that all of it is subject (its discs are
/// flat), and a renderer of it with its depth map.
fn detailed_scene() -> (Image<f32>, Image<f32>, Renderer) {
    let (image, depth) = scene();
    let data =
        image.as_raw().iter().enumerate().map(|(i, v)| v * if ((i / 3) % W / 2 + (i / 3) / W / 2).is_multiple_of(2) { 1.1 } else { 0.9 }).collect();
    let image = Image::from_vec(W, H, 3, ColorSpace::LINEAR, data).unwrap();
    let r = Renderer::new(&image).unwrap().with_depth(&depth).unwrap();
    (image, depth, r)
}

/// Whether the pixel at `(x, y)` lies within two pixels of a change of `inside`.
fn near_edge(inside: impl Fn(usize, usize) -> bool, x: usize, y: usize) -> bool {
    let here = inside(x, y);
    (x.saturating_sub(2)..(x + 3).min(W)).any(|i| (y.saturating_sub(2)..(y + 3).min(H)).any(|j| inside(i, j) != here))
}

#[test]
fn a_depth_range_selects_what_lies_in_it() {
    let (image, depth, r) = detailed_scene();
    // The scene's discs lie at depths 0.3 to 0.5, its background at 0.6 to 1 and its bristles at 0 to 0.2.
    // Darkened: much of the discs is bright, where brightening would meet the highlights' shoulder.
    let out = render(&r, &masked(Shape::Depth { from: 0.28, to: 0.52, feather: 0.0 }, Local { exposure: -1.0, ..Local::default() }));
    let d = |x, y| depth.get(x, y, 0).unwrap();
    // Away from the discs' edges, which the mask's matte (8 bits, bilinearly sampled) softens.
    let range = |x, y| (0.28..=0.52).contains(&d(x, y));
    let discs = |x, y| range(x, y) && !near_edge(range, x, y);
    let rest = |x, y| !range(x, y) && !near_edge(range, x, y);
    let ratio = mean_luma(&out, discs) / mean_luma(&image, discs);
    assert!(ratio < 0.6, "{ratio}");
    let ratio = mean_luma(&out, rest) / mean_luma(&image, rest);
    assert!((ratio - 1.0).abs() < 0.01, "{ratio}");
}

#[test]
fn subject_and_background_follow_the_detail() {
    // The scene's detail on the left half, a soft gradient on the right: the subject is the left half, found with no
    // map at all.
    let (scene, _) = scene();
    let noise = |x: usize, y: usize| ((x * 7919 + y * 104_729) % 97) as f32 / 97.0 * 0.004;
    let image = Image::from_fn(W, H, 3, ColorSpace::LINEAR, |x, y, c| {
        if x < W / 2 { scene.get(x, y, c).unwrap() } else { 0.2 + 0.1 * y as f32 / H as f32 + noise(x, y) }
    })
    .unwrap();
    let r = Renderer::new(&image).unwrap();
    let subject = r.subject().unwrap();
    let (on, off) = (subject.get(W / 4, H / 2, 0).unwrap(), subject.get(W * 3 / 4, H / 2, 0).unwrap());
    assert!(on > 0.9 && off < 0.1, "{on} {off}");
    // Beyond the edge, which the averaged detail spreads over a few pixels (a mask's `refine` makes it follow the image's).
    let (left, right) = (|x: usize, _| x < W / 2 - 12, |x: usize, _| x > W / 2 + 12);
    let out = render(&r, &masked(Shape::Subject, brighter()));
    assert!(mean_luma(&out, left) > 1.6 * mean_luma(&image, left));
    assert!((mean_luma(&out, right) / mean_luma(&image, right) - 1.0).abs() < 0.01);
    let out = render(&r, &masked(Shape::Background, brighter()));
    assert!((mean_luma(&out, left) / mean_luma(&image, left) - 1.0).abs() < 0.01);
    assert!(mean_luma(&out, right) > 1.6 * mean_luma(&image, right));
}

#[test]
fn several_depth_ranges_each_select_their_own() {
    let (image, depth, r) = detailed_scene();
    // Five ranges, the most an output may have, each darkening by another amount: the discs and four bands of the
    // receding background.
    let ranges = [(0.28, 0.52), (0.6, 0.7), (0.7, 0.8), (0.8, 0.9), (0.9, 1.0)];
    let stops = [-0.5, -1.0, -1.5, -2.0, -2.5];
    let a = Adjustments {
        masks: ranges
            .iter()
            .enumerate()
            .map(|(i, &(near, far))| Mask {
                id: u32::try_from(i).unwrap() + 1,
                components: vec![Component { op: Op::Add, invert: false, shape: Shape::Depth { from: near, to: far, feather: 0.0 } }],
                adjust: Local { exposure: stops[i], ..Local::default() },
                ..Mask::default()
            })
            .collect(),
        ..Adjustments::default()
    };
    let out = render(&r, &a);
    let d = |x, y| depth.get(x, y, 0).unwrap();
    for (i, &(near, far)) in ranges.iter().enumerate() {
        let range = |x, y| (near as f32..=far as f32).contains(&d(x, y));
        let inside = |x, y| range(x, y) && !near_edge(range, x, y);
        let ratio = mean_luma(&out, inside) / mean_luma(&image, inside);
        // Its own exposure, within the tone curve's give, and none of the others'.
        let expected = 2f32.powf(stops[i] as f32);
        assert!((ratio / expected - 1.0).abs() < 0.15, "range {i}: {ratio}, expected {expected}");
    }
    // A sixth range is refused.
    let mut six = a.clone();
    six.masks.push(Mask {
        id: 9,
        components: vec![Component { op: Op::Add, invert: false, shape: Shape::Depth { from: 0.1, to: 0.15, feather: 0.0 } }],
        ..Mask::default()
    });
    assert!(matches!(r.render(&six, &Request::FULL), Err(EditError::Invalid { .. })));
}

#[test]
fn depth_masks_need_the_depth_map() {
    let r = renderer();
    assert!(!r.has_depth());
    // The subject needs none.
    r.render(&masked(Shape::Subject, brighter()), &Request::FULL).unwrap();
    let e = r.render(&masked(Shape::Depth { from: 0.0, to: 0.5, feather: 0.1 }, brighter()), &Request::FULL).unwrap_err();
    assert!(matches!(e, EditError::NoDepth), "{e}");
    // A hidden one doesn't.
    let mut a = masked(Shape::Depth { from: 0.0, to: 0.5, feather: 0.1 }, brighter());
    a.masks[0].visible = false;
    r.render(&a, &Request::FULL).unwrap();
    // A map of another aspect ratio is refused; a smaller one of the same is taken.
    let small = Image::from_vec(10, 10, 1, ColorSpace::LINEAR, vec![0.5; 100]).unwrap();
    assert!(matches!(r.with_depth(&small).unwrap_err(), EditError::Guide { .. }));
    let quarter = Image::from_vec(W / 4, H / 4, 1, ColorSpace::LINEAR, vec![0.5; W * H / 16]).unwrap();
    assert!(r.with_depth(&quarter).unwrap().has_depth());
}

#[test]
fn an_overlay_shows_the_mask() {
    let (image, _) = scene();
    let r = Renderer::new(&image).unwrap();
    let a = masked(Shape::Linear { start: [0.0, 0.2], end: [0.0, 0.5] }, Local::default());
    let out = r.render(&a, &Request { size: Size::Full, draft: false, overlay: Some(1) }).unwrap();
    // The selected top is tinted red; the bottom isn't.
    let redness = |img: &Image<f32>, y0: usize, y1: usize| {
        let m = channel_means(&img.crop(florastack_core::Rect::new(0, y0, W, y1 - y0)).unwrap());
        m[0] / (m[1] + 1e-6)
    };
    assert!(redness(&out, 0, H / 5) > 1.5 * redness(&image, 0, H / 5), "{} {}", redness(&out, 0, H / 5), redness(&image, 0, H / 5));
    assert!((redness(&out, H * 6 / 10, H) / redness(&image, H * 6 / 10, H) - 1.0).abs() < 0.1);
}

#[test]
fn hostile_adjustments_are_refused() {
    let r = renderer();
    let bad = [
        Adjustments { global: Global { exposure: f64::NAN, ..Global::default() }, ..Adjustments::default() },
        Adjustments { global: Global { contrast: 101.0, ..Global::default() }, ..Adjustments::default() },
        masked(Shape::Radial { center: [0.5, f64::INFINITY], radius_x: 0.1, radius_y: 0.1, angle: 0.0, feather: 0.0 }, brighter()),
        masked(Shape::Luminance { low: 0.8, high: 0.2, low_feather: 0.0, high_feather: 0.0 }, brighter()),
        masked(Shape::Brush { strokes: vec![Stroke { size: 0.0, ..Stroke::default() }] }, brighter()),
        masked(Shape::Color { samples: vec![[0.5, 0.0, 0.0]; 17], refine: 50.0 }, brighter()),
        masked(Shape::Whole, Local { amount: 101.0, ..Local::default() }),
    ];
    for a in bad {
        assert!(matches!(r.render(&a, &Request::FULL), Err(EditError::Invalid { .. })), "{a:?}");
    }
    let mut twins = masked(Shape::Whole, brighter());
    twins.masks.push(twins.masks[0].clone());
    assert!(matches!(r.render(&twins, &Request::FULL), Err(EditError::Invalid { .. })));
    // Images it can't take.
    let gray = Image::from_vec(4, 4, 1, ColorSpace::LINEAR, vec![0.5; 16]).unwrap();
    assert!(matches!(Renderer::new(&gray), Err(EditError::Image { .. })));
}

#[test]
fn non_finite_samples_become_black() {
    let mut data = vec![0.5f32; 8 * 8 * 3];
    data[0] = f32::NAN;
    data[4] = f32::INFINITY;
    let image = Image::from_vec(8, 8, 3, ColorSpace::LINEAR, data).unwrap();
    let a = Adjustments { global: Global { exposure: 0.5, ..Global::default() }, ..Adjustments::default() };
    let out = render(&Renderer::new(&image).unwrap(), &a);
    assert!(out.as_raw().iter().all(|v| v.is_finite()));
}

#[test]
fn a_color_sample_is_in_oklab() {
    let r = renderer();
    let c = r.sample_color(&Adjustments::default(), [0.5, 0.5]).unwrap().unwrap();
    assert!((0.0..=1.0).contains(&c[0]) && c[1].abs() < 0.5 && c[2].abs() < 0.5, "{c:?}");
    assert_eq!(r.sample_color(&Adjustments::default(), [1.5, 0.5]).unwrap(), None);
    // A mask of the sampled color selects some of the image.
    let out = render(&r, &masked(Shape::Color { samples: vec![c], refine: 50.0 }, brighter()));
    let all = |_, _| true;
    assert!(mean_luma(&out, all) > 1.01 * mean_luma(&scene().0, all));
}

#[test]
fn a_stored_ai_selection_renders_without_the_model() {
    use florastack_edit::{Click, Selection};
    let (image, _) = scene();
    let r = Renderer::new(&image).unwrap();
    // A selection of the left half: logits far positive there, far negative elsewhere.
    let side = 64;
    let logits: Vec<f32> = (0..side * side).map(|i| if i % side < side / 2 { 12.0 } else { -12.0 }).collect();
    let selection = Some(Selection::from_logits(side, &logits));
    let clicks = vec![Click { at: [0.25, 0.5], include: true }];
    let darker = Local { exposure: -1.0, ..Local::default() };
    let (left, right) = (|x: usize, _| x < W / 2 - 4, |x: usize, _| x > W / 2 + 4);
    for shape in [Shape::Object { clicks: clicks.clone(), selection: selection.clone() }, Shape::Described { text: "the left".into(), selection }] {
        let out = render(&r, &masked(shape, darker));
        let (l, rt) = (mean_luma(&out, left) / mean_luma(&image, left), mean_luma(&out, right) / mean_luma(&image, right));
        assert!(l < 0.6 && (rt - 1.0).abs() < 0.01, "left {l}, right {rt}");
    }
    // Before the model has selected anything, the shape selects nothing.
    let out = render(&r, &masked(Shape::Object { clicks, selection: None }, darker));
    assert!(max_difference(&out, &image) < 1.0 / 65535.0);
    // Hostile selections and prompts are refused.
    for bad in [
        Shape::Object { clicks: vec![], selection: Some(Selection { side: 5000, data: String::new() }) },
        Shape::Object { clicks: vec![], selection: Some(Selection { side: 4, data: "A".repeat(10_000) }) },
        Shape::Object { clicks: vec![Click { at: [f64::NAN, 0.5], include: true }], selection: None },
        Shape::Object { clicks: vec![Click { at: [0.5, 0.5], include: true }; 65], selection: None },
        Shape::Described { text: String::new(), selection: None },
        Shape::Described { text: "x".repeat(201), selection: None },
        Shape::Described { text: "bee\n".into(), selection: None },
    ] {
        assert!(matches!(r.render(&masked(bad.clone(), darker), &Request::FULL), Err(EditError::Invalid { .. })), "{bad:?}");
    }
    // Damaged data that passes the checks selects nothing rather than failing.
    let damaged = Shape::Object { clicks: vec![], selection: Some(Selection { side: 8, data: "not base64!".into() }) };
    r.render(&masked(damaged, darker), &Request::FULL).unwrap();
}
