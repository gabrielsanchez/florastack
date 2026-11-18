//! Frames off the stack's trend: on a rail, or a lens focused in steady steps, each frame's shift, scale and rotation
//! change smoothly from one frame to the next (focus breathing changes the scale a little with every step, a rail
//! moves the camera by the same amount). A frame far from what its neighbors predict, such as one shot as the tripod
//! was knocked, or one whose alignment slipped, needs a look: [`off_trend`] finds them, for
//! [`Flag::OffTrend`](crate::Flag::OffTrend). It
//! only flags; the user decides whether to leave the frame out.
//!
//! Each frame's terms are predicted from up to [`HALF_WINDOW`] frames that aligned on each side of it, by a straight
//! line through them, in the frames' places in the stack, that leaves the frame itself out. Its residual is how far the
//! difference between its transform and the predicted one moves the frame's corner: the shift's, plus the scale's and
//! the rotation's at the corner's distance from the center. A frame is off the trend when its residual is both
//! [`FACTOR`] times the stack's median residual (fitted without that frame, which would pull its neighbors'
//! predictions along with it), so that a stack shot by hand, whose every frame wanders, has none, and
//! at least [`FLOOR`] pixels of the working resolution, so that a stack on a rail whose frames are in register to a
//! hundredth of a pixel doesn't flag noise. The worst frame is taken out and the others fitted again without it, one
//! frame at a time, so that a frame far off doesn't pull its neighbors' predictions along with it. Stacks of fewer
//! than [`MIN_FRAMES`] frames that aligned have no trend to tell.

use crate::model::Similarity;

/// The frames on each side a frame's terms are predicted from.
pub(crate) const HALF_WINDOW: usize = 3;
/// The fewest frames that aligned for a trend to be told: a frame and its window, one side of it at the ends.
pub(crate) const MIN_FRAMES: usize = 2 * HALF_WINDOW + 1;
/// How many times the stack's median residual a frame's must be, at least, to be off the trend.
pub(crate) const FACTOR: f64 = 6.0;
/// The least residual of a frame off the trend, in pixels at alignment's working resolution.
pub(crate) const FLOOR: f64 = 1.0;
/// At most one frame in this many is flagged: beyond it, the stack has no trend to be off.
const MAX_SHARE: usize = 4;

/// A frame's terms, as a line fits them: the shift in pixels, the logarithm of the scale and the rotation in radians.
fn terms(s: &Similarity) -> [f64; 4] {
    [s.shift_x, s.shift_y, s.scale.max(f64::MIN_POSITIVE).ln(), s.rotation_deg.to_radians()]
}

/// The value at `at` of the least-squares line through `points` (place, value); their mean when they are all at one
/// place, and `None` without points.
fn line_at(points: &[(f64, f64)], at: f64) -> Option<f64> {
    let n = points.len() as f64;
    if points.is_empty() {
        return None;
    }
    let (mx, my) = (points.iter().map(|p| p.0).sum::<f64>() / n, points.iter().map(|p| p.1).sum::<f64>() / n);
    let sxx: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    Some(my + slope * (at - mx))
}

/// Each frame of `kept` (its place in the stack and its terms, in stack order), with its residual against the line
/// through the frames of its window, itself left out: how far the difference moves a corner `radius` pixels from the
/// center.
fn residuals(kept: &[&(usize, [f64; 4])], radius: f64) -> Vec<(usize, f64)> {
    (0..kept.len())
        .filter_map(|k| {
            let (i, own) = kept.get(k)?;
            // The window about the frame; at the ends, as many frames from the other side.
            let lo = k.saturating_sub(HALF_WINDOW).min(kept.len().saturating_sub(MIN_FRAMES));
            let hi = (lo + MIN_FRAMES).min(kept.len());
            let window: Vec<&(usize, [f64; 4])> = kept.get(lo..hi)?.iter().copied().filter(|(j, _)| j != i).collect();
            let predicted: Option<Vec<f64>> =
                (0..4).map(|t| line_at(&window.iter().map(|(j, v)| (*j as f64, v[t])).collect::<Vec<_>>(), *i as f64)).collect();
            let d: Vec<f64> = predicted?.iter().zip(own).map(|(p, v)| v - p).collect();
            let (sx, sy, ls, rot) = (d.first()?, d.get(1)?, d.get(2)?, d.get(3)?);
            Some((*i, sx.hypot(*sy) + radius * ls.hypot(*rot)))
        })
        .collect()
}

