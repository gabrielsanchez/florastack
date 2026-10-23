//! Sensor defects on synthetic stacks (roadmap 2.3): dust and hot pixels are detected at their sensor coordinates,
//! nothing else is, and inpainting them before alignment leaves no dust trails in a simple max-contrast stack. Each
//! test prints what it measured (`cargo test -p florastack-align --test defects -- --nocapture`).
//!
//! The scenes are `scene::textured`'s with its background plane defocused, as a macro subject's backdrop is: the
//! subject (discs and bristles) stays sharp where the focus passes it. On the generator's own background, dense colored
//! texture that most frames show nearly in focus and that moves by little more than a pixel from frame to frame, dust
//! can't be told from the scene by its sensor position, its steadiness or its color: its coarse blobs stay darker than
//! their surroundings, at the same pixels, in every frame (`florastack_align::DefectDetector` says what can pass).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_align::{AlignSettings, DefectDetector, DefectMask, MIN_DETECTION_FRAMES, Spot, align_frames, detect_defects, inpaint};
use florastack_core::ops::{BlurMethod, Border, Interpolation, gaussian_blur_in_place, laplacian, warp};
use florastack_core::testutil::{DustSpot, StackParams, SyntheticStack, generate, scene};
use florastack_core::{Affine, ColorSpace, Image, Unmonitored};

const WIDTH: usize = 320;
const HEIGHT: usize = 240;
const FRAMES: usize = 16;

/// Hot pixels injected into every frame: a pixel of one channel, as a stuck photosite leaves after demosaicing.
const HOT: [(usize, usize, usize); 4] = [(40, 30, 0), (200, 100, 1), (301, 220, 2), (150, 200, 0)];

/// `scene::textured`'s scene with its background (depth 0.55 and beyond) blurred by 10 pixels, as a backdrop far
/// behind the subject is.
fn backdrop_scene(seed: u64) -> (Image<f32>, Image<f32>) {
    let (mut sharp, depth) = scene::textured(WIDTH, HEIGHT, seed).unwrap();
    let mut soft = sharp.clone();
    gaussian_blur_in_place(&mut soft, 10.0, BlurMethod::Exact).unwrap();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if depth.get(x, y, 0).unwrap() >= 0.55 {
                let far = soft.pixel(x, y).unwrap().to_vec();
                sharp.pixel_mut(x, y).unwrap().copy_from_slice(&far);
            }
        }
    }
    (sharp, depth)
}

fn stack(params: &StackParams) -> SyntheticStack {
    let (sharp, depth) = backdrop_scene(params.seed);
    generate(&sharp, &depth, params).unwrap()
}

/// Dust darker and larger than `noisy_dusty`'s, so that its trails are plain to see, anywhere on the frame.
fn dusty_anywhere(seed: u64) -> StackParams {
    let dust = DustSpot::random(8, WIDTH, HEIGHT, seed ^ 0xd057)
        .into_iter()
        .map(|s| DustSpot { radius: s.radius + 1.0, opacity: s.opacity + 0.2, ..s })
        .collect();
    StackParams { dust, ..StackParams::noisy_dusty(FRAMES, WIDTH, HEIGHT, seed) }
}

/// The same dust, but only where its shadow (to 1% darkening, plus the frames' shifts) falls on the backdrop: eight
/// spots drawn until that many fit.
fn dusty(seed: u64) -> StackParams {
    let (_, depth) = backdrop_scene(seed);
    let on_backdrop = |s: &DustSpot| {
        let reach = f64::from(s.radius) * (2.0 * (f64::from(s.opacity) / 0.01).ln()).sqrt() + 3.0;
        let near = |x: usize, y: usize| (x as f64 - f64::from(s.x)).hypot(y as f64 - f64::from(s.y)) <= reach;
        (0..HEIGHT).all(|y| (0..WIDTH).all(|x| !near(x, y) || depth.get(x, y, 0).unwrap() >= 0.55))
    };
    let dust: Vec<DustSpot> = (0..)
        .flat_map(|k| DustSpot::random(8, WIDTH, HEIGHT, seed ^ 0xd057 ^ (k << 8)))
        .map(|s| DustSpot { radius: s.radius + 1.0, opacity: s.opacity + 0.2, ..s })
        .filter(on_backdrop)
        .take(8)
        .collect();
    StackParams { dust, ..StackParams::noisy_dusty(FRAMES, WIDTH, HEIGHT, seed) }
}

fn with_hot_pixels(frames: &mut [Image<f32>]) {
    for frame in frames {
        for &(x, y, c) in &HOT {
            let v = frame.pixel_mut(x, y).unwrap();
            v[c] += 0.8;
        }
    }
}

/// The darkening of a dust spot at pixel `(x, y)`.
fn darkening(dust: &[DustSpot], x: usize, y: usize) -> f64 {
    1.0 - dust.iter().map(|s| s.transmission(x as f64, y as f64)).product::<f64>()
}

