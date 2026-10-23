//! Ordering, framing and focus on synthetic stacks (roadmap 2.2): the narrow end sets the processing order and the
//! master, sorting by scale restores a shuffled stack, frames focused in front of the scene or behind it are flagged,
//! and wide framing marks what each frame doesn't cover instead of smearing its edge into streaks, which
//! florastack-metrics checks against the ground truth. Each test prints what it measured
//! (`cargo test -p florastack-align --test ordering -- --nocapture`).
//!
//! The frames are generated a margin larger than the scene they show and cropped to their middle, so that no frame
//! shows the generator's own clamped scene edge: the streaks these tests look for are the stacking's, not the
//! generator's.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_align::{
    AlignError, AlignSettings, Canvas, DEFAULT_FEATHER, End, Flag, Framing, ProcessingOrder, StackAlignment, Status, align_by_scale, align_ordered,
    coverage_count, narrow_end, scale_order, warp_to_canvas,
};
use florastack_core::ops::{Border, Interpolation, warp_into};
use florastack_core::testutil::{StackParams, SyntheticStack, generate, scene, transform_error};
use florastack_core::{Affine, ColorSpace, Image, Rect, Unmonitored};
use florastack_metrics::{psnr, ssim};

/// The margin, in pixels, the frames are generated with on every side.
const MARGIN: usize = 12;

/// Frames cropped from a stack generated `MARGIN` pixels larger on every side, and that stack.
struct Cropped {
    frames: Vec<Image<f32>>,
    stack: SyntheticStack,
}

fn cropped(width: usize, height: usize, params: &StackParams) -> Cropped {
    let (sharp, depth) = scene::textured(width + 2 * MARGIN, height + 2 * MARGIN, params.seed).unwrap();
    let stack = generate(&sharp, &depth, params).unwrap();
    let frames = stack.frames.iter().map(|f| f.crop(Rect::new(MARGIN, MARGIN, width, height)).unwrap()).collect();
    Cropped { frames, stack }
}

impl Cropped {
    /// The true transform from frame `master`'s pixels to frame `frame`'s, in the cropped frames.
    fn relative(&self, frame: usize, master: usize) -> Affine {
        let m = MARGIN as f64;
        Affine::translation(m, m).then(&self.stack.truth.relative(frame, master).unwrap()).then(&Affine::translation(-m, -m))
    }

    /// The sharp scene on `canvas`, framed on frame `master`, at that frame's brightness.
    fn truth_on(&self, canvas: &Canvas, master: usize) -> Image<f32> {
        let m = MARGIN as f64;
        let truth = &self.stack.truth;
        let to_scene = canvas.to_master.then(&Affine::translation(m, m)).then(&truth.frames[master].transform.inverse().unwrap());
        let mut out = Image::new(canvas.width, canvas.height, truth.sharp.channels(), ColorSpace::LINEAR).unwrap();
        warp_into(&truth.sharp, &to_scene, Interpolation::Bicubic, Border::Clamp, &mut out).unwrap();
        let gain = truth.frames[master].gain;
        for v in out.rows_mut().flatten() {
            *v *= gain;
        }
        out
    }

    /// The mean transform error of `alignment` against the truth, relative to its master.
    fn error(&self, alignment: &StackAlignment) -> f64 {
        let (w, h) = (alignment.width, alignment.height);
        let errors: Vec<f64> =
            alignment.frames.iter().enumerate().map(|(i, f)| transform_error(&f.transform, &self.relative(i, alignment.master), w, h)).collect();
        errors.iter().sum::<f64>() / errors.len() as f64
    }
}

/// Frames moved, scaled by `breathing` over the stack, but not defocused.
fn moving(frames: usize, breathing: f32, seed: u64) -> StackParams {
    StackParams {
        breathing,
        max_shift: 1.5,
        max_rotation_deg: 0.3,
        brightness_jitter: 0.02,
        read_noise: 0.002,
        seed,
        ..StackParams::identity(frames)
    }
}

