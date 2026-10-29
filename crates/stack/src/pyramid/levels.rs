//! The pyramid's building blocks, the CPU references of the GPU kernels of roadmap 4.3: Burt and Adelson's REDUCE and
//! EXPAND (P. J. Burt and E. H. Adelson, "The Laplacian pyramid as a compact image code", IEEE Transactions on
//! Communications 31(4), 1983) with the 5-tap binomial generating kernel `[1, 4, 6, 4, 1] / 16` (their `a = 0.375`),
//! REDUCE with normalized convolution for frames that cover only part of the canvas (H. Knutsson and C.-F. Westin,
//! "Normalized and differential convolution", CVPR 1993), luminance, and the local energy that selection compares (the
//! "salience" of P. J. Burt and R. J. Kolczynski, "Enhanced image capture through fusion", ICCV 1993).
//!
//! # Lattices and spans
//!
//! Level `k` of a `W` x `H` canvas has `ceil(W / 2^k)` x `ceil(H / 2^k)` samples; sample `i` of level `k + 1` is
//! centered on sample `2 i` of level `k`. Outside a level the samples mirror about its edge samples (`... 2 1 | 0 1 2
//! ... n-2 n-1 | n-2 ...`). A [`Span`] is a rectangle of samples of one level, with the level's whole size, so that a
//! tile can compute a piece of each level: every operation here is defined on the whole level and evaluated on the
//! span it is given, mirroring at the level's edges and reading only samples inside the input span. A tap that would
//! fall outside the input span (which happens only within the method's context of the span's edge, where the results
//! aren't used) reads the nearest sample of the span instead.
//!
//! # Arithmetic
//!
//! Filters are separable, the horizontal pass first, and every output sums its taps in order from the first, in
//! `f32`, so a tile and the whole canvas compute every sample they share bit for bit alike, and a GPU port can
//! reproduce them within a documented tolerance. Rows are computed in parallel.

use florastack_core::{ColorSpace, Image};
use rayon::prelude::*;

use crate::error::StackError;

/// The generating kernel, `[1, 4, 6, 4, 1] / 16`: exact in `f32`, and summing to exactly 1.
pub const KERNEL: [f32; 5] = [0.0625, 0.25, 0.375, 0.25, 0.0625];

/// The Rec. 709 luminance weights (ITU-R BT.709), as the weighted average uses them.
pub const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// A rectangle of samples of one level, and the level's whole size. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    /// The level's whole width and height, in samples.
    pub full_width: usize,
    pub full_height: usize,
}

impl Span {
    /// The whole of a level of `width` x `height` samples.
    pub fn whole(width: usize, height: usize) -> Span {
        Span { x: 0, y: 0, width, height, full_width: width, full_height: height }
    }

    /// The span of the next coarser level that this one's samples reduce to. `x` and `y` must be even (lattice-aligned)
    /// for the result to hold exactly the samples centered on this span's even samples.
    #[must_use]
    pub fn coarser(self) -> Span {
        let (fw, fh) = (self.full_width.div_ceil(2), self.full_height.div_ceil(2));
        let (x, y) = (self.x / 2, self.y / 2);
        let end_x = (self.x + self.width).div_ceil(2).min(fw);
        let end_y = (self.y + self.height).div_ceil(2).min(fh);
        Span { x, y, width: end_x.saturating_sub(x).max(1), height: end_y.saturating_sub(y).max(1), full_width: fw, full_height: fh }
    }

    /// The samples `x0 .. x1`, `y0 .. y1` of this span's level, clipped to it.
    #[must_use]
    pub fn of_level(self, x0: isize, y0: isize, x1: isize, y1: isize) -> Span {
        let clip = |v: isize, n: usize| usize::try_from(v.max(0)).unwrap_or(0).min(n);
        let (x0, x1) = (clip(x0, self.full_width), clip(x1, self.full_width));
        let (y0, y1) = (clip(y0, self.full_height), clip(y1, self.full_height));
        Span {
            x: x0.min(self.full_width.saturating_sub(1)),
            y: y0.min(self.full_height.saturating_sub(1)),
            width: x1.saturating_sub(x0).max(1),
            height: y1.saturating_sub(y0).max(1),
            full_width: self.full_width,
            full_height: self.full_height,
        }
    }
}

