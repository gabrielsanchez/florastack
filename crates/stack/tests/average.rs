//! The weighted average on synthetic stacks (`florastack_core::testutil`), scored against their sharp ground truth with
//! florastack-metrics, and the streaming framework under it: coverage, tiling, determinism, progress, cancellation and
//! errors.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use florastack_core::ops::{Border, Interpolation, gaussian_blur, resize, warp};
use florastack_core::testutil::{StackParams, SyntheticStack, generate, scene};
use florastack_core::{Affine, ColorSpace, Image, Monitor, Rect, Unmonitored};
use florastack_metrics::{psnr, ssim};
use florastack_stack::{
    AverageSettings, CoveredFrame, FrameSource, Gained, Method, Patch, StackError, StackSettings, StackShape, Stacked, Stacker, Tiled, Tiling,
    WeightedAverage, preview, preview_size, reduce, stack,
};

/// A defocused stack of `frames` frames focused from the scene's nearest to its farthest depth, already aligned (no
/// breathing, shift or rotation), with sensor noise when `noisy`. `scale` enlarges the scene (`width` x `height`
/// pixels before it) and its defocus: the generator's scene has detail of 3 to 16 pixels, a camera frame's detail and
/// blur are several times larger in pixels, and the default settings are for those.
fn defocused(width: usize, height: usize, scale: usize, frames: usize, noisy: bool, seed: u64) -> SyntheticStack {
    let (sharp, depth) = scene::textured(width, height, seed).unwrap();
    let (sharp, depth) = if scale > 1 {
        (resize(&sharp, width * scale, height * scale).unwrap(), resize(&depth, width * scale, height * scale).unwrap())
    } else {
        (sharp, depth)
    };
    let blur = 8.0 * scale as f32;
    let mut params = StackParams { blur_per_depth: blur, max_blur: blur, blur_step: 0.5 * scale as f32, seed, ..StackParams::identity(frames) };
    if noisy {
        params.read_noise = 0.005;
        params.shot_noise = 0.01;
    }
    generate(&sharp, &depth, &params).unwrap()
}

/// Settings for the generator's own scale: its detail is a third the size of a camera frame's in pixels, and its
/// defocus, which blurs each pixel with its own depth's blur, has no occlusion, so no halos for the defaults' wide
/// smoothing (8 px, which camera stacks need around a subject in front of a smooth background) to hide: there smoothing
/// only costs detail.
fn small_scale() -> AverageSettings {
    AverageSettings { radius: 1.0, smoothing: 1.0, power: 3.0 }
}

fn average(settings: &AverageSettings, tiling: &Tiling, frames: &[impl FrameSource]) -> Stacked {
    let mut stacker = settings.stacker(tiling).unwrap();
    stack(&mut stacker, frames, &Unmonitored).unwrap()
}

/// The SSIM of the sharpest single frame, the best a stacker that picks one frame could do.
fn best_frame(frames: &[Image<f32>], truth: &Image<f32>) -> f64 {
    frames.iter().map(|f| ssim(f, truth).unwrap()).fold(0.0, f64::max)
}

#[test]
fn the_weighted_average_beats_the_best_single_frame() {
    // Twelve frames, each in focus on a twelfth of the scene's depth: no frame is sharp everywhere. At a camera's scale
    // (the scene enlarged three times, 720x540) with the default settings, and at the generator's own (320x240) with
    // settings for it. Measured on two seeds: 0.923 to 0.924 SSIM against 0.897 to 0.909 for the best frame, clean, and
    // 0.877 to 0.878 against 0.848 to 0.861 with noise, at the camera's scale; 0.961 to 0.962 against 0.894 to 0.895,
    // and 0.878 to 0.879 against 0.869 to 0.870, at the generator's (where noise is the larger share of the detail,
    // which the weights, selecting sharp frames, average away less).
    for (scale, width, height, settings) in [(3, 240, 180, AverageSettings::default()), (1, 320, 240, small_scale())] {
        for seed in [1, 2] {
            for noisy in [false, true] {
                let synthetic = defocused(width, height, scale, 12, noisy, seed);
                let truth = &synthetic.truth.sharp;
                let best = best_frame(&synthetic.frames, truth);
                let out = average(&settings, &Tiling::default(), &synthetic.frames);
                let (score, db) = (ssim(&out.image, truth).unwrap(), psnr(&out.image, truth).unwrap());
                println!("scale {scale}, seed {seed}, noisy {noisy}: SSIM {score:.4} (best frame {best:.4}), {db:.1} dB");
                let margin = if noisy { 0.005 } else { 0.01 };
                assert!(score > best + margin, "scale {scale}, seed {seed}, noisy {noisy}: {score:.4} against the best frame's {best:.4}");
                assert_eq!((out.frames, out.uncovered, out.alpha.is_none()), (12, 0, true));
            }
        }
    }
}

