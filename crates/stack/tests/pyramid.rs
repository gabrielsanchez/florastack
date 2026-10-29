//! The pyramid method on synthetic stacks whose layers occlude one another (`florastack_core::testutil::layered`), scored
//! against their sharp ground truth with florastack-metrics and against the weighted average; each of its settings for
//! the method's known weaknesses; and the framework under it: tiles, threads, spilling, coverage, progress,
//! cancellation, hostile samples and previews.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use florastack_core::testutil::{Backdrop, StackParams, SyntheticStack, generate_layered, scene};
use florastack_core::{ColorSpace, Image, Monitor, Rect, Unmonitored};
use florastack_metrics::{HaloParams, NoiseParams, color_shift, halo, noise, psnr, ssim};
use florastack_stack::{
    AverageSettings, CoveredFrame, Decisions, FrameSource, Method, Pyramid, PyramidSettings, StackError, StackShape, Stacked, Stacker, Tiled, Tiling,
    Tone, compress_range, out_of_range, preview, reduce, stack,
};

/// A stack of `frames` frames of a layered scene (`width` x `height`, every length `scale` times the scene's own),
/// focused from depth 0 to 1, already aligned, with sensor noise when `noisy`: the blur grows by 8 `scale` pixels per
/// unit of depth, as in the weighted average's tests.
fn layered(width: usize, height: usize, scale: f64, backdrop: Backdrop, frames: usize, noisy: bool, seed: u64) -> SyntheticStack {
    let scene = scene::layered(width, height, scale, backdrop, seed).unwrap();
    #[allow(clippy::cast_possible_truncation)]
    let s = scale as f32;
    let mut params = StackParams {
        focus_range: Some((0.0, 1.0)),
        blur_per_depth: 8.0 * s,
        max_blur: 8.0 * s,
        blur_step: 0.5 * s,
        seed,
        ..StackParams::identity(frames)
    };
    if noisy {
        params.read_noise = 0.005;
        params.shot_noise = 0.01;
    }
    generate_layered(&scene, &params).unwrap()
}

/// The weighted average's settings for a scale: its defaults for camera frames, smaller lengths for the scene's own.
fn average_for(scale: f64) -> AverageSettings {
    if scale >= 3.0 { AverageSettings::default() } else { AverageSettings { radius: 1.0, smoothing: 1.0, power: 3.0 } }
}

fn run(method: &impl Method, tiling: &Tiling, frames: &[impl FrameSource]) -> Stacked {
    let mut stacker = method.stacker(tiling).unwrap();
    stack(&mut stacker, frames, &Unmonitored).unwrap()
}

/// The pyramid's output as the engine makes it: the stack, brought into range by its tone setting.
fn pyramid(settings: &PyramidSettings, frames: &[impl FrameSource]) -> Image<f32> {
    let mut image = run(settings, &Tiling::default(), frames).image;
    compress_range(&mut image, settings.tone).unwrap();
    image
}

#[derive(Debug)]
struct Scores {
    ssim: f64,
    halo: f64,
    noise: f64,
    delta_e: f64,
    psnr: f64,
    out_of_range: f64,
}

fn scores(image: &Image<f32>, truth: &Image<f32>) -> Scores {
    Scores {
        ssim: ssim(image, truth).unwrap(),
        halo: halo(image, truth, &HaloParams::default()).unwrap().relative,
        noise: noise(image, truth, &NoiseParams::default()).map_or(f64::NAN, |n| n.sigma),
        delta_e: color_shift(image, truth).unwrap().mean,
        psnr: psnr(image, truth).unwrap(),
        out_of_range: out_of_range(image) as f64 / (image.width() * image.height()) as f64,
    }
}

fn show(name: &str, s: &Scores) {
    println!(
        "  {name:<18} SSIM {:.4}  halo {:.4}  noise {:.5}  dE {:.3}  PSNR {:.2} dB  out of range {:.3}%",
        s.ssim,
        s.halo,
        s.noise,
        s.delta_e,
        s.psnr,
        100.0 * s.out_of_range
    );
}