/// `v` as a signed index (lattice sizes are far below `isize::MAX`).
pub fn signed(v: usize) -> isize {
    isize::try_from(v).unwrap_or(isize::MAX)
}

/// The index of sample `i` of a level of `n` samples, mirrored about its edge samples.
pub fn mirror(i: isize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let n = isize::try_from(n).unwrap_or(isize::MAX);
    let period = 2 * (n - 1);
    let mut i = i.rem_euclid(period);
    if i >= n {
        i = period - i;
    }
    usize::try_from(i).unwrap_or(0)
}

/// Sample `global` of a level of `full` samples, as an index into a span of it starting at `start` of `len` samples:
/// mirrored at the level's edges, then clamped to the span.
fn local(global: isize, full: usize, start: usize, len: usize) -> usize {
    mirror(global, full).saturating_sub(start).min(len.saturating_sub(1))
}

/// The taps of a 1-D REDUCE from `src` (a span `start .. start + len` of a level of `full` samples) to the output
/// samples `out_start .. out_start + out_len` of the next level: for each output, its five inputs' indices into `src`.
fn reduce_taps(start: usize, len: usize, full: usize, out_start: usize, out_len: usize) -> Vec<[usize; 5]> {
    (out_start..out_start + out_len)
        .map(|j| {
            let center = 2 * signed(j);
            std::array::from_fn(|m| local(center + signed(m) - 2, full, start, len))
        })
        .collect()
}

/// The taps of a 1-D EXPAND from `src` (a span of a level of `full` samples) to output samples `out_start ..` of the
/// next finer level: for each output, up to three inputs and their weights (`2 KERNEL[x - 2 j + 2]` for every `j` with
/// `|x - 2 j| <= 2`), in order of `j`.
fn expand_taps(start: usize, len: usize, full: usize, out_start: usize, out_len: usize) -> Vec<Vec<(usize, f32)>> {
    (out_start..out_start + out_len)
        .map(|x| {
            let x = signed(x);
            let first = (x - 2).div_euclid(2) + isize::from((x - 2).rem_euclid(2) != 0);
            let last = (x + 2).div_euclid(2);
            (first..=last)
                .filter_map(|j| {
                    let m = usize::try_from(x - 2 * j + 2).ok()?;
                    Some((local(j, full, start, len), 2.0 * KERNEL.get(m).copied()?))
                })
                .collect()
        })
        .collect()
}

/// `src` (an image of `span`, any channels) filtered and decimated into the samples of `span.coarser()`.
pub fn reduce(src: &Image<f32>, span: Span) -> Result<(Image<f32>, Span), StackError> {
    let out = span.coarser();
    let c = src.channels();
    let xs = reduce_taps(span.x, span.width, span.full_width, out.x, out.width);
    let ys = reduce_taps(span.y, span.height, span.full_height, out.y, out.height);
    let ow = out.width;
    // Horizontal: every input row to the output's width.
    let mut rows = Image::new(ow, span.height, c, ColorSpace::LINEAR)?;
    rows.par_rows_mut().zip(src.par_rows()).for_each(|(dst, row)| {
        for (o, taps) in dst.chunks_exact_mut(c).zip(&xs) {
            for (ch, v) in o.iter_mut().enumerate() {
                let mut s = 0.0;
                for (k, &t) in KERNEL.iter().zip(taps) {
                    s += k * row.get(t * c + ch).copied().unwrap_or(0.0);
                }
                *v = s;
            }
        }
    });
    let mut dst = Image::new(ow, out.height, c, ColorSpace::LINEAR)?;
    let tmp: Vec<&[f32]> = rows.rows().collect();
    dst.par_rows_mut().zip(ys.par_iter()).for_each(|(o, taps)| {
        for (k, &t) in KERNEL.iter().zip(taps) {
            if let Some(r) = tmp.get(t) {
                for (v, &s) in o.iter_mut().zip(r.iter()) {
                    *v += k * s;
                }
            }
        }
    });
    Ok((dst, out))
}

