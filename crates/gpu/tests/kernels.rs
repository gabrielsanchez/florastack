//! Agreement of the basic kernels with their CPU references (AGENTS.md, CPU oracle): the conversions, the blur and
//! the Laplacian of florastack-core, the alignment warp of florastack-align and the pyramid's levels of
//! florastack-stack, on seeded random images of odd and narrow sizes, every channel count, values beyond [0, 1] and
//! partial coverage. Each tolerance says why it is enough next to its assertion; floats need not agree bit for bit.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{gpu, max_difference, max_difference_where, max_relative_difference, random};
use florastack_align::{Canvas, Coverage, warp_to_canvas};
use florastack_core::ops::{BlurMethod, gaussian_blur_in_place, laplacian};
use florastack_core::{Affine, ColorSpace, Encoding, Image, Rect};
use florastack_gpu::levels::{self, Span};
use florastack_gpu::ops::{self, WarpGeometry};
use florastack_stack::pyramid::levels as cpu;

/// Sizes that catch the edge cases: odd, narrow and single-row or single-column images.
const SIZES: [(usize, usize); 5] = [(37, 23), (64, 48), (1, 9), (13, 1), (130, 7)];

#[test]
fn every_kernel_compiles() {
    let Some(gpu) = gpu() else { return };
    gpu.prepare_kernels().unwrap();
    gpu.self_test().unwrap();
}

#[test]
fn blurs_agree_with_the_cpu() {
    let Some(gpu) = gpu() else { return };
    for (i, &(w, h)) in SIZES.iter().enumerate() {
        for c in 1..=4 {
            let image = random(w, h, c, 10 * i as u64 + c as u64, -0.5, 2.0);
            for (sigma, method) in [
                (0.0, BlurMethod::Auto),
                (0.75, BlurMethod::Auto),
                (3.0, BlurMethod::Auto),
                (8.0, BlurMethod::Auto),
                (5.0, BlurMethod::Boxes { passes: 3 }),
            ] {
                let mut want = image.clone();
                gaussian_blur_in_place(&mut want, sigma, method).unwrap();
                let got = ops::gaussian_blur(gpu, &image, sigma, method).unwrap();
                // The exact kernel sums the same taps in the same order in f32: up to 2 x 25 roundings of samples
                // below 2 (1.2e-7 each) apart, under 1e-5. The box passes run in f64 on the CPU and in f32 here: each
                // of 8 passes sums at most 2 x 13 + 3 samples below 2, about 3.5e-6 of rounding a pass; 5e-5 bounds
                // them with room.
                let tolerance = if sigma <= 4.0 && matches!(method, BlurMethod::Auto) { 1e-5 } else { 5e-5 };
                let d = max_difference(&got, &want);
                assert!(d <= tolerance, "{w}x{h}x{c}, sigma {sigma} {method:?}: {d}");
            }
        }
    }
}

#[test]
fn the_laplacian_agrees_with_the_cpu() {
    let Some(gpu) = gpu() else { return };
    for (i, &(w, h)) in SIZES.iter().enumerate() {
        for c in 1..=4 {
            let image = random(w, h, c, 100 + 10 * i as u64 + c as u64, -1.0, 3.0);
            let d = max_difference(&ops::laplacian(gpu, &image).unwrap(), &laplacian(&image).unwrap());
            // Five samples below 3 in the same order: only a fused multiply-add rounds differently, by an ulp of 12.
            assert!(d <= 4e-6, "{w}x{h}x{c}: {d}");
        }
    }
}

/// The frame of `canvas` for `transform` as florastack-align and the GPU see it.
fn geometry(canvas: &Canvas, transform: &Affine, frame: &Image<f32>, feather: f64, gain: f32) -> WarpGeometry {
    WarpGeometry { to_frame: canvas.to_frame(transform), frame_width: frame.width(), frame_height: frame.height(), feather, gain }
}

