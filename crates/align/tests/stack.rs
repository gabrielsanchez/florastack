//! What the aligner does beyond its accuracy (tests/accuracy.rs): the terms that can be turned off, the flags and the
//! rejection of frames that can't be trusted, its order and errors, and hostile input.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicU64, Ordering};

use florastack_align::{AlignError, AlignSettings, Flag, StackAligner, StackAlignment, Status, Term, align_frames};
use florastack_core::ops::{Border, Interpolation, warp};
use florastack_core::testutil::{StackParams, SyntheticStack, generate, scene, transform_error};
use florastack_core::{Affine, Cancelled, ColorSpace, Image, Monitor, Unmonitored};

const WIDTH: usize = 160;
const HEIGHT: usize = 120;

/// A small stack of the textured test scene, moved but not defocused.
fn stack(params: StackParams) -> SyntheticStack {
    let (sharp, depth) = scene::textured(WIDTH, HEIGHT, params.seed).unwrap();
    generate(&sharp, &depth, &StackParams { blur_per_depth: 0.0, max_blur: 0.0, ..params }).unwrap()
}

/// The mean transform error of the frames of `result`, those listed in `skip` left out.
fn mean_error(result: &StackAlignment, stack: &SyntheticStack, skip: &[usize]) -> f64 {
    let errors: Vec<f64> = (0..stack.frames.len())
        .filter(|i| !skip.contains(i))
        .map(|i| transform_error(&result.frames[i].transform, &stack.truth.relative(i, result.master).unwrap(), WIDTH, HEIGHT))
        .collect();
    errors.iter().sum::<f64>() / errors.len() as f64
}

/// `frame` with its content moved by `(dx, dy)` pixels.
fn moved(frame: &Image<f32>, dx: f64, dy: f64) -> Image<f32> {
    warp(frame, &Affine::translation(-dx, -dy), Interpolation::Bicubic, Border::Clamp).unwrap()
}

#[test]
fn terms_turned_off_stay_exact_and_cost_no_accuracy() {
    // A rail: the camera shifts and the magnification changes, but nothing turns, and the light is steady.
    let rail = stack(StackParams { max_rotation_deg: 0.0, read_noise: 0.01, ..StackParams::clean(8, 3) });
    let all = align_frames(&rail.frames, 0, &AlignSettings::default(), &Unmonitored).unwrap();
    let settings = AlignSettings { rotation: false, brightness: false, ..AlignSettings::default() };
    let fewer = align_frames(&rail.frames, 0, &settings, &Unmonitored).unwrap();
    for frame in &fewer.frames {
        assert_eq!((frame.similarity.rotation_deg, frame.gain), (0.0, 1.0), "{frame:?}");
    }
    let (with, without) = (mean_error(&all, &rail, &[]), mean_error(&fewer, &rail, &[]));
    println!("rail: {with:.4} px with every term, {without:.4} px without rotation");
    assert!(without <= with && without < 0.05);

    // Shifts alone: scale and rotation stay exactly as they are.
    let shifts = stack(StackParams { breathing: 0.0, max_rotation_deg: 0.0, read_noise: 0.01, ..StackParams::clean(8, 4) });
    let all = align_frames(&shifts.frames, 0, &AlignSettings::default(), &Unmonitored).unwrap();
    let settings = AlignSettings { scale: false, rotation: false, ..AlignSettings::default() };
    let fewer = align_frames(&shifts.frames, 0, &settings, &Unmonitored).unwrap();
    for frame in &fewer.frames {
        assert_eq!((frame.similarity.scale, frame.similarity.rotation_deg), (1.0, 0.0), "{frame:?}");
    }
    let (with, without) = (mean_error(&all, &shifts, &[]), mean_error(&fewer, &shifts, &[]));
    println!("shifts: {with:.4} px with every term, {without:.4} px with shifts only");
    assert!(without <= with && without < 0.05);

    // Nothing at all: every frame stays where it is, and only the scores are measured.
    let none = AlignSettings { shift_x: false, shift_y: false, scale: false, rotation: false, brightness: false, ..AlignSettings::default() };
    let still = align_frames(&shifts.frames, 0, &none, &Unmonitored).unwrap();
    assert!(still.frames.iter().all(|f| f.transform.is_identity() && f.score > 0.0));
}