/// `src` (an image of `span`) interpolated onto the samples `out` of the next finer level.
pub fn expand(src: &Image<f32>, span: Span, out: Span) -> Result<Image<f32>, StackError> {
    let c = src.channels();
    let xs = expand_taps(span.x, span.width, span.full_width, out.x, out.width);
    let ys = expand_taps(span.y, span.height, span.full_height, out.y, out.height);
    let mut rows = Image::new(out.width, span.height, c, ColorSpace::LINEAR)?;
    rows.par_rows_mut().zip(src.par_rows()).for_each(|(dst, row)| {
        for (o, taps) in dst.chunks_exact_mut(c).zip(&xs) {
            for (ch, v) in o.iter_mut().enumerate() {
                let mut s = 0.0;
                for &(t, k) in taps {
                    s += k * row.get(t * c + ch).copied().unwrap_or(0.0);
                }
                *v = s;
            }
        }
    });
    let mut dst = Image::new(out.width, out.height, c, ColorSpace::LINEAR)?;
    let tmp: Vec<&[f32]> = rows.rows().collect();
    dst.par_rows_mut().zip(ys.par_iter()).for_each(|(o, taps)| {
        for &(t, k) in taps {
            if let Some(r) = tmp.get(t) {
                for (v, &s) in o.iter_mut().zip(r.iter()) {
                    *v += k * s;
                }
            }
        }
    });
    Ok(dst)
}

/// REDUCE by normalized convolution: `reduce(coverage * src) / reduce(coverage)`, and the reduced coverage, so that the
/// zeros beyond a frame's edge don't darken its coarser levels. A sample whose reduced coverage is below `1e-6` is 0.
pub fn reduce_covered(src: &Image<f32>, coverage: &Image<f32>, span: Span) -> Result<(Image<f32>, Image<f32>, Span), StackError> {
    let c = src.channels();
    let mut weighted = src.clone();
    weighted.par_rows_mut().zip(coverage.par_rows()).for_each(|(row, cov)| {
        for (pixel, &k) in row.chunks_exact_mut(c).zip(cov) {
            for v in pixel.iter_mut() {
                *v *= k;
            }
        }
    });
    let (mut values, out) = reduce(&weighted, span)?;
    drop(weighted);
    let (reduced, _) = reduce(coverage, span)?;
    values.par_rows_mut().zip(reduced.par_rows()).for_each(|(row, cov)| {
        for (pixel, &k) in row.chunks_exact_mut(c).zip(cov) {
            if k >= 1e-6 {
                let inverse = 1.0 / k;
                for v in pixel.iter_mut() {
                    *v *= inverse;
                }
            } else {
                pixel.fill(0.0);
            }
        }
    });
    Ok((values, reduced, out))
}

/// `a - b`, sample by sample (images of the same shape).
pub fn subtract(a: &Image<f32>, b: &Image<f32>) -> Result<Image<f32>, StackError> {
    let mut out = a.clone();
    out.par_rows_mut().zip(b.par_rows()).for_each(|(o, r)| o.iter_mut().zip(r).for_each(|(v, &s)| *v -= s));
    Ok(out)
}

/// The Rec. 709 luminance of each pixel of `coefficients` (the first three channels of color images, the first of gray
/// ones): a linear combination, so the luminance of a band of a frame's pyramid is the band of its luminance.
pub fn luminance(coefficients: &Image<f32>) -> Result<Image<f32>, StackError> {
    let c = coefficients.channels();
    let mut out = Image::new(coefficients.width(), coefficients.height(), 1, ColorSpace::LINEAR)?;
    out.par_rows_mut().zip(coefficients.par_rows()).for_each(|(o, row)| {
        for (y, pixel) in o.iter_mut().zip(row.chunks_exact(c)) {
            *y = match pixel {
                [r, g, b, ..] => LUMA[0] * r + LUMA[1] * g + LUMA[2] * b,
                [v, ..] => *v,
                [] => 0.0,
            };
        }
    });
    Ok(out)
}