/// A frame's coverage of the scene: the weight that ramps from 0 at its edge to 1 at 2 pixels inside, as
/// florastack-align's `Coverage` gives it, where `to_frame` maps scene pixels to the frame's.
fn coverage(to_frame: &Affine, width: usize, height: usize) -> Image<f32> {
    Image::from_fn(width, height, 1, ColorSpace::LINEAR, |x, y, _| {
        let (u, v) = to_frame.apply(x as f64, y as f64);
        let d = (u + 0.5).min(width as f64 - 0.5 - u).min(v + 0.5).min(height as f64 - 0.5 - v);
        #[allow(clippy::cast_possible_truncation)]
        let w = (d / 2.0).clamp(0.0, 1.0) as f32;
        w
    })
    .unwrap()
}

#[test]
fn frames_aligned_onto_the_scene_with_their_coverage_beat_the_best_single_frame() {
    // A clean stack that breathes, shifts and rotates, put back onto the scene's grid with its true transforms, as the
    // engine warps frames onto the canvas, with their coverage; scored away from the edges, which the frames' fields of
    // view don't all reach; with the settings for the generator's scale. Measured: 0.951 to 0.956 SSIM
    // against 0.887 to 0.890 for the best warped frame.
    for seed in [4, 5] {
        let (sharp, depth) = scene::textured(320, 240, seed).unwrap();
        let synthetic = generate(&sharp, &depth, &StackParams::clean(10, seed)).unwrap();
        let frames: Vec<CoveredFrame> = synthetic
            .frames
            .iter()
            .zip(&synthetic.truth.frames)
            .map(|(frame, truth)| {
                let image = warp(frame, &truth.transform, Interpolation::Bicubic, Border::Constant(0.0)).unwrap();
                CoveredFrame::new(image, coverage(&truth.transform, 320, 240)).unwrap()
            })
            .collect();
        let inner = Rect::new(12, 12, 296, 216);
        let truth = sharp.crop(inner).unwrap();
        let best = frames.iter().map(|f| ssim(&f.image.crop(inner).unwrap(), &truth).unwrap()).fold(0.0, f64::max);
        let out = average(&small_scale(), &Tiling::default(), &frames);
        let score = ssim(&out.image.crop(inner).unwrap(), &truth).unwrap();
        println!("seed {seed}: SSIM {score:.4} (best warped frame {best:.4})");
        assert!(score > best + 0.04, "seed {seed}: {score:.4} against {best:.4}");
    }
}

/// `image` with every pixel from column `from` on replaced by the pixel at `from - 1`: what a warp that clamps at a
/// frame's edge puts where the frame has nothing.
fn streaked(image: &Image<f32>, from: usize) -> Image<f32> {
    let c = image.channels();
    Image::from_fn(image.width(), image.height(), c, ColorSpace::LINEAR, |x, y, ch| image.get(x.min(from - 1), y, ch).unwrap()).unwrap()
}

/// A coverage of 1 left of column `edge`, ramping to 0 over the last 2 pixels, and 0 from it on.
fn left_of(edge: usize, width: usize, height: usize) -> Image<f32> {
    Image::from_fn(width, height, 1, ColorSpace::LINEAR, |x, _, _| ((edge as f32 - x as f32 - 0.5) / 2.0).clamp(0.0, 1.0)).unwrap()
}

