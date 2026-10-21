//! The develop step on synthetic frames, whose true scene is known: the output stays linear (a gray ramp), identical
//! settings give identical output, the preview and output demosaics agree on smooth regions, the stack's white and
//! corrections come from its reference frame, and the corrections do what they say.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{camera_neutral, dng_color, frame, identical, max_difference, px, smooth_scene, xtrans};
use florastack_core::Encoding;
use florastack_raw::{
    CameraColor, Cfa, ChromaticAberration, Demosaic, DevelopSettings, DevelopWarning, Highlights, NoiseReduction, Quality, RawDevelop, RawError,
    Resolved, WhiteBalance,
};

/// The settings every test starts from: the defaults, with chromatic-aberration correction resolved to none (the
/// automatic estimate is LightCraft's; its own tests cover it).
fn base() -> DevelopSettings {
    DevelopSettings { chromatic_aberration: ChromaticAberration::Off, ..DevelopSettings::default() }
}

/// A gray step wedge, as a camera with [`common::CAMERA_MATRIX`] records neutrals: 16 steps 16 photosites wide, from
/// 0.02 to 0.92 of white (below the clip level everywhere), every channel the step's level times the camera's neutral.
const STEP: usize = 16;
const STEPS: usize = 16;

fn step_level(step: usize) -> f32 {
    0.02 + 0.06 * step as f32
}

#[allow(clippy::cast_possible_truncation)]
fn neutral(level: f32) -> [f32; 3] {
    let n = camera_neutral();
    [level * n[0] as f32, level * n[1] as f32, level * n[2] as f32]
}

fn wedge(cfa: Option<Cfa>) -> florastack_raw::RawFrame {
    frame(STEP * STEPS, 32, cfa, dng_color(), |x, _| neutral(step_level(x / STEP)))
}

#[test]
fn a_gray_ramp_stays_a_linear_gray_ramp() {
    // Every step's flat middle develops to its level times 2^EV, in every channel: no tone curve, no shoulder, no
    // black point. Every demosaic reproduces a flat field exactly, and the multipliers and the matrix are computed in
    // f64 and applied in f32, so what is left is rounding: 2e-5 relative is several times what it comes to. A line
    // fitted through the steps has slope 2^EV and no offset, to the same precision.
    for (cfa, name) in [(Cfa::bayer("RGGB"), "Bayer"), (Some(xtrans()), "X-Trans")] {
        let frame = wedge(cfa);
        for exposure in [-1.0f32, 0.0, 1.5] {
            for demosaic in [Demosaic::Fast, Demosaic::HighQuality] {
                let settings = DevelopSettings { exposure, demosaic, ..base() };
                let out = RawDevelop::new(&settings, &frame).unwrap().develop(&frame, Quality::Output).unwrap();
                assert_eq!(out.image.color_space().encoding, Encoding::Linear);
                let gain = exposure.exp2();
                let mut points = Vec::new();
                for step in 0..STEPS {
                    let want = step_level(step) * gain;
                    let mut sum = 0.0f64;
                    for y in 10..22 {
                        for x in step * STEP + 5..step * STEP + 11 {
                            for (c, v) in px(&out.image, x, y).into_iter().enumerate() {
                                assert!(
                                    (v / want - 1.0).abs() < 2e-5,
                                    "{name} {demosaic:?} {exposure} EV: ({x}, {y}) channel {c} is {v}, not {want}"
                                );
                                sum += f64::from(v);
                            }
                        }
                    }
                    points.push((f64::from(step_level(step)), sum / (12.0 * 6.0 * 3.0)));
                }
                let n = points.len() as f64;
                let (mx, my) = (points.iter().map(|p| p.0).sum::<f64>() / n, points.iter().map(|p| p.1).sum::<f64>() / n);
                let slope = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / points.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>();
                let offset = my - slope * mx;
                assert!(
                    (slope / f64::from(gain) - 1.0).abs() < 2e-5 && offset.abs() < 2e-6,
                    "{name} {demosaic:?} {exposure} EV: {slope} x + {offset}"
                );
            }
        }
    }
    // On a Bayer sensor a continuous ramp comes out exactly too: both demosaics reproduce linear gradients.
    let ramp = frame(128, 48, Cfa::bayer("GBRG"), dng_color(), |x, _| neutral(0.02 + 0.88 * x as f32 / 127.0));
    for quality in [Quality::Preview, Quality::Output] {
        let out = RawDevelop::new(&base(), &ramp).unwrap().develop(&ramp, quality).unwrap();
        for y in 8..40 {
            for x in 8..120 {
                let want = 0.02 + 0.88 * x as f32 / 127.0;
                assert!(px(&out.image, x, y).iter().all(|v| (v / want - 1.0).abs() < 1e-4), "{quality:?} ({x}, {y})");
            }
        }
    }
}