/// The local energy of `band` (an image of `span`, every channel on its own): its squares filtered by the 5x5 binomial
/// kernel, mirrored at the level's edges.
pub fn local_energy(band: &Image<f32>, span: Span) -> Result<Image<f32>, StackError> {
    let c = band.channels();
    let mut squares = band.clone();
    squares.par_rows_mut().for_each(|row| row.iter_mut().for_each(|v| *v *= *v));
    let xs: Vec<[usize; 5]> = (span.x..span.x + span.width)
        .map(|x| std::array::from_fn(|m| local(signed(x) + signed(m) - 2, span.full_width, span.x, span.width)))
        .collect();
    let ys: Vec<[usize; 5]> = (span.y..span.y + span.height)
        .map(|y| std::array::from_fn(|m| local(signed(y) + signed(m) - 2, span.full_height, span.y, span.height)))
        .collect();
    let mut rows = Image::new(span.width, span.height, c, ColorSpace::LINEAR)?;
    rows.par_rows_mut().zip(squares.par_rows()).for_each(|(dst, row)| {
        for (o, taps) in dst.chunks_exact_mut(c).zip(&xs) {
            for (ch, v) in o.iter_mut().enumerate() {
                let mut s = 0.0;
                for (k, &t) in KERNEL.iter().zip(taps) {
                    s += k * row.get(t * c + ch).copied().unwrap_or(0.0);
                }
                *v = s;
            }
        }
    });
    let mut out = Image::new(span.width, span.height, c, ColorSpace::LINEAR)?;
    let tmp: Vec<&[f32]> = rows.rows().collect();
    out.par_rows_mut().zip(ys.par_iter()).for_each(|(o, taps)| {
        for (k, &t) in KERNEL.iter().zip(taps) {
            if let Some(r) = tmp.get(t) {
                for (v, &s) in o.iter_mut().zip(r.iter()) {
                    *v += k * s;
                }
            }
        }
    });
    Ok(out)
}

/// The Laplacian pyramid of a whole image: `levels` band-pass levels, finest first, then the low-pass residual. The
/// reference form of what a tile computes on its span; [`collapse`] inverts it exactly up to rounding.
pub fn laplacian_pyramid(image: &Image<f32>, levels: usize) -> Result<Vec<Image<f32>>, StackError> {
    let mut out = Vec::with_capacity(levels + 1);
    let mut span = Span::whole(image.width(), image.height());
    let mut g = image.clone();
    for _ in 0..levels {
        let (next, coarse) = reduce(&g, span)?;
        let up = expand(&next, coarse, span)?;
        out.push(subtract(&g, &up)?);
        g = next;
        span = coarse;
    }
    out.push(g);
    Ok(out)
}

