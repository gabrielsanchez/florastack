//! The depth-map method on synthetic stacks: the depth it recovers against the scenes' true depth, the color of its
//! composite against the frames' and the truth's (CIEDE2000), and how it compares with the other methods on stacks
//! whose layers occlude one another; the two-pass API (a new threshold after pass 1 re-composites fast, and gives what
//! a new stack would), the threshold as a level or a percentile, the low-confidence mask; and the framework under it:
//! tiles, threads, spilling, coverage, progress, cancellation, hostile input and previews.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use florastack_core::testutil::{Backdrop, StackParams, SyntheticStack, generate, generate_layered, scene};
use florastack_core::{ColorSpace, Image, Monitor, Rect, Unmonitored};
use florastack_metrics::{HaloParams, NoiseParams, color_shift, halo, noise, psnr, ssim};
use florastack_stack::{
    AverageSettings, CoveredFrame, DepthSettings, DepthStack, FrameSource, Method, PyramidSettings, StackError, StackSettings, StackShape, Stacker,
    Threshold, Tiling, compress_range, depth_stack, reduce, stack,
};

/// A stack's making: `frames` frames focused from depth 0 to 1, already aligned, the blur growing by 8 `scale` pixels
/// per unit of depth as in the other methods' tests, with sensor noise when `noisy`.
fn params(frames: usize, scale: f64, noisy: bool, seed: u64) -> StackParams {
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
    params
}

/// A layered scene's stack (`florastack_core::testutil::layered`), `width` x `height`, every length `scale` times the
/// scene's own.
fn layered(width: usize, height: usize, scale: f64, backdrop: Backdrop, frames: usize, noisy: bool, seed: u64) -> SyntheticStack {
    let scene = scene::layered(width, height, scale, backdrop, seed).unwrap();
    generate_layered(&scene, &params(frames, scale, noisy, seed)).unwrap()
}

/// The layered scene's textured backdrop alone on a depth that slopes across the whole range and undulates down it:
/// every pixel has texture, so every pixel's depth can be told.
fn sloped(width: usize, height: usize, scale: f64, frames: usize, noisy: bool, seed: u64) -> SyntheticStack {
    let scene = scene::layered(width, height, scale, Backdrop::Textured, seed).unwrap();
    let (w, h) = (width as f32, height as f32);
    let depth = Image::from_fn(width, height, 1, ColorSpace::LINEAR, |x, y, _| {
        0.05 + 0.9 * (0.75 * x as f32 / w + 0.25 * (0.5 + 0.5 * (std::f32::consts::TAU * y as f32 / h).sin()))
    })
    .unwrap();
    generate(&scene.background, &depth, &params(frames, scale, noisy, seed)).unwrap()
}

/// The depth-map method's settings for a scale: its defaults for camera frames (three times the scenes' own lengths),
/// shorter radii for the scenes' own.
fn settings_for(scale: f64) -> DepthSettings {
    DepthSettings::default().reduced(3.0 / scale)
}

fn depth(settings: &DepthSettings, tiling: &Tiling, frames: &[impl FrameSource]) -> DepthStack {
    depth_stack(settings, tiling, frames, None, &Unmonitored).unwrap()
}

fn run(method: &impl Method, frames: &[impl FrameSource]) -> Image<f32> {
    let mut stacker = method.stacker(&Tiling::default()).unwrap();
    stack(&mut stacker, frames, &Unmonitored).unwrap().image
}

/// Pearson's correlation of `a` and `b`.
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().map(|&v| f64::from(v)).sum::<f64>() / n, b.iter().map(|&v| f64::from(v)).sum::<f64>() / n);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (&x, &y) in a.iter().zip(b) {
        let (dx, dy) = (f64::from(x) - ma, f64::from(y) - mb);
        ab += dx * dy;
        aa += dx * dx;
        bb += dy * dy;
    }
    ab / (aa * bb).sqrt()
}

/// The true depth of every pixel, in frames: the scenes' depth from 0 to 1 is focused from the first frame to the last.
fn true_frames(synthetic: &SyntheticStack) -> Vec<f32> {
    #[allow(clippy::cast_precision_loss)]
    let last = (synthetic.frames.len() - 1) as f32;
    synthetic.truth.depth.as_raw().iter().map(|d| d * last).collect()
}

/// The depth map's correlation with the true depth, and its mean error in frames.
fn depth_scores(map: &Image<f32>, truth: &[f32]) -> (f64, f64) {
    let error = map.as_raw().iter().zip(truth).map(|(&a, &b)| f64::from((a - b).abs())).sum::<f64>() / truth.len() as f64;
    (correlation(map.as_raw(), truth), error)
}

