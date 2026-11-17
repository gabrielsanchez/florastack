//! Sorting a stack by scale, to repair one shot or imported out of order: a stack's magnification changes steadily
//! with its focus (focus breathing, or a rail moving the camera), so the frames' recovered scales put them back in
//! focus order.
//!
//! 1. **Measuring**: a first alignment pass in the stack's present order ([`measuring_settings`]): scale on, whatever
//!    the settings say, and nothing rejected, since neighbors out of order differ more than neighbors should, and a
//!    rejected frame would keep its reference's scale rather than get its own. At most 1024 pixels: only the scales'
//!    order matters.
//! 2. **Sorting** ([`scale_order`]): the frames by scale, in the direction that keeps the present order's trend (the
//!    sign of the correlation between position and scale), so that a stack with a few frames out of place keeps its
//!    direction; equal scales keep their present order. The scales must spread over at least [`MIN_SPREAD`] pixels at
//!    the frame's corner, or there is nothing to sort by.
//! 3. **Aligning again** in the new order, with the settings as they are, so that every frame is chained to its true
//!    neighbor in focus ([`align_by_scale`] for frames in memory; the engine does the same reading frames from files).
//!
//! A stack that is aligned already has its scales: [`order_of_scales`] sorts by them without a measuring pass, and
//! [`out_of_place`] tells which frames a sort would move, for a hint to sort.

use florastack_core::{Image, Monitor};

use crate::error::AlignError;
use crate::model::center;
use crate::order::{ProcessingOrder, order_stack};
use crate::settings::AlignSettings;
use crate::stack::{StackAligner, StackAlignment, align_frames};

/// The scales must spread over at least this many pixels at a frame's corner to be sorted by.
pub const MIN_SPREAD: f64 = 1.0;
/// The finest working resolution of the measuring pass.
const MEASURING_RESOLUTION: u32 = 1024;

/// The settings of the measuring pass, from the stack's settings: see the module documentation.
pub fn measuring_settings(settings: &AlignSettings) -> AlignSettings {
    AlignSettings { scale: true, reject: false, refine: false, resolution: settings.resolution.min(MEASURING_RESOLUTION), ..settings.clone() }
}

/// A frame is out of place when its scale is on the wrong side of a neighbor's by enough to move a frame's corner by
/// this many pixels: less is what alignment mistakes between frames of nearly the same scale.
pub const OUT_OF_PLACE_MIN: f64 = 0.5;

/// The frames of `alignment` sorted by their scale: `order[k]` is the index, in the present order, of the frame that
/// goes to position `k`. See the module documentation.
pub fn scale_order(alignment: &StackAlignment) -> Result<Vec<usize>, AlignError> {
    order_of_scales(&scales(alignment), alignment.width, alignment.height)
}

/// The scales of the frames of `alignment`, 1 for one that isn't finite.
fn scales(alignment: &StackAlignment) -> Vec<f64> {
    alignment.frames.iter().map(|f| f.similarity.scale).collect()
}

/// The scales as they are compared: 1 for one that isn't finite.
fn finite(scales: &[f64]) -> Vec<f64> {
    scales.iter().map(|&s| if s.is_finite() { s } else { 1.0 }).collect()
}

/// How far the scales spread, in pixels at the corner of a `width` x `height` frame, and the pixels there per unit
/// of scale.
fn spread(scales: &[f64], width: usize, height: usize) -> (f64, f64) {
    let (lo, hi) = scales.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &s| (lo.min(s), hi.max(s)));
    let c = center(width, height);
    let radius = c.0.hypot(c.1).max(1.0);
    (if scales.len() < 2 { 0.0 } else { (hi - lo) * radius }, radius)
}

/// The present trend of the scales along the stack: the covariance of position and scale, negative when they fall.
fn trend(scales: &[f64]) -> f64 {
    let n = scales.len();
    let mean_i = n.saturating_sub(1) as f64 / 2.0;
    let mean_s = scales.iter().sum::<f64>() / n.max(1) as f64;
    scales.iter().enumerate().map(|(i, s)| (i as f64 - mean_i) * (s - mean_s)).sum()
}

/// Frames of `width` x `height` pixels sorted by their `scales` (each relative to the same frame, whichever):
/// `order[k]` is the index, in the present order, of the frame that goes to position `k`. See the module
/// documentation.
pub fn order_of_scales(scales: &[f64], width: usize, height: usize) -> Result<Vec<usize>, AlignError> {
    let scales = finite(scales);
    let (spread, _) = spread(&scales, width, height);
    if spread < MIN_SPREAD {
        return Err(AlignError::NoScaleChange { spread });
    }
    let falling = trend(&scales) < 0.0;
    let mut order: Vec<usize> = (0..scales.len()).collect();
    order.sort_by(|&a, &b| {
        let (sa, sb) = (scales.get(a).copied().unwrap_or(1.0), scales.get(b).copied().unwrap_or(1.0));
        let by_scale = if falling { sb.total_cmp(&sa) } else { sa.total_cmp(&sb) };
        by_scale.then(a.cmp(&b))
    });
    Ok(order)
}