#[test]
fn the_pyramid_beats_the_weighted_average_on_overlapping_thin_structures() {
    // Twelve frames of three discs and twelve crossing bristles at depths from 0 to 0.55 over a receding backdrop,
    // whose out-of-focus edges veil what lies behind them: at a camera's scale (720x540, lengths three times the
    // scene's, the weighted average's defaults) and at the scene's own (320x240), clean and noisy, on a textured
    // backdrop and, at the camera's scale, a smooth one. Thresholds from the prototype's measurements (three seeds:
    // SSIM at least 0.044 above the average's at the camera's scale and 0.011 at the scene's, halos at most 0.64 and
    // 0.85 times the average's), re-measured on this implementation (the test prints them), with headroom.
    let cases = [(720, 540, 3.0, Backdrop::Textured), (320, 240, 1.0, Backdrop::Textured), (720, 540, 3.0, Backdrop::SmoothRight)];
    for (w, h, scale, backdrop) in cases {
        let seeds: &[u64] = if backdrop == Backdrop::Textured { &[1, 2] } else { &[1] };
        for &seed in seeds {
            for noisy in [false, true] {
                let synthetic = layered(w, h, scale, backdrop, 12, noisy, seed);
                let truth = &synthetic.truth.sharp;
                let best = synthetic.frames.iter().map(|f| ssim(f, truth).unwrap()).fold(0.0, f64::max);
                let average = scores(&run(&average_for(scale), &Tiling::default(), &synthetic.frames).image, truth);
                let pyr = scores(&pyramid(&PyramidSettings::default(), &synthetic.frames), truth);
                println!("{w}x{h} {backdrop:?}, seed {seed}, noisy {noisy}: best frame SSIM {best:.4}");
                show("weighted average", &average);
                show("pyramid", &pyr);
                // On the smooth backdrop the weighted average has less to lose (the prototype measured 0.029 of SSIM
                // between them, this implementation 0.0285).
                let (ssim_margin, halo_ratio) = match (scale >= 3.0, backdrop) {
                    (true, Backdrop::SmoothRight) => (0.02, 0.75),
                    (true, Backdrop::Textured) => (0.03, 0.75),
                    (false, _) => (0.005, 0.92),
                };
                assert!(pyr.ssim > average.ssim + ssim_margin, "{w}x{h} seed {seed} noisy {noisy}: SSIM {:.4} against {:.4}", pyr.ssim, average.ssim);
                assert!(pyr.halo < halo_ratio * average.halo, "{w}x{h} seed {seed} noisy {noisy}: halo {:.4} against {:.4}", pyr.halo, average.halo);
                assert_eq!(pyr.out_of_range, 0.0, "tone compression brings the output into range");
            }
        }
    }
}

#[test]
fn grit_suppression_blends_the_finest_level_noise_away() {
    // Picking the finest level's coefficients picks the largest of the frames' noise; the blend averages it. Measured
    // in the prototype: 0.0040 against 0.0084 for picking and 0.0048 for the weighted average, for 0.0005 of SSIM on
    // clean stacks.
    let noisy = layered(720, 540, 3.0, Backdrop::Textured, 12, true, 3);
    let truth = &noisy.truth.sharp;
    let blend = scores(&pyramid(&PyramidSettings::default(), &noisy.frames), truth);
    let pick = scores(&pyramid(&PyramidSettings { grit: 0.0, ..PyramidSettings::default() }, &noisy.frames), truth);
    let average = scores(&run(&average_for(3.0), &Tiling::default(), &noisy.frames).image, truth);
    println!("noisy, 720x540:");
    show("grit 0.5", &blend);
    show("grit 0 (picks)", &pick);
    show("weighted average", &average);
    assert!(blend.noise < 0.6 * pick.noise, "{:.5} against {:.5}", blend.noise, pick.noise);
    assert!(blend.noise < 1.1 * average.noise, "{:.5} against {:.5}", blend.noise, average.noise);
    let clean = layered(720, 540, 3.0, Backdrop::Textured, 12, false, 3);
    let truth = &clean.truth.sharp;
    let (blend, pick) = (
        scores(&pyramid(&PyramidSettings::default(), &clean.frames), truth),
        scores(&pyramid(&PyramidSettings { grit: 0.0, ..PyramidSettings::default() }, &clean.frames), truth),
    );
    println!("clean: SSIM {:.4} blended, {:.4} picked", blend.ssim, pick.ssim);
    assert!(blend.ssim > pick.ssim - 0.005, "a little fine detail for much less noise: {:.4} against {:.4}", blend.ssim, pick.ssim);
}