#[test]
fn dust_and_hot_pixels_are_found_where_they_are() {
    for seed in 1..=3 {
        let params = dusty(seed);
        let mut frames = stack(&params).frames;
        with_hot_pixels(&mut frames);
        let mask = detect_defects(&frames, &Unmonitored).unwrap();
        // Every pixel that dust darkens by 25% or more, nearly every one it darkens by 10%, and every hot pixel, is
        // masked; and every spot of the mask holds a defect.
        let (mut missed, mut faint, mut faint_missed) = (0, 0, 0);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let dark = darkening(&params.dust, x, y);
                let masked = mask.is_masked(x, y);
                if (dark >= 0.25 || HOT.iter().any(|&(hx, hy, _)| (hx, hy) == (x, y))) && !masked {
                    missed += 1;
                }
                if (0.1..0.25).contains(&dark) {
                    faint += 1;
                    faint_missed += usize::from(!masked);
                }
            }
        }
        let holds_a_defect = |spot: &Spot| {
            (spot.y..spot.y + spot.height).any(|y| {
                (spot.x..spot.x + spot.width).any(|x| darkening(&params.dust, x, y) >= 0.05 || HOT.iter().any(|&(hx, hy, _)| (hx, hy) == (x, y)))
            })
        };
        let wrong: usize = mask.spots().filter(|s| !holds_a_defect(s)).map(|s| s.area).sum();
        let dusty_area = (0..HEIGHT).flat_map(|y| (0..WIDTH).map(move |x| (x, y))).filter(|&(x, y)| darkening(&params.dust, x, y) >= 0.01).count();
        println!(
            "seed {seed}: {} pixels in {} spots masked (dust darkens {dusty_area} pixels by 1% or more); missed: {missed} of the hot pixels and the pixels darkened by 25% or more, {faint_missed} of the {faint} darkened by 10 to 25%; {wrong} pixels in spots without a defect",
            mask.count(),
            mask.spots().len()
        );
        assert_eq!(missed, 0, "seed {seed}");
        assert!(faint_missed * 10 <= faint, "seed {seed}: {faint_missed} of {faint}");
        assert_eq!(wrong, 0, "seed {seed}");
    }
}

#[test]
fn clean_stacks_have_nothing_masked() {
    for seed in 1..=3 {
        // Moved and breathing as a camera does, or held still with only the focus changing.
        let moving = StackParams { read_noise: 0.005, shot_noise: 0.01, ..StackParams::clean(FRAMES, seed) };
        let still = StackParams { breathing: 0.0, max_shift: 0.0, max_rotation_deg: 0.0, ..moving.clone() };
        for (name, params) in [("moving", moving), ("still", still)] {
            let mask = detect_defects(&stack(&params).frames, &Unmonitored).unwrap();
            println!("seed {seed}, {name}: {} pixels in {} spots masked", mask.count(), mask.spots().len());
            assert_eq!(mask.count(), 0, "seed {seed}, {name}");
        }
    }
}

#[test]
fn detection_is_streamed_and_needs_five_frames() {
    let frames = stack(&dusty(1)).frames;
    let mut detector = DefectDetector::new(WIDTH, HEIGHT).unwrap();
    for frame in &frames {
        detector.add(frame, &Unmonitored).unwrap();
    }
    assert_eq!(detector.finish().unwrap(), detect_defects(&frames, &Unmonitored).unwrap(), "every frame of a short stack");
    assert!(detect_defects(&frames[..MIN_DETECTION_FRAMES - 1], &Unmonitored).is_err());
}

/// A simple max-contrast stack of frames aligned to frame `master`: each pixel from the frame whose luminance has the
/// most Laplacian energy around it.
fn max_contrast(frames: &[Image<f32>], transforms: &[Affine]) -> Image<f32> {
    let warped: Vec<Image<f32>> = frames.iter().zip(transforms).map(|(f, t)| warp(f, t, Interpolation::Bicubic, Border::Clamp).unwrap()).collect();
    let energy: Vec<Image<f32>> = warped
        .iter()
        .map(|f| {
            let luma = Image::from_fn(WIDTH, HEIGHT, 1, ColorSpace::LINEAR, |x, y, _| {
                let p = f.pixel(x, y).unwrap();
                0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
            })
            .unwrap();
            let mut e = laplacian(&luma).unwrap();
            for v in e.rows_mut().flatten() {
                *v *= *v;
            }
            gaussian_blur_in_place(&mut e, 2.0, BlurMethod::Exact).unwrap();
            e
        })
        .collect();
    Image::from_fn(WIDTH, HEIGHT, 3, ColorSpace::LINEAR, |x, y, c| {
        let best = (0..frames.len()).max_by(|&a, &b| energy[a].get(x, y, 0).unwrap().total_cmp(&energy[b].get(x, y, 0).unwrap())).unwrap();
        warped[best].get(x, y, c).unwrap()
    })
    .unwrap()
}

/// The mean luminance of `image` over the pixels within `radius` of `points`.
fn mean_near(image: &Image<f32>, points: &[(f64, f64)], radius: f64) -> f64 {
    let (mut sum, mut n) = (0.0, 0);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if points.iter().any(|&(px, py)| (x as f64 - px).hypot(y as f64 - py) <= radius) {
                let p = image.pixel(x, y).unwrap();
                sum += f64::from(0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]);
                n += 1;
            }
        }
    }
    sum / f64::from(n.max(1))
}