#[test]
fn scaling_the_light_scales_the_output_with_every_step_on() {
    // Linearity with highlight reconstruction, noise reduction and a manual chromatic-aberration correction on: two
    // exposures one stop apart develop to images one stop apart. Noise reduction filters log luminance and chroma
    // ratios, which a gain doesn't change (but for their floors far below these levels), so the factor holds to
    // 1e-4 relative (f32 arithmetic through blurs and logarithms).
    let frame = frame(96, 64, Cfa::bayer("GRBG"), dng_color(), |x, y| smooth_scene(x, y).map(|v| v * 0.5));
    let settings = DevelopSettings {
        noise_reduction: NoiseReduction { luminance: 0.3, color: 0.5 },
        chromatic_aberration: ChromaticAberration::Manual { red: 0.002, blue: -0.001 },
        ..base()
    };
    let one = RawDevelop::new(&settings, &frame).unwrap().develop(&frame, Quality::Output).unwrap();
    let two = RawDevelop::new(&DevelopSettings { exposure: 1.0, ..settings }, &frame).unwrap().develop(&frame, Quality::Output).unwrap();
    for y in 0..64 {
        for x in 0..96 {
            for (a, b) in px(&one.image, x, y).iter().zip(px(&two.image, x, y)) {
                assert!((b / a - 2.0).abs() < 2e-4, "({x}, {y}): {a} and {b}");
            }
        }
    }
}

#[test]
fn identical_settings_give_identical_output() {
    let frame = frame(96, 72, Some(xtrans()), dng_color(), smooth_scene);
    let settings = DevelopSettings {
        noise_reduction: NoiseReduction { luminance: 0.2, color: 0.4 },
        chromatic_aberration: ChromaticAberration::Manual { red: 0.001, blue: -0.002 },
        white_balance: WhiteBalance::Custom { temperature: 4800.0, tint: 6.0 },
        ..base()
    };
    let develop = RawDevelop::new(&settings, &frame).unwrap();
    let first = develop.develop(&frame, Quality::Output).unwrap();
    // The same development again, one rebuilt from the settings as a project would store them, and one run on a
    // single thread: rows are developed in parallel, and no result may depend on how.
    let again = develop.develop(&frame, Quality::Output).unwrap();
    let stored: DevelopSettings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
    let resolved: Resolved = serde_json::from_str(&serde_json::to_string(&develop.resolved()).unwrap()).unwrap();
    let rebuilt = RawDevelop::from_resolved(&stored, resolved).unwrap();
    let reopened = rebuilt.develop(&frame, Quality::Output).unwrap();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let single = pool.install(|| develop.develop(&frame, Quality::Output)).unwrap();
    for other in [&again, &reopened, &single] {
        assert!(identical(&first.image, &other.image));
        assert_eq!(first.fingerprint, other.fingerprint);
    }
    // And a frame of the stack shot with another white balance still gets the stack's.
    let mut shifted = frame.clone();
    shifted.color.as_shot_neutral = Some([0.4, 1.0, 0.8]);
    assert!(identical(&develop.develop(&shifted, Quality::Output).unwrap().image, &first.image));
}

#[test]
fn preview_and_output_demosaics_agree_on_smooth_regions() {
    // On a smooth field the fast and the high-quality demosaic differ by little. On a Bayer sensor both reproduce
    // linear gradients (bilinear interpolation averages neighbors placed symmetrically about the pixel), so only the
    // field's curvature over their few-pixel stencils separates them: below 0.2 % of white here. On X-Trans the
    // neighbors of a color sit off-center, so bilinear interpolation errs by up to the gradient times about a pixel:
    // this field rises by at most 0.8 % of white per pixel, hence 1.2 %. The edges, where the mosaic is mirrored, are
    // left out. Fine detail is where the methods differ, as they must.
    for (cfa, tolerance) in [(Cfa::bayer("RGGB").unwrap(), 2e-3), (Cfa::bayer("GBRG").unwrap(), 2e-3), (xtrans(), 1.2e-2)] {
        let frame = frame(160, 120, Some(cfa.clone()), dng_color(), smooth_scene);
        let develop = RawDevelop::new(&base(), &frame).unwrap();
        let preview = develop.develop(&frame, Quality::Preview).unwrap();
        let output = develop.develop(&frame, Quality::Output).unwrap();
        assert_ne!(preview.fingerprint, output.fingerprint);
        let diff = max_difference(&preview.image, &output.image, 6);
        assert!(diff < tolerance, "{}: the demosaics differ by {diff}", cfa.name());
        let fast = RawDevelop::new(&DevelopSettings { demosaic: Demosaic::Fast, ..base() }, &frame).unwrap();
        assert!(identical(&fast.develop(&frame, Quality::Output).unwrap().image, &preview.image), "previews use the fast demosaic");
    }
    // Fine detail: a one-pixel checkerboard of luminance, which the two methods treat differently.
    let n = camera_neutral();
    #[allow(clippy::cast_possible_truncation)]
    let detail = frame(64, 64, Cfa::bayer("RGGB"), dng_color(), |x, y| {
        let l = if (x / 3 + y / 2) % 2 == 0 { 0.7 } else { 0.1 };
        [l * n[0] as f32, l * n[1] as f32, l * n[2] as f32]
    });
    let develop = RawDevelop::new(&base(), &detail).unwrap();
    let diff =
        max_difference(&develop.develop(&detail, Quality::Preview).unwrap().image, &develop.develop(&detail, Quality::Output).unwrap().image, 6);
    assert!(diff > 1e-2, "detail tells the methods apart: {diff}");
}

