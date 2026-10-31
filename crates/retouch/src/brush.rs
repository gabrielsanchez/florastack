//! The brushes' arithmetic. Every pixel a stroke paints is computed from that pixel of the result before the stroke,
//! the stroke's mask there, and two images that painting never changes: the source and the output as it was before
//! any retouching (the base). What a pixel needs from around it, the Details brush's matching and adaptive hardness,
//! comes from those two images alone, computed the same way whatever region is being painted. So a stroke gives the
//! same pixels painted whole, tile by tile, live as it is drawn, or replayed over the region an undo uncovers.
//!
//! - **Mask**: each dab's profile, 1 out to its hardness (a share of its radius) and fading smoothly (a smoothstep)
//!   to 0 at its edge; a pixel takes the strongest dab that reaches it, which no order of the dabs changes.
//! - **Matching** (Details): the source's channels are scaled by the ratio of the base's sums to the source's over a
//!   window about the brush's size, so that a frame a little brighter or tinted than the output paints seamlessly.
//!   The sums are taken over blocks on a grid fixed to the image's origin, in fixed point, exactly, then over the
//!   blocks around, and the ratios are interpolated between block centers; the ratio is held within 1/2 and 2.
//! - **Adaptive hardness** (Details): where the source or the base has fine detail (its luminance's Laplacian, large
//!   for its brightness, anywhere in the pixel's 3 x 3 neighborhood), the edge is hard, so that detail is painted
//!   whole rather than ghosted; where there is little, the brush keeps its own hardness, so that it blends.

use florastack_core::{Image, Rect};

use crate::RetouchError;
use crate::stroke::{Brush, Dab};

/// The images a stroke reads, which painting never changes: the output before any retouching, and the stroke's source
/// with how much of each pixel a frame covers (a frame warped onto the canvas covers it partly at its edges).
#[derive(Clone, Copy)]
pub struct Fields<'a> {
    pub base: &'a Image<f32>,
    pub source: Option<&'a Image<f32>>,
    pub coverage: Option<&'a Image<f32>>,
}

/// Luminance weights of the working space's primaries (Rec. 2020, which the RAW develop step gives; close enough for
/// the comparisons here in any linear RGB).
const LUMA: [f32; 3] = [0.2627, 0.678, 0.0593];
/// Fixed point for the matching's sums: values clipped to ±8, times 2^20.
const FIXED: f64 = 1_048_576.0;
const FIXED_CLIP: f32 = 8.0;
/// How far from 1 the matching's ratio may go.
const MIN_GAIN: f64 = 0.5;
const MAX_GAIN: f64 = 2.0;
/// Below this sum (in fixed point, per pixel of the window) a channel is too dark to match: its ratio stays 1.
const DARK: f64 = 1e-4 * FIXED;
/// The relative contrast at which adaptive hardness starts to harden the edge, and where the edge is fully hard.
const DETAIL_SOFT: f32 = 0.03;
const DETAIL_HARD: f32 = 0.25;
/// How many blocks on each side of a block its matching window spans.
const WINDOW_BLOCKS: isize = 2;

/// An image's channels: how many hold color (1 or 3) and which, if any, is alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub color: usize,
    pub alpha: Option<usize>,
}

impl Layout {
    pub(crate) fn of(image: &Image<f32>) -> Result<Layout, RetouchError> {
        match image.channels() {
            1 => Ok(Layout { color: 1, alpha: None }),
            2 => Ok(Layout { color: 1, alpha: Some(1) }),
            3 => Ok(Layout { color: 3, alpha: None }),
            4 => Ok(Layout { color: 3, alpha: Some(3) }),
            n => Err(RetouchError::Mismatch { what: format!("an image with {n} channels") }),
        }
    }
}

fn luminance(px: &[f32], color: usize) -> f32 {
    if color == 3 { LUMA[0] * px[0] + LUMA[1] * px[1] + LUMA[2] * px[2] } else { px[0] }
}

/// A dab's mask at `t` of its radius from its center, with `hardness`.
pub(crate) fn profile(t: f32, hardness: f32) -> f32 {
    if t >= 1.0 {
        0.0
    } else if t <= hardness {
        1.0
    } else {
        let s = (1.0 - t) / (1.0 - hardness);
        s * s * (3.0 - 2.0 * s)
    }
}