/// The frames whose scale doesn't fit their place in the stack, by their index: the fewest frames that, left out,
/// leave the others' `scales` rising or falling steadily, the way the stack's trend goes, give or take
/// [`OUT_OF_PLACE_MIN`] pixels at the corner of a `width` x `height` frame between one frame and the next. They are
/// the frames sorting by scale would move past others. None when the scales spread too little to sort by
/// ([`MIN_SPREAD`]), as on a stack shot without focus breathing.
///
/// A stack shot in order has none. A camera's focus stacking mode that shoots the frame at the focus point first
/// and then from in front of it to behind it has its first frame out of place; so has a stack whose files were
/// renamed or merged out of order. Narrow framing keeps the field of view of the frame at the stack's narrow end,
/// which on such a stack needn't be the narrowest frame.
#[must_use]
pub fn out_of_place(scales: &[f64], width: usize, height: usize) -> Vec<usize> {
    let scales = finite(scales);
    let n = scales.len();
    let (spread, radius) = spread(&scales, width, height);
    if n < 3 || spread < MIN_SPREAD {
        return Vec::new();
    }
    let sign = if trend(&scales) < 0.0 { -1.0 } else { 1.0 };
    let slack = OUT_OF_PLACE_MIN / radius;
    // The longest run of frames, in the stack's order, each of which may follow the one before: `longest[i]` is the
    // length of the longest that ends with frame `i`, and `before[i]` the frame before it there.
    let (mut longest, mut before) = (vec![1_usize; n], vec![None; n]);
    for i in 0..n {
        for j in 0..i {
            let (Some(&a), Some(&b), Some(&run)) = (scales.get(j), scales.get(i), longest.get(j)) else { continue };
            if sign * (b - a) >= -slack
                && longest.get(i).is_some_and(|&l| run + 1 > l)
                && let (Some(l), Some(p)) = (longest.get_mut(i), before.get_mut(i))
            {
                *l = run + 1;
                *p = Some(j);
            }
        }
    }
    // The first of the longest, so that the same stack always names the same frames.
    let mut last = (0..n).max_by_key(|&i| (longest.get(i).copied().unwrap_or(0), std::cmp::Reverse(i)));
    let mut in_place = vec![false; n];
    while let Some(i) = last {
        if let Some(kept) = in_place.get_mut(i) {
            *kept = true;
        }
        last = before.get(i).copied().flatten();
    }
    (0..n).filter(|&i| in_place.get(i) == Some(&false)).collect()
}

/// The frames of `alignment` whose scale doesn't fit their place in the stack ([`out_of_place`]).
#[must_use]
pub fn frames_out_of_place(alignment: &StackAlignment) -> Vec<usize> {
    out_of_place(&scales(alignment), alignment.width, alignment.height)
}

/// Sorts frames in memory by scale and aligns them in the new order (see the module documentation): the order
/// (`order[k]` is the index in `frames` of the frame at position `k`), and the alignment and processing order of the
/// frames in that order, as [`crate::align_ordered`] gives them.
pub fn align_by_scale(
    frames: &[Image<f32>],
    settings: &AlignSettings,
    monitor: &dyn Monitor,
) -> Result<(Vec<usize>, StackAlignment, ProcessingOrder), AlignError> {
    let measured = align_frames(frames, frames.len() / 2, &measuring_settings(settings), monitor)?;
    let order = scale_order(&measured)?;
    let mut aligner = StackAligner::new(settings, order.len(), order.len() / 2)?;
    while let Some(k) = aligner.next_frame() {
        let frame = order.get(k).and_then(|&i| frames.get(i)).ok_or(AlignError::Finished)?;
        aligner.add(frame, monitor)?;
    }
    let (alignment, processing) = order_stack(&aligner.finish()?, settings)?;
    Ok((order, alignment, processing))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scales of a 2000 x 1500 frame's stack, whose corner is 1250 pixels from its center: a step of 0.001 moves it
    /// by 1.25 pixels.
    fn found(scales: &[f64]) -> Vec<usize> {
        out_of_place(scales, 2000, 1500)
    }

    #[test]
    fn a_stack_in_order_has_no_frame_out_of_place() {
        assert_eq!(found(&[1.0, 1.002, 1.004, 1.006, 1.008]), Vec::<usize>::new());
        assert_eq!(found(&[1.008, 1.006, 1.004, 1.002, 1.0]), Vec::<usize>::new(), "falling");
        // Noise below half a pixel at the corner (0.0004) is not an order.
        assert_eq!(found(&[1.0, 1.002, 1.0018, 1.004, 1.0038, 1.006]), Vec::<usize>::new());
        // Nothing to sort by, and too few frames to be out of order.
        assert_eq!(found(&[1.0, 1.0003, 1.0001, 1.0002]), Vec::<usize>::new());
        assert_eq!(found(&[1.01, 1.0]), Vec::<usize>::new());
        assert_eq!(found(&[]), Vec::<usize>::new());
    }

    #[test]
    fn the_frames_a_sort_would_move_are_out_of_place() {
        // The frame at the focus point shot first, then from the nearest to the farthest.
        assert_eq!(found(&[1.005, 1.0, 1.002, 1.004, 1.006, 1.008]), vec![0]);
        assert_eq!(found(&[1.003, 1.008, 1.006, 1.004, 1.002, 1.0]), vec![0], "falling");
        // Two frames swapped: one of the two moves, and always the same one.
        assert_eq!(found(&[1.0, 1.002, 1.006, 1.004, 1.008]), vec![3]);
        // A frame that isn't finite counts as the master's scale.
        assert_eq!(found(&[1.01, 1.02, f64::NAN, 1.03, 1.04]), vec![2]);
        // Sorting puts them where they belong.
        let scales = [1.005, 1.0, 1.002, 1.004, 1.006, 1.008];
        let order = order_of_scales(&scales, 2000, 1500).unwrap();
        assert_eq!(order, vec![1, 2, 3, 0, 4, 5]);
        let sorted: Vec<f64> = order.iter().map(|&i| scales[i]).collect();
        assert_eq!(found(&sorted), Vec::<usize>::new());
    }
}
