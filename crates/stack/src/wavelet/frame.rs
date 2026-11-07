//! What both passes do to a frame: split it into its 1/8-size low-pass and what that leaves, and gather the low-pass of
//! a frame's tiles for the whole-canvas part. See the module documentation of [`super`].

use florastack_core::{Image, Rect, Tile};
use rayon::prelude::*;

use super::transform::{Bank, extension};
use crate::average::MIN_COVERAGE;
use crate::error::StackError;
use crate::pyramid::TILED_LEVELS;
use crate::pyramid::kernel::{Gaussian, clean, core_span, crop, filled, paste};
use crate::pyramid::levels::{Span, expand, reduce_covered, subtract};
use crate::source::Patch;
use crate::tiled::{Share, TileView};

/// What a stack in progress knows about its canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Layout {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    /// All the levels, tiled and coarse.
    pub levels: usize,
    /// The finest levels, undecimated in tiles: `min(TILED_LEVELS, levels)`. It is also how many times the low-pass is
    /// halved for the whole-canvas part.
    pub tiled: usize,
}

impl Layout {
    /// The levels of the whole-canvas part.
    pub fn coarse(&self) -> usize {
        self.levels - self.tiled
    }

    /// The whole span of the pyramid's level `k`.
    pub fn level(&self, k: usize) -> Span {
        let a = 1usize << k;
        Span::whole(self.width.div_ceil(a).max(1), self.height.div_ceil(a).max(1))
    }

    /// The whole span of the whole-canvas part.
    pub fn small(&self) -> Span {
        self.level(self.tiled)
    }

    /// The canvas as a span holding `region`.
    pub fn span(&self, region: Rect) -> Span {
        Span { x: region.x, y: region.y, width: region.width, height: region.height, full_width: self.width, full_height: self.height }
    }
}

/// How far a tile's plane is extended for `tiled` levels: the transform there and back, and the reach of the finest
/// level's blend mask on top of it (its filter and the activity's window), which pass 2 reads out to there.
pub(super) fn tile_extension(bank: &Bank, tiled: usize) -> usize {
    extension(bank, tiled) + bank.reach() + 2
}

/// How far a kernel on tiles reads: the extended transform, the low-pass the residual subtracts (halved
/// [`TILED_LEVELS`] times and expanded again), and the lattice's rounding.
pub(super) fn context(bank: &Bank) -> usize {
    let a = 1usize << TILED_LEVELS;
    let split = 4 * (a - 1) + 6;
    tile_extension(bank, TILED_LEVELS) + split + a
}

/// The region a tile whose outer region is `outer` reads: grown to the lattice of the low-pass, as the pyramid's are.
pub(super) fn read_region(outer: Rect, width: usize, height: usize) -> Rect {
    let a = 1 << TILED_LEVELS;
    let (x, y) = (outer.x / a * a, outer.y / a * a);
    let (x1, y1) = ((outer.x + outer.width).min(width), (outer.y + outer.height).min(height));
    Rect::new(x, y, x1 - x, y1 - y)
}

/// A frame's patch, split.
pub(super) struct Split {
    pub gaussian: Gaussian,
    /// What the expanded low-pass leaves of the frame: 0 where it doesn't cover, so that nothing of its edge reaches the
    /// transform.
    pub residual: Image<f32>,
}

impl Split {
    pub fn new(patch: &Patch, span: Span, tiled: usize) -> Result<Split, StackError> {
        let (image, coverage) = clean(patch)?;
        let gaussian = Gaussian::new(image, coverage, span, tiled)?;
        let mut up = gaussian.levels[tiled].clone();
        for k in (0..tiled).rev() {
            up = expand(&up, gaussian.spans[k + 1], gaussian.spans[k])?;
        }
        let mut residual = subtract(&gaussian.levels[0], &up)?;
        if let Some(coverage) = &gaussian.coverage[0] {
            let c = residual.channels();
            residual.par_rows_mut().zip(coverage.par_rows()).for_each(|(row, cov)| {
                for (pixel, &k) in row.chunks_exact_mut(c).zip(cov) {
                    if k < MIN_COVERAGE {
                        pixel.fill(0.0);
                    }
                }
            });
        }
        Ok(Split { gaussian, residual })
    }

    /// The frame, cleaned.
    pub fn image(&self) -> &Image<f32> {
        &self.gaussian.levels[0]
    }

    pub fn coverage(&self) -> Option<&Image<f32>> {
        self.gaussian.coverage[0].as_ref()
    }

    /// The tile's share of the frame's low-pass: its core's samples and their coverage.
    pub fn share(&self, layout: &Layout, view: &TileView) -> Result<Share, StackError> {
        let k = layout.tiled;
        let Some(core) = core_span(layout.small(), view.core, k) else { return Ok(Vec::new()) };
        let values = crop(&self.gaussian.levels[k], self.gaussian.spans[k], core)?;
        let coverage = match &self.gaussian.coverage[k] {
            Some(cov) => crop(cov, self.gaussian.spans[k], core)?,
            None => filled(core.width, core.height, 1, 1.0)?,
        };
        Ok(vec![values, coverage])
    }
}

/// A frame's low-pass and its coverage, from its tiles' shares, with what the frame doesn't cover filled in.
pub(super) fn gather(layout: &Layout, shares: Vec<(Tile, Share)>) -> Result<(Image<f32>, Image<f32>), StackError> {
    let small = layout.small();
    let mut values = filled(small.width, small.height, layout.channels, 0.0)?;
    let mut coverage = filled(small.width, small.height, 1, 0.0)?;
    for (tile, share) in shares {
        let Some(core) = core_span(small, tile.core, layout.tiled) else { continue };
        let mut planes = share.into_iter();
        let (Some(v), Some(k)) = (planes.next(), planes.next()) else { continue };
        paste(&mut values, &v, core);
        paste(&mut coverage, &k, core);
    }
    Ok((fill_holes(values, &coverage, small)?, coverage))
}

/// `image` (of `span`) with the samples its coverage leaves out taken from its own coarser levels, pulled down and
/// pushed back up, so that a frame's edge isn't a step the transform would read as detail.
fn fill_holes(mut image: Image<f32>, coverage: &Image<f32>, span: Span) -> Result<Image<f32>, StackError> {
    if coverage.par_rows().all(|row| row.iter().all(|&k| k >= MIN_COVERAGE)) || coverage.par_rows().all(|row| row.iter().all(|&k| k < MIN_COVERAGE)) {
        return Ok(image);
    }
    if span.width <= 1 && span.height <= 1 {
        return Ok(image);
    }
    let (small, small_coverage, small_span) = reduce_covered(&image, coverage, span)?;
    let up = expand(&fill_holes(small, &small_coverage, small_span)?, small_span, span)?;
    let c = image.channels();
    image.par_rows_mut().zip(up.par_rows()).zip(coverage.par_rows()).for_each(|((row, up), cov)| {
        for ((pixel, up), &k) in row.chunks_exact_mut(c).zip(up.chunks_exact(c)).zip(cov) {
            if k < MIN_COVERAGE {
                pixel.copy_from_slice(up);
            }
        }
    });
    Ok(image)
}