#[test]
fn a_frame_beyond_a_limit_is_rejected_and_the_chain_goes_around_it() {
    let mut s = stack(StackParams::clean(7, 5));
    // Frame 3 jumps 12 pixels to the right, beyond 5% of 160 pixels.
    s.frames[3] = moved(&s.frames[3], 12.0, 0.0);
    let settings = AlignSettings { max_shift: 5.0, ..AlignSettings::default() };
    let result = align_frames(&s.frames, 0, &settings, &Unmonitored).unwrap();
    let jumped = &result.frames[3];
    assert_eq!(jumped.status, Status::Rejected, "{jumped:?}");
    assert!(jumped.flags.contains(&Flag::Limit(Term::ShiftX)), "{jumped:?}");
    assert_eq!(jumped.transform, result.frames[2].transform, "it keeps its reference's transform");
    assert_eq!(result.frames[4].reference, Some(2), "the next frame is aligned to the last one accepted");
    assert!(result.frames.iter().enumerate().all(|(i, f)| i == 3 || f.flags.is_empty()));
    assert!(mean_error(&result, &s, &[3]) < 0.05);

    // Without rejection, the frame is only flagged, and the chain goes through it.
    let flagged = align_frames(&s.frames, 0, &AlignSettings { reject: false, ..settings }, &Unmonitored).unwrap();
    assert_eq!(flagged.frames[3].status, Status::Aligned);
    assert!(flagged.frames[3].flags.contains(&Flag::Limit(Term::ShiftX)));
    assert_eq!(flagged.frames[4].reference, Some(3));
}

#[test]
fn an_unrelated_frame_is_rejected() {
    let mut s = stack(StackParams::clean(7, 6));
    let (other, _) = scene::textured(WIDTH, HEIGHT, 1234).unwrap();
    s.frames[2] = other;
    let result = align_frames(&s.frames, 0, &AlignSettings::default(), &Unmonitored).unwrap();
    let stranger = &result.frames[2];
    println!("unrelated frame: score {:.3}, flags {:?}", stranger.score, stranger.flags);
    assert_eq!(stranger.status, Status::Rejected, "{stranger:?}");
    assert!(stranger.flags.iter().any(|f| f.rejects()));
    assert_eq!(result.frames[3].reference, Some(1));
    assert!(mean_error(&result, &s, &[2]) < 0.05);
}

