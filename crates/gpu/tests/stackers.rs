//! Agreement of whole stacks on the GPU with florastack-stack's (AGENTS.md, CPU oracle): every method, from the same
//! frames, warped on the GPU as florastack-align warps them (rotated, scaled, framed wider than some frames) or already
//! on the canvas with partial coverage, with highlights beyond 1, a sample that isn't a number and two identical frames
//! (ties); tiled finely, and with a budget so small that tiles go back and forth between the GPU and the host.
//!
//! The weighted average and the composite are smooth in their inputs: their GPU results stay within a small absolute
//! tolerance everywhere. The pyramid and the depth map's statistics choose a frame per position: where two frames'
//! energies agree to the last bits, the GPU's rounding may choose the other one, so those comparisons bound the share
//! of pixels that differ, as well as the mean difference.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use common::{gpu, max_difference};
use florastack_align::{Canvas, warp_to_canvas};
use florastack_core::testutil::{Backdrop, StackParams, generate_layered, scene};
use florastack_core::{Affine, ColorSpace, Image, Rect, Unmonitored};
use florastack_gpu::ops::WarpGeometry;
use florastack_gpu::{Frame, GpuStacked, GpuStacker, GpuTiling, Method, levels_for};
use florastack_stack::{
    AverageSettings, CoveredFrame, Decisions, DepthSettings, FocusStats, FrameSource, Gained, Method as _, Patch, PyramidSettings, StackError,
    Stacked, Stacker, Threshold, Tiling, stack,
};

/// A frame as the engine stacks it on the CPU: warped onto each rectangle as it is read, then its gain applied.
struct Warped<'a> {
    frame: &'a Image<f32>,
    transform: Affine,
    canvas: Canvas,
    feather: f64,
}

impl FrameSource for Warped<'_> {
    fn width(&self) -> usize {
        self.canvas.width
    }

    fn height(&self) -> usize {
        self.canvas.height
    }

    fn channels(&self) -> usize {
        self.frame.channels()
    }

    fn read(&self, rect: Rect) -> Result<Patch, StackError> {
        let covered = warp_to_canvas(self.frame, &self.transform, &self.canvas, rect, self.feather).unwrap();
        Ok(Patch { image: covered.image, coverage: Some(covered.coverage) })
    }
}

/// The test stack: `n` frames of a layered scene, `width` x `height`, one with a highlight block at 3 and a sample
/// that isn't a number, its last frame a copy of the one before (a tie everywhere).
fn frames(width: usize, height: usize, n: usize, seed: u64) -> Vec<Image<f32>> {
    let scene = scene::layered(width, height, 1.0, Backdrop::Textured, seed).unwrap();
    let params =
        StackParams { focus_range: Some((0.0, 1.0)), blur_per_depth: 6.0, max_blur: 6.0, blur_step: 0.5, seed, ..StackParams::identity(n - 1) };
    let mut frames = generate_layered(&scene, &params).unwrap().frames;
    for y in 10..16 {
        for x in 20..30 {
            frames[1].pixel_mut(x, y).unwrap().fill(3.0);
        }
    }
    frames[2].pixel_mut(40, 30).unwrap()[1] = f32::NAN;
    let last = frames.last().unwrap().clone();
    frames.push(last);
    frames
}

/// Each frame's alignment, its gain, and the canvas, framed wider than the frames so that some pixels are uncovered.
fn alignment(width: usize, height: usize, n: usize) -> (Vec<(Affine, f32)>, Canvas) {
    let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
    let transforms = (0..n)
        .map(|i| {
            let t = i as f64 / n as f64;
            (Affine::similarity((cx, cy), 1.0 + 0.02 * t, 0.03 * (t - 0.5), (1.5 * t - 0.4, -0.7 * t)), 1.0 + 0.01 * (i % 3) as f32)
        })
        .collect();
    (transforms, Canvas { width: width + 6, height: height + 4, to_master: Affine::translation(-3.0, -2.0) })
}