/// The least and the largest value of each sample over the frames.
fn envelope(frames: &[Image<f32>]) -> (Vec<f32>, Vec<f32>) {
    let n = frames[0].as_raw().len();
    let mut low = vec![f32::INFINITY; n];
    let mut high = vec![f32::NEG_INFINITY; n];
    for f in frames {
        for ((l, h), &v) in low.iter_mut().zip(high.iter_mut()).zip(f.as_raw()) {
            *l = l.min(v);
            *h = h.max(v);
        }
    }
    (low, high)
}

#[test]
fn the_range_limit_holds_every_pixel_within_the_frames_and_tone_compression_brings_it_into_range() {
    let synthetic = layered(320, 240, 1.0, Backdrop::Textured, 10, true, 4);
    let (low, high) = envelope(&synthetic.frames);
    let within = |image: &Image<f32>| {
        image
            .as_raw()
            .iter()
            .zip(low.iter().zip(&high))
            .filter(|&(&v, (&l, &h))| v < l - l.abs() * 0.008 - 1e-6 || v > h + h.abs() * 0.008 + 1e-6)
            .count()
    };
    let limited = run(&PyramidSettings::default(), &Tiling::default(), &synthetic.frames).image;
    let free = run(&PyramidSettings { range_limit: 0.0, ..PyramidSettings::default() }, &Tiling::default(), &synthetic.frames).image;
    println!("samples beyond the frames' range: {} limited, {} without the limit", within(&limited), within(&free));
    assert_eq!(within(&limited), 0, "within the frames' range, up to the bounds' bfloat16 rounding");
    assert!(within(&free) > 0, "selection alone overshoots");
    let half = run(&PyramidSettings { range_limit: 0.5, ..PyramidSettings::default() }, &Tiling::default(), &synthetic.frames).image;
    assert!(within(&half) <= within(&free));

    // Frames brighter than white (a RAW development with exposure added): the stack keeps them unclipped, and tone
    // compression brings it into range, or keeps it for extended-range output.
    let bright: Vec<Image<f32>> = synthetic
        .frames
        .iter()
        .map(|f| Image::from_vec(f.width(), f.height(), 3, ColorSpace::LINEAR, f.as_raw().iter().map(|v| v * 1.6).collect()).unwrap())
        .collect();
    let stacked = run(&PyramidSettings::default(), &Tiling::default(), &bright).image;
    let over = out_of_range(&stacked);
    assert!(over > 0, "the unclipped result keeps the frames' highlights");
    let mut compressed = stacked.clone();
    let changed = compress_range(&mut compressed, Tone::Compress).unwrap();
    assert!(changed >= over && out_of_range(&compressed) == 0, "{changed} changed, {over} out of range");
    let mut kept = stacked.clone();
    assert_eq!(compress_range(&mut kept, Tone::Keep).unwrap(), 0);
    assert_eq!(kept, stacked);
}

#[test]
fn decisions_on_luminance_keep_channels_together_and_on_channels_take_each_from_its_sharpest_frame() {
    // Two frames: red is sharp in the first and green in the second, the other channel blurred in each. Green weighs
    // most in luminance, so luminance decisions take both channels from the second frame; channel decisions take each
    // channel's detail from the frame where it is sharp.
    let texture = |seed: usize| {
        Image::from_fn(96, 96, 1, ColorSpace::LINEAR, move |x, y, _| if ((x + seed) / 3 + y / 3).is_multiple_of(2) { 0.7 } else { 0.2 }).unwrap()
    };
    let (red, green) = (texture(0), texture(1));
    let blur = |i: &Image<f32>| florastack_core::ops::gaussian_blur(i, 2.5).unwrap();
    let (red_soft, green_soft) = (blur(&red), blur(&green));
    let frame = |r: &Image<f32>, g: &Image<f32>| {
        Image::from_fn(96, 96, 3, ColorSpace::LINEAR, |x, y, c| match c {
            0 => r.get(x, y, 0).unwrap(),
            1 => g.get(x, y, 0).unwrap(),
            _ => 0.3,
        })
        .unwrap()
    };
    let frames = [frame(&red, &green_soft), frame(&red_soft, &green)];
    let channel = |image: &Image<f32>, c: usize| Image::from_fn(96, 96, 1, ColorSpace::LINEAR, |x, y, _| image.get(x, y, c).unwrap()).unwrap();
    let inner = Rect::new(8, 8, 80, 80);
    let score =
        |image: &Image<f32>, c: usize, truth: &Image<f32>| psnr(&channel(image, c).crop(inner).unwrap(), &truth.crop(inner).unwrap()).unwrap();
    for decisions in [Decisions::Luminance, Decisions::Channels] {
        let settings = PyramidSettings { decisions, levels: 4, ..PyramidSettings::default() };
        let out = run(&settings, &Tiling::default(), &frames).image;
        let (r, g) = (score(&out, 0, &red), score(&out, 1, &green));
        println!("{decisions:?}: red {r:.1} dB, green {g:.1} dB from their sharp versions");
        assert!(g > 25.0, "green is sharp either way: {g:.1}");
        match decisions {
            Decisions::Luminance => assert!(r < 20.0, "red comes from the frame chosen on luminance, where it is soft: {r:.1}"),
            Decisions::Channels => assert!(r > 25.0, "red comes from its own sharpest frame: {r:.1}"),
        }
    }
    // On a layered stack, both, with their color shift.
    let synthetic = layered(320, 240, 1.0, Backdrop::Textured, 12, true, 5);
    for decisions in [Decisions::Luminance, Decisions::Channels] {
        let s = scores(&pyramid(&PyramidSettings { decisions, ..PyramidSettings::default() }, &synthetic.frames), &synthetic.truth.sharp);
        show(&format!("{decisions:?}"), &s);
    }
}

