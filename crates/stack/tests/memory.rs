//! Memory stays flat as the stack grows: the peak heap of a weighted average of 200 frames is that of 20 frames.
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
use florastack_stack::{AverageSettings, FrameProgress, Method, StackShape, Tiling};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const WIDTH: usize = 640;
const HEIGHT: usize = 480;

/// The peak heap, in bytes, while stacking `depth` frames from `distinct`, and the bytes left at the end.
fn measure(distinct: &[Image<f32>], depth: usize) -> (usize, usize) {
    let profiler = dhat::Profiler::builder().testing().build();
    let settings = AverageSettings::default();
    let mut stacker = settings.stacker(&Tiling { tile_size: 256, ..Tiling::default() }).unwrap();
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
    let (shallow, left_shallow) = measure(&distinct, 20);
    let (deep, left_deep) = measure(&distinct, 200);
    let mib = |b: usize| b as f64 / f64::from(1 << 20);
    println!(
        "peak heap: {:.2} MiB for 20 frames, {:.2} MiB for 200 ({:+} bytes); one frame is {:.2} MiB; left after: {left_shallow} and {left_deep} bytes",
        mib(shallow),
        mib(deep),
        deep.cast_signed() - shallow.cast_signed(),
        mib(frame_bytes)
    );
    // Flat: ten times the frames add less than 1% (rayon's and the test harness's own bookkeeping vary a little).
    assert!(deep <= shallow + shallow / 100, "{shallow} bytes for 20 frames, {deep} for 200");
    // And small: the accumulators (16 bytes a pixel for RGB) and the result, a frame in flight, and per-tile scratch,
    // far from the 200 frames' 352 MiB.
    assert!(deep < 8 * frame_bytes, "{deep} bytes");
}