/// Stacks `frames` warped on the CPU with `stacker`.
fn cpu_warped(stacker: &mut dyn Stacker, frames: &[Image<f32>], alignment: &[(Affine, f32)], canvas: &Canvas) -> Stacked {
    let sources: Vec<Gained<Warped<'_>>> = frames
        .iter()
        .zip(alignment)
        .map(|(frame, &(transform, gain))| Gained { frame: Warped { frame, transform, canvas: *canvas, feather: 2.0 }, gain })
        .collect();
    stack(stacker, &sources, &Unmonitored).unwrap()
}

/// Stacks `frames` warped on the GPU.
fn gpu_warped(stacker: &mut GpuStacker, frames: &[Image<f32>], alignment: &[(Affine, f32)], canvas: &Canvas) -> GpuStacked {
    stacker.begin(canvas.width, canvas.height, frames[0].channels()).unwrap();
    for (frame, &(transform, gain)) in frames.iter().zip(alignment) {
        let geometry = WarpGeometry { to_frame: canvas.to_frame(&transform), frame_width: 0, frame_height: 0, feather: 2.0, gain };
        stacker.add_frame(&Frame::Warp { image: frame, geometry }, &Unmonitored).unwrap();
    }
    stacker.finish(&Unmonitored).unwrap()
}

/// Frames on the canvas with partial coverage: each covers all but a band on the left that grows frame by frame.
fn covered(frames: &[Image<f32>]) -> Vec<CoveredFrame> {
    frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let coverage =
                Image::from_fn(f.width(), f.height(), 1, ColorSpace::LINEAR, |x, _, _| ((x as f32 - 2.0 * i as f32) / 3.0).clamp(0.0, 1.0)).unwrap();
            CoveredFrame::new(f.clone(), coverage).unwrap()
        })
        .collect()
}

fn gpu_covered(stacker: &mut GpuStacker, frames: &[CoveredFrame]) -> GpuStacked {
    let first = &frames[0].image;
    stacker.begin(first.width(), first.height(), first.channels()).unwrap();
    for f in frames {
        stacker.add_frame(&Frame::Canvas { image: &f.image, coverage: Some(&f.coverage) }, &Unmonitored).unwrap();
    }
    stacker.finish(&Unmonitored).unwrap()
}

/// How far two results are apart: the largest difference, the mean one, and the share of samples beyond `tolerance`.
fn apart(a: &Image<f32>, b: &Image<f32>, tolerance: f32) -> (f32, f64, f64) {
    assert_eq!((a.width(), a.height(), a.channels()), (b.width(), b.height(), b.channels()));
    let (mut sum, mut over, mut n) = (0.0f64, 0usize, 0usize);
    for (u, v) in a.rows().flatten().zip(b.rows().flatten()) {
        let d = (u - v).abs();
        sum += f64::from(d);
        over += usize::from(d > tolerance);
        n += 1;
    }
    let result = (max_difference(a, b), sum / n as f64, over as f64 / n as f64);
    println!("  max {:.3e}, mean {:.3e}, beyond {tolerance:e}: {:.4}%", result.0, result.1, 100.0 * result.2);
    result
}

/// Checks the parts of a stack that don't depend on rounding: frame count, uncovered pixels and the alpha.
fn same_coverage(gpu: &GpuStacked, cpu: &Stacked) {
    assert_eq!(gpu.frames, cpu.frames);
    // A pixel at the very edge of a frame's coverage may count as covered on one side only (see kernels.rs): a few.
    assert!(gpu.uncovered.abs_diff(cpu.uncovered) <= 4, "uncovered {} against {}", gpu.uncovered, cpu.uncovered);
    assert_eq!(gpu.alpha.is_some(), cpu.alpha.is_some());
}

fn tiling(tile_size: usize, budget: usize) -> GpuTiling {
    GpuTiling { tile_size, budget, ..GpuTiling::default() }
}

