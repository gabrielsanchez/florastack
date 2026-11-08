//! The wavelet method: its transform on its own (round trips, shift invariance), the whole method with nothing to fuse,
//! its fusion rules on synthetic stacks against the pyramid, and the framework under it: tiles, threads, spilling,
//! coverage, progress, cancellation, hostile samples and tiny canvases.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicU64, Ordering};

use florastack_core::ops::{gaussian_blur, laplacian};
use florastack_core::testutil::{Backdrop, StackParams, SyntheticStack, generate_layered, scene};
use florastack_core::{ColorSpace, Image, Monitor, Rect, Unmonitored};
use florastack_metrics::{HaloParams, NoiseParams, color_shift, halo, noise, psnr, ssim};
use florastack_stack::{
    CoveredFrame, FrameSource, Lowpass, Method, PyramidSettings, StackError, Stacked, Stacker, Tiling, Tone, WAVELET, Wavelet, WaveletSettings,
    compress_range, decompose, out_of_range, stack, wavelet_stack,
};

const WAVELETS: [Wavelet; 2] = [Wavelet::Bior22, Wavelet::Db2];

/// Reproducible samples from 0 to 1.
fn noise_image(width: usize, height: usize, channels: usize, seed: u64) -> Image<f32> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    Image::from_fn(width, height, channels, ColorSpace::LINEAR, |_, _, _| {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((state >> 40) as f32) / ((1u64 << 24) as f32)
    })
    .unwrap()
}

fn max_error(a: &Image<f32>, b: &Image<f32>) -> f32 {
    assert_eq!((a.width(), a.height(), a.channels()), (b.width(), b.height(), b.channels()));
    a.as_raw().iter().zip(b.as_raw()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max)
}

/// A stack as `crates/stack/tests/pyramid.rs` makes them, of a layered scene whose layers occlude one another.
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

fn run(settings: &WaveletSettings, tiling: &Tiling, frames: &[impl FrameSource]) -> Stacked {
    wavelet_stack(settings, tiling, frames, &Unmonitored).unwrap()
}

/// The output as the engine makes it: the stack, brought into range by its tone setting.
fn wavelet(settings: &WaveletSettings, frames: &[impl FrameSource]) -> Image<f32> {
    let mut image = run(settings, &Tiling::default(), frames).image;
    compress_range(&mut image, settings.tone).unwrap();
    image
}

fn pyramid(frames: &[impl FrameSource]) -> Image<f32> {
    let settings = PyramidSettings::default();
    let mut stacker = settings.stacker(&Tiling::default()).unwrap();
    let mut image = stack(&mut stacker, frames, &Unmonitored).unwrap().image;
    compress_range(&mut image, settings.tone).unwrap();
    image
}

/// The variance of `image`'s Laplacian, on its luminance's first channel: how much fine detail it holds.
fn sharpness(image: &Image<f32>) -> f64 {
    let lap = laplacian(image).unwrap();
    let n = lap.as_raw().len() as f64;
    let mean = lap.as_raw().iter().map(|&v| f64::from(v)).sum::<f64>() / n;
    lap.as_raw().iter().map(|&v| (f64::from(v) - mean).powi(2)).sum::<f64>() / n
}

#[test]
fn the_transform_reconstructs_random_data_on_any_size() {
    // Non-powers of 2, primes, one-pixel sides, and planes smaller than the filters' reach. The tolerance: samples are
    // about 1, and each of up to 7 levels sums a few dozen `f32` products there and back, each rounded by 6e-8 of its size.
    for wavelet in WAVELETS {
        for (w, h) in [(64, 64), (101, 67), (97, 128), (33, 1), (1, 29), (1, 1), (2, 3), (5, 5)] {
            for levels in [1, 2, 3, 5, 7] {
                let plane = noise_image(w, h, 1, (w * h + levels) as u64);
                let d = decompose(&plane, wavelet, levels).unwrap();
                assert_eq!(d.details.len(), levels);
                let error = max_error(&d.reconstruct().unwrap(), &plane);
                assert!(error < 2e-5, "{wavelet:?}, {w}x{h}, {levels} levels: {error}");
            }
        }
    }
}

