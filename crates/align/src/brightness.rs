//! The brightness gain that matches a frame to the master: the median, over the parts of the scene both show, of the
//! ratio between the master's luminance and the frame's, both low-passed to a few percent of the frame's size.
//!
//! Defocus moves light around but keeps the local average (a blur's kernel sums to one), so after a strong low-pass
//! a frame focused elsewhere has the same luminance as the master, up to its gain. What the low-pass doesn't remove,
//! near the edges of strongly defocused objects, and what differs for other reasons (dust, which stays put on the
//! sensor while the scene moves, a specular highlight) are a minority of the ratios, which the median ignores. Dark
//! pixels, whose ratio noise and black-level errors dominate, are left out. The gain is measured against the master
//! directly, never along the chain, so that its errors don't add up.

use florastack_core::Affine;

use crate::prepare::Prepared;

/// Pixels this close to the brightness plane's edges are left out: its blur sees the clamped border there.
const MARGIN: usize = 2;
/// Pixels darker than this fraction of the master's mean are left out.
const DARK: f64 = 0.05;
/// The fewest ratios a gain is computed from.
const MIN_SAMPLES: usize = 16;

/// The gain that matches `frame`'s brightness to `master`'s, where `transform` maps the master's pixels to the
/// frame's; `None` when either has no brightness plane or they share too little bright content.
pub(crate) fn gain(master: &Prepared, frame: &Prepared, transform: &Affine) -> Option<f64> {
    let ((mapping, reference), (_, plane)) = (master.brightness.as_ref()?, frame.brightness.as_ref()?);
    let (w, h) = (reference.width(), reference.height());
    if plane.width() != w || plane.height() != h || w <= 2 * MARGIN || h <= 2 * MARGIN {
        return None;
    }
    let mean = reference.rows().flatten().map(|&v| f64::from(v)).sum::<f64>() / (w * h) as f64;
    if !(mean.is_finite() && mean > 0.0) {
        return None;
    }
    let floor = DARK * mean;
    let [[m0, m1, m2], [m3, m4, m5]] = mapping.level_transform(transform).m;
    let (hi_x, hi_y) = ((w - 1) as f64, (h - 1) as f64);
    let mut ratios = Vec::new();
    for y in MARGIN..h - MARGIN {
        for x in MARGIN..w - MARGIN {
            let (xf, yf) = (x as f64, y as f64);
            let (u, v) = (m0 * xf + m1 * yf + m2, m3 * xf + m4 * yf + m5);
            if !(u >= 0.0 && u <= hi_x && v >= 0.0 && v <= hi_y) {
                continue;
            }
            let (Some(m), Some(f)) = (reference.get(x, y, 0).map(f64::from), bilinear(plane, u, v)) else {
                continue;
            };
            if m > floor && f > floor {
                ratios.push(m / f);
            }
        }
    }
    if ratios.len() < MIN_SAMPLES {
        return None;
    }
    let middle = ratios.len() / 2;
    let (_, &mut median, _) = ratios.select_nth_unstable_by(middle, f64::total_cmp);
    median.is_finite().then_some(median)
}

/// `plane` at `(u, v)`, which lies inside it, interpolated bilinearly.
fn bilinear(plane: &florastack_core::Image<f32>, u: f64, v: f64) -> Option<f64> {
    let (fu, fv) = (u.floor(), v.floor());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Inside the plane: non-negative and small.
    let (x, y) = (fu as usize, fv as usize);
    let (w, h) = (plane.width(), plane.height());
    let at = |x: usize, y: usize| plane.get(x.min(w - 1), y.min(h - 1), 0).map(f64::from);
    let (tu, tv) = (u - fu, v - fv);
    let upper = at(x, y)? + tu * (at(x + 1, y)? - at(x, y)?);
    let lower = at(x, y + 1)? + tu * (at(x + 1, y + 1)? - at(x, y + 1)?);
    Some(upper + tv * (lower - upper))
}

#[cfg(test)]
mod tests {
    use florastack_core::ops::{BlurMethod, Border, Interpolation, gaussian_blur_in_place, warp};
    use florastack_core::testutil::scene;
    use florastack_core::{ColorSpace, Image};

    use super::*;
    use crate::model::Similarity;

    #[test]
    fn the_gain_ignores_defocus_motion_and_a_dark_spot() {
        let (sharp, _) = scene::textured(240, 180, 5).unwrap();
        let master = Prepared::new(&sharp, 2048, true).unwrap();
        // The frame: defocused, moved, 7% darker, with a black spot.
        let truth = Similarity { shift_x: 2.5, shift_y: -1.0, scale: 1.01, rotation_deg: 0.2 };
        let transform = truth.to_affine(240, 180);
        let mut frame = warp(&sharp, &transform.inverse().unwrap(), Interpolation::Bicubic, Border::Clamp).unwrap();
        gaussian_blur_in_place(&mut frame, 4.0, BlurMethod::Exact).unwrap();
        for (y, row) in frame.rows_mut().enumerate() {
            for (i, v) in row.iter_mut().enumerate() {
                let x = i / 3;
                *v *= if (100..130).contains(&x) && (60..90).contains(&y) { 0.0 } else { 0.93 };
            }
        }
        let frame = Prepared::new(&frame, 2048, true).unwrap();
        let g = gain(&master, &frame, &transform).unwrap();
        assert!((g - 1.0 / 0.93).abs() < 0.005, "{g}");
        let without = Prepared::new(&sharp, 2048, false).unwrap();
        assert_eq!(gain(&master, &without, &transform), None);
        let black = Prepared::new(&Image::<f32>::new(240, 180, 3, ColorSpace::LINEAR).unwrap(), 2048, true).unwrap();
        assert_eq!(gain(&black, &master, &Affine::IDENTITY), None, "no bright content");
    }
}