#[test]
fn repeating_patterns_are_flagged() {
    // A grid with a period of 10 pixels, flat: every link matches a period away about as well.
    let grid = Image::from_fn(WIDTH, HEIGHT, 3, ColorSpace::LINEAR, |x, y, _| {
        let (fx, fy) = (x as f32 * std::f32::consts::TAU / 10.0, y as f32 * std::f32::consts::TAU / 10.0);
        0.5 + 0.3 * fx.sin() * fy.sin()
    })
    .unwrap();
    let depth = Image::from_fn(WIDTH, HEIGHT, 1, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
    let s = generate(&grid, &depth, &StackParams { max_shift: 2.0, max_rotation_deg: 0.2, ..StackParams::identity(4) }).unwrap();
    let result = align_frames(&s.frames, 0, &AlignSettings::default(), &Unmonitored).unwrap();
    for frame in &result.frames[1..] {
        assert!(frame.flags.contains(&Flag::RepeatingPattern), "{frame:?}");
    }
}

#[test]
fn frames_are_aligned_outward_from_the_master() {
    let s = stack(StackParams::clean(5, 7));
    let mut aligner = StackAligner::new(&AlignSettings::default(), 5, 2).unwrap();
    assert_eq!(aligner.order(), [2, 3, 4, 1, 0]);
    while let Some(index) = aligner.next_frame() {
        let frame = aligner.add(&s.frames[index], &Unmonitored).unwrap();
        assert_eq!(frame.status == Status::Master, index == 2);
    }
    assert_eq!(aligner.add(&s.frames[0], &Unmonitored).unwrap_err(), AlignError::Finished);
    let result = aligner.finish().unwrap();
    let references: Vec<Option<usize>> = result.frames.iter().map(|f| f.reference).collect();
    assert_eq!(references, [Some(1), Some(2), None, Some(2), Some(3)]);
    assert_eq!((result.master, result.frames[2].score, result.frames[2].gain), (2, 1.0, 1.0));
    assert!(mean_error(&result, &s, &[]) < 0.05);

    let one = align_frames(&s.frames[..1], 0, &AlignSettings::default(), &Unmonitored).unwrap();
    assert_eq!(one.frames.len(), 1);
    assert!(one.frames[0].transform.is_identity());
}

#[test]
fn impossible_stacks_and_settings_are_errors() {
    let s = stack(StackParams::clean(3, 8));
    let settings = AlignSettings::default();
    assert_eq!(StackAligner::new(&settings, 0, 0).err(), Some(AlignError::NoFrames));
    assert_eq!(StackAligner::new(&settings, 3, 3).err(), Some(AlignError::Master { master: 3, frames: 3 }));
    let bad = AlignSettings { max_scale: f64::NAN, ..AlignSettings::default() };
    assert!(matches!(StackAligner::new(&bad, 3, 0).err(), Some(AlignError::Setting { name: "max_scale", .. })));

    let mut aligner = StackAligner::new(&settings, 3, 0).unwrap();
    aligner.add(&s.frames[0], &Unmonitored).unwrap();
    let small = s.frames[1].crop(florastack_core::Rect::new(0, 0, 100, 80)).unwrap();
    assert!(matches!(aligner.add(&small, &Unmonitored), Err(AlignError::FrameSize { index: 1, width: 100, height: 80, .. })));
    assert_eq!(aligner.finish().err(), Some(AlignError::Incomplete { added: 1, frames: 3 }));

    let tiny = Image::<f32>::new(15, 40, 3, ColorSpace::LINEAR).unwrap();
    assert_eq!(align_frames(&[tiny], 0, &settings, &Unmonitored).err(), Some(AlignError::TooSmall { width: 15, height: 40 }));
}

#[test]
fn hostile_frames_are_flagged_not_crashed_on() {
    let s = stack(StackParams::clean(4, 9));
    // Frames with no detail at all, or nothing but not-a-number: no match, rejected, and the stack still aligns.
    let flat = Image::from_fn(WIDTH, HEIGHT, 3, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
    let nan = Image::from_fn(WIDTH, HEIGHT, 3, ColorSpace::LINEAR, |_, _, _| f32::NAN).unwrap();
    let frames = [s.frames[0].clone(), flat, nan, s.frames[3].clone()];
    let result = align_frames(&frames, 0, &AlignSettings::default(), &Unmonitored).unwrap();
    for frame in &result.frames[1..3] {
        assert_eq!(frame.status, Status::Rejected, "{frame:?}");
        assert!(frame.flags.contains(&Flag::NoMatch), "{frame:?}");
    }
    // The flat frame's brightness can still be compared with the master's; the empty one has none.
    assert!(result.frames[1].gain.is_finite() && result.frames[1].gain > 0.0);
    assert_eq!(result.frames[2].gain, 1.0);
    assert!(result.frames[3].flags.is_empty(), "{:?}", result.frames[3]);
    assert!(transform_error(&result.frames[3].transform, &s.truth.relative(3, 0).unwrap(), WIDTH, HEIGHT) < 0.05);

    // Stray infinities and huge values in a frame are absorbed.
    let mut spiky = s.frames[1].clone();
    for (i, v) in spiky.rows_mut().flatten().enumerate() {
        if i % 997 == 0 {
            *v = if i % 2 == 0 { f32::INFINITY } else { f32::MAX };
        }
    }
    let result = align_frames(&[s.frames[0].clone(), spiky], 0, &AlignSettings::default(), &Unmonitored).unwrap();
    assert!(result.frames[1].transform.m.iter().flatten().all(|v| v.is_finite()) && result.frames[1].gain.is_finite());

    // The smallest frames, gray with alpha, and the most extreme settings.
    let small: Vec<Image<f32>> = (0..3)
        .map(|i| Image::from_fn(16, 16, 2, ColorSpace::SRGB, |x, y, c| if c == 1 { 1.0 } else { ((x * 7 + y * 3 + i) % 5) as f32 / 4.0 }).unwrap())
        .collect();
    let extreme = AlignSettings { max_shift: 0.0, max_scale: 50.0, max_rotation: 45.0, min_score: 1.0, resolution: 64, ..AlignSettings::default() };
    let result = align_frames(&small, 1, &extreme, &Unmonitored).unwrap();
    assert_eq!(result.frames.len(), 3);
}

#[test]
fn results_dont_depend_on_the_thread_count() {
    let s = stack(StackParams::noisy_dusty(5, WIDTH, HEIGHT, 10));
    let settings = AlignSettings { refine: true, ..AlignSettings::default() };
    let default = align_frames(&s.frames, 2, &settings, &Unmonitored).unwrap();
    let one =
        rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap().install(|| align_frames(&s.frames, 2, &settings, &Unmonitored).unwrap());
    let three =
        rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap().install(|| align_frames(&s.frames, 2, &settings, &Unmonitored).unwrap());
    assert_eq!(default, one);
    assert_eq!(default, three);
}

/// A monitor that is cancelled after `steps` steps, and counts them.
struct CancelAfter {
    steps: u64,
    seen: AtomicU64,
}

impl Monitor for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.seen.load(Ordering::Relaxed) >= self.steps
    }

    fn progress(&self, done: u64, _total: u64) {
        self.seen.store(done, Ordering::Relaxed);
    }
}

#[test]
fn cancellation_stops_at_the_next_frame() {
    let s = stack(StackParams::clean(6, 11));
    let monitor = CancelAfter { steps: 2, seen: AtomicU64::new(0) };
    assert_eq!(align_frames(&s.frames, 0, &AlignSettings::default(), &monitor), Err(AlignError::Cancelled(Cancelled)));
    assert_eq!(monitor.seen.load(Ordering::Relaxed), 2, "it stopped after the second frame");
    let done = CancelAfter { steps: u64::MAX, seen: AtomicU64::new(0) };
    align_frames(&s.frames, 0, &AlignSettings::default(), &done).unwrap();
    assert_eq!(done.seen.load(Ordering::Relaxed), 6, "progress reaches the last frame");
}