#[test]
fn the_transform_reconstructs_edges_as_exactly_as_the_middle() {
    // A step right at the edge, where a wrong boundary rule shows.
    for wavelet in WAVELETS {
        let plane = Image::from_fn(45, 37, 1, ColorSpace::LINEAR, |x, y, _| if x < 2 || y > 34 { 1.0 } else { 0.0 }).unwrap();
        let error = max_error(&decompose(&plane, wavelet, 4).unwrap().reconstruct().unwrap(), &plane);
        assert!(error < 1e-5, "{wavelet:?}: {error}");
    }
}

#[test]
fn a_shifted_plane_gives_shifted_coefficients() {
    // What a decimated transform can't do, and why this one has no blocks at edges. On a periodic pattern, so that the
    // shifted plane's edges are the same pattern, compared well inside the plane.
    for wavelet in WAVELETS {
        let at = |x: usize, y: usize| ((x as f32 * 0.7).sin() + (y as f32 * 0.31).cos() + ((x + 2 * y) as f32 * 0.17).sin()) * 0.25 + 0.5;
        let plane = Image::from_fn(160, 160, 1, ColorSpace::LINEAR, |x, y, _| at(x, y)).unwrap();
        let shifted = Image::from_fn(160, 160, 1, ColorSpace::LINEAR, |x, y, _| at(x + 3, y + 5)).unwrap();
        let (a, b) = (decompose(&plane, wavelet, 3).unwrap(), decompose(&shifted, wavelet, 3).unwrap());
        let by = a.extended;
        for (k, (a, b)) in a.details.iter().zip(&b.details).enumerate() {
            for (name, p, q) in
                [("horizontal", &a.horizontal, &b.horizontal), ("vertical", &a.vertical, &b.vertical), ("diagonal", &a.diagonal, &b.diagonal)]
            {
                let (p, q) = (p.crop(Rect::new(by + 43, by + 45, 80, 80)).unwrap(), q.crop(Rect::new(by + 40, by + 40, 80, 80)).unwrap());
                let error = max_error(&p, &q);
                assert!(error < 1e-5, "{wavelet:?}, level {k}, {name}: {error}");
            }
        }
    }
}

#[test]
fn one_frame_comes_back_through_the_whole_method() {
    // Decompose and reconstruct with nothing to fuse, through tiles, the low-pass split and the whole-canvas levels.
    // Odd sizes, every level count, both banks, gray and color; with the range limit off, so that nothing hides an error.
    // Blending the finest level is left to `bior22`: its energies mirror as the plane does, so a frame's share is 1 up to
    // the extended margin too.
    for (w, h, c) in [(301, 203, 3), (129, 257, 1), (64, 64, 3), (37, 11, 2)] {
        let frame = noise_image(w, h, c, 11);
        for levels in [0, 1, 2, 3, 4, 6] {
            for (wavelet, grit) in [(Wavelet::Bior22, 0.5), (Wavelet::Bior22, 0.0), (Wavelet::Db2, 0.0)] {
                let settings = WaveletSettings { levels, wavelet, grit, range_limit: 0.0, ..WaveletSettings::default() };
                let out = run(&settings, &Tiling::default(), std::slice::from_ref(&frame));
                let error = max_error(&out.image, &frame);
                assert!(error < 2e-5, "{w}x{h}x{c}, {levels} levels, {wavelet:?}, grit {grit}: {error}");
            }
        }
    }
}

/// `sharp` blurred on one side of its middle column, and left alone on the other.
fn half_blurred(sharp: &Image<f32>, blurred: &Image<f32>, left: bool) -> Image<f32> {
    let middle = sharp.width() / 2;
    Image::from_fn(sharp.width(), sharp.height(), sharp.channels(), ColorSpace::LINEAR, |x, y, c| {
        let source = if (x < middle) == left { blurred } else { sharp };
        source.get(x, y, c).unwrap()
    })
    .unwrap()
}