/// The image whose Laplacian pyramid `pyramid` is ([`laplacian_pyramid`]'s layout).
pub fn collapse(pyramid: &[Image<f32>]) -> Result<Image<f32>, StackError> {
    let Some((top, bands)) = pyramid.split_last() else {
        return Err(StackError::NoFrames);
    };
    let mut g = top.clone();
    for band in bands.iter().rev() {
        let (fine, coarse) = (Span::whole(band.width(), band.height()), Span::whole(g.width(), g.height()));
        let up = expand(&g, coarse, fine)?;
        let mut next = band.clone();
        next.par_rows_mut().zip(up.par_rows()).for_each(|(o, r)| o.iter_mut().zip(r).for_each(|(v, &s)| *v += s));
        g = next;
    }
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize, c: usize) -> Image<f32> {
        Image::from_fn(w, h, c, ColorSpace::LINEAR, |x, y, ch| ((x * 7919 + y * 104_729 + ch * 31) % 101) as f32 / 101.0).unwrap()
    }

    #[test]
    fn mirroring_reflects_about_the_edge_samples() {
        let got: Vec<usize> = (-3..8).map(|i| mirror(i, 5)).collect();
        assert_eq!(got, [3, 2, 1, 0, 1, 2, 3, 4, 3, 2, 1]);
        assert_eq!((mirror(-7, 1), mirror(9, 1)), (0, 0));
        assert_eq!((mirror(-1, 2), mirror(2, 2)), (1, 0));
    }

    #[test]
    fn pyramids_collapse_back_to_their_image() {
        for (w, h) in [(37, 23), (64, 48), (1, 9), (5, 1)] {
            let image = ramp(w, h, 3);
            let pyramid = laplacian_pyramid(&image, 4).unwrap();
            assert_eq!(pyramid.len(), 5);
            let back = collapse(&pyramid).unwrap();
            let err = back.as_raw().iter().zip(image.as_raw()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(err < 1e-5, "{w}x{h}: {err}");
        }
    }

    #[test]
    #[allow(clippy::float_cmp)] // The kernel's weights are powers of two over 16: flat images stay exactly flat.
    fn flat_images_have_no_band_pass_and_stay_flat() {
        let flat = Image::from_fn(20, 13, 2, ColorSpace::LINEAR, |_, _, c| 0.25 + 0.5 * c as f32).unwrap();
        let pyramid = laplacian_pyramid(&flat, 3).unwrap();
        for band in &pyramid[..3] {
            assert!(band.as_raw().iter().all(|&v| v == 0.0));
        }
        assert!(pyramid[3].as_raw().as_chunks::<2>().0.iter().all(|p| p == &[0.25, 0.75]));
        let cover = Image::from_fn(20, 13, 1, ColorSpace::LINEAR, |x, _, _| if x < 10 { 1.0 } else { 0.0 }).unwrap();
        let (values, reduced, _) = reduce_covered(&flat, &cover, Span::whole(20, 13)).unwrap();
        // Normalized convolution: the covered part keeps its value up to its edge, instead of fading into the zeros.
        assert!(
            values
                .as_raw()
                .as_chunks::<2>()
                .0
                .iter()
                .zip(reduced.as_raw())
                .all(|(p, &k)| k < 1e-6 || ((p[0] - 0.25).abs() < 1e-6 && (p[1] - 0.75).abs() < 1e-6))
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // Bit for bit is the point.
    fn spans_compute_the_whole_level_s_samples_bit_for_bit() {
        // A span of a level, with enough margin, reduces and expands to exactly the samples of the whole level.
        let image = ramp(70, 45, 3);
        let whole = Span::whole(70, 45);
        let (g1, s1) = reduce(&image, whole).unwrap();
        let (g2, s2) = reduce(&g1, s1).unwrap();
        let up = expand(&g2, s2, s1).unwrap();
        // The span from 16 (aligned to 4) to the right edge, and from 8 to 37 vertically.
        let span = Span { x: 16, y: 8, width: 54, height: 29, full_width: 70, full_height: 45 };
        let piece = image.crop(florastack_core::Rect::new(16, 8, 54, 29)).unwrap();
        let (p1, ps1) = reduce(&piece, span).unwrap();
        assert_eq!((ps1.x, ps1.y, ps1.width, ps1.height, ps1.full_width), (8, 4, 27, 15, 35));
        let (p2, ps2) = reduce(&p1, ps1).unwrap();
        let pup = expand(&p2, ps2, ps1).unwrap();
        // Samples far enough from the span's inner edges agree exactly (a level-1 sample reads level 0 within 2
        // samples, an expanded one level 1 within 3 through level 2); the right edge is the level's own.
        for y in 2..ps1.height - 2 {
            for x in 2..ps1.width {
                for c in 0..3 {
                    assert_eq!(p1.get(x, y, c), g1.get(ps1.x + x, ps1.y + y, c), "level 1 at {x},{y}");
                }
            }
        }
        for y in 6..ps1.height - 6 {
            for x in 6..ps1.width {
                for c in 0..3 {
                    assert_eq!(pup.get(x, y, c), up.get(ps1.x + x, ps1.y + y, c), "expanded at {x},{y}");
                }
            }
        }
        let e = local_energy(&g1, s1).unwrap();
        let pe = local_energy(&p1, ps1).unwrap();
        assert_eq!(pe.get(10, 6, 1), e.get(18, 10, 1));
        assert_eq!(
            luminance(&image).unwrap().get(3, 4, 0),
            Some(LUMA[0] * image.get(3, 4, 0).unwrap() + LUMA[1] * image.get(3, 4, 1).unwrap() + LUMA[2] * image.get(3, 4, 2).unwrap())
        );
    }
}
