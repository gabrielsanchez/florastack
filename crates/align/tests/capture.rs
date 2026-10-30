//! Incremental capture keeps the same neighbor chain as alignment of the complete stack from its first frame.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use florastack_align::{AlignSettings, StackAligner, align_frames};
use florastack_core::Unmonitored;
use florastack_core::testutil::{StackParams, generate, scene};

#[test]
fn capture_snapshots_preserve_the_chain_and_match_full_alignment() {
    let (sharp, depth) = scene::textured(96, 80, 7).unwrap();
    let stack = generate(&sharp, &depth, &StackParams::clean(4, 7)).unwrap();
    let settings = AlignSettings::default();
    let mut capture = StackAligner::new(&settings, 1, 0).unwrap();
    for (i, frame) in stack.frames.iter().enumerate() {
        if i == 0 {
            capture.add(frame, &Unmonitored).unwrap();
        } else {
            capture.append(frame, &Unmonitored).unwrap();
        }
        let snapshot = capture.snapshot().unwrap();
        let full = align_frames(&stack.frames[..=i], 0, &settings, &Unmonitored).unwrap();
        assert_eq!(snapshot.frames.len(), i + 1);
        for (incremental, reference) in snapshot.frames.iter().zip(&full.frames) {
            // Both paths perform exactly the same operations; a snapshot must not mutate the live state.
            assert_eq!(incremental.transform, reference.transform);
            assert_eq!(incremental.gain, reference.gain);
            assert_eq!(incremental.flags, reference.flags);
            assert_eq!(incremental.sharp_fraction, reference.sharp_fraction);
        }
    }
    assert_eq!(capture.finish().unwrap().frames.len(), 4);
}