/// The darkest trail of `params`' dust in a max-contrast stack aligned on the inpainted frames, as a share of the light
/// there: unmasked, of the stack without dust; masked, of the stack of the frames without dust inpainted under the
/// same mask, which leaves out what inpainting itself loses, the detail under the mask (printed, against the stack
/// without dust).
fn trails(seed: u64, params: &StackParams) -> (f64, f64) {
    let dirty = stack(params).frames;
    let clean = stack(&StackParams { dust: Vec::new(), ..params.clone() }).frames;
    let mask = detect_defects(&dirty, &Unmonitored).unwrap();
    let inpainted = |frames: &[Image<f32>]| {
        let mut frames = frames.to_vec();
        for frame in &mut frames {
            inpaint(frame, &mask).unwrap();
        }
        frames
    };
    let (cleaned, clean_inpainted) = (inpainted(&dirty), inpainted(&clean));
    let alignment = align_frames(&cleaned, FRAMES / 2, &AlignSettings::default(), &Unmonitored).unwrap();
    let transforms: Vec<Affine> = alignment.frames.iter().map(|f| f.transform).collect();
    let stacked = |frames: &[Image<f32>]| max_contrast(frames, &transforms);
    let (truth, unmasked, masked, reference) = (stacked(&clean), stacked(&dirty), stacked(&cleaned), stacked(&clean_inpainted));
    // Each spot's trail: where it lies in every frame, once aligned to the master.
    let (mut trail_unmasked, mut trail_masked, mut loss) = (f64::INFINITY, f64::INFINITY, f64::INFINITY);
    for spot in &params.dust {
        let trail: Vec<(f64, f64)> = transforms.iter().map(|t| t.inverse().unwrap().apply(f64::from(spot.x), f64::from(spot.y))).collect();
        let mean = |image: &Image<f32>| mean_near(image, &trail, 1.5 * f64::from(spot.radius));
        trail_unmasked = trail_unmasked.min(mean(&unmasked) / mean(&truth));
        trail_masked = trail_masked.min(mean(&masked) / mean(&reference));
        loss = loss.min(mean(&reference) / mean(&truth));
    }
    println!(
        "seed {seed}: the darkest trail keeps {trail_unmasked:.3} of the light unmasked, {trail_masked:.3} masked (of the frames without dust inpainted alike); inpainting alone keeps {loss:.3} of the light there"
    );
    (trail_unmasked, trail_masked)
}

/// Masking the detected dust removes every trace of it from the backdrop: the max-contrast stack of the dusty frames,
/// inpainted, is that of the same frames without dust, inpainted alike, where the unmasked stack is darker along each
/// spot's trail.
#[test]
fn masked_dust_leaves_no_trails_in_a_max_contrast_stack() {
    for seed in 1..=3 {
        let (unmasked, masked) = trails(seed, &dusty(seed));
        assert!(unmasked < 0.85, "seed {seed}: the test's dust must leave trails to remove: {unmasked}");
        assert!(masked > 0.98, "seed {seed}: a dust trail is left: {masked}");
    }
}

/// Dust anywhere on the frame, the subject included: a spot that straddles the edge of a dark object is found only in
/// part (over the object, noise hides how steady its shadow is; beside it, the ring samples in the object hide its
/// darkening), so masking leaves part of its trail, but at most half of its darkening.
#[test]
fn masking_dust_on_the_subject_at_least_halves_its_trails() {
    for seed in 1..=3 {
        let (unmasked, masked) = trails(seed, &dusty_anywhere(seed));
        assert!(unmasked < 0.85, "seed {seed}: the test's dust must leave trails to remove: {unmasked}");
        assert!(1.0 - masked <= 0.5 * (1.0 - unmasked), "seed {seed}: {masked} masked, {unmasked} unmasked");
    }
}

#[test]
fn a_user_mask_is_inpainted_as_given() {
    let params = dusty(2);
    let frames = stack(&params).frames;
    // The user's mask: a disc of 3 radii around each spot, drawn black on white.
    let drawn = Image::from_fn(WIDTH, HEIGHT, 3, ColorSpace::SRGB, |x, y, _| {
        let inside = params.dust.iter().any(|s| (x as f32 - s.x).hypot(y as f32 - s.y) <= 3.0 * s.radius);
        if inside { 0.0 } else { 1.0 }
    })
    .unwrap();
    let mask = DefectMask::from_image(&drawn).unwrap();
    let mut frame = frames[0].clone();
    inpaint(&mut frame, &mask).unwrap();
    let truth = stack(&StackParams { dust: Vec::new(), ..params.clone() }).frames;
    let mean = |image: &Image<f32>| {
        let spot = &params.dust[0];
        mean_near(image, &[(f64::from(spot.x), f64::from(spot.y))], f64::from(spot.radius))
    };
    let (before, after, clean) = (mean(&frames[0]), mean(&frame), mean(&truth[0]));
    println!("the first spot's core: {before:.4} dusty, {after:.4} inpainted, {clean:.4} without dust");
    assert!((after / clean - 1.0).abs() < 0.1 && before / clean < 0.8);
}