#[test]
fn uncovered_pixels_never_smear_and_get_no_alpha() {
    // A wide canvas: a sharp frame covers its left part and a blurred one its middle, but its edge pixels repeat beyond
    // its edge (a clamping warp's streaks), and neither covers the right end.
    let (sharp, _) = scene::textured(240, 120, 6).unwrap();
    let blurred = gaussian_blur(&sharp, 1.5).unwrap();
    let left = CoveredFrame::new(streaked(&sharp, 150), left_of(150, 240, 120)).unwrap();
    let middle_cov = Image::from_fn(240, 120, 1, ColorSpace::LINEAR, |x, _, _| {
        let inside = (x as f32 + 0.5 - 60.0).min(200.0 - x as f32 - 0.5);
        (inside / 2.0).clamp(0.0, 1.0)
    })
    .unwrap();
    let mut middle_image = streaked(&blurred, 200);
    for y in 0..120 {
        for x in 0..60 {
            middle_image.pixel_mut(x, y).unwrap().fill(0.0);
        }
    }
    let middle = CoveredFrame::new(middle_image, middle_cov).unwrap();
    let frames = [left, middle];
    for tiling in [Tiling::default(), Tiling { tile_size: 64, ..Tiling::default() }] {
        let out = average(&AverageSettings::default(), &tiling, &frames);
        let alpha = out.alpha.as_ref().expect("an alpha channel: some pixels are covered by no frame");
        assert_eq!(out.uncovered, 40 * 120, "columns 200 to 239");
        for y in 0..120 {
            for x in 0..240 {
                let expected = if x < 200 { 1.0 } else { 0.0 };
                assert_eq!(alpha.get(x, y, 0), Some(expected), "alpha at {x},{y}");
                if x >= 200 {
                    assert_eq!(out.image.pixel(x, y), Some(&[0.0, 0.0, 0.0][..]), "an uncovered pixel is 0");
                }
            }
        }
        // Where only the blurred frame covers, the result is that frame, not the sharp frame's streaks.
        let only_middle = Rect::new(152, 0, 48, 120);
        let error = psnr(&out.image.crop(only_middle).unwrap(), &blurred.crop(only_middle).unwrap()).unwrap();
        assert!(error > 60.0, "{error:.1} dB");
        // Where only the sharp frame covers, it is the sharp frame.
        let only_left = Rect::new(0, 0, 60, 120);
        assert!(psnr(&out.image.crop(only_left).unwrap(), &sharp.crop(only_left).unwrap()).unwrap() > 60.0);
        assert!(out.image.as_raw().iter().all(|v| v.is_finite()));
    }
}

