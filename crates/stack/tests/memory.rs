//! Memory stays flat as the stack grows: the peak heap of a stack of 200 frames is that of 20 frames, for the weighted
//! average, the pyramid method and both passes of the depth-map method.
//!
//! The measure is the heap, counted by `dhat`'s global allocator (this test binary's only), which records every
//! allocation and its peak in bytes: the memory that depends on FloraStack's code, the same on every platform and run.
//! A process's peak resident set would add what it doesn't control (code, thread stacks, pages the allocator keeps)
//! and can't be reset between two runs in one process. Each frame is made, as a decoder would make it, just before it
//! is added, and dropped after, so the peak counts one frame in flight besides the stacker's accumulators and scratch.
//! The frames cycle through eight defocused copies of the scene, made before measuring.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_core::testutil::{StackParams, generate, scene};
use florastack_core::{Image, Unmonitored};
use florastack_stack::{AverageSettings, DepthSettings, FrameProgress, Method, PyramidSettings, StackShape, Stacker, Tiling};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const WIDTH: usize = 640;
const HEIGHT: usize = 480;

/// The peak heap, in bytes, while stacking `depth` frames from `distinct` with `method`, and the bytes left at the end.
fn measure(method: &impl Method, distinct: &[Image<f32>], depth: usize) -> (usize, usize) {
    let profiler = dhat::Profiler::builder().testing().build();
    let mut stacker = method.stacker(&Tiling { tile_size: 256, ..Tiling::default() }).unwrap();
    stacker.begin(StackShape { width: WIDTH, height: HEIGHT, channels: 3 }).unwrap();
    for i in 0..depth {
        // A fresh frame, as decoding one makes it.
        let frame = distinct[i % distinct.len()].clone();
        stacker.add_frame(&frame, &FrameProgress { inner: &Unmonitored, frame: i as u64, frames: depth as u64 }).unwrap();
        drop(frame);
    }
    let result = stacker.finish(&Unmonitored).unwrap();
    assert_eq!(result.frames, depth);
    drop((result, stacker));
    let stats = dhat::HeapStats::get();
    drop(profiler);
    (stats.max_bytes, stats.curr_bytes)
}

#[test]
fn peak_memory_does_not_grow_with_the_stack() {
    let (sharp, depth) = scene::textured(WIDTH, HEIGHT, 1).unwrap();
    let distinct = generate(&sharp, &depth, &StackParams { blur_per_depth: 8.0, max_blur: 8.0, ..StackParams::identity(8) }).unwrap().frames;
    drop((sharp, depth));
    let frame_bytes = WIDTH * HEIGHT * 3 * 4;
    let mib = |b: usize| b as f64 / f64::from(1 << 20);
    // The bytes a method may hold at its peak besides the frame: the weighted average's accumulators (16 bytes a
    // pixel for RGB), the pyramid's (33 bytes a pixel, its tiles' inner regions a little larger than their cores) and
    // whole-canvas part, the result, and the scratch of the tiles in flight; far from the 200 frames' 352 MiB.
    //
    // Every method runs on one thread. With several, a frame's tiles in flight overlap their scratch planes more or
    // less from frame to frame, as the threads happen to run, and a longer stack only has more chances to meet the
    // worst overlap: nothing grows, but the peaks differ. Measured: the pyramid at 46.46 MiB for 20 frames and 47.00
    // for 200 on 16 threads; the weighted average at 20.88 and 21.76 MiB on the 4-core Windows CI runner, though 21.76
    // for both on 4 threads elsewhere; nothing left allocated after any of them. On one thread every frame allocates in
    // the same order.
    for (name, bound, peak) in [
        ("weighted average", 8 * frame_bytes, on_one_thread(|| measure_pair(&AverageSettings::default(), &distinct))),
        ("pyramid", 20 * frame_bytes, on_one_thread(|| measure_pair(&PyramidSettings::default(), &distinct))),
        // Both passes: pass 1's records (32 bytes a pixel), the focus statistics, the depth map and the confidence map
        // (4 bytes a pixel each), and pass 2's sums (32 bytes a pixel for RGB).
        ("depth map", 12 * frame_bytes, on_one_thread(|| (measure_depth(&distinct, 20), measure_depth(&distinct, 200)))),
    ] {
        let ((shallow, left_shallow), (deep, left_deep)) = peak;
        println!(
            "{name}: peak heap {:.2} MiB for 20 frames, {:.2} MiB for 200 ({:+} bytes); one frame is {:.2} MiB; left after: {left_shallow} and {left_deep} bytes",
            mib(shallow),
            mib(deep),
            deep.cast_signed() - shallow.cast_signed(),
            mib(frame_bytes)
        );
        // Flat: ten times the frames add less than 1% (rayon's and the test harness's own bookkeeping vary a little).
        assert!(deep <= shallow + shallow / 100, "{name}: {shallow} bytes for 20 frames, {deep} for 200");
        assert!(deep < bound, "{name}: {deep} bytes");
    }
}

/// The peak heap, in bytes, while stacking `depth` frames from `distinct` by the depth-map method, each pass making
/// its frames afresh, and the bytes left at the end.
fn measure_depth(distinct: &[Image<f32>], depth: usize) -> (usize, usize) {
    let profiler = dhat::Profiler::builder().testing().build();
    let settings = DepthSettings::default();
    let tiling = Tiling { tile_size: 256, ..Tiling::default() };
    let shape = StackShape { width: WIDTH, height: HEIGHT, channels: 3 };
    let progress = |i: usize| FrameProgress { inner: &Unmonitored, frame: i as u64, frames: depth as u64 };
    let mut estimator = settings.estimator(&tiling).unwrap();
    estimator.begin(shape).unwrap();
    for i in 0..depth {
        let frame = distinct[i % distinct.len()].clone();
        estimator.add_frame(&frame, &progress(i)).unwrap();
        drop(frame);
    }
    let stats = estimator.finish(&Unmonitored).unwrap();
    drop(estimator);
    let map = stats.resolve(settings.contrast_threshold(), settings.smoothing, None).unwrap();
    drop(stats);
    let mut compositor = map.compositor(&tiling).unwrap();
    compositor.begin(shape).unwrap();
    for i in 0..depth {
        let frame = distinct[i % distinct.len()].clone();
        compositor.add_frame(&frame, &progress(i)).unwrap();
        drop(frame);
    }
    let result = compositor.finish(&Unmonitored).unwrap();
    assert_eq!(result.frames, depth);
    drop((result, compositor, map));
    let stats = dhat::HeapStats::get();
    drop(profiler);
    (stats.max_bytes, stats.curr_bytes)
}

/// `f`, run on a pool of one thread.
fn on_one_thread<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap().install(f)
}

/// The peak heap of stacks of 20 and of 200 frames.
fn measure_pair(method: &impl Method, distinct: &[Image<f32>]) -> ((usize, usize), (usize, usize)) {
    (measure(method, distinct, 20), measure(method, distinct, 200))
}