#[test]
fn the_warp_and_its_coverage_agree_with_florastack_align() {
    let Some(gpu) = gpu() else { return };
    let frame = random(151, 97, 3, 7, 0.0, 1.0);
    let canvas = Canvas { width: 170, height: 110, to_master: Affine::translation(-9.0, -6.5) };
    let transforms = [
        Affine::IDENTITY,
        Affine::translation(3.25, -1.75),
        Affine::similarity((75.0, 48.0), 1.03, 0.04, (0.6, 0.2)),
        Affine::similarity((75.0, 48.0), 0.96, -0.12, (-2.0, 1.0)),
    ];
    for (t, transform) in transforms.iter().enumerate() {
        for feather in [0.0, 2.0] {
            for rect in [Rect::new(0, 0, 170, 110), Rect::new(23, 17, 61, 40), Rect::new(160, 100, 10, 10)] {
                let want = warp_to_canvas(&frame, transform, &canvas, rect, feather).unwrap();
                let (image, coverage) = ops::warp_to_canvas(gpu, &frame, &geometry(&canvas, transform, &frame, feather, 1.0), rect).unwrap();
                // A pixel whose sampled point lies within 1e-3 frame pixels of the frame's edge, or of the feather's
                // end, may fall on either side of it: the GPU's coordinates are f32, off by up to about 1e-4 pixels
                // here (coordinates below 200 times 2^-24, a few operations).
                let c = Coverage::new(&canvas, transform, frame.width(), frame.height(), feather);
                let sure = |x: usize, y: usize| {
                    let d = c.inset((rect.x + x) as f64, (rect.y + y) as f64);
                    d.abs() > 1e-3 && (d - feather).abs() > 1e-3
                };
                let dc = max_difference_where(&coverage, &want.coverage, sure);
                // Inside the ramp the weight is the inset over the feather: 1e-4 pixels of coordinates is 5e-5.
                assert!(dc <= 1e-4, "transform {t}, feather {feather}, {rect:?}: coverage {dc}");
                // Samples in [0, 1]: the f32 coordinates move a sample by at most its slope times 1e-4 pixels (random
                // pixels: a slope up to about 2 through Keys' overshoot), and f32 weights and sums add a few 1e-7.
                let di = max_difference_where(&image, &want.image, sure);
                assert!(di <= 5e-4, "transform {t}, feather {feather}, {rect:?}: image {di}");
            }
        }
    }
    // The gain multiplies every sample, uncovered zeros included.
    let rect = Rect::new(5, 5, 40, 30);
    let (plain, _) = ops::warp_to_canvas(gpu, &frame, &geometry(&canvas, &Affine::IDENTITY, &frame, 2.0, 1.0), rect).unwrap();
    let (gained, _) = ops::warp_to_canvas(gpu, &frame, &geometry(&canvas, &Affine::IDENTITY, &frame, 2.0, 1.25), rect).unwrap();
    let scaled = Image::from_fn(40, 30, 3, ColorSpace::LINEAR, |x, y, ch| plain.get(x, y, ch).unwrap() * 1.25).unwrap();
    assert_eq!(max_difference(&gained, &scaled), 0.0, "the gain is one multiplication, as on the CPU");
    // A transform that isn't finite is an error, as on the CPU.
    let broken =
        WarpGeometry { to_frame: Affine { m: [[f64::NAN, 0.0, 0.0], [0.0, 1.0, 0.0]] }, ..geometry(&canvas, &Affine::IDENTITY, &frame, 2.0, 1.0) };
    assert!(ops::warp_to_canvas(gpu, &frame, &broken, rect).is_err());
}