#[test]
fn results_are_the_same_whatever_the_threads_and_the_tiles() {
    let synthetic = defocused(300, 200, 1, 6, true, 7);
    // Blurs of up to 4 px, which use the exact kernel.
    let settings = AverageSettings { radius: 3.0, smoothing: 4.0, power: 3.0 };
    let run = |threads: usize, tile_size: usize| {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        pool.install(|| average(&settings, &Tiling { tile_size, ..Tiling::default() }, &synthetic.frames))
    };
    let reference = run(4, 4096);
    assert_eq!(run(4, 4096), reference, "the same on every run");
    assert_eq!(run(1, 4096), reference, "on one thread as on four");
    // Blurs up to 4 px use the exact kernel, whose taps a tile sees as the whole canvas does: tiles change nothing.
    let mut stacker = Tiled::new(WeightedAverage::new(&settings).unwrap(), Tiling { tile_size: 48, ..Tiling::default() });
    stacker.begin(StackShape { width: 300, height: 200, channels: 3 }).unwrap();
    assert!(stacker.grid().unwrap().len() > 1, "{:?}", stacker.grid());
    assert_eq!(run(3, 48), reference, "tiles of 48 px as one tile");
    // Larger ones, such as the default smoothing, blur with box filters, whose running sums round a little differently
    // from tile to tile; the threads still change nothing.
    // Tiles are at least four times the overlap, which the default smoothing makes 84 px: a larger canvas.
    let large = defocused(240, 160, 3, 4, true, 7);
    let wide = AverageSettings { radius: 6.0, ..AverageSettings::default() };
    let whole = average(&wide, &Tiling { tile_size: 4096, ..Tiling::default() }, &large.frames);
    let mut stacker = Tiled::new(WeightedAverage::new(&wide).unwrap(), Tiling { tile_size: 64, ..Tiling::default() });
    stacker.begin(StackShape { width: 720, height: 480, channels: 3 }).unwrap();
    assert!(stacker.grid().unwrap().len() > 1, "{:?}", stacker.grid());
    let tiled = average(&wide, &Tiling { tile_size: 64, ..Tiling::default() }, &large.frames);
    let largest = whole.image.as_raw().iter().zip(tiled.image.as_raw()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(largest < 1e-5, "{largest}");
    let one = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    assert_eq!(one.install(|| average(&wide, &Tiling { tile_size: 64, ..Tiling::default() }, &large.frames)), tiled);
}

#[test]
fn accumulators_spilled_to_disk_give_the_same_stack() {
    let synthetic = defocused(200, 150, 1, 4, false, 8);
    let dir = std::env::temp_dir().join(format!("florastack-stack-spill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Settings with a small reach, so that tiles of 64 px are allowed: 12 of them.
    let in_memory = average(&small_scale(), &Tiling { tile_size: 64, ..Tiling::default() }, &synthetic.frames);
    // A budget of one tile's planes: every frame reads the others back from their files.
    let spilling = Tiling { tile_size: 64, budget: 80 * 80 * 16, spill_dir: Some(dir.clone()) };
    assert_eq!(average(&small_scale(), &spilling, &synthetic.frames), in_memory);
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(left.is_empty(), "the tile folder goes with the stack: {left:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A monitor that records progress and cancels after `limit` steps.
struct Recording {
    limit: u64,
    steps: AtomicU64,
    cancelled: AtomicBool,
    seen: Mutex<Vec<(u64, u64)>>,
}

impl Recording {
    fn new(limit: u64) -> Recording {
        Recording { limit, steps: AtomicU64::new(0), cancelled: AtomicBool::new(false), seen: Mutex::new(Vec::new()) }
    }
}

impl Monitor for Recording {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn progress(&self, done: u64, total: u64) {
        self.seen.lock().unwrap().push((done, total));
        if self.steps.fetch_add(1, Ordering::Relaxed) + 1 >= self.limit {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

#[test]
fn progress_runs_over_the_whole_stack_and_cancelling_ends_it() {
    let synthetic = defocused(200, 150, 1, 3, false, 9);
    let tiling = Tiling { tile_size: 64, ..Tiling::default() };
    let all = Recording::new(u64::MAX);
    let mut stacker = small_scale().stacker(&tiling).unwrap();
    stack(&mut stacker, &synthetic.frames, &all).unwrap();
    let seen = all.seen.lock().unwrap().clone();
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0 && w[0].1 == w[1].1), "never goes back: {seen:?}");
    assert_eq!(seen.last().map(|&(d, t)| d == t), Some(true), "{seen:?}");

    // Cancelled during the second frame: the stack ends, and nothing more can be added to it.
    let cancelling = Recording::new(2);
    let mut stacker = small_scale().stacker(&tiling).unwrap();
    assert!(matches!(stack(&mut stacker, &synthetic.frames, &cancelling), Err(StackError::Cancelled(_))));
    assert!(matches!(stacker.add_frame(&synthetic.frames[0], &Unmonitored), Err(StackError::NotStarted)));
    assert!(matches!(stacker.finish(&Unmonitored), Err(StackError::NotStarted)));
    // It can start again.
    assert_eq!(stack(&mut stacker, &synthetic.frames, &Unmonitored).unwrap().frames, 3);
}

/// A frame source that answers every read with a patch of the wrong size.
struct Liar;

impl FrameSource for Liar {
    fn width(&self) -> usize {
        40
    }
    fn height(&self) -> usize {
        30
    }
    fn channels(&self) -> usize {
        3
    }
    fn read(&self, _: Rect) -> Result<Patch, StackError> {
        Ok(Patch { image: Image::new(2, 2, 3, ColorSpace::LINEAR)?, coverage: None })
    }
}

#[test]
fn frames_and_settings_that_dont_fit_are_errors() {
    let tiling = Tiling::default();
    let mut stacker = AverageSettings::default().stacker(&tiling).unwrap();
    let frame = Image::<f32>::new(40, 30, 3, ColorSpace::LINEAR).unwrap();
    assert!(matches!(stacker.add_frame(&frame, &Unmonitored), Err(StackError::NotStarted)));
    assert!(matches!(stacker.finish(&Unmonitored), Err(StackError::NotStarted)));
    stacker.begin(StackShape { width: 40, height: 30, channels: 3 }).unwrap();
    assert!(matches!(stacker.finish(&Unmonitored), Err(StackError::NoFrames)));
    stacker.begin(StackShape { width: 40, height: 30, channels: 3 }).unwrap();
    let small = Image::<f32>::new(40, 29, 3, ColorSpace::LINEAR).unwrap();
    assert!(matches!(stacker.add_frame(&small, &Unmonitored), Err(StackError::FrameShape { .. })));
    let gray = Image::<f32>::new(40, 30, 1, ColorSpace::LINEAR).unwrap();
    assert!(matches!(stacker.add_frame(&gray, &Unmonitored), Err(StackError::FrameShape { .. })));
    stacker.add_frame(&frame, &Unmonitored).unwrap();
    assert_eq!(stacker.finish(&Unmonitored).unwrap().frames, 1, "a frame of the wrong shape leaves the stack as it was");
    for shape in [StackShape { width: 0, height: 30, channels: 3 }, StackShape { width: 40, height: 30, channels: 5 }] {
        assert!(matches!(stacker.begin(shape), Err(StackError::Shape { .. })), "{shape:?}");
    }
    assert!(AverageSettings { power: 9.0, ..AverageSettings::default() }.stacker(&tiling).is_err());
    let empty: [Image<f32>; 0] = [];
    assert!(matches!(stack(&mut stacker, &empty, &Unmonitored), Err(StackError::NoFrames)));

    // A source that answers with the wrong size is caught.
    stacker.begin(StackShape { width: 40, height: 30, channels: 3 }).unwrap();
    assert!(matches!(stacker.add_frame(&Liar, &Unmonitored), Err(StackError::Patch { .. })));
    // Reading outside a frame is an error, not a panic.
    assert!(matches!(frame.read(Rect::new(30, 0, 20, 10)), Err(StackError::Region { .. })));
}

#[test]
fn a_noiseless_flat_region_beside_sharp_detail_stays_covered() {
    // A sharp checkerboard on the left, flat gray on the right without any noise: the weights span from about 1e18 to
    // 1e-24 within a row. The smoothing's box passes once kept the sharp side's rounding in their running sums, which
    // made the flat side's weights fall below 0 in every frame, so that its pixels came out black and uncovered.
    let (w, h) = (400, 120);
    let frame =
        |level: f32| Image::from_fn(w, h, 3, ColorSpace::LINEAR, move |x, y, _| if x < 100 { ((x / 2 + y / 2) % 2) as f32 } else { level }).unwrap();
    let frames = [frame(0.25), frame(0.75)];
    let stacked = average(&AverageSettings::default(), &Tiling::default(), &frames);
    assert_eq!((stacked.uncovered, stacked.alpha.is_some()), (0, false));
    for x in 250..w {
        let v = stacked.image.get(x, 60, 1).unwrap();
        assert!((v - 0.5).abs() < 1e-4, "the flat region is the plain average of its frames at {x}: {v}");
    }
}

#[test]
fn hostile_samples_give_a_finite_stack() {
    // Regression: samples that aren't finite, or are huge, must neither poison the sums nor overflow them.
    let mut frames = vec![defocused(64, 48, 1, 3, false, 10).frames[0].clone(); 3];
    frames[0].pixel_mut(10, 10).unwrap().copy_from_slice(&[f32::NAN, f32::INFINITY, 1.0]);
    frames[1].pixel_mut(10, 10).unwrap().copy_from_slice(&[1e38, -1e38, f32::MAX]);
    frames[2].pixel_mut(30, 20).unwrap().copy_from_slice(&[f32::NEG_INFINITY; 3]);
    for power in [0.5, 2.0, 4.0] {
        let out = average(&AverageSettings { power, ..AverageSettings::default() }, &Tiling::default(), &frames);
        assert!(out.image.as_raw().iter().all(|v| v.is_finite()), "power {power}");
    }
    // A frame that is not finite anywhere covers nothing.
    let nothing = vec![Image::from_fn(16, 16, 3, ColorSpace::LINEAR, |_, _, _| f32::NAN).unwrap()];
    let out = average(&AverageSettings::default(), &Tiling::default(), &nothing);
    assert_eq!(out.uncovered, 256);
    assert!(out.image.as_raw().iter().all(|&v| v == 0.0));
}

#[test]
fn gains_scale_a_frame_as_it_is_read() {
    let frame = Image::from_fn(8, 4, 1, ColorSpace::LINEAR, |x, _, _| x as f32 / 8.0).unwrap();
    let gained = Gained { frame: &frame, gain: 2.0 };
    let patch = gained.read(Rect::new(2, 1, 4, 2)).unwrap();
    assert_eq!(patch.image.row(0), Some(&[0.5, 0.75, 1.0, 1.25][..]));
    // A stack of the same frame, darker by half, with its gain, is the frame.
    let dark = Image::from_fn(8, 4, 1, ColorSpace::LINEAR, |x, _, _| x as f32 / 16.0).unwrap();
    let frames = [Gained { frame: &frame, gain: 1.0 }, Gained { frame: &dark, gain: 2.0 }];
    let out = average(&AverageSettings::default(), &Tiling::default(), &frames);
    assert!(out.image.as_raw().iter().zip(frame.as_raw()).all(|(a, b)| (a - b).abs() < 1e-6));
}

#[test]
fn the_quarter_scale_preview_looks_like_the_full_stack() {
    // At a camera's scale, as the default settings are made for.
    let synthetic = defocused(240, 180, 3, 12, false, 11);
    let settings = AverageSettings::default();
    let tiling = Tiling::default();
    let mut quarter = preview(&settings, &tiling).unwrap();
    let small = stack(&mut quarter, &synthetic.frames, &Unmonitored).unwrap();
    assert_eq!((small.image.width(), small.image.height()), preview_size(720, 540));
    assert_eq!(preview_size(720, 540), (180, 135));
    assert_eq!(preview_size(321, 7), (81, 2));
    // Against the reduced truth, the preview beats the best reduced frame, and it is close to the full stack, reduced.
    let truth = reduce(&synthetic.truth.sharp).unwrap();
    let reduced: Vec<Image<f32>> = synthetic.frames.iter().map(|f| reduce(f).unwrap()).collect();
    let best = best_frame(&reduced, &truth);
    let score = ssim(&small.image, &truth).unwrap();
    let full = reduce(&average(&settings, &tiling, &synthetic.frames).image).unwrap();
    let likeness = psnr(&small.image, &full).unwrap();
    println!("preview: SSIM {score:.4} (best reduced frame {best:.4}), {likeness:.1} dB from the full stack reduced");
    assert!(score > best, "{score:.4} against {best:.4}");
    assert!(likeness > 30.0, "{likeness:.1} dB");

    // A frame's coverage is reduced with it, and its zeros don't darken its edge.
    let (sharp, _) = scene::textured(160, 80, 12).unwrap();
    let half = CoveredFrame::new(streaked(&sharp, 80), left_of(80, 160, 80)).unwrap();
    let mut quarter = preview(&settings, &tiling).unwrap();
    quarter.begin(StackShape { width: 160, height: 80, channels: 3 }).unwrap();
    quarter.add_frame(&half, &Unmonitored).unwrap();
    let out = quarter.finish(&Unmonitored).unwrap();
    let alpha = out.alpha.unwrap();
    assert_eq!((alpha.get(10, 10, 0), alpha.get(35, 10, 0)), (Some(1.0), Some(0.0)));
    let near_edge = Rect::new(14, 2, 4, 16);
    let truth = reduce(&sharp).unwrap();
    let ratio = mean(&out.image.crop(near_edge).unwrap()) / mean(&truth.crop(near_edge).unwrap());
    assert!((ratio - 1.0).abs() < 0.05, "{ratio}");
}

fn mean(image: &Image<f32>) -> f64 {
    image.as_raw().iter().map(|&v| f64::from(v)).sum::<f64>() / image.as_raw().len() as f64
}

#[test]
fn settings_round_trip_as_project_files_and_presets_store_them() {
    let settings = StackSettings { average: AverageSettings { radius: 3.0, smoothing: 1.5, power: 2.5 }, ..StackSettings::default() };
    let json = serde_json::to_value(&settings).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "average": { "radius": 3.0, "smoothing": 1.5, "power": 2.5 },
            "pyramid": { "levels": 0, "grit": 0.5, "decisions": "luminance", "range_limit": 1.0, "tone": "compress" },
            "depth_map": { "radius": 2.0, "smoothing": 8.0, "threshold": "auto", "level": 0.001, "percentile": 30.0 },
        })
    );
    assert_eq!(serde_json::from_value::<StackSettings>(json).unwrap(), settings);
    let partial: StackSettings = toml::from_str(
        "[average]
radius = 5.0
",
    )
    .unwrap();
    assert_eq!(partial.average, AverageSettings { radius: 5.0, ..AverageSettings::default() });
    assert!(serde_json::from_value::<StackSettings>(serde_json::json!({ "average": { "radious": 1.0 } })).is_err());
    assert!(serde_json::from_value::<StackSettings>(serde_json::json!({ "depthmap": {} })).is_err());
    let invalid = StackSettings { average: AverageSettings { smoothing: -1.0, ..AverageSettings::default() }, ..StackSettings::default() };
    assert!(invalid.validate().is_err());
    assert_eq!(AverageSettings::default().name(), "average");
}