#[test]
fn the_narrow_end_sets_the_order_and_the_master() {
    for (breathing, narrow) in [(0.06, End::Last), (-0.06, End::First)] {
        let s = cropped(200, 150, &moving(7, breathing, 3));
        let settings = AlignSettings::default();
        let (alignment, order) = align_ordered(&s.frames, &settings, &Unmonitored).unwrap();
        let end_frame = if narrow == End::Last { 6 } else { 0 };
        println!("breathing {breathing:+}: narrow {:?}, master {}, error {:.4} px", order.narrow, order.master, s.error(&alignment));
        assert_eq!((order.narrow, order.start, order.master, order.narrow_frame), (Some(narrow), narrow, end_frame, Some(end_frame)));
        assert_eq!(order.frames.first(), Some(&end_frame), "processing starts at the narrow end");
        assert_eq!(order.frames.len(), 7);
        // Rebased onto the narrow end, exactly as if it had been aligned to it.
        assert_eq!((alignment.master, alignment.frames[end_frame].status), (end_frame, Status::Master));
        assert_eq!((alignment.frames[end_frame].transform, alignment.frames[end_frame].gain), (Affine::IDENTITY, 1.0));
        assert_eq!(alignment.frames[3].status, Status::Aligned, "the middle frame, which it was aligned from, is a frame like the others");
        assert!(s.error(&alignment) < 0.05, "{}", s.error(&alignment));
        for (i, f) in alignment.frames.iter().enumerate() {
            let truth = f64::from(s.stack.truth.frames[end_frame].gain / s.stack.truth.frames[i].gain);
            assert!((f.gain / truth - 1.0).abs() < 0.002, "frame {i}: gain {} for {truth}", f.gain);
        }
        // Every frame shows more of the scene than the narrow end's.
        assert!(
            alignment.frames.iter().all(|f| f.similarity.scale <= 1.0 + 1e-9),
            "{:?}",
            alignment.frames.iter().map(|f| f.similarity.scale).collect::<Vec<_>>()
        );

        // The overrides: from the first frame, and reversed.
        let first = ProcessingOrder::new(&alignment, &AlignSettings { automatic_order: false, ..settings.clone() });
        assert_eq!((first.start, first.master, first.frames[0], first.narrow), (End::First, 0, 0, Some(narrow)));
        let reversed = ProcessingOrder::new(&alignment, &AlignSettings { reverse_order: true, ..settings.clone() });
        assert_eq!(reversed.start, narrow.other(), "from the wide end");
        assert_eq!(reversed.frames, order.frames.iter().rev().copied().collect::<Vec<_>>());
        assert_eq!(reversed.narrow_frame, Some(end_frame), "narrow framing still keeps the narrow end's field of view");
        let last = ProcessingOrder::new(&alignment, &AlignSettings { automatic_order: false, reverse_order: true, ..settings.clone() });
        assert_eq!((last.start, last.master), (End::Last, 6));
    }

    // Without breathing, the ends can't be told apart: the stack is processed from its first frame.
    let s = cropped(200, 150, &moving(5, 0.0, 4));
    let (alignment, order) = align_ordered(&s.frames, &AlignSettings::default(), &Unmonitored).unwrap();
    assert_eq!((narrow_end(&alignment), order.start, order.master, order.narrow_frame), (None, End::First, 0, None));
    assert!(alignment.rebased(9).is_err());
}

#[test]
fn sorting_by_scale_restores_a_shuffled_stack() {
    let params = StackParams { breathing: 0.1, ..StackParams::clean(10, 5) };
    let s = cropped(240, 180, &params);
    let shuffled_order = [7, 2, 9, 0, 5, 3, 8, 1, 6, 4];
    let shuffled: Vec<Image<f32>> = shuffled_order.iter().map(|&i| s.frames[i].clone()).collect();
    let (order, alignment, processing) = align_by_scale(&shuffled, &AlignSettings::default(), &Unmonitored).unwrap();
    let restored: Vec<usize> = order.iter().map(|&k| shuffled_order[k]).collect();
    println!("shuffled {shuffled_order:?}, restored {restored:?}");
    let forward: Vec<usize> = (0..10).collect();
    let backward: Vec<usize> = (0..10).rev().collect();
    assert!(restored == forward || restored == backward, "{restored:?}");
    // Aligned again in focus order: every frame chained to its neighbor, none flagged but for focus.
    assert!(alignment.frames.iter().all(|f| f.status != Status::Rejected && f.flags.iter().all(|f| *f == Flag::OutOfFocus)), "{alignment:?}");
    let truth_index = |k: usize| restored[k];
    let errors: Vec<f64> = (0..10)
        .map(|k| transform_error(&alignment.frames[k].transform, &s.relative(truth_index(k), truth_index(alignment.master)), 240, 180))
        .collect();
    let mean = errors.iter().sum::<f64>() / 10.0;
    println!("aligned again: mean error {mean:.4} px, narrow end {:?}", processing.narrow);
    assert!(mean < 0.15, "{errors:?}");

    // A stack with a few frames out of place keeps its direction.
    let mixed_order = [0, 1, 4, 3, 2, 5, 6, 8, 7, 9];
    let mixed: Vec<Image<f32>> = mixed_order.iter().map(|&i| s.frames[i].clone()).collect();
    let (order, _, _) = align_by_scale(&mixed, &AlignSettings::default(), &Unmonitored).unwrap();
    assert_eq!(order.iter().map(|&k| mixed_order[k]).collect::<Vec<_>>(), forward);

    // Without breathing there is nothing to sort by.
    let flat = cropped(160, 120, &moving(4, 0.0, 6));
    let e = align_by_scale(&flat.frames, &AlignSettings::default(), &Unmonitored).unwrap_err();
    assert!(matches!(e, AlignError::NoScaleChange { .. }), "{e}");
    let one = florastack_align::align_frames(&flat.frames[..1], 0, &AlignSettings::default(), &Unmonitored).unwrap();
    assert!(matches!(scale_order(&one), Err(AlignError::NoScaleChange { .. })));
}