#[test]
fn conversions_agree_with_florastack_core() {
    let Some(gpu) = gpu() else { return };
    for c in 1..=4 {
        for encoding in [Encoding::Srgb, Encoding::Linear] {
            let color = ColorSpace { encoding, icc: None };
            let small = Image::from_fn(37, 9, c, color.clone(), |x, y, ch| u8::try_from((x * 31 + y * 7 + ch * 3) % 256).unwrap()).unwrap();
            let wide = Image::from_fn(29, 11, c, color.clone(), |x, y, ch| u16::try_from((x * 2311 + y * 977 + ch * 13) % 65536).unwrap()).unwrap();
            // The CPU decodes through tables exact to the nearest f32; the GPU evaluates the curve in f32, whose pow is
            // good to a few ulps: 2e-6 on values in [0, 1]. Normalizing alone is one division, correctly rounded on the
            // CPU and within 2.5 ulps in WGSL: 2 ulps of 1 (1.2e-7) at most.
            let d8 = max_difference(&ops::to_linear_f32(gpu, &small).unwrap(), &small.to_linear_f32().unwrap());
            let d16 = max_difference(&ops::to_linear_f32(gpu, &wide).unwrap(), &wide.to_linear_f32().unwrap());
            let tolerance = if encoding == Encoding::Srgb { 2e-6 } else { 1.2e-7 };
            assert!(d8 <= tolerance && d16 <= tolerance, "{c} channels {encoding:?}: {d8} {d16}");
        }
        // f32 through the curve both ways, extended range and negative values included; alpha untouched.
        let linear = random(41, 13, c, 900 + c as u64, -0.5, 4.0);
        let encoded = ops::srgb_transfer(gpu, &linear, true).unwrap();
        let mut want = linear.clone();
        let alpha = match c {
            2 => Some(1),
            4 => Some(3),
            _ => None,
        };
        for row in want.rows_mut() {
            for (i, v) in row.iter_mut().enumerate() {
                if Some(i % c) != alpha {
                    *v = florastack_core::convert::linear_to_srgb(*v);
                }
            }
        }
        // Relative: the curve is good to a few ulps of its value, which reaches 1.8 at 4.
        assert!(max_relative_difference(&encoded, &want, 1e-3) <= 4e-6, "{c} channels: encode");
        let back = ops::srgb_transfer(gpu, &want, false).unwrap();
        assert!(max_relative_difference(&back, &linear, 1e-3) <= 1e-5, "{c} channels: decode");
    }
}

/// A span of a `w` x `h` level from (`x`, `y`), `sw` x `sh` samples.
fn span(x: usize, y: usize, sw: usize, sh: usize, w: usize, h: usize) -> Span {
    Span { x, y, width: sw, height: sh, full_width: w, full_height: h }
}

fn cpu_span(s: Span) -> cpu::Span {
    cpu::Span { x: s.x, y: s.y, width: s.width, height: s.height, full_width: s.full_width, full_height: s.full_height }
}