#[test]
fn the_coarse_limit_keeps_coarse_choices_near_the_depth_consistent_frame() {
    // The first frame is sharp everywhere; the others are its blur plus a broad bright blob, which has the most energy
    // at the coarse levels but no fine detail. Without the limit the coarse levels take the blob; with a limit of 0
    // frames they take the depth-consistent frame, the one with the most fine detail: the first.
    let sharp = Image::from_fn(160, 160, 1, ColorSpace::LINEAR, |x, y, _| if (x / 2 + y / 2) % 2 == 0 { 0.6 } else { 0.3 }).unwrap();
    let soft = florastack_core::ops::gaussian_blur(&sharp, 3.0).unwrap();
    let blob = Image::from_fn(160, 160, 1, ColorSpace::LINEAR, |x, y, _| {
        let r2 = (x as f32 - 80.0).powi(2) + (y as f32 - 80.0).powi(2);
        soft.get(x, y, 0).unwrap() + 0.3 * (-r2 / (2.0 * 14.0 * 14.0)).exp()
    })
    .unwrap();
    let frames = [sharp.clone(), blob.clone(), blob];
    let at_center = |image: &Image<f32>| image.crop(Rect::new(72, 72, 16, 16)).unwrap().as_raw().iter().map(|&v| f64::from(v)).sum::<f64>() / 256.0;
    let base = PyramidSettings { levels: 5, range_limit: 0.0, ..PyramidSettings::default() };
    let free = run(&base, &Tiling::default(), &frames).image;
    let limited = run(&PyramidSettings { coarse_limit: Some(0), ..base.clone() }, &Tiling::default(), &frames).image;
    let (free_center, limited_center, sharp_center) = (at_center(&free), at_center(&limited), at_center(&sharp));
    println!("mean at the blob's center: {free_center:.4} free, {limited_center:.4} limited, {sharp_center:.4} in the sharp frame");
    assert!(free_center > sharp_center + 0.05, "without the limit the coarse levels take the blob");
    assert!(limited_center < free_center - 0.04, "with it the depth-consistent frame's");

    // On a layered stack it is off by default, and costs a little halo where it is on (measured in the prototype); at
    // the camera's scale there are coarse levels (5 levels, 3 of them in tiles) for it to act on.
    let synthetic = layered(720, 540, 3.0, Backdrop::Textured, 12, false, 6);
    for limit in [None, Some(1)] {
        let s = scores(&pyramid(&PyramidSettings { coarse_limit: limit, ..PyramidSettings::default() }, &synthetic.frames), &synthetic.truth.sharp);
        show(&format!("coarse limit {limit:?}"), &s);
    }
    assert_eq!(PyramidSettings::default().coarse_limit, None);
}