#[test]
fn two_frames_blurred_on_different_halves_fuse_into_the_sharp_image() {
    let (sharp, _) = scene::textured(480, 320, 3).unwrap();
    let blurred = gaussian_blur(&sharp, 3.0).unwrap();
    let frames = [half_blurred(&sharp, &blurred, true), half_blurred(&sharp, &blurred, false)];
    let fused = wavelet(&WaveletSettings::default(), &frames);
    let reference = pyramid(&frames);
    let (a, b) = (sharpness(&frames[0]), sharpness(&frames[1]));
    let (s, p, truth) = (sharpness(&fused), sharpness(&reference), sharpness(&sharp));
    let (fused_psnr, pyramid_psnr) = (psnr(&fused, &sharp).unwrap(), psnr(&reference, &sharp).unwrap());
    let (fused_ssim, pyramid_ssim) = (ssim(&fused, &sharp).unwrap(), ssim(&reference, &sharp).unwrap());
    println!("Laplacian variance: frames {a:.6} and {b:.6}, wavelet {s:.6}, pyramid {p:.6}, the sharp image {truth:.6}");
    println!("wavelet: PSNR {fused_psnr:.2} dB, SSIM {fused_ssim:.4}; pyramid: PSNR {pyramid_psnr:.2} dB, SSIM {pyramid_ssim:.4}");
    assert!(s > a.max(b), "sharper than either frame");
    assert!(s > 0.9 * truth, "holds about the sharp image's detail");
    assert!(fused_ssim > 0.98, "close to the sharp image: {fused_ssim}");
    assert!(fused_psnr > 35.0, "{fused_psnr}");
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
        "  {name:<26} SSIM {:.4}  halo {:.4}  noise {:.5}  dE {:.3}  PSNR {:.2} dB  out of range {:.3}%",
        s.ssim,
        s.halo,
        s.noise,
        s.delta_e,
        s.psnr,
        100.0 * s.out_of_range
    );
}

#[test]
fn the_method_stacks_overlapping_thin_structures_near_the_pyramid() {
    // The pyramid's own test stacks. Printed for the record, with each rule's setting beside the defaults; the floors are
    // well below the pyramid's scores, which `tests/pyramid.rs` holds: this checks the method works, not that it wins.
    for (w, h, scale, backdrop) in [(720, 540, 3.0, Backdrop::Textured), (320, 240, 1.0, Backdrop::Textured), (720, 540, 3.0, Backdrop::SmoothRight)]
    {
        for noisy in [false, true] {
            let synthetic = layered(w, h, scale, backdrop, 12, noisy, 1);
            let truth = &synthetic.truth.sharp;
            println!("{w}x{h} {backdrop:?}, noisy {noisy}");
            let pyr = scores(&pyramid(&synthetic.frames), truth);
            show("pyramid", &pyr);
            let defaults = WaveletSettings::default();
            let ours = scores(&wavelet(&defaults, &synthetic.frames), truth);
            show("wavelet", &ours);
            for (name, settings) in [
                ("wavelet, db2", WaveletSettings { wavelet: Wavelet::Db2, ..defaults.clone() }),
                ("wavelet, grit 0", WaveletSettings { grit: 0.0, ..defaults.clone() }),
                ("wavelet, no consistency", WaveletSettings { consistency: false, ..defaults.clone() }),
                ("wavelet, no range limit", WaveletSettings { range_limit: 0.0, ..defaults.clone() }),
                ("wavelet, window 5", WaveletSettings { window: 5, ..defaults.clone() }),
                ("wavelet, select low-pass", WaveletSettings { lowpass: Lowpass::Select, ..defaults.clone() }),
                ("wavelet, average low-pass", WaveletSettings { lowpass: Lowpass::Average, ..defaults.clone() }),
                ("wavelet, no noise floor", WaveletSettings { noise_floor: 0.0, ..defaults.clone() }),
                ("wavelet, noise floor 1%", WaveletSettings { noise_floor: 1.0, ..defaults.clone() }),
            ] {
                show(name, &scores(&wavelet(&settings, &synthetic.frames), truth));
            }
            assert!(ours.ssim > pyr.ssim - 0.05, "SSIM {} against the pyramid's {}", ours.ssim, pyr.ssim);
            assert!(ours.halo < pyr.halo * 1.25, "halo {} against the pyramid's {}", ours.halo, pyr.halo);
        }
    }
}