#[test]
fn the_stack_white_comes_from_the_reference_frame() {
    let reference = frame(48, 32, Cfa::bayer("RGGB"), dng_color(), smooth_scene);
    let develop = RawDevelop::new(&base(), &reference).unwrap();
    // As shot: the reference's neutral is the camera's D65 neutral, so the stack's white is D65.
    let white = develop.resolved().white;
    assert!((white.x - 0.3127).abs() < 1e-3 && (white.y - 0.329).abs() < 1e-3, "{white:?}");
    // A custom temperature resolves to its own white whatever the reference recorded.
    let custom =
        RawDevelop::new(&DevelopSettings { white_balance: WhiteBalance::Custom { temperature: 3200.0, tint: 0.0 }, ..base() }, &reference).unwrap();
    let (t, _) = custom.resolved().white.temperature();
    assert!((t - 3200.0).abs() < 5.0, "{t}");
    // Auto on a gray world: the reference is gray under D65 in camera terms, so the estimate is D65 too.
    let gray = frame(96, 64, Cfa::bayer("RGGB"), dng_color(), |x, y| neutral(0.15 + 0.3 * (x + 2 * y) as f32 / 224.0));
    let auto = RawDevelop::new(&DevelopSettings { white_balance: WhiteBalance::Auto, ..base() }, &gray).unwrap();
    // LightCraft rounds its estimate to whole kelvin and tint units, and D65 lies a little off the Planckian locus
    // (a tint of about +10), so compare with D65's own temperature and tint.
    let (t, tint) = auto.resolved().white.temperature();
    let (t65, tint65) = florastack_raw::Chromaticity::D65.temperature();
    assert!((t - t65).abs() < 150.0 && (tint - tint65).abs() < 3.0, "auto found {t} K, tint {tint}; D65 is {t65} K, tint {tint65}");
    let out = auto.develop(&gray, Quality::Output).unwrap();
    let p = px(&out.image, 40, 30);
    assert!((p[0] / p[1] - 1.0).abs() < 0.02 && (p[2] / p[1] - 1.0).abs() < 0.02, "gray stays gray: {p:?}");
}

#[test]
fn a_file_without_a_recorded_white_is_flagged() {
    let unknown = frame(16, 16, Cfa::bayer("RGGB"), CameraColor::default(), smooth_scene);
    let develop = RawDevelop::new(&base(), &unknown).unwrap();
    let out = develop.develop(&unknown, Quality::Preview).unwrap();
    assert_eq!(out.warnings, vec![DevelopWarning::NoRecordedWhite, DevelopWarning::ApproximateColor]);
}

#[test]
fn clipped_highlights_are_rebuilt_neutral_or_left_alone() {
    // A neutral patch the sensor clipped in every channel: as-is it comes out tinted by the white balance (the
    // channels the multipliers lift exceed the others); rebuilt, it comes out neutral and at least as bright.
    let n = camera_neutral();
    #[allow(clippy::cast_possible_truncation)]
    let frame = frame(64, 64, Cfa::bayer("RGGB"), dng_color(), |x, y| {
        let l = if (20..44).contains(&x) && (20..44).contains(&y) { 3.0 } else { 0.3 };
        [((l * n[0]) as f32).min(1.0), ((l * n[1]) as f32).min(1.0), ((l * n[2]) as f32).min(1.0)]
    });
    let rebuilt = RawDevelop::new(&base(), &frame).unwrap().develop(&frame, Quality::Output).unwrap();
    let kept = RawDevelop::new(&DevelopSettings { highlights: Highlights::Off, ..base() }, &frame).unwrap().develop(&frame, Quality::Output).unwrap();
    let spread = |p: [f32; 3]| p.iter().copied().fold(f32::MIN, f32::max) / p.iter().copied().fold(f32::MAX, f32::min);
    let (r, k) = (px(&rebuilt.image, 32, 32), px(&kept.image, 32, 32));
    assert!(spread(k) > 1.2, "clipped and white balanced, the patch is tinted: {k:?}");
    assert!(spread(r) < 1.02, "rebuilt, it is neutral: {r:?}");
    // Away from the patch nothing is touched.
    assert!(px(&rebuilt.image, 4, 4).iter().zip(px(&kept.image, 4, 4)).all(|(a, b)| a.to_bits() == b.to_bits()));
}