/// A frame's coverage that ramps from 0 at `inset` pixels inside the canvas's left and top edges to 1 two pixels in.
fn inset_coverage(width: usize, height: usize, inset: f32) -> Image<f32> {
    Image::from_fn(width, height, 1, ColorSpace::LINEAR, |x, y, _| f32::midpoint(x.min(y) as f32 - inset, 0.5).clamp(0.0, 1.0)).unwrap()
}

#[test]
fn results_are_the_same_whatever_the_threads_the_tiles_and_the_spilling() {
    // An odd-sized canvas, so that tiles start off the lattice and the levels' last samples are partial, frames with and
    // without coverage, and every setting that changes the state's layout.
    let synthetic = layered(701, 503, 1.0, Backdrop::Textured, 4, true, 7);
    let covered: Vec<CoveredFrame> = synthetic
        .frames
        .iter()
        .enumerate()
        .map(|(i, f)| CoveredFrame::new(f.clone(), inset_coverage(701, 503, 2.0 + 3.0 * i as f32)).unwrap())
        .collect();
    let dir = std::env::temp_dir().join(format!("florastack-pyramid-spill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for settings in [
        PyramidSettings::default(),
        PyramidSettings { grit: 0.0, decisions: Decisions::Channels, coarse_limit: Some(1), range_limit: 0.7, ..PyramidSettings::default() },
        PyramidSettings { levels: 2, range_limit: 0.0, ..PyramidSettings::default() },
    ] {
        let one_tile = Tiling { tile_size: 4096, ..Tiling::default() };
        let small = Tiling { tile_size: 64, ..Tiling::default() };
        let threads = |n: usize, tiling: &Tiling| {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(n).build().unwrap();
            pool.install(|| (run(&settings, tiling, &synthetic.frames), run(&settings, tiling, &covered)))
        };
        let whole = threads(4, &one_tile);
        let tiled = threads(3, &small);
        let mut probe = Tiled::new(Pyramid::new(&settings).unwrap(), small.clone());
        probe.begin(StackShape { width: 701, height: 503, channels: 3 }).unwrap();
        assert!(probe.grid().unwrap().len() > 1, "{:?}", probe.grid());
        assert_eq!(whole, tiled, "{settings:?}: tiles change nothing, bit for bit");
        assert_eq!(threads(1, &small), tiled, "{settings:?}: nor threads");
        let spilling = Tiling { tile_size: 64, budget: 1 << 20, spill_dir: Some(dir.clone()) };
        assert_eq!(run(&settings, &spilling, &covered), whole.1, "{settings:?}: nor spilling to disk");
        assert!(whole.1.alpha.is_some(), "the inset frames leave the corner uncovered");
    }
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(left.is_empty(), "the tile folder goes with the stack: {left:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `image` with every pixel from column `from` on replaced by the pixel at `from - 1`, as a clamping warp streaks a
/// frame's edge.
fn streaked(image: &Image<f32>, from: usize) -> Image<f32> {
    Image::from_fn(image.width(), image.height(), image.channels(), ColorSpace::LINEAR, |x, y, c| image.get(x.min(from - 1), y, c).unwrap()).unwrap()
}

#[test]
fn uncovered_pixels_never_smear_and_get_no_alpha() {
    let synthetic = layered(240, 120, 1.0, Backdrop::Textured, 2, false, 8);
    let ramp =
        |edge: usize| Image::from_fn(240, 120, 1, ColorSpace::LINEAR, |x, _, _| ((edge as f32 - x as f32 - 0.5) / 2.0).clamp(0.0, 1.0)).unwrap();
    let frames = [
        CoveredFrame::new(streaked(&synthetic.frames[0], 150), ramp(150)).unwrap(),
        CoveredFrame::new(streaked(&synthetic.frames[1], 200), ramp(200)).unwrap(),
    ];
    for tiling in [Tiling::default(), Tiling { tile_size: 64, ..Tiling::default() }] {
        let out = run(&PyramidSettings::default(), &tiling, &frames);
        let alpha = out.alpha.as_ref().expect("an alpha channel: some pixels are covered by no frame");
        assert_eq!(out.uncovered, 40 * 120, "columns 200 to 239");
        for y in 0..120 {
            for x in 0..240 {
                assert_eq!(alpha.get(x, y, 0), Some(if x < 200 { 1.0 } else { 0.0 }), "alpha at {x},{y}");
            }
            assert_eq!(out.image.pixel(230, y), Some(&[0.0, 0.0, 0.0][..]));
        }
        // Where only the second frame covers, the result is that frame, not the first frame's streaks.
        let only_second = Rect::new(156, 4, 40, 112);
        let error = psnr(&out.image.crop(only_second).unwrap(), &synthetic.frames[1].crop(only_second).unwrap()).unwrap();
        assert!(error > 40.0, "{error:.1} dB");
        assert!(out.image.as_raw().iter().all(|v| v.is_finite()));
    }
}

/// A monitor that records progress and cancels after `limit` steps.
struct Recording {
    limit: u64,
    steps: AtomicU64,
    cancelled: AtomicBool,
    seen: Mutex<Vec<(u64, u64)>>,
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
fn progress_cancellation_and_frames_that_dont_fit() {
    let synthetic = layered(400, 300, 1.0, Backdrop::Textured, 3, false, 9);
    let tiling = Tiling { tile_size: 64, ..Tiling::default() };
    let settings = PyramidSettings::default();
    let all = Recording { limit: u64::MAX, steps: AtomicU64::new(0), cancelled: AtomicBool::new(false), seen: Mutex::new(Vec::new()) };
    let mut stacker = settings.stacker(&tiling).unwrap();
    stack(&mut stacker, &synthetic.frames, &all).unwrap();
    let seen = all.seen.lock().unwrap().clone();
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0 && w[0].1 == w[1].1), "never goes back: {seen:?}");
    assert_eq!(seen.last().map(|&(d, t)| d == t), Some(true));
    let cancelling = Recording { limit: 2, steps: AtomicU64::new(0), cancelled: AtomicBool::new(false), seen: Mutex::new(Vec::new()) };
    let mut stacker = settings.stacker(&tiling).unwrap();
    assert!(matches!(stack(&mut stacker, &synthetic.frames, &cancelling), Err(StackError::Cancelled(_))));
    assert!(matches!(stacker.finish(&Unmonitored), Err(StackError::NotStarted)));
    assert_eq!(stack(&mut stacker, &synthetic.frames, &Unmonitored).unwrap().frames, 3, "it can start again");

    stacker.begin(StackShape { width: 400, height: 300, channels: 3 }).unwrap();
    assert!(matches!(stacker.finish(&Unmonitored), Err(StackError::NoFrames)));
    stacker.begin(StackShape { width: 400, height: 300, channels: 3 }).unwrap();
    let gray = Image::<f32>::new(400, 300, 1, ColorSpace::LINEAR).unwrap();
    assert!(matches!(stacker.add_frame(&gray, &Unmonitored), Err(StackError::FrameShape { .. })));
    stacker.add_frame(&synthetic.frames[0], &Unmonitored).unwrap();
    assert_eq!(stacker.finish(&Unmonitored).unwrap().frames, 1, "a frame of the wrong shape leaves the stack as it was");
}

#[test]
fn hostile_samples_and_tiny_canvases_give_finite_stacks() {
    // Regression: samples that aren't finite, or are huge, must neither poison the sums nor overflow them.
    let mut frames = vec![layered(64, 48, 1.0, Backdrop::Textured, 1, false, 10).frames[0].clone(); 3];
    frames[0].pixel_mut(10, 10).unwrap().copy_from_slice(&[f32::NAN, f32::INFINITY, 1.0]);
    frames[1].pixel_mut(10, 10).unwrap().copy_from_slice(&[1e38, -1e38, f32::MAX]);
    frames[2].pixel_mut(30, 20).unwrap().copy_from_slice(&[f32::NEG_INFINITY; 3]);
    for settings in [
        PyramidSettings::default(),
        PyramidSettings { grit: 0.25, decisions: Decisions::Channels, coarse_limit: Some(2), ..PyramidSettings::default() },
    ] {
        let out = run(&settings, &Tiling::default(), &frames);
        assert!(out.image.as_raw().iter().all(|v| v.is_finite()), "{settings:?}");
    }
    let nothing = vec![Image::from_fn(16, 16, 3, ColorSpace::LINEAR, |_, _, _| f32::NAN).unwrap()];
    let out = run(&PyramidSettings::default(), &Tiling::default(), &nothing);
    assert_eq!(out.uncovered, 256);
    assert!(out.image.as_raw().iter().all(|&v| v == 0.0));
    // Canvases smaller than the pyramid's levels and tiles.
    for (w, h) in [(1, 1), (3, 2), (17, 1), (9, 40)] {
        let frames = vec![Image::from_fn(w, h, 2, ColorSpace::LINEAR, |x, y, c| ((x + y + c) % 3) as f32 * 0.3).unwrap(); 2];
        let out = run(&PyramidSettings { levels: 10, ..PyramidSettings::default() }, &Tiling::default(), &frames);
        assert_eq!((out.image.width(), out.image.height()), (w, h));
        let err = out.image.as_raw().iter().zip(frames[0].as_raw()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 1e-5, "two identical frames stack to themselves: {w}x{h} {err}");
    }
}

#[test]
fn settings_are_checked_and_scale_for_previews() {
    assert!(PyramidSettings::default().validate().is_ok());
    for bad in [
        PyramidSettings { levels: 11, ..PyramidSettings::default() },
        PyramidSettings { grit: 0.1, ..PyramidSettings::default() },
        PyramidSettings { grit: 1.5, ..PyramidSettings::default() },
        PyramidSettings { grit: f64::NAN, ..PyramidSettings::default() },
        PyramidSettings { range_limit: -0.1, ..PyramidSettings::default() },
        PyramidSettings { range_limit: f64::INFINITY, ..PyramidSettings::default() },
        PyramidSettings { coarse_limit: Some(1001), ..PyramidSettings::default() },
    ] {
        assert!(matches!(bad.validate(), Err(StackError::Setting { .. })), "{bad:?}");
        assert!(bad.stacker(&Tiling::default()).is_err());
    }
    let defaults = PyramidSettings::default();
    assert_eq!((defaults.levels_for(720, 540), defaults.levels_for(5184, 3888), defaults.levels_for(320, 240)), (5, 7, 3));
    assert_eq!((defaults.levels_for(20, 20), defaults.levels_for(1, 1)), (1, 1));
    assert_eq!(PyramidSettings { levels: 9, ..defaults.clone() }.levels_for(100, 60), 5, "never more than the canvas allows");
    assert_eq!(PyramidSettings { levels: 6, ..defaults.clone() }.reduced(4.0).levels, 4);
    assert_eq!(defaults.reduced(4.0), defaults, "automatic levels follow the reduced canvas");
    assert_eq!(defaults.name(), "pyramid");
    let partial: PyramidSettings = toml::from_str("grit = 0.25\ntone = \"clip\"\ncoarse_limit = 2\n").unwrap();
    assert_eq!(partial, PyramidSettings { grit: 0.25, tone: Tone::Clip, coarse_limit: Some(2), ..PyramidSettings::default() });
    let json = serde_json::to_value(&partial).unwrap();
    assert_eq!(json["coarse_limit"], 2);
    assert_eq!(serde_json::from_value::<PyramidSettings>(json).unwrap(), partial);
    assert!(serde_json::from_value::<PyramidSettings>(serde_json::json!({ "grits": 0.5 })).is_err());
    assert!(serde_json::from_value::<PyramidSettings>(serde_json::json!({ "decisions": "hue" })).is_err());
}

#[test]
fn the_quarter_scale_preview_looks_like_the_full_stack() {
    let synthetic = layered(720, 540, 3.0, Backdrop::Textured, 12, false, 11);
    let settings = PyramidSettings::default();
    let mut quarter = preview(&settings, &Tiling::default()).unwrap();
    let small = stack(&mut quarter, &synthetic.frames, &Unmonitored).unwrap();
    assert_eq!((small.image.width(), small.image.height()), (180, 135));
    let truth = reduce(&synthetic.truth.sharp).unwrap();
    let reduced: Vec<Image<f32>> = synthetic.frames.iter().map(|f| reduce(f).unwrap()).collect();
    let best = reduced.iter().map(|f| ssim(f, &truth).unwrap()).fold(0.0, f64::max);
    let score = ssim(&small.image, &truth).unwrap();
    let full = reduce(&run(&settings, &Tiling::default(), &synthetic.frames).image).unwrap();
    let likeness = psnr(&small.image, &full).unwrap();
    println!("preview: SSIM {score:.4} (best reduced frame {best:.4}), {likeness:.1} dB from the full stack reduced");
    assert!(score > best + 0.02, "{score:.4} against {best:.4}");
    assert!(likeness > 28.0, "{likeness:.1} dB");
}