#[test]
fn the_noise_floor_lowers_noise_and_keeps_the_detail() {
    // The noisy stacks' noise (read 0.5%, shot 1% in linear light) is about 0.65% of white in gamma-encoded values.
    for (w, h, scale, backdrop) in [(720, 540, 3.0, Backdrop::Textured), (320, 240, 1.0, Backdrop::Textured), (720, 540, 3.0, Backdrop::SmoothRight)]
    {
        let synthetic = layered(w, h, scale, backdrop, 12, true, 1);
        let truth = &synthetic.truth.sharp;
        let off = WaveletSettings { noise_floor: 0.0, ..WaveletSettings::default() };
        let (without, with) =
            (scores(&wavelet(&off, &synthetic.frames), truth), scores(&wavelet(&WaveletSettings::default(), &synthetic.frames), truth));
        println!("{w}x{h} {backdrop:?}");
        show("no noise floor", &without);
        show("noise floor 0.4%", &with);
        assert!(with.noise < without.noise, "noise {} against {} without the floor", with.noise, without.noise);
        assert!(with.ssim > without.ssim - 0.002, "SSIM {} against {} without the floor", with.ssim, without.ssim);
    }
    // With grit 0 the finest level picks, and the floor changes nothing.
    let synthetic = layered(160, 120, 1.0, Backdrop::Textured, 6, true, 2);
    let picking = WaveletSettings { grit: 0.0, ..WaveletSettings::default() };
    let unfloored = WaveletSettings { noise_floor: 0.0, ..picking.clone() };
    assert_eq!(wavelet(&picking, &synthetic.frames).as_raw(), wavelet(&unfloored, &synthetic.frames).as_raw());
}

#[test]
fn a_dark_object_only_the_sharp_frames_show_stays_dark() {
    // On real stacks a ladybug's black spot came out at 33 of 255 and a wasp's antenna at 62, about their frames' mean,
    // where the frames in focus have 15 and 7: an average low-pass lifted what only the sharp frames show dark. Here, a
    // dark disc that blurs away frame by frame, as a focus series does. It must be wide against the coarsest levels: a
    // small one's contrast is all in the details, which any low-pass leaves dark.
    let size = 1024;
    let disc = Image::from_fn(size, size, 1, ColorSpace::LINEAR, |x, y, _| {
        let (dx, dy) = (x as f64 - 512.0, y as f64 - 512.0);
        if dx.hypot(dy) < 64.0 { 0.01 } else { 0.6 }
    })
    .unwrap();
    let mut frames = vec![disc.clone()];
    frames.extend((1..8).map(|i| gaussian_blur(&disc, 16.0 * f64::from(i)).unwrap()));
    let center = |lowpass| wavelet(&WaveletSettings { lowpass, ..WaveletSettings::default() }, &frames).get(512, 512, 0).unwrap();
    let (focus, average) = (center(Lowpass::Focus), center(Lowpass::Average));
    assert!(focus < 0.02, "the disc's center is {focus}, 0.01 in the sharp frame");
    assert!(average > 3.0 * focus, "the scene no longer shows what an average does: {average} against {focus}");
}

#[test]
fn gray_and_color_8_and_16_bit_frames_and_two_frames_stack() {
    // Frames reach a stacker as linear `f32` whatever their files held: the engine's readers convert them, and export
    // converts back. Here, frames quantized to each depth, the result brought into range and quantized the same way.
    let (sharp, _) = scene::textured(240, 180, 5).unwrap();
    let blurred = gaussian_blur(&sharp, 3.0).unwrap();
    for channels in [1, 3] {
        let select = |image: &Image<f32>| {
            Image::from_fn(240, 180, channels, ColorSpace::LINEAR, |x, y, c| image.get(x, y, if channels == 1 { 1 } else { c }).unwrap()).unwrap()
        };
        let (truth, soft) = (select(&sharp), select(&blurred));
        for levels in [255.0_f32, 65535.0] {
            let quantize = |image: Image<f32>| {
                let mut image = image;
                image.par_rows_mut_for_test(|v| *v = (*v * levels).round() / levels);
                image
            };
            let frames = [quantize(half_blurred(&truth, &soft, true)), quantize(half_blurred(&truth, &soft, false))];
            let settings = WaveletSettings { tone: Tone::Clip, ..WaveletSettings::default() };
            let out = quantize(wavelet(&settings, &frames));
            assert_eq!(out.channels(), channels);
            assert!(out.as_raw().iter().all(|v| (0.0..=1.0).contains(v)), "{channels} channels in range");
            let error = psnr(&out, &truth).unwrap();
            assert!(error > 35.0, "{channels} channels at {levels}: {error:.1} dB");
        }
    }
}