#[derive(Debug)]
struct Scores {
    ssim: f64,
    halo: f64,
    noise: f64,
    delta_e: f64,
    psnr: f64,
}

fn scores(image: &Image<f32>, truth: &Image<f32>) -> Scores {
    Scores {
        ssim: ssim(image, truth).unwrap(),
        halo: halo(image, truth, &HaloParams::default()).unwrap().relative,
        noise: noise(image, truth, &NoiseParams::default()).map_or(f64::NAN, |n| n.sigma),
        delta_e: color_shift(image, truth).unwrap().mean,
        psnr: psnr(image, truth).unwrap(),
    }
}

fn show(name: &str, s: &Scores) {
    println!("  {name:<18} SSIM {:.4}  halo {:.4}  noise {:.5}  dE {:.3}  PSNR {:.2} dB", s.ssim, s.halo, s.noise, s.delta_e, s.psnr);
}

#[test]
fn the_depth_map_follows_the_scenes_depth() {
    // Where every pixel has texture, the depth map follows the true depth closely; on a scene of flat discs and thin
    // bristles over a receding plane (`scene::textured`, at its own scale), less so: the discs' insides have no detail
    // and take their depth from their edges, and the window the focus is measured over widens the thin bristles.
    // Thresholds from the measured values (printed), with headroom.
    for noisy in [false, true] {
        let synthetic = sloped(720, 540, 3.0, 12, noisy, 1);
        let truth = true_frames(&synthetic);
        let result = depth(&settings_for(3.0), &Tiling::default(), &synthetic.frames);
        let (raw, raw_error) = depth_scores(&result.stats.depth, &truth);
        let (r, error) = depth_scores(&result.map.depth, &truth);
        println!("sloped 720x540, noisy {noisy}: correlation {r:.4} (pass 1 alone {raw:.4}), mean error {error:.3} frames ({raw_error:.3})");
        assert!(r > 0.97, "{r:.4}");
        assert!(error < 0.5, "{error:.3} frames");

        let (sharp, depth_map) = scene::textured(320, 240, 2).unwrap();
        let synthetic = generate(&sharp, &depth_map, &params(12, 1.0, noisy, 2)).unwrap();
        let truth = true_frames(&synthetic);
        let result = depth(&settings_for(1.0), &Tiling::default(), &synthetic.frames);
        let (r, error) = depth_scores(&result.map.depth, &truth);
        println!("textured 320x240, noisy {noisy}: correlation {r:.4}, mean error {error:.3} frames");
        assert!(r > 0.6, "{r:.4}");
        assert!(error < 1.0, "{error:.3} frames");
    }
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
fn the_composite_never_alters_color_or_contrast() {
    // Every sample is a blend of the frames' samples at that pixel, so it lies within their range there, up to the
    // rounding of the blend's sums (a few units in the last place).
    let synthetic = layered(320, 240, 1.0, Backdrop::Textured, 10, true, 3);
    let (low, high) = envelope(&synthetic.frames);
    let out = depth(&settings_for(1.0), &Tiling::default(), &synthetic.frames).stacked.image;
    let beyond = out
        .as_raw()
        .iter()
        .zip(low.iter().zip(&high))
        .filter(|&(&v, (&l, &h))| v < l - l.abs() * 1e-6 - 1e-7 || v > h + h.abs() * 1e-6 + 1e-7)
        .count();
    assert_eq!(beyond, 0, "every sample within the frames' range");
    // Frames alike stack to themselves.
    let frame = &synthetic.frames[4];
    let same = depth(&settings_for(1.0), &Tiling::default(), &vec![frame.clone(); 5]).stacked.image;
    let error = same.as_raw().iter().zip(frame.as_raw()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(error < 1e-6, "{error}");

    // Against the truth, the color shift (mean CIEDE2000) is near zero where every pixel's depth can be told, and below
    // the other methods' on clean stacks: what the composite takes from a frame keeps that frame's color.
    let synthetic = sloped(720, 540, 3.0, 12, false, 4);
    let shift = color_shift(&depth(&settings_for(3.0), &Tiling::default(), &synthetic.frames).stacked.image, &synthetic.truth.sharp).unwrap();
    println!("sloped 720x540: mean CIEDE2000 {:.3}, largest {:.2}", shift.mean, shift.max);
    assert!(shift.mean < 0.6, "near zero: {:.3}", shift.mean);
    for backdrop in [Backdrop::Textured, Backdrop::SmoothRight] {
        let synthetic = layered(720, 540, 3.0, backdrop, 12, false, 4);
        let truth = &synthetic.truth.sharp;
        let average = scores(&run(&AverageSettings::default(), &synthetic.frames), truth);
        let mut p = run(&PyramidSettings::default(), &synthetic.frames);
        compress_range(&mut p, PyramidSettings::default().tone).unwrap();
        let pyramid = scores(&p, truth);
        let d = scores(&depth(&settings_for(3.0), &Tiling::default(), &synthetic.frames).stacked.image, truth);
        println!("layered 720x540 {backdrop:?}, clean:");
        show("weighted average", &average);
        show("pyramid", &pyramid);
        show("depth map", &d);
        assert!(
            d.delta_e < pyramid.delta_e && d.delta_e < average.delta_e,
            "{:.3} against {:.3} and {:.3}",
            d.delta_e,
            pyramid.delta_e,
            average.delta_e
        );
    }
}

#[test]
fn the_depth_map_method_does_better_than_the_weighted_average_on_occluding_layers() {
    // Taking each pixel from the frames at its depth leaves fewer of the weighted average's veils over the background:
    // higher SSIM where the backdrop is textured, less halo everywhere. The pyramid method stays sharper where near and
    // far detail overlap (printed).
    for backdrop in [Backdrop::Textured, Backdrop::SmoothRight] {
        for &seed in &[1, 2] {
            for noisy in [false, true] {
                let synthetic = layered(720, 540, 3.0, backdrop, 12, noisy, seed);
                let truth = &synthetic.truth.sharp;
                let average = scores(&run(&AverageSettings::default(), &synthetic.frames), truth);
                let mut p = run(&PyramidSettings::default(), &synthetic.frames);
                compress_range(&mut p, PyramidSettings::default().tone).unwrap();
                let pyramid = scores(&p, truth);
                let d = scores(&depth(&settings_for(3.0), &Tiling::default(), &synthetic.frames).stacked.image, truth);
                println!("720x540 {backdrop:?}, seed {seed}, noisy {noisy}:");
                show("weighted average", &average);
                show("pyramid", &pyramid);
                show("depth map", &d);
                if backdrop == Backdrop::Textured {
                    assert!(d.ssim > average.ssim + 0.015, "seed {seed} noisy {noisy}: SSIM {:.4} against {:.4}", d.ssim, average.ssim);
                }
                assert!(d.halo < 0.85 * average.halo, "{backdrop:?} seed {seed} noisy {noisy}: halo {:.4} against {:.4}", d.halo, average.halo);
            }
        }
    }
}

#[test]
fn a_new_threshold_after_pass_1_re_composites_in_well_under_a_second() {
    // Pass 1 once; then each threshold is a new depth map from the cached statistics and a new composite of the frames
    // (here in memory, as a preview's reduced frames are), which together take well under a second, and give what a
    // whole new stack with that threshold gives, bit for bit.
    let synthetic = layered(720, 540, 3.0, Backdrop::SmoothRight, 12, true, 5);
    let settings = settings_for(3.0);
    let tiling = Tiling::default();
    let start = Instant::now();
    let mut estimator = settings.estimator(&tiling).unwrap();
    estimator.begin(StackShape::of(&synthetic.frames[0])).unwrap();
    for frame in &synthetic.frames {
        estimator.add_frame(frame, &Unmonitored).unwrap();
    }
    let stats = estimator.finish(&Unmonitored).unwrap();
    println!("pass 1: {:.0} ms", start.elapsed().as_secs_f64() * 1e3);
    let pixels = 720 * 540;
    let mut filled = Vec::new();
    for threshold in [Threshold::Percentile(10.0), Threshold::Percentile(50.0), Threshold::Level(f64::from(stats.level(Threshold::Percentile(80.0))))]
    {
        // The fastest of three, so that other tests running beside this one don't count.
        let (mut resolved, mut total) = (Duration::MAX, Duration::MAX);
        let (mut map, mut composite) = (None, None);
        for _ in 0..3 {
            let start = Instant::now();
            let m = stats.resolve(threshold, settings.smoothing, None).unwrap();
            resolved = resolved.min(start.elapsed());
            let mut compositor = m.compositor(&tiling).unwrap();
            compositor.begin(StackShape::of(&synthetic.frames[0])).unwrap();
            for frame in &synthetic.frames {
                compositor.add_frame(frame, &Unmonitored).unwrap();
            }
            composite = Some(compositor.finish(&Unmonitored).unwrap());
            total = total.min(start.elapsed());
            map = Some(m);
        }
        let (map, composite) = (map.unwrap(), composite.unwrap());
        println!(
            "{threshold:?}: level {:.5}, {:.1}% filled; depth map {:.0} ms, with the composite {:.0} ms",
            map.level,
            100.0 * map.filled as f64 / f64::from(pixels),
            resolved.as_secs_f64() * 1e3,
            total.as_secs_f64() * 1e3
        );
        assert!(total.as_secs_f64() < 1.0, "{threshold:?}: {total:?}");
        let fresh = depth(&settings.with_threshold(threshold), &tiling, &synthetic.frames);
        assert_eq!(fresh.stats, stats, "pass 1 doesn't depend on the threshold");
        assert_eq!(fresh.map, map);
        assert_eq!(fresh.stacked, composite, "{threshold:?}: the same as a new stack");
        filled.push(map.filled);
    }
    assert!(filled.windows(2).all(|w| w[0] < w[1]), "a higher threshold fills more: {filled:?}");
}

#[test]
fn the_threshold_is_a_level_or_a_percentile_and_a_mask_forces_low_confidence() {
    let synthetic = sloped(320, 240, 1.0, 10, true, 6);
    let settings = settings_for(1.0);
    let stats = depth(&settings, &Tiling::default(), &synthetic.frames).stats;
    let pixels: usize = 320 * 240;
    // A percentile's level leaves that share of the pixels below it, or the pixels without confidence when they are
    // more, and they are filled, with the confident pixels that lack support among them; the same level fills the same
    // pixels.
    let none = 100.0 * stats.confidence.as_raw().iter().filter(|&&c| c <= 0.0).count() as f64 / pixels as f64;
    let mut levels = Vec::new();
    for p in [0.0, 25.0, 50.0, 90.0] {
        let map = stats.resolve(Threshold::Percentile(p), settings.smoothing, None).unwrap();
        let below = stats.confidence.as_raw().iter().filter(|&&c| c <= 0.0 || c < map.level).count();
        let (share, filled) = (100.0 * below as f64 / pixels as f64, 100.0 * map.filled as f64 / pixels as f64);
        println!("percentile {p}: level {:.5}, {share:.2}% below it ({none:.2}% without confidence), {filled:.2}% filled", map.level);
        assert!((share - p.max(none)).abs() < 0.5, "{p}: {share:.2}%");
        assert!(map.filled >= below && filled < p + 10.0, "{p}: {filled:.2}% filled");
        let by_level = stats.resolve(Threshold::Level(f64::from(map.level)), settings.smoothing, None).unwrap();
        assert_eq!(by_level, map, "{p}");
        levels.push(map.level);
    }
    assert!(levels.windows(2).all(|w| w[0] <= w[1]), "{levels:?}");
    // Without smoothing there is no neighborhood to support a pixel: the threshold alone decides.
    let alone = stats.resolve(Threshold::Percentile(25.0), 0.0, None).unwrap();
    let below = stats.confidence.as_raw().iter().filter(|&&c| c <= 0.0 || c < alone.level).count();
    assert_eq!(alone.filled, below);
    // A level above every pixel's confidence leaves none confident: each pixel's own depth is smoothed instead.
    let none = stats.resolve(Threshold::Level(1e6), settings.smoothing, None).unwrap();
    assert_eq!(none.filled, pixels);
    assert!(none.depth.as_raw().iter().all(|d| d.is_finite() && (0.0..=9.0).contains(d)));

    // A mask makes its pixels low-confidence whatever their contrast: their depth comes from around them, which on a
    // smooth depth is close to their own.
    let square = Rect::new(120, 80, 60, 60);
    let inside = |x: usize, y: usize| (square.x..square.x + square.width).contains(&x) && (square.y..square.y + square.height).contains(&y);
    let mask = Image::from_fn(320, 240, 1, ColorSpace::LINEAR, |x, y, _| if inside(x, y) { 1.0 } else { 0.0 }).unwrap();
    let free = stats.resolve(Threshold::Percentile(10.0), settings.smoothing, None).unwrap();
    let masked = stats.resolve(Threshold::Percentile(10.0), settings.smoothing, Some(&mask)).unwrap();
    assert!(masked.filled >= free.filled + 3000, "{} against {}", masked.filled, free.filled);
    let truth = true_frames(&synthetic);
    let error_inside = |map: &Image<f32>| {
        let mut sum = 0.0;
        for y in square.y..square.y + square.height {
            for x in square.x..square.x + square.width {
                sum += f64::from((map.get(x, y, 0).unwrap() - truth[y * 320 + x]).abs());
            }
        }
        sum / (square.width * square.height) as f64
    };
    let (own, filled) = (error_inside(&free.depth), error_inside(&masked.depth));
    println!("mean error inside the mask: {own:.3} frames from the pixels' own depth, {filled:.3} filled from around");
    assert!(filled < 1.0, "{filled:.3}");
    assert!(masked.confidence.get(150, 110, 0).unwrap() > 0.0, "the confidence map is the statistics', mask or not");
    let wrong = Image::<f32>::new(160, 120, 1, ColorSpace::LINEAR).unwrap();
    assert!(matches!(stats.resolve(Threshold::Percentile(10.0), 4.0, Some(&wrong)), Err(StackError::Plane { .. })));
    for bad in [Threshold::Percentile(-1.0), Threshold::Level(f64::NAN)] {
        assert!(matches!(stats.resolve(bad, 4.0, None), Err(StackError::Setting { .. })), "{bad:?}");
    }
    assert!(matches!(stats.resolve(Threshold::Level(0.0), 300.0, None), Err(StackError::Setting { .. })));
}

#[test]
fn regions_without_detail_have_no_confidence() {
    // On the smooth right of the backdrop, nothing but the frames' noise and the out-of-focus edges of what lies in
    // front: where nothing in front crosses it, the focus curve never falls far below its peak, and has no confidence,
    // so that it is filled whatever the threshold. On the textured left, nearly every pixel has some. Measured: 22.9%
    // and 0.4%.
    let synthetic = layered(720, 540, 3.0, Backdrop::SmoothRight, 12, true, 14);
    let stats = depth(&settings_for(3.0), &Tiling::default(), &synthetic.frames).stats;
    let share = |x0: usize, x1: usize| {
        let (mut none, mut all) = (0_usize, 0_u32);
        for y in 0..540 {
            for x in x0..x1 {
                all += 1;
                none += usize::from(stats.confidence.get(x, y, 0) == Some(0.0));
            }
        }
        100.0 * none as f64 / f64::from(all)
    };
    let (smooth, textured) = (share(500, 720), share(0, 200));
    println!("no confidence: {smooth:.1}% of the smooth right, {textured:.1}% of the textured left");
    assert!(smooth > 15.0 && textured < 2.0, "{smooth:.1}% and {textured:.1}%");
    let without = stats.confidence.as_raw().iter().filter(|&&c| c <= 0.0).count();
    let map = stats.resolve(Threshold::Percentile(0.0), 8.0, None).unwrap();
    assert!(map.filled >= without, "the pixels without confidence are filled at any threshold: {} of {without}", map.filled);
    // Frames alike have no peak at all.
    let flat = vec![Image::from_fn(32, 32, 3, ColorSpace::LINEAR, |x, y, _| ((x + y) % 4) as f32 * 0.1).unwrap(); 3];
    let stats = depth(&DepthSettings::default(), &Tiling::default(), &flat).stats;
    assert!(stats.confidence.as_raw().iter().all(|&c| c == 0.0));
}

#[test]
fn the_automatic_threshold_fills_what_follows_noise_and_keeps_texture() {
    // A textured square in front of a smooth backdrop that covers three quarters of the canvas, with sensor noise, as a
    // macro subject on an out-of-focus background: the backdrop's depth follows noise, and the automatic threshold
    // fills it, where the default percentile alone would fill 10%.
    let inside = |x: usize, y: usize| (80..240).contains(&x) && (60..180).contains(&y);
    let sharp =
        Image::from_fn(
            320,
            240,
            3,
            ColorSpace::LINEAR,
            |x, y, c| if inside(x, y) { 0.2 + 0.4 * ((x / 3 + y / 3) % 2) as f32 } else { 0.4 + 0.02 * c as f32 },
        )
        .unwrap();
    let depth_map = Image::from_fn(320, 240, 1, ColorSpace::LINEAR, |x, y, _| if inside(x, y) { 0.4 } else { 1.0 }).unwrap();
    let synthetic = generate(&sharp, &depth_map, &params(12, 1.0, true, 15)).unwrap();
    let settings = settings_for(1.0);
    let stats = depth(&settings, &Tiling::default(), &synthetic.frames).stats;
    let auto = stats.resolve(Threshold::Auto, settings.smoothing, None).unwrap();
    let filled = 100.0 * auto.filled as f64 / (320.0 * 240.0);
    println!("backdrop of noise: level {:.6}, {filled:.1}% filled", auto.level);
    assert!(filled > 70.0, "{filled:.1}%");
    assert!(auto.level > stats.level(Threshold::Percentile(10.0)));
    // The square keeps its depth: the frame focused at 0.4.
    let error = (auto.depth.get(160, 120, 0).unwrap() - 0.4 * 11.0).abs();
    assert!(error < 0.5, "{error}");

    // With texture everywhere, the classes below and above Otsu's level are both coherent: the percentile.
    let synthetic = layered(720, 540, 3.0, Backdrop::Textured, 12, true, 16);
    let stats = depth(&settings_for(3.0), &Tiling::default(), &synthetic.frames).stats;
    assert_eq!(stats.level(Threshold::Auto), stats.level(Threshold::Percentile(10.0)));
}

#[test]
fn the_maps_are_16_bit_with_the_frame_index_scaled() {
    let synthetic = sloped(160, 120, 1.0, 6, false, 7);
    let result = depth(&settings_for(1.0), &Tiling::default(), &synthetic.frames);
    let depth16 = result.map.depth_u16().unwrap();
    let confidence16 = result.map.confidence_u16().unwrap();
    assert_eq!((depth16.width(), depth16.height(), depth16.channels()), (160, 120, 1));
    for (&d, &q) in result.map.depth.as_raw().iter().zip(depth16.as_raw()) {
        assert!((f32::from(q) / 65535.0 * 5.0 - d).abs() <= 5.0 / 65535.0, "{d} as {q}");
    }
    let (low, high) = (depth16.as_raw().iter().min().copied().unwrap(), depth16.as_raw().iter().max().copied().unwrap());
    assert!(low < 8000 && high > 57000, "the slope spans the frames: {low} to {high}");
    assert!(confidence16.as_raw().iter().zip(result.map.confidence.as_raw()).all(|(&q, &c)| (f32::from(q) / 65535.0 - c).abs() <= 1.0 / 65535.0));
    let near_threshold = result.map.confidence.as_raw().iter().filter(|&&c| (c - 0.5).abs() < 0.01).count();
    assert!(near_threshold > 0, "0.5 at the threshold");
}

/// A frame's coverage that ramps from 0 at `inset` pixels inside the canvas's left and top edges to 1 two pixels in.
fn inset_coverage(width: usize, height: usize, inset: f32) -> Image<f32> {
    Image::from_fn(width, height, 1, ColorSpace::LINEAR, |x, y, _| f32::midpoint(x.min(y) as f32 - inset, 0.5).clamp(0.0, 1.0)).unwrap()
}

#[test]
fn results_are_the_same_whatever_the_threads_the_tiles_and_the_spilling() {
    // An odd-sized canvas, frames with and without coverage, and radii whose blurs take the exact kernel (box passes
    // beyond 4 pixels round their running sums differently from tile to tile).
    let synthetic = layered(701, 503, 1.0, Backdrop::Textured, 4, true, 8);
    let covered: Vec<CoveredFrame> = synthetic
        .frames
        .iter()
        .enumerate()
        .map(|(i, f)| CoveredFrame::new(f.clone(), inset_coverage(701, 503, 2.0 + 3.0 * i as f32)).unwrap())
        .collect();
    let dir = std::env::temp_dir().join(format!("florastack-depth-spill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let other = DepthSettings { radius: 4.0, smoothing: 0.0, ..DepthSettings::default() }.with_threshold(Threshold::Level(0.002));
    for settings in [DepthSettings::default(), other] {
        let one_tile = Tiling { tile_size: 4096, ..Tiling::default() };
        let small = Tiling { tile_size: 64, ..Tiling::default() };
        let threads = |n: usize, tiling: &Tiling| {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(n).build().unwrap();
            pool.install(|| (depth(&settings, tiling, &synthetic.frames), depth(&settings, tiling, &covered)))
        };
        let whole = threads(4, &one_tile);
        let tiled = threads(3, &small);
        assert_eq!(whole, tiled, "{settings:?}: tiles change nothing, bit for bit");
        assert_eq!(threads(1, &small), tiled, "{settings:?}: nor threads");
        let spilling = Tiling { tile_size: 64, budget: 1 << 20, spill_dir: Some(dir.clone()) };
        assert_eq!(depth(&settings, &spilling, &covered), whole.1, "{settings:?}: nor spilling to disk");
        assert!(whole.1.stacked.alpha.is_some(), "the inset frames leave the corner uncovered");
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
    let synthetic = layered(240, 120, 1.0, Backdrop::Textured, 3, false, 9);
    let ramp =
        |edge: usize| Image::from_fn(240, 120, 1, ColorSpace::LINEAR, |x, _, _| ((edge as f32 - x as f32 - 0.5) / 2.0).clamp(0.0, 1.0)).unwrap();
    // The first and the last frame stop at column 150, the middle one at 200.
    let frames = [
        CoveredFrame::new(streaked(&synthetic.frames[0], 150), ramp(150)).unwrap(),
        CoveredFrame::new(streaked(&synthetic.frames[1], 200), ramp(200)).unwrap(),
        CoveredFrame::new(streaked(&synthetic.frames[2], 150), ramp(150)).unwrap(),
    ];
    for tiling in [Tiling::default(), Tiling { tile_size: 64, ..Tiling::default() }] {
        let result = depth(&settings_for(1.0), &tiling, &frames);
        let out = &result.stacked;
        let alpha = out.alpha.as_ref().expect("an alpha channel: some pixels are covered by no frame");
        assert_eq!(out.uncovered, 40 * 120, "columns 200 to 239");
        assert_eq!((result.stats.uncovered, result.map.depth.get(230, 60, 0)), (40 * 120, Some(0.0)));
        for y in 0..120 {
            for x in 0..240 {
                assert_eq!(alpha.get(x, y, 0), Some(if x < 200 { 1.0 } else { 0.0 }), "alpha at {x},{y}");
            }
            assert_eq!(out.image.pixel(230, y), Some(&[0.0, 0.0, 0.0][..]));
        }
        // Where only the middle frame covers, the composite is that frame, whatever depth the smoothing gives there.
        let only_middle = Rect::new(156, 4, 40, 112);
        let error = psnr(&out.image.crop(only_middle).unwrap(), &synthetic.frames[1].crop(only_middle).unwrap()).unwrap();
        assert!(error > 60.0, "{error:.1} dB");
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
fn progress_cancellation_and_frames_that_dont_fit() {
    let synthetic = layered(400, 300, 1.0, Backdrop::Textured, 3, false, 10);
    let tiling = Tiling { tile_size: 64, ..Tiling::default() };
    let settings = settings_for(1.0);
    let all = Recording::new(u64::MAX);
    depth_stack(&settings, &tiling, &synthetic.frames, None, &all).unwrap();
    let seen = all.seen.lock().unwrap().clone();
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0 && w[0].1 == w[1].1), "both passes, never going back: {seen:?}");
    assert_eq!(seen.last().map(|&(d, t)| d == t), Some(true));
    // Cancelled in pass 1, and in pass 2.
    for limit in [2, seen.len() as u64 * 3 / 4] {
        let cancelling = Recording::new(limit);
        assert!(matches!(depth_stack(&settings, &tiling, &synthetic.frames, None, &cancelling), Err(StackError::Cancelled(_))), "{limit}");
    }

    // Pass 2 composites the frames pass 1 measured: no more, no fewer, of the same shape.
    let map = depth(&settings, &tiling, &synthetic.frames).map;
    let shape = StackShape::of(&synthetic.frames[0]);
    let mut compositor = map.compositor(&tiling).unwrap();
    compositor.begin(shape).unwrap();
    compositor.add_frame(&synthetic.frames[0], &Unmonitored).unwrap();
    assert!(matches!(compositor.finish(&Unmonitored), Err(StackError::FrameCount { expected: 3, got: 1 })));
    compositor.begin(shape).unwrap();
    for frame in &synthetic.frames {
        compositor.add_frame(frame, &Unmonitored).unwrap();
    }
    assert!(matches!(compositor.add_frame(&synthetic.frames[0], &Unmonitored), Err(StackError::FrameCount { expected: 3, got: 4 })));
    compositor.begin(shape).unwrap();
    let gray = Image::<f32>::new(400, 300, 1, ColorSpace::LINEAR).unwrap();
    assert!(matches!(compositor.add_frame(&gray, &Unmonitored), Err(StackError::FrameShape { .. })));
    assert!(matches!(compositor.begin(StackShape { width: 200, height: 300, channels: 3 }), Err(StackError::Plane { .. })));
    let mut estimator = settings.estimator(&tiling).unwrap();
    assert!(matches!(estimator.finish(&Unmonitored), Err(StackError::NotStarted)));
    estimator.begin(shape).unwrap();
    assert!(matches!(estimator.finish(&Unmonitored), Err(StackError::NoFrames)));
    let empty: [Image<f32>; 0] = [];
    assert!(matches!(depth_stack(&settings, &tiling, &empty, None, &Unmonitored), Err(StackError::NoFrames)));
}

#[test]
fn hostile_samples_and_tiny_canvases_give_finite_stacks() {
    // Regression: samples that aren't finite, or are huge, must neither poison the statistics nor the composite.
    let mut frames = vec![layered(64, 48, 1.0, Backdrop::Textured, 1, false, 11).frames[0].clone(); 3];
    frames[0].pixel_mut(10, 10).unwrap().copy_from_slice(&[f32::NAN, f32::INFINITY, 1.0]);
    frames[1].pixel_mut(10, 10).unwrap().copy_from_slice(&[1e38, -1e38, f32::MAX]);
    frames[2].pixel_mut(30, 20).unwrap().copy_from_slice(&[f32::NEG_INFINITY; 3]);
    let result = depth(&settings_for(1.0), &Tiling::default(), &frames);
    assert!(result.stacked.image.as_raw().iter().all(|v| v.is_finite()));
    assert!(result.map.depth.as_raw().iter().chain(result.map.confidence.as_raw()).all(|v| v.is_finite()));
    let nothing = vec![Image::from_fn(16, 16, 3, ColorSpace::LINEAR, |_, _, _| f32::NAN).unwrap(); 2];
    let result = depth(&settings_for(1.0), &Tiling::default(), &nothing);
    assert_eq!((result.stacked.uncovered, result.map.filled), (256, 0));
    assert!(result.stacked.image.as_raw().iter().all(|&v| v == 0.0));
    // Canvases smaller than the filters, and a single frame, which is its own stack at depth 0.
    for (w, h) in [(1, 1), (3, 2), (17, 1), (9, 40)] {
        let frame = Image::from_fn(w, h, 2, ColorSpace::LINEAR, |x, y, c| ((x + y + c) % 3) as f32 * 0.3).unwrap();
        for frames in [vec![frame.clone(); 2], vec![frame.clone()]] {
            let result = depth(&DepthSettings::default(), &Tiling::default(), &frames);
            assert_eq!((result.stacked.image.width(), result.stacked.image.height()), (w, h));
            let err = result.stacked.image.as_raw().iter().zip(frame.as_raw()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(err < 1e-6, "{w}x{h}, {} frames: {err}", frames.len());
        }
    }
    let single = depth(&DepthSettings::default(), &Tiling::default(), &[layered(64, 48, 1.0, Backdrop::Textured, 1, false, 12).frames[0].clone()]);
    assert!(single.map.depth.as_raw().iter().all(|&d| d == 0.0));
    assert!(single.map.depth_u16().unwrap().as_raw().iter().all(|&d| d == 0));
}

#[test]
fn the_quarter_scale_preview_looks_like_the_full_stack() {
    let synthetic = layered(720, 540, 3.0, Backdrop::Textured, 12, false, 13);
    let settings = DepthSettings::default();
    let reduced: Vec<Image<f32>> = synthetic.frames.iter().map(|f| reduce(f).unwrap()).collect();
    let small = depth(&settings.reduced(4.0), &Tiling::default(), &reduced).stacked;
    assert_eq!((small.image.width(), small.image.height()), (180, 135));
    let truth = reduce(&synthetic.truth.sharp).unwrap();
    let best = reduced.iter().map(|f| ssim(f, &truth).unwrap()).fold(0.0, f64::max);
    let score = ssim(&small.image, &truth).unwrap();
    let full = reduce(&depth(&settings, &Tiling::default(), &synthetic.frames).stacked.image).unwrap();
    let likeness = psnr(&small.image, &full).unwrap();
    println!("preview: SSIM {score:.4} (best reduced frame {best:.4}), {likeness:.1} dB from the full stack reduced");
    assert!(score > best + 0.02, "{score:.4} against {best:.4}");
    assert!(likeness > 28.0, "{likeness:.1} dB");
}

#[test]
fn settings_round_trip_as_project_files_and_presets_store_them() {
    let settings = DepthSettings { radius: 1.5, smoothing: 12.0, ..DepthSettings::default() }.with_threshold(Threshold::Level(0.0025));
    let json = serde_json::to_value(&settings).unwrap();
    assert_eq!(json, serde_json::json!({ "radius": 1.5, "smoothing": 12.0, "threshold": "level", "level": 0.0025, "percentile": 30.0 }));
    assert_eq!(serde_json::from_value::<DepthSettings>(json).unwrap(), settings);
    let toml = toml::to_string(&settings).unwrap();
    assert_eq!(toml::from_str::<DepthSettings>(&toml).unwrap(), settings);
    assert_eq!(StackSettings::default().depth_map, DepthSettings::default());
    let preset: StackSettings = toml::from_str("[depth_map]\nthreshold = \"percentile\"\npercentile = 45.0\n").unwrap();
    assert_eq!(preset.depth_map.contrast_threshold(), Threshold::Percentile(45.0));
    assert!(serde_json::from_value::<StackSettings>(serde_json::json!({ "depth_map": { "threshold": "percent" } })).is_err());
}
