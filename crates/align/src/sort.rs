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

/// The frames of `alignment` sorted by their scale: `order[k]` is the index, in the present order, of the frame that
/// goes to position `k`. See the module documentation.
pub fn scale_order(alignment: &StackAlignment) -> Result<Vec<usize>, AlignError> {
    let n = alignment.frames.len();
    let scales: Vec<f64> = alignment.frames.iter().map(|f| if f.similarity.scale.is_finite() { f.similarity.scale } else { 1.0 }).collect();
    let (lo, hi) = scales.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &s| (lo.min(s), hi.max(s)));
    let c = center(alignment.width, alignment.height);
    let spread = if n < 2 { 0.0 } else { (hi - lo) * c.0.hypot(c.1).max(1.0) };
    if spread < MIN_SPREAD {
        return Err(AlignError::NoScaleChange { spread });
    }
    // The present trend: the covariance of position and scale.
    let mean_i = (n - 1) as f64 / 2.0;
    let mean_s = scales.iter().sum::<f64>() / n as f64;
    let trend: f64 = scales.iter().enumerate().map(|(i, s)| (i as f64 - mean_i) * (s - mean_s)).sum();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let (sa, sb) = (scales.get(a).copied().unwrap_or(1.0), scales.get(b).copied().unwrap_or(1.0));
        let by_scale = if trend < 0.0 { sb.total_cmp(&sa) } else { sa.total_cmp(&sb) };
        by_scale.then(a.cmp(&b))
    });
    Ok(order)
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