/// Runs `f` on every sample of an image: the test's stand-in for an in-place map.
trait MapSamples {
    fn par_rows_mut_for_test(&mut self, f: impl Fn(&mut f32) + Sync);
}

impl MapSamples for Image<f32> {
    fn par_rows_mut_for_test(&mut self, f: impl Fn(&mut f32) + Sync) {
        use rayon::prelude::*;
        self.par_rows_mut().for_each(|row| row.iter_mut().for_each(&f));
    }
}

#[test]
fn colors_stay_together() {
    // One frame red in focus, the other green in focus at the same place: a choice per channel would mix them into a
    // color neither frame has. Luminance decides once, for all channels.
    let texture = noise_image(96, 96, 1, 21);
    let soft = gaussian_blur(&texture, 4.0).unwrap();
    let frames: Vec<Image<f32>> = [(&texture, &soft), (&soft, &texture)]
        .iter()
        .map(|(r, g)| {
            Image::from_fn(96, 96, 3, ColorSpace::LINEAR, |x, y, c| match c {
                0 => r.get(x, y, 0).unwrap(),
                1 => g.get(x, y, 0).unwrap(),
                _ => 0.25,
            })
            .unwrap()
        })
        .collect();
    let settings = WaveletSettings { grit: 0.0, consistency: false, range_limit: 0.0, ..WaveletSettings::default() };
    let out = run(&settings, &Tiling::default(), &frames);
    // Red and green can't both be the sharp ones: the green channel, weighted most in luminance, leads every choice.
    assert!(sharpness(&plane(&out.image, 1)) > 1.5 * sharpness(&plane(&out.image, 0)));
}

fn plane(image: &Image<f32>, channel: usize) -> Image<f32> {
    Image::from_fn(image.width(), image.height(), 1, ColorSpace::LINEAR, |x, y, _| image.get(x, y, channel).unwrap()).unwrap()
}

/// A frame's coverage that ramps from 0 at `inset` pixels inside the canvas's left and top edges to 1 two pixels in.
fn inset_coverage(width: usize, height: usize, inset: f32) -> Image<f32> {
    Image::from_fn(width, height, 1, ColorSpace::LINEAR, |x, y, _| f32::midpoint(x.min(y) as f32 - inset, 0.5).clamp(0.0, 1.0)).unwrap()
}