#[test]
fn the_weighted_average_agrees_with_the_cpu() {
    let Some(gpu) = gpu() else { return };
    let (w, h) = (150, 110);
    let frames = frames(w, h, 7, 3);
    let (align, canvas) = alignment(w, h, frames.len());
    // The defaults (smoothing 8: the box passes), and settings that take the exact kernel and powf.
    for settings in [AverageSettings::default(), AverageSettings { radius: 1.0, smoothing: 2.5, power: 2.5 }] {
        let method = Method::Average { radius: settings.radius, smoothing: settings.smoothing, power: settings.power };
        let want = cpu_warped(&mut *settings.stacker(&Tiling::default()).unwrap(), &frames, &align, &canvas);
        for (tile, budget) in [(1536, 1 << 30), (64, 1 << 30), (64, 300_000)] {
            let mut stacker = GpuStacker::new(gpu, method.clone(), tiling(tile, budget)).unwrap();
            let got = gpu_warped(&mut stacker, &frames, &align, &canvas);
            same_coverage(&got, &want);
            // Weights are energies raised to the power, which multiplies their relative rounding (f32 blurs, the
            // box passes in f32 instead of f64) by up to 4; the stack is their weighted mean of samples up to 3: 1e-5
            // relative of the weights moves a pixel by far less than 1e-4, except at the coverage edges, where a
            // pixel's sampled point may fall on the other side of the edge (kernels.rs) and its frames weigh in or not.
            let (max, mean, over) = apart(&got.image, &want.image, 1e-4);
            assert!(mean < 1e-6 && over < 2e-3, "{settings:?}, tiles {tile} budget {budget}: max {max}, mean {mean}, over {over}");
        }
    }
    // On the canvas with partial coverage: no warp, so no coverage edges to round.
    let on_canvas = covered(&frames);
    let settings = AverageSettings::default();
    let want = stack(&mut *settings.stacker(&Tiling::default()).unwrap(), &on_canvas, &Unmonitored).unwrap();
    let mut stacker = GpuStacker::new(gpu, Method::Average { radius: 3.0, smoothing: 8.0, power: 3.0 }, tiling(64, 1 << 30)).unwrap();
    let got = gpu_covered(&mut stacker, &on_canvas);
    same_coverage(&got, &want);
    let d = max_difference(&got.image, &want.image);
    assert!(d <= 1e-4, "on the canvas: {d}");
}

#[test]
fn small_budgets_move_tiles_to_the_host_and_back_without_changing_the_stack() {
    let Some(gpu) = gpu() else { return };
    // Tiles are at least four times the overlap, twice a method's reach: short filters for small canvases, and a
    // canvas of several of the pyramid's 384-pixel tiles.
    let cases = [
        (Method::Average { radius: 1.0, smoothing: 1.0, power: 3.0 }, (130, 90), 200_000),
        (Method::Focus { radius: 1.0 }, (130, 90), 400_000),
        (Method::Pyramid { levels: 0, blend: Some(2.0), channel_decisions: false, range_limit: 1.0 }, (800, 420), 10_000_000),
    ];
    for (method, (w, h), budget) in cases {
        let frames = frames(w, h, 5, 11);
        let (align, canvas) = alignment(w, h, frames.len());
        let mut resident = GpuStacker::new(gpu, method.clone(), tiling(64, 1 << 30)).unwrap();
        let whole = gpu_warped(&mut resident, &frames, &align, &canvas);
        let mut spilling = GpuStacker::new(gpu, method.clone(), tiling(64, budget)).unwrap();
        spilling.begin(canvas.width, canvas.height, 3).unwrap();
        for (frame, &(transform, gain)) in frames.iter().zip(&align) {
            let geometry = WarpGeometry { to_frame: canvas.to_frame(&transform), frame_width: 0, frame_height: 0, feather: 2.0, gain };
            spilling.add_frame(&Frame::Warp { image: frame, geometry }, &Unmonitored).unwrap();
        }
        assert!(spilling.evictions() > 0, "{}: the budget holds a few tiles", method.name());
        let spilled = spilling.finish(&Unmonitored).unwrap();
        // A tile read back and uploaded again is the same bits, and tiles compute alike in any order.
        assert_eq!(spilled, whole, "{}", method.name());
    }
}