fn smoothstep(edge0: f32, edge1: f32, v: f32) -> f32 {
    let s = ((v - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    s * s * (3.0 - 2.0 * s)
}

/// What a stroke needs over a region, from the fields alone: each pixel's hardness and matching ratios.
pub(crate) struct Prepared {
    pub rect: Rect,
    /// Per pixel of `rect`, row by row.
    pub hardness: Vec<f32>,
    /// Per pixel of `rect`: the ratio for each color channel (all 1 but for Details).
    pub gains: Vec<[f32; 3]>,
}

/// Prepares a stroke with `brush`, `size` and `hardness` over `rect` of the fields.
pub(crate) fn prepare(fields: &Fields<'_>, brush: Brush, size: f64, hardness: f64, rect: Rect) -> Result<Prepared, RetouchError> {
    let n = rect.width * rect.height;
    #[allow(clippy::cast_possible_truncation)] // From 0 to 1.
    let base_hardness = hardness as f32;
    let (Brush::Details, Some(source)) = (brush, fields.source) else {
        return Ok(Prepared { rect, hardness: vec![base_hardness; n], gains: vec![[1.0; 3]; n] });
    };
    let layout = Layout::of(fields.base)?;
    let detail = detail(fields.base, source, layout, rect);
    let hardness = detail.iter().map(|&d| base_hardness + (1.0 - base_hardness) * smoothstep(DETAIL_SOFT, DETAIL_HARD, d)).collect();
    let gains = gains(fields.base, source, layout, size, rect);
    Ok(Prepared { rect, hardness, gains })
}

/// The relative contrast of `image`'s luminance at `(x, y)` (coordinates clamped to the image): its Laplacian over
/// its brightness.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, clippy::cast_possible_truncation)] // Image sides and block indices, far below isize::MAX; clamped before they index.
fn contrast(image: &Image<f32>, layout: Layout, x: isize, y: isize) -> f32 {
    let (w, h) = (image.width() as isize, image.height() as isize);
    let y_at = |x: isize, y: isize| {
        #[allow(clippy::cast_sign_loss)] // Clamped to the image.
        let (cx, cy) = (x.clamp(0, w - 1) as usize, y.clamp(0, h - 1) as usize);
        image.pixel(cx, cy).map_or(0.0, |p| luminance(p, layout.color))
    };
    let c = y_at(x, y);
    let around = y_at(x - 1, y) + y_at(x + 1, y) + y_at(x, y - 1) + y_at(x, y + 1);
    let laplacian = (4.0 * c - around).abs();
    let mean = (c + around) / 5.0;
    laplacian / (mean.abs() + 0.02)
}

/// Each pixel of `rect`'s detail: the largest relative contrast of the base or the source in its 3 x 3 neighborhood.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, clippy::cast_possible_truncation)] // Image sides and block indices, far below isize::MAX; clamped before they index.
fn detail(base: &Image<f32>, source: &Image<f32>, layout: Layout, rect: Rect) -> Vec<f32> {
    // The contrast over the rectangle and a pixel around it, then the 3 x 3 maximum.
    let (cw, ch) = (rect.width + 2, rect.height + 2);
    let mut c = vec![0.0f32; cw * ch];
    for (j, row) in c.chunks_exact_mut(cw).enumerate() {
        let y = rect.y as isize + j as isize - 1;
        for (i, v) in row.iter_mut().enumerate() {
            let x = rect.x as isize + i as isize - 1;
            *v = contrast(base, layout, x, y).max(contrast(source, layout, x, y));
        }
    }
    let mut out = Vec::with_capacity(rect.width * rect.height);
    for j in 1..=rect.height {
        for i in 1..=rect.width {
            let mut m = 0.0f32;
            for dj in [j - 1, j, j + 1] {
                for di in [i - 1, i, i + 1] {
                    m = m.max(c[dj * cw + di]);
                }
            }
            out.push(m);
        }
    }
    out
}

/// A value in the matching's fixed point.
#[allow(clippy::cast_possible_truncation)] // Clipped to ±8 times 2^20 first.
fn fixed(v: f32) -> i64 {
    if v.is_finite() { (f64::from(v.clamp(-FIXED_CLIP, FIXED_CLIP)) * FIXED).round() as i64 } else { 0 }
}

/// The side of the matching's blocks for a brush of `size`: a quarter of it, at least 2 pixels.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // From 1 to 1000.
fn block_side(size: f64) -> usize {
    (size / 4.0).round().clamp(2.0, 1000.0) as usize
}

/// Every pixel of `rect`'s matching ratios. See the module documentation.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, clippy::cast_possible_truncation)] // Image sides and block indices, far below isize::MAX; clamped before they index.
fn gains(base: &Image<f32>, source: &Image<f32>, layout: Layout, size: f64, rect: Rect) -> Vec<[f32; 3]> {
    let b = block_side(size);
    let (w, h) = (base.width(), base.height());
    let (blocks_x, blocks_y) = (w.div_ceil(b) as isize, h.div_ceil(b) as isize);
    // The blocks whose centers interpolate the rectangle's pixels, and the windows around those.
    let reach = WINDOW_BLOCKS + 1;
    let bx0 = (rect.x / b) as isize - reach;
    let by0 = (rect.y / b) as isize - reach;
    let bx1 = ((rect.x + rect.width) / b) as isize + reach + 1;
    let by1 = ((rect.y + rect.height) / b) as isize + reach + 1;
    let (bx0, by0, bx1, by1) = (bx0.max(0), by0.max(0), bx1.min(blocks_x), by1.min(blocks_y));
    let (nx, ny) = ((bx1 - bx0) as usize, (by1 - by0) as usize);
    // Each block's sums of the base's and the source's color channels, over the whole block.
    let mut sums = vec![([0i64; 3], [0i64; 3]); nx * ny];
    for j in 0..ny {
        for i in 0..nx {
            let (x0, y0) = ((bx0 as usize + i) * b, (by0 as usize + j) * b);
            let (x1, y1) = ((x0 + b).min(w), (y0 + b).min(h));
            let mut s = ([0i64; 3], [0i64; 3]);
            for y in y0..y1 {
                let (Some(br), Some(sr)) = (base.row(y), source.row(y)) else { continue };
                let n = base.channels();
                let m = source.channels();
                for x in x0..x1 {
                    for c in 0..layout.color {
                        s.0[c] += br.get(x * n + c).copied().map_or(0, fixed);
                        s.1[c] += sr.get(x * m + c).copied().map_or(0, fixed);
                    }
                }
            }
            sums[j * nx + i] = s;
        }
    }
    // Each block's ratios, from the sums over the blocks around it (all within the range computed above, or beyond
    // the image, which has none).
    let ratio_at = |bx: isize, by: isize| -> [f32; 3] {
        let (mut base_sum, mut source_sum) = ([0i64; 3], [0i64; 3]);
        for y in (by - WINDOW_BLOCKS).max(0)..(by + WINDOW_BLOCKS + 1).min(blocks_y) {
            for x in (bx - WINDOW_BLOCKS).max(0)..(bx + WINDOW_BLOCKS + 1).min(blocks_x) {
                let (i, j) = (x - bx0, y - by0);
                if i < 0 || j < 0 || i >= nx as isize || j >= ny as isize {
                    continue;
                }
                let (bs, ss) = sums[j as usize * nx + i as usize];
                for c in 0..3 {
                    base_sum[c] += bs[c];
                    source_sum[c] += ss[c];
                }
            }
        }
        let mut r = [1.0f32; 3];
        for c in 0..layout.color {
            #[allow(clippy::cast_precision_loss)] // Sums of fixed-point values, as ratios.
            let (bsum, ssum) = (base_sum[c] as f64, source_sum[c] as f64);
            let pixels = (b * b) as f64;
            if ssum > DARK * pixels && bsum > DARK * pixels {
                #[allow(clippy::cast_possible_truncation)] // From 0.5 to 2.
                let g = (bsum / ssum).clamp(MIN_GAIN, MAX_GAIN) as f32;
                r[c] = g;
            }
        }
        if layout.color == 1 {
            r = [r[0]; 3];
        }
        r
    };
    // The block ratios that interpolation reads, then each pixel's.
    let (rx0, ry0) = ((rect.x / b) as isize - 1, (rect.y / b) as isize - 1);
    let (rx1, ry1) = (((rect.x + rect.width) / b + 1) as isize, ((rect.y + rect.height) / b + 1) as isize);
    let (rnx, rny) = ((rx1 - rx0 + 1) as usize, (ry1 - ry0 + 1) as usize);
    let clamp_x = |bx: isize| bx.clamp(0, blocks_x - 1);
    let clamp_y = |by: isize| by.clamp(0, blocks_y - 1);
    let mut ratios = Vec::with_capacity(rnx * rny);
    for by in ry0..=ry1 {
        for bx in rx0..=rx1 {
            ratios.push(ratio_at(clamp_x(bx), clamp_y(by)));
        }
    }
    let block = b as f32;
    let mut out = Vec::with_capacity(rect.width * rect.height);
    for y in rect.y..rect.y + rect.height {
        let v = (y as f32 + 0.5) / block - 0.5;
        let (j0, ty) = (v.floor() as isize, v - v.floor());
        for x in rect.x..rect.x + rect.width {
            let u = (x as f32 + 0.5) / block - 0.5;
            let (i0, tx) = (u.floor() as isize, u - u.floor());
            let at = |i: isize, j: isize| {
                let (i, j) = ((i - rx0).clamp(0, rnx as isize - 1) as usize, (j - ry0).clamp(0, rny as isize - 1) as usize);
                ratios[j * rnx + i]
            };
            let (a, bb, c, d) = (at(i0, j0), at(i0 + 1, j0), at(i0, j0 + 1), at(i0 + 1, j0 + 1));
            let mut g = [1.0f32; 3];
            for k in 0..3 {
                let top = a[k] + (bb[k] - a[k]) * tx;
                let bottom = c[k] + (d[k] - c[k]) * tx;
                g[k] = top + (bottom - top) * ty;
            }
            out.push(g);
        }
    }
    out
}

/// Adds `dab` to `mask`, the mask of `rect` (row by row), with each pixel's hardness from `hardness`: a pixel keeps
/// the stronger of what it had and the dab's profile.
pub(crate) fn rasterize(dab: &Dab, rect: Rect, hardness: &[f32], mask: &mut [f32]) {
    let r = dab.radius;
    let Some(reach) = crate::stroke::clip_rect(dab.x - r, dab.y - r, dab.x + r, dab.y + r, usize::MAX, usize::MAX) else { return };
    let Some(area) = crate::stroke::intersect(reach, rect) else { return };
    for y in area.y..area.y + area.height {
        let dy = y as f64 + 0.5 - dab.y;
        let row = (y - rect.y) * rect.width;
        for x in area.x..area.x + area.width {
            let dx = x as f64 + 0.5 - dab.x;
            #[allow(clippy::cast_possible_truncation)] // A share of the radius.
            let t = (dx.hypot(dy) / r) as f32;
            let i = row + x - rect.x;
            if let (Some(m), Some(&h)) = (mask.get_mut(i), hardness.get(i)) {
                *m = m.max(profile(t, h));
            }
        }
    }
}

/// `pre` moved toward `target` by `w`: `pre + (target - pre) * w`, and the target itself at full weight, so that a pixel
/// painted with what it already holds stays exactly as it was.
fn mix(pre: f32, target: f32, w: f32) -> f32 {
    if w >= 1.0 { target } else { pre + (target - pre) * w }
}

/// The brushes' one formula for a pixel, into `out`: `pre` (the result before the stroke) painted with `brush` at
/// weight `weight` (the mask times the opacity, times the source's coverage) toward the pixel's `source`, its ratios
/// `gain` applied for Details, or toward `base` for the eraser; Darken and Lighten paint only where the source's
/// luminance is below or above the result's.
#[allow(clippy::too_many_arguments)] // A pixel's inputs.
pub(crate) fn paint(
    brush: Brush,
    layout: Layout,
    pre: &[f32],
    source: Option<&[f32]>,
    base: &[f32],
    coverage: f32,
    weight: f32,
    gain: [f32; 3],
    out: &mut [f32],
) {
    out.copy_from_slice(pre);
    let source_layout_alpha = |s: &[f32]| if s.len() == layout.color + 1 { s.get(layout.color).copied() } else { None };
    let (from, mut w) = match (brush, source) {
        (Brush::Eraser, _) => (base, weight),
        (_, Some(s)) => {
            // A source's own alpha covers like a frame's coverage.
            let a = if layout.alpha.is_none() { source_layout_alpha(s).unwrap_or(1.0) } else { 1.0 };
            (s, weight * coverage * a.clamp(0.0, 1.0))
        }
        (_, None) => return,
    };
    match brush {
        Brush::Darken if luminance(from, layout.color) >= luminance(pre, layout.color) => w = 0.0,
        Brush::Lighten if luminance(from, layout.color) <= luminance(pre, layout.color) => w = 0.0,
        _ => {}
    }
    if w <= 0.0 {
        return;
    }
    for c in 0..layout.color {
        let target = if brush == Brush::Details { from[c] * gain[c] } else { from[c] };
        out[c] = mix(pre[c], target, w);
    }
    if let Some(a) = layout.alpha {
        out[a] = mix(pre[a], from.get(a).copied().unwrap_or(1.0), w);
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Exactly 1, and exactly as before: what these tests are about.
mod tests {
    use super::*;
    use florastack_core::ColorSpace;

    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 3]) -> Image<f32> {
        let f = &f;
        let data = (0..h).flat_map(|y| (0..w).flat_map(move |x| f(x, y))).collect();
        Image::from_vec(w, h, 3, ColorSpace::LINEAR, data).unwrap()
    }

    #[test]
    fn the_profile_is_full_inside_the_hardness_and_fades_to_the_edge() {
        assert_eq!((profile(0.0, 0.5), profile(0.5, 0.5), profile(1.0, 0.5)), (1.0, 1.0, 0.0));
        assert!(profile(0.75, 0.5) > 0.0 && profile(0.75, 0.5) < 1.0);
        assert_eq!(profile(0.999, 1.0), 1.0, "a hard brush has no fade");
        assert!(profile(0.1, 0.0) < 1.0, "a soft brush fades from its center");
    }

    #[test]
    fn matching_scales_a_brighter_source_to_the_base_and_leaves_an_identical_one() {
        let base = image(64, 48, |x, y| [0.2 + 0.001 * x as f32, 0.3, 0.1 + 0.002 * y as f32]);
        let bright = image(64, 48, |x, y| [(0.2 + 0.001 * x as f32) * 1.5, 0.3 * 1.5, (0.1 + 0.002 * y as f32) * 1.5]);
        let layout = Layout::of(&base).unwrap();
        let rect = Rect::new(10, 10, 20, 20);
        for g in gains(&base, &bright, layout, 16.0, rect) {
            assert!(g.iter().all(|&g| (g - 1.0 / 1.5).abs() < 1e-3), "{g:?}");
        }
        assert!(gains(&base, &base, layout, 16.0, rect).iter().all(|g| *g == [1.0; 3]), "exactly 1 for an identical source");
    }

    #[test]
    fn a_pixels_ratio_doesnt_depend_on_the_region_painted() {
        let base = image(64, 48, |x, y| [((x * 7 + y * 3) % 13) as f32 / 13.0, 0.4, 0.2]);
        let source = image(64, 48, |x, y| [((x * 5 + y * 11) % 17) as f32 / 17.0, 0.3, 0.25]);
        let layout = Layout::of(&base).unwrap();
        let whole = gains(&base, &source, layout, 10.0, Rect::new(0, 0, 64, 48));
        let part = gains(&base, &source, layout, 10.0, Rect::new(17, 9, 13, 21));
        for (j, row) in part.chunks(13).enumerate() {
            for (i, g) in row.iter().enumerate() {
                assert_eq!(*g, whole[(9 + j) * 64 + 17 + i]);
            }
        }
        let whole = detail(&base, &source, layout, Rect::new(0, 0, 64, 48));
        let part = detail(&base, &source, layout, Rect::new(17, 9, 13, 21));
        assert_eq!(part[0], whole[9 * 64 + 17]);
    }

    #[test]
    fn darken_and_lighten_paint_only_one_way() {
        let layout = Layout { color: 3, alpha: None };
        let mut out = [0.0; 3];
        let (pre, dark, light) = ([0.5; 3], [0.2; 3], [0.8; 3]);
        paint(Brush::Darken, layout, &pre, Some(&light), &pre, 1.0, 1.0, [1.0; 3], &mut out);
        assert_eq!(out, pre, "a lighter source doesn't darken");
        paint(Brush::Darken, layout, &pre, Some(&dark), &pre, 1.0, 1.0, [1.0; 3], &mut out);
        assert_eq!(out, dark);
        paint(Brush::Lighten, layout, &pre, Some(&dark), &pre, 1.0, 1.0, [1.0; 3], &mut out);
        assert_eq!(out, pre);
        paint(Brush::Eraser, layout, &dark, None, &pre, 1.0, 0.5, [1.0; 3], &mut out);
        assert!(out.iter().all(|v| (v - 0.35).abs() < 1e-6), "the eraser goes halfway back to the base: {out:?}");
        paint(Brush::Pixels, layout, &pre, Some(&dark), &pre, 0.0, 1.0, [1.0; 3], &mut out);
        assert_eq!(out, pre, "nothing where the source frame doesn't cover");
    }
}