#[test]
fn results_are_the_same_whatever_the_threads_the_tiles_and_the_spilling() {
    // An odd-sized canvas, so that tiles start off the lattice, large enough for several of the method's tiles, frames
    // with and without coverage, and every setting that changes the state's layout or a filter's reach.
    let (w, h) = (1501, 1303);
    let synthetic = layered(w, h, 1.0, Backdrop::Textured, 3, true, 7);
    let covered: Vec<CoveredFrame> =
        synthetic.frames.iter().enumerate().map(|(i, f)| CoveredFrame::new(f.clone(), inset_coverage(w, h, 2.0 + 3.0 * i as f32)).unwrap()).collect();
    let dir = std::env::temp_dir().join(format!("florastack-wavelet-spill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for settings in [
        WaveletSettings::default(),
        WaveletSettings { wavelet: Wavelet::Db2, grit: 0.0, window: 3, lowpass: Lowpass::Select, range_limit: 0.7, ..WaveletSettings::default() },
        WaveletSettings { levels: 2, range_limit: 0.0, consistency: false, ..WaveletSettings::default() },
    ] {
        let one_tile = Tiling { tile_size: 4096, ..Tiling::default() };
        let small = Tiling { tile_size: 64, ..Tiling::default() };
        let threads = |n: usize, tiling: &Tiling| {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(n).build().unwrap();
            pool.install(|| (run(&settings, tiling, &synthetic.frames), run(&settings, tiling, &covered)))
        };
        let whole = threads(4, &one_tile);
        let tiled = threads(3, &small);
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
    let out = run(&WaveletSettings::default(), &Tiling::default(), &frames);
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

/// A monitor that counts progress and cancels after `limit` reports.
struct Counting {
    limit: u64,
    steps: AtomicU64,
}

impl Monitor for Counting {
    fn is_cancelled(&self) -> bool {
        self.steps.load(Ordering::Relaxed) >= self.limit
    }

    fn progress(&self, _: u64, _: u64) {
        self.steps.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn progress_and_cancellation_cover_both_passes() {
    let synthetic = layered(400, 300, 1.0, Backdrop::Textured, 3, false, 9);
    let settings = WaveletSettings::default();
    let all = Counting { limit: u64::MAX, steps: AtomicU64::new(0) };
    wavelet_stack(&settings, &Tiling::default(), &synthetic.frames, &all).unwrap();
    let steps = all.steps.load(Ordering::Relaxed);
    assert!(steps >= 6, "each frame reports in each pass: {steps}");
    for limit in [1, steps / 2 + 1] {
        let cancelling = Counting { limit, steps: AtomicU64::new(0) };
        let result = wavelet_stack(&settings, &Tiling::default(), &synthetic.frames, &cancelling);
        assert!(matches!(result, Err(StackError::Cancelled(_))), "after {limit} of {steps} reports");
    }
    assert!(matches!(wavelet_stack::<Image<f32>>(&settings, &Tiling::default(), &[], &Unmonitored), Err(StackError::NoFrames)));
}

#[test]
fn pass_two_needs_the_frames_pass_one_saw() {
    let frames = layered(96, 80, 1.0, Backdrop::Textured, 3, false, 12).frames;
    let shape = florastack_stack::StackShape::of(&frames[0]);
    let tiling = Tiling::default();
    let mut selector = florastack_stack::Selector::new(&WaveletSettings::default(), &tiling).unwrap();
    selector.begin(shape).unwrap();
    for frame in &frames {
        selector.add_frame(frame, &Unmonitored).unwrap();
    }
    let choices = std::sync::Arc::new(selector.finish(&Unmonitored).unwrap());
    assert_eq!(choices.frames(), 3);
    let mut compositor = choices.clone().compositor(&tiling).unwrap();
    compositor.begin(shape).unwrap();
    compositor.add_frame(&frames[0], &Unmonitored).unwrap();
    assert!(matches!(compositor.finish(&Unmonitored), Err(StackError::FrameCount { expected: 3, got: 1 })), "too few");
    compositor.begin(shape).unwrap();
    for frame in &frames {
        compositor.add_frame(frame, &Unmonitored).unwrap();
    }
    assert!(matches!(compositor.add_frame(&frames[0], &Unmonitored), Err(StackError::FrameCount { expected: 3, got: 4 })), "too many");
    let mut other = choices.compositor(&tiling).unwrap();
    assert!(other.begin(florastack_stack::StackShape { width: 50, ..shape }).is_err(), "another canvas");
    assert!(other.begin(florastack_stack::StackShape { channels: 1, ..shape }).is_err(), "other channels");
}

#[test]
fn hostile_samples_and_tiny_canvases_give_finite_stacks() {
    let mut frames = vec![layered(64, 48, 1.0, Backdrop::Textured, 1, false, 10).frames[0].clone(); 3];
    frames[0].pixel_mut(10, 10).unwrap().copy_from_slice(&[f32::NAN, f32::INFINITY, 1.0]);
    frames[1].pixel_mut(10, 10).unwrap().copy_from_slice(&[1e38, -1e38, f32::MAX]);
    frames[2].pixel_mut(30, 20).unwrap().copy_from_slice(&[f32::NEG_INFINITY; 3]);
    for settings in
        [WaveletSettings::default(), WaveletSettings { grit: 0.0, wavelet: Wavelet::Db2, lowpass: Lowpass::Select, ..WaveletSettings::default() }]
    {
        let out = run(&settings, &Tiling::default(), &frames);
        assert!(out.image.as_raw().iter().all(|v| v.is_finite()), "{settings:?}");
    }
    let nothing = vec![Image::from_fn(16, 16, 3, ColorSpace::LINEAR, |_, _, _| f32::NAN).unwrap()];
    let out = run(&WaveletSettings::default(), &Tiling::default(), &nothing);
    assert_eq!(out.uncovered, 256);
    assert!(out.image.as_raw().iter().all(|&v| v == 0.0));
    // Canvases smaller than the levels, the filters and the tiles.
    for (w, h) in [(1, 1), (3, 2), (17, 1), (9, 40)] {
        let frames = vec![Image::from_fn(w, h, 2, ColorSpace::LINEAR, |x, y, c| ((x + y + c) % 3) as f32 * 0.3).unwrap(); 2];
        for lowpass in [Lowpass::Focus, Lowpass::Average, Lowpass::Select] {
            let settings = WaveletSettings { levels: 10, lowpass, ..WaveletSettings::default() };
            let out = run(&settings, &Tiling::default(), &frames);
            assert_eq!((out.image.width(), out.image.height()), (w, h));
            let err = max_error(&out.image, &frames[0]);
            assert!(err < 1e-5, "two identical frames stack to themselves: {w}x{h} {lowpass:?} {err}");
        }
    }
}

#[test]
fn settings_are_checked_and_scale_for_previews() {
    assert!(WaveletSettings::default().validate().is_ok());
    for bad in [
        WaveletSettings { levels: 11, ..WaveletSettings::default() },
        WaveletSettings { window: 4, ..WaveletSettings::default() },
        WaveletSettings { window: 0, ..WaveletSettings::default() },
        WaveletSettings { grit: 0.1, ..WaveletSettings::default() },
        WaveletSettings { grit: f64::NAN, ..WaveletSettings::default() },
        WaveletSettings { range_limit: -0.1, ..WaveletSettings::default() },
        WaveletSettings { range_limit: f64::INFINITY, ..WaveletSettings::default() },
        WaveletSettings { noise_floor: -0.1, ..WaveletSettings::default() },
        WaveletSettings { noise_floor: 2.5, ..WaveletSettings::default() },
        WaveletSettings { noise_floor: f64::NAN, ..WaveletSettings::default() },
    ] {
        assert!(matches!(bad.validate(), Err(StackError::Setting { .. })), "{bad:?}");
        assert!(florastack_stack::Selector::new(&bad, &Tiling::default()).is_err());
    }
    let defaults = WaveletSettings::default();
    assert_eq!((defaults.levels_for(720, 540), defaults.levels_for(5184, 3888), defaults.levels_for(1, 1)), (5, 7, 1));
    assert_eq!(WaveletSettings { levels: 9, ..defaults.clone() }.levels_for(100, 60), 5, "never more than the canvas allows");
    assert_eq!(WaveletSettings { levels: 6, ..defaults.clone() }.reduced(4.0).levels, 4);
    assert_eq!(defaults.reduced(4.0).levels, 0, "automatic levels follow the reduced canvas");
    assert!((defaults.reduced(4.0).noise_floor - 0.1).abs() < 1e-12, "a quarter-size frame averages 16 pixels: a quarter of the noise");
    assert_eq!(WaveletSettings { noise_floor: 0.0, ..defaults.clone() }.reduced(4.0).noise_floor, 0.0);
    assert_eq!(defaults.noise_floor, florastack_stack::DEFAULT_NOISE_FLOOR);
    assert_eq!(WAVELET, "wavelet");
    let partial: WaveletSettings = toml::from_str("wavelet = \"db2\"\nlowpass = \"select\"\nwindow = 3\n").unwrap();
    assert_eq!(partial, WaveletSettings { wavelet: Wavelet::Db2, lowpass: Lowpass::Select, window: 3, ..WaveletSettings::default() });
    let json = serde_json::to_value(&partial).unwrap();
    assert_eq!(serde_json::from_value::<WaveletSettings>(json).unwrap(), partial);
    assert!(serde_json::from_value::<WaveletSettings>(serde_json::json!({ "windows": 3 })).is_err());
    assert!(serde_json::from_value::<WaveletSettings>(serde_json::json!({ "wavelet": "haar" })).is_err());
}