#[test]
fn the_pyramid_agrees_with_the_cpu() {
    let Some(gpu) = gpu() else { return };
    let (w, h) = (150, 110);
    let frames = frames(w, h, 7, 5);
    let (align, canvas) = alignment(w, h, frames.len());
    let cases = [
        PyramidSettings::default(),
        PyramidSettings { grit: 0.0, ..PyramidSettings::default() },
        PyramidSettings { grit: 0.3, decisions: Decisions::Channels, ..PyramidSettings::default() },
        PyramidSettings { levels: 4, range_limit: 0.0, grit: 1.0, ..PyramidSettings::default() },
        PyramidSettings { levels: 2, range_limit: 0.5, ..PyramidSettings::default() },
    ];
    for settings in cases {
        assert_eq!(levels_for(settings.levels, canvas.width, canvas.height), settings.levels_for(canvas.width, canvas.height));
        #[allow(clippy::cast_possible_truncation)]
        let method = Method::Pyramid {
            levels: settings.levels,
            blend: (settings.grit > 0.0).then(|| (1.0 / settings.grit) as f32),
            channel_decisions: settings.decisions == Decisions::Channels,
            range_limit: settings.range_limit as f32,
        };
        let want = cpu_warped(&mut *settings.stacker(&Tiling::default()).unwrap(), &frames, &align, &canvas);
        for tile in [1536, 64] {
            let mut stacker = GpuStacker::new(gpu, method.clone(), tiling(tile, 1 << 30)).unwrap();
            let got = gpu_warped(&mut stacker, &frames, &align, &canvas);
            same_coverage(&got, &want);
            // Every filter has exact weights summed in the CPU's order: the levels agree to an ulp or two, and so
            // the choices, but where two frames' energies are equal to their last bits (or a pixel at a coverage
            // edge is covered on one side only); a coefficient taken from another frame moves the pixels its
            // support reaches. Bounded: a tenth of a percent of the samples, and the mean.
            let (max, mean, over) = apart(&got.image, &want.image, 1e-4);
            assert!(mean < 2e-5 && over < 3e-3, "{settings:?}, tiles {tile}: max {max}, mean {mean}, over {over}");
        }
    }
    let on_canvas = covered(&frames);
    let settings = PyramidSettings::default();
    let want = stack(&mut *settings.stacker(&Tiling::default()).unwrap(), &on_canvas, &Unmonitored).unwrap();
    let method = Method::Pyramid { levels: 0, blend: Some(2.0), channel_decisions: false, range_limit: 1.0 };
    let got = gpu_covered(&mut GpuStacker::new(gpu, method, tiling(64, 1 << 30)).unwrap(), &on_canvas);
    same_coverage(&got, &want);
    let (max, mean, over) = apart(&got.image, &want.image, 1e-4);
    assert!(mean < 2e-5 && over < 3e-3, "on the canvas: max {max}, mean {mean}, over {over}");
}

#[test]
fn the_depth_map_agrees_with_the_cpu() {
    let Some(gpu) = gpu() else { return };
    let (w, h) = (150, 110);
    let frames = frames(w, h, 8, 9);
    let (align, canvas) = alignment(w, h, frames.len());
    for settings in [DepthSettings::default(), DepthSettings { radius: 5.0, ..DepthSettings::default() }] {
        // Pass 1: the statistics.
        let mut estimator = settings.estimator(&Tiling::default()).unwrap();
        let sources: Vec<Gained<Warped<'_>>> = frames
            .iter()
            .zip(&align)
            .map(|(frame, &(transform, gain))| Gained { frame: Warped { frame, transform, canvas, feather: 2.0 }, gain })
            .collect();
        estimator.begin(florastack_stack::StackShape::of(&sources[0])).unwrap();
        for s in &sources {
            estimator.add_frame(s, &Unmonitored).unwrap();
        }
        let want: FocusStats = estimator.finish(&Unmonitored).unwrap();
        for tile in [1536, 64] {
            let mut stacker = GpuStacker::new(gpu, Method::Focus { radius: settings.radius }, tiling(tile, 1 << 30)).unwrap();
            let got = gpu_warped(&mut stacker, &frames, &align, &canvas);
            assert_eq!(got.frames, want.frames);
            assert!(got.uncovered.abs_diff(want.uncovered) <= 4);
            let plane = |k: usize| {
                Image::from_fn(got.image.width(), got.image.height(), 1, ColorSpace::LINEAR, |x, y, _| got.image.get(x, y, k).unwrap()).unwrap()
            };
            // The depth: the sharpest frame interpolated, which changes by a frame where two frames' focus agree to
            // the last bits; elsewhere the f32 logarithms and blurs agree to 1e-4 of a frame.
            let (max, mean, over) = apart(&plane(0), &want.depth, 1e-3);
            assert!(over < 5e-3 && mean < 2e-3, "{settings:?} tiles {tile}: depth max {max}, mean {mean}, over {over}");
            // The confidence and the guide: square roots of focus, and the sharpest frame's luminance.
            let (max, mean, over) = apart(&plane(1), &want.confidence, 1e-4);
            assert!(over < 5e-3 && mean < 1e-4, "{settings:?} tiles {tile}: confidence max {max}, mean {mean}, over {over}");
            let (max, mean, over) = apart(&plane(2), &want.guide, 1e-4);
            assert!(over < 5e-3 && mean < 1e-4, "{settings:?} tiles {tile}: guide max {max}, mean {mean}, over {over}");
        }
        // Pass 2 from the CPU's depth map, so that only the composite is compared.
        let map = want.resolve(Threshold::Auto, settings.smoothing, None).unwrap();
        let mut compositor = map.compositor(&Tiling::default()).unwrap();
        let composite = stack(&mut compositor, &sources, &Unmonitored).unwrap();
        for tile in [1536, 64] {
            let method = Method::Composite { depth: Arc::clone(&map.depth), frames: map.frames };
            let mut stacker = GpuStacker::new(gpu, method, tiling(tile, 1 << 30)).unwrap();
            let got = gpu_warped(&mut stacker, &frames, &align, &canvas);
            same_coverage(&got, &composite);
            // Weights linear in the depth and the coverage: only the warp's rounding and the coverage edges remain.
            let (max, mean, over) = apart(&got.image, &composite.image, 1e-4);
            assert!(mean < 1e-6 && over < 2e-3, "{settings:?} tiles {tile}: composite max {max}, mean {mean}, over {over}");
        }
    }
}