#[test]
fn chromatic_aberration_correction_puts_the_planes_back_together() {
    // A smooth pattern whose red plane the "lens" magnified by 1 % about the image's center (an edge green shows at
    // radius r, red shows at r * 1.01): corrected by that scale, red lines up with green again, far closer than
    // uncorrected; the opposite scale makes it worse.
    let (w, h) = (192, 128);
    let center = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let scale = 0.01;
    #[allow(clippy::cast_possible_truncation)]
    let pattern = |x: f64, y: f64| 0.4 + 0.3 * ((x / 5.0).sin() * (y / 5.0).sin()) as f32;
    let n = camera_neutral();
    #[allow(clippy::cast_possible_truncation)]
    let scene = move |x: usize, y: usize| {
        let (fx, fy) = (x as f64, y as f64);
        let (rx, ry) = (center.0 + (fx - center.0) / (1.0 + scale), center.1 + (fy - center.1) / (1.0 + scale));
        [pattern(rx, ry) * n[0] as f32, pattern(fx, fy) * n[1] as f32, pattern(fx, fy) * n[2] as f32]
    };
    let mosaic = frame(w, h, Cfa::bayer("RGGB"), dng_color(), scene);
    let misfit = |red: f32| {
        let settings = DevelopSettings { chromatic_aberration: ChromaticAberration::Manual { red, blue: 0.0 }, ..base() };
        let out = RawDevelop::new(&settings, &mosaic).unwrap().develop(&mosaic, Quality::Output).unwrap();
        let mut sum = 0.0f64;
        for y in 8..h - 8 {
            for x in 8..w - 8 {
                let p = px(&out.image, x, y);
                sum += f64::from((p[0] - p[1]).abs());
            }
        }
        sum / ((w - 16) * (h - 16)) as f64
    };
    let (off, on, wrong) = (misfit(0.0), misfit(0.01), misfit(-0.01));
    assert!(on < off * 0.3, "corrected {on} against {off}");
    assert!(wrong > off, "the opposite scale makes it worse: {wrong} against {off}");
}

#[test]
fn monochrome_sensors_develop_to_gray() {
    let n = 32 * 24;
    let samples: Vec<f32> = (0..n).map(|i| (i % 32) as f32 / 40.0).collect();
    let info = florastack_raw::RawInfo {
        format: florastack_raw::RawFormat::Dng,
        make: None,
        model: None,
        width: 32,
        height: 24,
        layout: florastack_raw::SensorLayout::Monochrome,
        bits: 16,
        orientation: florastack_core::Orientation::Normal,
        color: florastack_raw::ColorSource::Fallback,
        as_shot_white: None,
        distortion_correction: false,
    };
    let sensor = florastack_raw::Mosaic { width: 32, height: 24, channels: 1, samples, cfa: None, float: false };
    let frame = florastack_raw::RawFrame::new(info, sensor, CameraColor::default(), florastack_core::Rect::new(0, 0, 32, 24), 1.0).unwrap();
    let out = RawDevelop::new(&DevelopSettings { exposure: 1.0, ..base() }, &frame).unwrap().develop(&frame, Quality::Output).unwrap();
    for x in 0..32 {
        let p = px(&out.image, x, 5);
        let want = x as f32 / 40.0 * 2.0;
        assert!(p.iter().all(|v| (v - want).abs() < 1e-6), "{x}: {p:?}");
    }
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
}

#[test]
fn broken_frames_and_settings_are_errors() {
    let mut frame = frame(16, 16, Cfa::bayer("RGGB"), dng_color(), smooth_scene);
    let develop = RawDevelop::new(&base(), &frame).unwrap();
    frame.sensor.samples.truncate(10);
    assert!(matches!(develop.develop(&frame, Quality::Output), Err(RawError::Frame(_))));
    let good = common::frame(16, 16, Cfa::bayer("RGGB"), dng_color(), smooth_scene);
    assert!(matches!(RawDevelop::new(&DevelopSettings { exposure: 40.0, ..base() }, &good), Err(RawError::Setting { name: "exposure", .. })));
    // Not-a-number samples (a float DNG can hold them) develop to numbers, never a panic.
    let mut nan = good.clone();
    for v in nan.sensor.samples.iter_mut().step_by(3) {
        *v = f32::NAN;
    }
    let out = develop.develop(&nan, Quality::Output).unwrap();
    assert!(out.image.rows().flatten().all(|v| v.is_finite()));
}