/// The frames off the trend (see the module documentation), by their index in `frames`: each frame's place in the
/// stack and its transform, `None` for a frame that didn't align (a rejected one, whose transform is its
/// reference's); `width` x `height` the frames' size, `resolution` the longer side alignment works at.
pub(crate) fn off_trend(frames: &[Option<Similarity>], width: usize, height: usize, resolution: usize) -> Vec<usize> {
    let aligned: Vec<(usize, [f64; 4])> = frames.iter().enumerate().filter_map(|(i, s)| s.as_ref().map(|s| (i, terms(s)))).collect();
    if aligned.len() < MIN_FRAMES || aligned.iter().any(|(_, t)| t.iter().any(|v| !v.is_finite())) {
        return Vec::new();
    }
    let radius = (width as f64 / 2.0).hypot(height as f64 / 2.0);
    let side = width.max(height) as f64;
    let floor = FLOOR * (side / (resolution.max(1) as f64)).max(1.0);
    let most = (aligned.len() / MAX_SHARE).max(1);
    let mut out: Vec<usize> = Vec::new();
    while out.len() < most {
        let kept: Vec<&(usize, [f64; 4])> = aligned.iter().filter(|(i, _)| !out.contains(i)).collect();
        let Some((worst, residual)) = residuals(&kept, radius).into_iter().max_by(|a, b| a.1.total_cmp(&b.1)) else { break };
        // The stack's own residuals without the worst frame, which would pull its neighbors' predictions along.
        let others: Vec<&(usize, [f64; 4])> = kept.iter().copied().filter(|(i, _)| *i != worst).collect();
        let mut sorted: Vec<f64> = residuals(&others, radius).into_iter().map(|r| r.1).collect();
        sorted.sort_by(f64::total_cmp);
        let Some(&median) = sorted.get(sorted.len() / 2) else { break };
        if residual < floor || residual < FACTOR * median {
            break;
        }
        out.push(worst);
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_wraps)] // The frames' type, where `None` is a rejected frame.
    fn similarity(shift_x: f64, shift_y: f64, scale: f64) -> Option<Similarity> {
        Some(Similarity { shift_x, shift_y, scale, rotation_deg: 0.0 })
    }

    #[test]
    fn a_frame_off_a_steady_ramp_is_flagged_and_its_neighbors_are_not() {
        // A rail: 0.8 px and 0.2% of scale a frame, which a line follows exactly.
        let ramp: Vec<Option<Similarity>> = (0..15).map(|i| similarity(0.8 * f64::from(i), 0.1, 1.0 + 0.002 * f64::from(i))).collect();
        assert_eq!(off_trend(&ramp, 6000, 4000, 2048), Vec::<usize>::new());
        for jolted in [0, 1, 7, 14] {
            let mut frames = ramp.clone();
            // 4 px of a 6000-pixel frame, 1.4 px at the working resolution.
            frames[jolted] = frames[jolted].map(|s| Similarity { shift_y: s.shift_y + 4.0, ..s });
            assert_eq!(off_trend(&frames, 6000, 4000, 2048), [jolted], "frame {jolted}");
        }
        // Below the floor, even far from a perfect line, it isn't.
        let mut frames = ramp.clone();
        frames[7] = frames[7].map(|s| Similarity { shift_y: s.shift_y + 2.0, ..s });
        assert_eq!(off_trend(&frames, 6000, 4000, 2048), Vec::<usize>::new());
        // A rejected frame is left out of the fits, and the stack's trend still told around it.
        let mut frames = ramp.clone();
        frames[3] = None;
        frames[9] = frames[9].map(|s| Similarity { scale: s.scale * 1.002, ..s });
        assert_eq!(off_trend(&frames, 6000, 4000, 2048), [9]);
    }

    #[test]
    fn a_stack_shot_by_hand_or_too_short_has_no_trend_to_be_off() {
        // Every frame wanders by up to 3 px: no frame stands out from the others.
        let mut seed = 7_u64;
        let mut next = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (seed >> 11) as f64 / (1_u64 << 53) as f64 * 6.0 - 3.0
        };
        let wandering: Vec<Option<Similarity>> = (0..20).map(|_| similarity(next(), next(), 1.0)).collect();
        assert_eq!(off_trend(&wandering, 1024, 768, 1024), Vec::<usize>::new());
        let short: Vec<Option<Similarity>> = (0..6).map(|i| similarity(if i == 3 { 20.0 } else { 0.0 }, 0.0, 1.0)).collect();
        assert_eq!(off_trend(&short, 1024, 768, 1024), Vec::<usize>::new());
    }
}