#[test]
fn hostile_settings_and_frames_are_errors() {
    let Some(gpu) = gpu() else { return };
    let tiling = GpuTiling::default();
    assert!(GpuStacker::new(gpu, Method::Average { radius: -1.0, smoothing: 8.0, power: 3.0 }, tiling.clone()).is_err());
    assert!(GpuStacker::new(gpu, Method::Average { radius: 3.0, smoothing: 8.0, power: f64::NAN }, tiling.clone()).is_err());
    assert!(GpuStacker::new(gpu, Method::Pyramid { levels: 11, blend: None, channel_decisions: false, range_limit: 1.0 }, tiling.clone()).is_err());
    assert!(GpuStacker::new(gpu, Method::Focus { radius: f64::INFINITY }, tiling.clone()).is_err());
    let depth = Arc::new(Image::new(10, 10, 1, ColorSpace::LINEAR).unwrap());
    assert!(GpuStacker::new(gpu, Method::Composite { depth: Arc::clone(&depth), frames: 0 }, tiling.clone()).is_err());
    let mut stacker = GpuStacker::new(gpu, Method::Average { radius: 1.0, smoothing: 1.0, power: 2.0 }, tiling.clone()).unwrap();
    let frame = Image::new(20, 10, 3, ColorSpace::LINEAR).unwrap();
    assert!(stacker.add_frame(&Frame::Canvas { image: &frame, coverage: None }, &Unmonitored).is_err(), "no stack begun");
    assert!(stacker.begin(0, 10, 3).is_err());
    assert!(stacker.begin(10, 10, 5).is_err());
    stacker.begin(10, 10, 3).unwrap();
    assert!(stacker.add_frame(&Frame::Canvas { image: &frame, coverage: None }, &Unmonitored).is_err(), "a frame of another size");
    let broken = WarpGeometry { to_frame: Affine { m: [[f64::NAN; 3]; 2] }, frame_width: 0, frame_height: 0, feather: 2.0, gain: 1.0 };
    assert!(stacker.add_frame(&Frame::Warp { image: &frame, geometry: broken }, &Unmonitored).is_err());
    assert!(stacker.finish(&Unmonitored).is_err(), "refused frames are never added: a stack of none");
    // A composite of another canvas's depth map.
    let mut composite = GpuStacker::new(gpu, Method::Composite { depth, frames: 2 }, tiling).unwrap();
    assert!(composite.begin(20, 10, 3).is_err());
}