/// The test scene's depths run from 0 to 1; frames focused from 0.75 in front of it to 0.75 behind it, every quarter,
/// at 8 pixels of blur per unit of depth.
///
/// The frames focused 0.5 and 0.75 off the scene (0, 1, 9 and 10), blurred by 4 pixels or more everywhere, are
/// flagged, and none of the frames focused on the scene (3 to 7). The frames focused 0.25 off it (2 and 8), 2 pixels
/// off its nearest or farthest detail, score far below the scene's frames but aren't held to the flag: the generator
/// blurs each pixel with its own depth's blur, so where a near bristle crosses the far background the frames' detail
/// mixes the two depths, and frame 8 is about as sharp as the in-focus frames on a few such cells (the roadmap's
/// follow-up on the generator's depth edges).
#[test]
fn frames_focused_off_the_scene_are_flagged_out_of_focus() {
    let params = StackParams { focus_range: Some((-0.75, 1.75)), ..StackParams::clean(11, 7) };
    let s = cropped(240, 180, &params);
    let (alignment, _) = align_ordered(&s.frames, &AlignSettings::default(), &Unmonitored).unwrap();
    let fractions: Vec<f64> = alignment.frames.iter().map(|f| f.sharp_fraction.unwrap()).collect();
    let flagged: Vec<usize> = (0..11).filter(|&i| alignment.frames[i].flags.contains(&Flag::OutOfFocus)).collect();
    println!("sharp-content fractions {:?}, flagged {flagged:?}", fractions.iter().map(|f| (f * 1e4).round() / 1e4).collect::<Vec<_>>());
    for frame in [0, 1, 9, 10] {
        assert!(flagged.contains(&frame), "frame {frame}: {fractions:?}");
    }
    assert!(flagged.iter().all(|f| ![3, 4, 5, 6, 7].contains(f)), "{flagged:?}");
    let least_in_focus = fractions[3..8].iter().copied().fold(1.0, f64::min);
    assert!(least_in_focus > 0.05, "{fractions:?}");
    assert!(fractions[2] < least_in_focus / 4.0 && fractions[8] < least_in_focus / 4.0, "{fractions:?}");
    assert!(alignment.frames.iter().all(|f| f.status != Status::Rejected), "only flagged, never rejected");
    // Off, nothing is flagged; and a stack whose frames are all focused on the scene has no frame flagged.
    let off = AlignSettings { min_sharp_fraction: 0.0, ..AlignSettings::default() };
    let (alignment, _) = align_ordered(&s.frames, &off, &Unmonitored).unwrap();
    assert!(alignment.frames.iter().all(|f| !f.flags.contains(&Flag::OutOfFocus)));
    let inside = cropped(240, 180, &StackParams::clean(8, 8));
    let (alignment, _) = align_ordered(&inside.frames, &AlignSettings::default(), &Unmonitored).unwrap();
    assert!(alignment.frames.iter().all(|f| !f.flags.contains(&Flag::OutOfFocus) && f.sharp_fraction.is_some()), "{alignment:?}");
}

/// `values`, one per sample of a `width` x `height` RGB image, as that image.
fn image(width: usize, height: usize, values: &[f64]) -> Image<f32> {
    #[allow(clippy::cast_possible_truncation)] // Means of f32 samples.
    let data = values.iter().map(|&v| v as f32).collect();
    Image::from_vec(width, height, 3, ColorSpace::LINEAR, data).unwrap()
}

