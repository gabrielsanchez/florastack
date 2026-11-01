//! Contribution maps must reconstruct the compositor, including partial coverage, missing support and bad samples.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use florastack_core::{ColorSpace, Image, Rect, Unmonitored};
use florastack_stack::{CoveredFrame, DepthMap, FrameSource, Patch, Tiling, stack};

#[test]
fn normalized_contributions_reconstruct_depth_compositing_and_its_fallbacks() {
    let depth = Image::from_vec(5, 1, 1, ColorSpace::LINEAR, vec![0.25, 1.5, 1.0, 1.0, 0.5]).unwrap();
    let frames = [
        Patch {
            image: Image::from_vec(5, 1, 1, ColorSpace::LINEAR, vec![0.2, 0.2, 0.2, 0.2, f32::NAN]).unwrap(),
            coverage: Some(Image::from_vec(5, 1, 1, ColorSpace::LINEAR, vec![0.5, 0.0, 1.0, 0.0, 1.0]).unwrap()),
        },
        Patch {
            image: Image::from_fn(5, 1, 1, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap(),
            coverage: Some(Image::from_vec(5, 1, 1, ColorSpace::LINEAR, vec![1.0, 0.2, 0.0, 0.0, 1.0]).unwrap()),
        },
        Patch {
            image: Image::from_fn(5, 1, 1, ColorSpace::LINEAR, |_, _, _| 0.8).unwrap(),
            coverage: Some(Image::from_vec(5, 1, 1, ColorSpace::LINEAR, vec![1.0, 1.0, 1.0, 0.0, 1.0]).unwrap()),
        },
    ];
    let mut contributions = florastack_stack::depth::Contributions::new(depth.clone(), frames.len()).unwrap();
    for frame in &frames {
        contributions.add_frame(frame).unwrap();
    }
    let maps: Vec<_> = frames.iter().enumerate().map(|(i, f)| contributions.map(i, f).unwrap()).collect();
    let map = DepthMap {
        frames: frames.len(),
        depth: Arc::new(depth),
        confidence: Image::new(5, 1, 1, ColorSpace::LINEAR).unwrap(),
        level: 0.0,
        filled: 0,
    };
    let sources: Vec<_> =
        frames.iter().map(|p| CoveredFrame::new(p.image.clone(), p.coverage.as_ref().unwrap().clone())).collect::<Result<_, _>>().unwrap();
    let mut compositor = map.compositor(&Tiling::default()).unwrap();
    let output = stack(&mut compositor, &sources, &Unmonitored).unwrap();
    for x in 0..5 {
        let sum: f32 = maps.iter().map(|m| m.get(x, 0, 0).unwrap()).sum();
        let reconstructed: f32 = maps
            .iter()
            .zip(&frames)
            .map(|(m, f)| {
                let v = f.image.get(x, 0, 0).unwrap();
                if v.is_finite() { m.get(x, 0, 0).unwrap() * v } else { 0.0 }
            })
            .sum();
        let expected_sum = if x == 3 { 0.0 } else { 1.0 };
        assert!((sum - expected_sum).abs() < 1e-6, "weights sum to {sum} at {x}");
        assert!((reconstructed - output.image.get(x, 0, 0).unwrap()).abs() < 1e-6, "contributions reconstruct the float compositor within rounding");
    }
    // No support at depth 1: frames 0 and 2 tie; the compositor chooses the first covering frame.
    assert_eq!(maps[0].get(2, 0, 0), Some(1.0));
    assert_eq!(maps[2].get(2, 0, 0), Some(0.0));
    assert!(sources[0].read(Rect::new(0, 0, 5, 1)).is_ok());
}
