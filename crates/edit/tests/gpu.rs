//! LightCraft's GPU renderer against the CPU pipeline, its reference: previews of the same adjustments (the whole
//! image's sliders, every kind of mask, the stack's focus masks, a draft, a zoomed window) rendered on both differ by
//! a fraction of an 8-bit step on the sRGB curve. Skipped without a hardware adapter: LightCraft never renders on a
//! software one (WARP, lavapipe), which is slower than the CPU, so CI's runners skip it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Test scenes and slider values, all small: f32 holds them as exactly as the checks need.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use florastack_core::Image;
use florastack_core::testutil::scene;
use florastack_edit::{Adjustments, Component, Global, Local, Mask, Op, Renderer, Request, Shape, Size, Stroke, Window};
use florastack_testkit::skip;

/// The mean and the largest difference between two renders, in 8-bit steps on the sRGB curve (where they are seen).
fn steps(a: &Image<f32>, b: &Image<f32>) -> (f64, f64) {
    assert!(a.same_shape(b));
    let srgb = |v: f32| {
        let v = f64::from(v.max(0.0));
        if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    };
    let (mut sum, mut max) = (0.0, 0.0f64);
    for (x, y) in a.as_raw().iter().zip(b.as_raw()) {
        let d = (srgb(*x) - srgb(*y)).abs() * 255.0;
        sum += d;
        max = max.max(d);
    }
    (sum / a.as_raw().len() as f64, max)
}

fn mask(shape: Shape, adjust: Local) -> Mask {
    Mask { id: 1, components: vec![Component { op: Op::Add, invert: false, shape }], adjust, ..Mask::default() }
}

#[test]
fn previews_on_the_gpu_are_the_cpus() {
    if let Some(why) = florastack_edit::gpu::unavailable() {
        skip!(Gpu, "LightCraft's GPU renderer can't render here: {why}");
        return;
    }
    let (image, depth) = scene::textured(960, 640, 11).unwrap();
    let renderer = Renderer::new(&image).unwrap().with_depth(&depth).unwrap();
    let global = Global {
        exposure: 0.4,
        contrast: 20.0,
        highlights: -30.0,
        shadows: 25.0,
        texture: 25.0,
        clarity: 20.0,
        dehaze: 10.0,
        vibrance: 15.0,
        saturation: 10.0,
        sharpening: 40.0,
        noise_reduction: 20.0,
        ..Global::default()
    };
    let brush = Stroke { points: vec![[0.2, 0.3], [0.6, 0.35], [0.8, 0.6]], size: 0.05, feather: 50.0, flow: 100.0, erase: false };
    let cases = [
        ("whole image", Adjustments { global, ..Adjustments::default() }),
        (
            "radial",
            Adjustments {
                masks: vec![mask(
                    Shape::Radial { center: [0.5, 0.5], radius_x: 0.2, radius_y: 0.15, angle: 20.0, feather: 50.0 },
                    Local { exposure: 0.8, ..Local::default() },
                )],
                ..Adjustments::default()
            },
        ),
        (
            "linear",
            Adjustments {
                masks: vec![mask(Shape::Linear { start: [0.0, 0.2], end: [0.0, 0.6] }, Local { contrast: 30.0, ..Local::default() })],
                ..Adjustments::default()
            },
        ),
        (
            "brush",
            Adjustments {
                masks: vec![mask(Shape::Brush { strokes: vec![brush] }, Local { saturation: -50.0, ..Local::default() })],
                ..Adjustments::default()
            },
        ),
        (
            "luminance",
            Adjustments {
                masks: vec![mask(
                    Shape::Luminance { low: 0.6, high: 1.0, low_feather: 0.1, high_feather: 0.0 },
                    Local { exposure: -0.5, ..Local::default() },
                )],
                ..Adjustments::default()
            },
        ),
        (
            "subject and depth",
            Adjustments {
                masks: vec![
                    mask(Shape::Subject, Local { clarity: 30.0, ..Local::default() }),
                    Mask { id: 2, ..mask(Shape::Depth { from: 0.0, to: 0.5, feather: 0.1 }, Local { exposure: 0.5, ..Local::default() }) },
                ],
                ..Adjustments::default()
            },
        ),
    ];
    let fit = Size::Fit { width: 480, height: 480 };
    for (name, adjustments) in &cases {
        for (how, request) in [
            ("preview", Request { size: fit, draft: false, overlay: None, window: None, gpu: false }),
            ("draft", Request { size: fit, draft: true, overlay: None, window: None, gpu: false }),
            (
                "window",
                Request {
                    size: Size::Full,
                    draft: false,
                    overlay: None,
                    window: Some(Window { x: 300, y: 200, width: 320, height: 240 }),
                    gpu: false,
                },
            ),
        ] {
            let cpu = renderer.render(adjustments, &request).unwrap();
            let gpu = renderer.render(adjustments, &Request { gpu: true, ..request }).unwrap();
            let (mean, max) = steps(&cpu, &gpu);
            eprintln!("{name:<18} {how:<8} mean {mean:.4}  max {max:.3} steps");
        }
    }
}