/// The frames of `s` warped onto `canvas` and averaged, each weighted by its coverage, with its gain applied; pixels
/// no frame covers are 0.
fn covered_mean(s: &Cropped, alignment: &StackAlignment, canvas: &Canvas) -> Image<f32> {
    let all = Rect::new(0, 0, canvas.width, canvas.height);
    let mut sum = vec![0.0_f64; canvas.width * canvas.height * 3];
    let mut weight = vec![0.0_f64; canvas.width * canvas.height];
    for (frame, a) in s.frames.iter().zip(&alignment.frames) {
        let covered = warp_to_canvas(frame, &a.transform, canvas, all, DEFAULT_FEATHER).unwrap();
        for (i, (pixel, &w)) in covered.image.as_raw().as_chunks::<3>().0.iter().zip(covered.coverage.as_raw()).enumerate() {
            for (c, &v) in pixel.iter().enumerate() {
                sum[3 * i + c] += f64::from(w) * a.gain * f64::from(v);
            }
            weight[i] += f64::from(w);
        }
    }
    let means: Vec<f64> = sum.iter().enumerate().map(|(k, &v)| if weight[k / 3] > 0.0 { v / weight[k / 3] } else { 0.0 }).collect();
    image(canvas.width, canvas.height, &means)
}

/// The frames warped onto `canvas` as a warp that clamps at the frame's border does, and averaged: every frame
/// everywhere, its edge pixels repeated where it has none.
fn smeared_mean(s: &Cropped, alignment: &StackAlignment, canvas: &Canvas) -> Image<f32> {
    let mut sum = vec![0.0_f64; canvas.width * canvas.height * 3];
    let mut warped = Image::new(canvas.width, canvas.height, 3, ColorSpace::LINEAR).unwrap();
    for (frame, a) in s.frames.iter().zip(&alignment.frames) {
        warp_into(frame, &canvas.to_frame(&a.transform), Interpolation::Bicubic, Border::Clamp, &mut warped).unwrap();
        for (d, &v) in sum.iter_mut().zip(warped.as_raw()) {
            *d += a.gain * f64::from(v) / s.frames.len() as f64;
        }
    }
    image(canvas.width, canvas.height, &sum)
}

/// A whole number of pixels from a canvas offset.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A small, whole, positive number.
fn pixels(v: f64) -> usize {
    v.round() as usize
}

#[test]
fn wide_framing_leaves_no_streaks() {
    // 15% of breathing: the wide end sees 7.5% more of the scene on every side than the narrow end.
    let (w, h, n) = (240, 180, 8);
    let s = cropped(w, h, &moving(n, 0.15, 9));
    let (alignment, order) = align_ordered(&s.frames, &AlignSettings::default(), &Unmonitored).unwrap();
    assert_eq!((order.narrow, order.master), (Some(End::Last), n - 1));
    let canvas = alignment.canvas(Framing::Wide, order.narrow_frame).unwrap();
    let truth = s.truth_on(&canvas, order.master);
    let transforms: Vec<Affine> = alignment.frames.iter().map(|f| f.transform).collect();
    let count = coverage_count(&canvas, &transforms, w, h).unwrap();
    let counts: Vec<u16> = count.as_raw().to_vec();
    let uncovered = counts.iter().filter(|&&c| c == 0).count();
    let partial = counts.iter().filter(|&&c| c > 0 && usize::from(c) < n).count();
    println!("wide canvas {}x{}: {partial} pixels covered by some frames only, {uncovered} by none", canvas.width, canvas.height);
    assert!(canvas.width > w + 30 && canvas.height > h + 22, "{canvas:?}");
    assert!(partial > canvas.width * 20, "the edges are covered by the wide end's frames only");
    // The widest frame, turned by a fraction of a degree, leaves thin slivers of its bounding box uncovered.
    assert!(uncovered * 50 < counts.len(), "only the slivers that rotation leaves along the edges are covered by nothing");

    let covered = covered_mean(&s, &alignment, &canvas);
    let smeared = smeared_mean(&s, &alignment, &canvas);
    // Pixels no frame covers have nothing to compare: the truth stands in for both.
    let compared = |result: &Image<f32>| {
        let values: Vec<f64> =
            result.as_raw().iter().zip(truth.as_raw()).enumerate().map(|(k, (&r, &t))| f64::from(if counts[k / 3] == 0 { t } else { r })).collect();
        image(canvas.width, canvas.height, &values)
    };
    let (covered, smeared) = (compared(&covered), compared(&smeared));
    // The strips around the narrow end's field of view, which is the master's grid in the canvas.
    let (x0, y0) = (pixels(-canvas.to_master.m[0][2]), pixels(-canvas.to_master.m[1][2]));
    let strips = [
        ("left", Rect::new(0, 0, x0, canvas.height)),
        ("right", Rect::new(x0 + w, 0, canvas.width - x0 - w, canvas.height)),
        ("top", Rect::new(x0, 0, w, y0)),
        ("bottom", Rect::new(x0, y0 + h, w, canvas.height - y0 - h)),
    ];
    for (name, strip) in strips {
        let (t, c, m) = (truth.crop(strip).unwrap(), covered.crop(strip).unwrap(), smeared.crop(strip).unwrap());
        let (pc, sc) = (psnr(&c, &t).unwrap(), ssim(&c, &t).unwrap());
        let (pm, sm) = (psnr(&m, &t).unwrap(), ssim(&m, &t).unwrap());
        println!(
            "{name:6} {}x{}: coverage-weighted PSNR {pc:.1} dB, SSIM {sc:.4}; clamped and smeared PSNR {pm:.1} dB, SSIM {sm:.4}",
            strip.width, strip.height
        );
        // Measured: 45.7 to 58.4 dB and an SSIM of 0.9986 to 0.9995 with coverage, 28.7 to 32.3 dB and 0.86 to 0.91
        // for the clamped warp, whose streaks the strips show.
        assert!(pc > 40.0 && sc > 0.99, "{name}: streaks where the frames have no pixels");
        assert!(pm < pc - 10.0 && sm < sc - 0.05, "{name}: the test would see the streaks of a clamped warp");
    }
    // Inside the narrow end's field of view, every frame covers every pixel and both are the same stack.
    let inner = Rect::new(x0 + 4, y0 + 4, w - 8, h - 8);
    assert!(psnr(&covered.crop(inner).unwrap(), &truth.crop(inner).unwrap()).unwrap() > 35.0);
}