#[test]
fn the_pyramids_levels_agree_with_florastack_stack() {
    let Some(gpu) = gpu() else { return };
    // Whole levels, and spans of a level with their own edges, as tiles have them.
    let cases = [
        (37, 23, span(0, 0, 37, 23, 37, 23)),
        (70, 45, span(16, 8, 54, 29, 70, 45)),
        (1, 9, span(0, 0, 1, 9, 1, 9)),
        (130, 7, span(8, 0, 64, 7, 130, 7)),
    ];
    for (i, &(w, h, s)) in cases.iter().enumerate() {
        for c in [1, 3, 4] {
            let full = random(w, h, c, 300 + 10 * i as u64 + c as u64, -0.25, 1.5);
            let image = full.crop(Rect::new(s.x, s.y, s.width, s.height)).unwrap();
            let (want, want_span) = cpu::reduce(&image, cpu_span(s)).unwrap();
            let (got, got_span) = levels::reduce_image(gpu, &image, s).unwrap();
            assert_eq!(cpu_span(got_span), want_span);
            // Five exact weights in the same order, twice: an ulp or two of 1.5 at most, where the GPU fuses a
            // multiply-add.
            assert!(max_difference(&got, &want) <= 1e-6, "reduce {w}x{h}x{c}");
            let up_want = cpu::expand(&want, want_span, cpu_span(s)).unwrap();
            let up = levels::expand_image(gpu, &got, got_span, s).unwrap();
            assert!(max_difference(&up, &up_want) <= 2e-6, "expand {w}x{h}x{c}");
            // Partial coverage, zero where the frame doesn't cover: normalized convolution divides by the reduced
            // coverage, which can make the samples next to the edge sensitive (1 / k for k down to 1e-6), so the
            // comparison is relative there.
            let coverage = Image::from_fn(s.width, s.height, 1, ColorSpace::LINEAR, |x, y, _| ((x * 3 + y) % 7) as f32 / 6.0).unwrap();
            let (wv, wc, _) = cpu::reduce_covered(&image, &coverage, cpu_span(s)).unwrap();
            let (gv, gc, _) = levels::reduce_covered_image(gpu, &image, &coverage, s).unwrap();
            assert!(max_difference(&gc, &wc) <= 1e-6, "reduced coverage {w}x{h}x{c}");
            assert!(max_relative_difference(&gv, &wv, 1.0) <= 1e-5, "reduce_covered {w}x{h}x{c}");
            // The local energy, of the luminance and of every channel.
            let lum = cpu::local_energy(&cpu::luminance(&image).unwrap(), cpu_span(s)).unwrap();
            let glum = levels::local_energy_image(gpu, &image, s, true).unwrap();
            assert!(max_relative_difference(&glum, &lum, 1e-3) <= 1e-5, "luminance energy {w}x{h}x{c}");
            let each = cpu::local_energy(&image, cpu_span(s)).unwrap();
            let geach = levels::local_energy_image(gpu, &image, s, false).unwrap();
            assert!(max_relative_difference(&geach, &each, 1e-3) <= 1e-5, "channel energy {w}x{h}x{c}");
        }
    }
}

#[test]
fn laplacian_pyramids_and_their_collapse_agree_with_florastack_stack() {
    let Some(gpu) = gpu() else { return };
    for (i, &(w, h)) in SIZES.iter().enumerate() {
        let image = random(w, h, 3, 500 + i as u64, 0.0, 1.0);
        let want = cpu::laplacian_pyramid(&image, 4).unwrap();
        let got = levels::laplacian_pyramid(gpu, &image, 4).unwrap();
        assert_eq!(got.len(), want.len());
        for (k, (g, wnt)) in got.iter().zip(&want).enumerate() {
            // Each level is a few exact-weight filters and a subtraction from the level above: a few ulps of 1.
            assert!(max_difference(g, wnt) <= 4e-6, "{w}x{h} level {k}");
        }
        let back = levels::collapse(gpu, &want).unwrap();
        assert!(max_difference(&back, &cpu::collapse(&want).unwrap()) <= 4e-6, "{w}x{h} collapse");
        assert!(max_difference(&back, &image) <= 1e-5, "{w}x{h}: the collapse inverts the pyramid");
    }
}

#[test]
fn readbacks_from_threads_sharing_the_device_all_arrive() {
    // A poll fires the callbacks of every mapping it finds done, on its own thread: one thread's poll can take
    // another's readback, whose own poll then returns before the callback has run (the race CI's lavapipe and Metal
    // runners hit when the test threads shared the device). Every readback must arrive anyway.
    let Some(gpu) = gpu() else { return };
    std::thread::scope(|scope| {
        for t in 0..8u64 {
            scope.spawn(move || {
                for i in 0..60usize {
                    let image = random(37 + i % 5, 11, 3, 1000 * t + u64::try_from(i).unwrap(), 0.0, 1.0);
                    let back = gpu.download(&gpu.upload(&image).unwrap()).unwrap();
                    assert_eq!(max_difference(&back, &image), 0.0, "thread {t}, round trip {i}");
                }
            });
        }
    });
}