#[test]
fn narrow_and_common_framings_are_covered_by_every_frame() {
    let (w, h, n) = (240, 180, 8);
    let s = cropped(w, h, &moving(n, 0.06, 10));
    let (alignment, order) = align_ordered(&s.frames, &AlignSettings::default(), &Unmonitored).unwrap();
    let transforms: Vec<Affine> = alignment.frames.iter().map(|f| f.transform).collect();
    let narrow = alignment.canvas(Framing::Narrow, order.narrow_frame).unwrap();
    assert_eq!(narrow, Canvas::master(w, h), "processed from the narrow end, the master's own grid");
    let count = coverage_count(&narrow, &transforms, w, h).unwrap();
    // Every frame covers the narrow end's field of view, up to the few pixels the shifts between frames uncover.
    let short: Vec<(usize, usize)> =
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| usize::from(count.get(x, y, 0).unwrap()) < n).collect();
    println!("narrow: {} of {} pixels not covered by every frame", short.len(), w * h);
    assert!(short.iter().all(|&(x, y)| x.min(w - 1 - x).min(y).min(h - 1 - y) < 4), "only along the edges: {short:?}");
    assert!(count.as_raw().iter().all(|&c| c >= 1));

    let common = alignment.canvas(Framing::Common, order.narrow_frame).unwrap();
    println!("common: {}x{} at {:?}", common.width, common.height, common.to_master.m);
    assert!(common.width >= w - 8 && common.height >= h - 8 && common.width <= w && common.height <= h);
    let count = coverage_count(&common, &transforms, w, h).unwrap();
    assert!(count.as_raw().iter().all(|&c| usize::from(c) == n), "every frame covers every pixel");

    // Reversed, the stack is aligned to its wide end, and narrow framing still crops to the narrow end's view.
    let reversed = AlignSettings { reverse_order: true, ..AlignSettings::default() };
    let (alignment, order) = align_ordered(&s.frames, &reversed, &Unmonitored).unwrap();
    assert_eq!(order.master, 0);
    let narrow = alignment.canvas(Framing::Narrow, order.narrow_frame).unwrap();
    let to_narrow = narrow.to_frame(&alignment.frames[n - 1].transform);
    let p = to_narrow.apply(10.0, 20.0);
    assert!((p.0 - 10.0).abs() < 1e-9 && (p.1 - 20.0).abs() < 1e-9, "the narrow end's own pixels: {p:?}");
}
