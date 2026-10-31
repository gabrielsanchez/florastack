//! Painting strokes onto a result: a whole stroke ([`apply`]), a stroke as it is drawn ([`LiveStroke`]), a list of
//! strokes over an output ([`render`]) and again over the region an undo uncovers ([`replay`]). All four give the same
//! pixels for the same records (see `brush.rs`).

use std::collections::BTreeMap;

use florastack_core::{Image, Rect};
use rayon::prelude::*;

use crate::RetouchError;
use crate::brush::{Fields, Layout, Prepared, paint, prepare, rasterize};
use crate::stroke::{Dab, Stroke, intersect};

/// The side of the tiles a live stroke keeps the result from before it in.
const TILE: usize = 64;

/// A stroke to paint, with the images it reads.
#[derive(Clone, Copy)]
pub struct Painted<'a> {
    pub stroke: &'a Stroke,
    pub fields: Fields<'a>,
}

/// Checks that the fields fit `canvas`: the same size, the same color channels; the coverage one channel.
fn check(canvas: &Image<f32>, fields: &Fields<'_>) -> Result<Layout, RetouchError> {
    let size = (canvas.width(), canvas.height());
    let layout = Layout::of(canvas)?;
    let mismatch = |what: &str| Err(RetouchError::Mismatch { what: what.to_owned() });
    if (fields.base.width(), fields.base.height()) != size || Layout::of(fields.base)? != layout {
        return mismatch("an output whose base image differs from the result in size or channels");
    }
    if let Some(source) = fields.source
        && ((source.width(), source.height()) != size || Layout::of(source)?.color != layout.color)
    {
        return mismatch("a source that differs from the output in size or color channels");
    }
    if let Some(coverage) = fields.coverage
        && ((coverage.width(), coverage.height()) != size || coverage.channels() != 1)
    {
        return mismatch("a source coverage that isn't one channel of the output's size");
    }
    Ok(layout)
}

/// The pixels a stroke's dabs reach, within `clip`.
fn reach(dabs: &[Dab], canvas: &Image<f32>, clip: Rect) -> Option<Rect> {
    Stroke::reach(dabs, 1.0, canvas.width(), canvas.height()).and_then(|r| intersect(r, clip))
}

/// Where a stroke's pixels are painted from: the result itself (a whole stroke, read before it is written), or a live
/// stroke's tiles of the result from before it (pixels outside them are left as they are).
enum Pre<'a> {
    Canvas,
    Tiles(&'a BTreeMap<(usize, usize), Tile>),
}

/// Paints the pixels of `rect` from the result before the stroke (`pre`) with the mask `mask(x, y)`, into `canvas`.
#[allow(clippy::too_many_arguments)] // The stroke, where, and the mask and the result before it.
fn paint_rect(
    canvas: &mut Image<f32>,
    fields: &Fields<'_>,
    stroke: &Stroke,
    layout: Layout,
    prepared: &Prepared,
    rect: Rect,
    mask: &(dyn Fn(usize, usize) -> f32 + Sync),
    pre: &Pre<'_>,
) {
    let n = canvas.channels();
    #[allow(clippy::cast_possible_truncation)] // From 0 to 1.
    let opacity = stroke.opacity as f32;
    let range = rect.x * n..(rect.x + rect.width) * n;
    let current = &*canvas;
    let rows: Vec<(usize, Vec<f32>)> = (rect.y..rect.y + rect.height)
        .into_par_iter()
        .filter_map(|y| {
            let mut row = current.row(y)?.get(range.clone())?.to_vec();
            let base_row = fields.base.row(y)?;
            let source_row = fields.source.and_then(|s| s.row(y));
            let source_n = fields.source.map_or(n, Image::channels);
            let coverage_row = fields.coverage.and_then(|c| c.row(y));
            let mut before = [0.0f32; 4];
            for i in 0..rect.width {
                let x = rect.x + i;
                let before = &mut before[..n];
                match pre {
                    Pre::Canvas => before.copy_from_slice(&row[i * n..i * n + n]),
                    Pre::Tiles(tiles) => {
                        let Some(tile) = tiles.get(&(x / TILE, y / TILE)) else { continue };
                        let k = ((y % TILE) * TILE + x % TILE) * n;
                        before.copy_from_slice(&tile.pre[k..k + n]);
                    }
                }
                let k = (y - prepared.rect.y) * prepared.rect.width + (x - prepared.rect.x);
                let gain = prepared.gains.get(k).copied().unwrap_or([1.0; 3]);
                let Some(base) = base_row.get(x * n..x * n + n) else { continue };
                let source = source_row.and_then(|r| r.get(x * source_n..x * source_n + source_n));
                let coverage = coverage_row.and_then(|r| r.get(x).copied()).unwrap_or(1.0);
                paint(stroke.brush, layout, before, source, base, coverage, mask(x, y) * opacity, gain, &mut row[i * n..i * n + n]);
            }
            Some((y, row))
        })
        .collect();
    for (y, values) in rows {
        if let Some(dst) = canvas.row_mut(y).and_then(|r| r.get_mut(range.clone())) {
            dst.copy_from_slice(&values);
        }
    }
}

/// Paints `stroke` onto `canvas` within `clip`, from `fields`. Returns the pixels it may have changed.
pub fn apply(canvas: &mut Image<f32>, painted: Painted<'_>, clip: Rect) -> Result<Option<Rect>, RetouchError> {
    let Painted { stroke, fields } = painted;
    stroke.validate_for(canvas.width(), canvas.height())?;
    let layout = check(canvas, &fields)?;
    let dabs = stroke.dabs();
    let Some(rect) = reach(&dabs, canvas, clip) else { return Ok(None) };
    let prepared = prepare(&fields, stroke.brush, stroke.size, stroke.hardness, rect)?;
    let mut mask = vec![0.0f32; rect.width * rect.height];
    for dab in &dabs {
        rasterize(dab, rect, &prepared.hardness, &mut mask);
    }
    let mask_at = |x: usize, y: usize| mask.get((y - rect.y) * rect.width + (x - rect.x)).copied().unwrap_or(0.0);
    paint_rect(canvas, &fields, stroke, layout, &prepared, rect, &mask_at, &Pre::Canvas);
    Ok(Some(rect))
}

/// A tile a live stroke has reached: the result there before the stroke, and the stroke's mask so far.
struct Tile {
    pre: Vec<f32>,
    mask: Vec<f32>,
}

/// A stroke as it is drawn: samples come in a few at a time ([`LiveStroke::extend`]), and the result shows the stroke
/// so far, exactly as [`apply`] would paint it whole. It keeps, for each tile it reaches, the result from before it,
/// to paint from and to put back if it is cancelled.
pub struct LiveStroke {
    stroke: Stroke,
    /// The dabs painted so far.
    painted: usize,
    tiles: BTreeMap<(usize, usize), Tile>,
}

impl LiveStroke {
    /// A stroke with `stroke`'s settings and samples, none painted yet.
    pub fn new(stroke: Stroke) -> Result<LiveStroke, RetouchError> {
        stroke.validate()?;
        Ok(LiveStroke { stroke, painted: 0, tiles: BTreeMap::new() })
    }

    /// The record so far.
    pub fn stroke(&self) -> &Stroke {
        &self.stroke
    }

    /// Adds `samples` to the stroke (none to paint what it has) and paints its new dabs onto `canvas`. Returns the
    /// pixels that may have changed.
    pub fn extend(&mut self, canvas: &mut Image<f32>, fields: &Fields<'_>, samples: &[[f64; 3]]) -> Result<Option<Rect>, RetouchError> {
        let before = self.stroke.samples.len();
        self.stroke.samples.extend_from_slice(samples);
        if let Err(e) = self.stroke.validate_for(canvas.width(), canvas.height()) {
            self.stroke.samples.truncate(before);
            return Err(e);
        }
        let layout = check(canvas, fields)?;
        let dabs = self.stroke.dabs();
        let new = dabs.get(self.painted..).unwrap_or(&[]);
        let whole = Rect::new(0, 0, canvas.width(), canvas.height());
        let Some(rect) = reach(new, canvas, whole) else {
            self.painted = dabs.len();
            return Ok(None);
        };
        // The tiles the new dabs reach keep the result from before the stroke, taken before they are painted.
        let n = canvas.channels();
        let (w, h) = (canvas.width(), canvas.height());
        for ty in rect.y / TILE..=(rect.y + rect.height - 1) / TILE {
            for tx in rect.x / TILE..=(rect.x + rect.width - 1) / TILE {
                self.tiles.entry((tx, ty)).or_insert_with(|| {
                    let mut pre = vec![0.0f32; TILE * TILE * n];
                    for j in 0..TILE.min(h.saturating_sub(ty * TILE)) {
                        let (x0, x1) = (tx * TILE, (tx * TILE + TILE).min(w));
                        if let Some(src) = canvas.row(ty * TILE + j).and_then(|r| r.get(x0 * n..x1 * n)) {
                            pre[j * TILE * n..j * TILE * n + src.len()].copy_from_slice(src);
                        }
                    }
                    Tile { pre, mask: vec![0.0; TILE * TILE] }
                });
            }
        }
        let prepared = prepare(fields, self.stroke.brush, self.stroke.size, self.stroke.hardness, rect)?;
        // The new dabs, into a mask of the region, then into the tiles' masks.
        let mut mask = vec![0.0f32; rect.width * rect.height];
        for dab in new {
            rasterize(dab, rect, &prepared.hardness, &mut mask);
        }
        for (j, row) in mask.chunks_exact(rect.width).enumerate() {
            let y = rect.y + j;
            for (i, &m) in row.iter().enumerate() {
                let x = rect.x + i;
                if let Some(tile) = self.tiles.get_mut(&(x / TILE, y / TILE)) {
                    let k = (y % TILE) * TILE + x % TILE;
                    tile.mask[k] = tile.mask[k].max(m);
                }
            }
        }
        self.painted = dabs.len();
        let tiles = &self.tiles;
        let mask_at = |x: usize, y: usize| tiles.get(&(x / TILE, y / TILE)).map_or(0.0, |t| t.mask[(y % TILE) * TILE + x % TILE]);
        paint_rect(canvas, fields, &self.stroke, layout, &prepared, rect, &mask_at, &Pre::Tiles(tiles));
        Ok(Some(rect))
    }

    /// Puts back the result from before the stroke. Returns the pixels it changed.
    pub fn cancel(self, canvas: &mut Image<f32>) -> Option<Rect> {
        let n = canvas.channels();
        let (w, h) = (canvas.width(), canvas.height());
        let mut changed = None;
        for ((tx, ty), tile) in self.tiles {
            let (x0, x1) = (tx * TILE, (tx * TILE + TILE).min(w));
            let rows = TILE.min(h.saturating_sub(ty * TILE));
            for j in 0..rows {
                if let Some(dst) = canvas.row_mut(ty * TILE + j).and_then(|r| r.get_mut(x0 * n..x1 * n)) {
                    let len = dst.len();
                    dst.copy_from_slice(&tile.pre[j * TILE * n..j * TILE * n + len]);
                }
            }
            changed = crate::stroke::union(changed, Some(Rect::new(x0, ty * TILE, x1 - x0, rows)));
        }
        changed
    }
}

/// The output `base` with `strokes` painted over it in order.
pub fn render(base: &Image<f32>, strokes: &[Painted<'_>]) -> Result<Image<f32>, RetouchError> {
    let mut canvas = base.clone();
    let whole = Rect::new(0, 0, base.width(), base.height());
    for painted in strokes {
        apply(&mut canvas, *painted, whole)?;
    }
    Ok(canvas)
}

/// Paints `rect` of `canvas` again from the output `base` with `strokes`, as [`render`] paints it: what an undo, a redo
/// or a layer shown or hidden needs where the strokes changed. Strokes that don't reach `rect` cost nothing.
pub fn replay(canvas: &mut Image<f32>, base: &Image<f32>, strokes: &[Painted<'_>], rect: Rect) -> Result<(), RetouchError> {
    let n = canvas.channels();
    if (base.width(), base.height(), base.channels()) != (canvas.width(), canvas.height(), n) {
        return Err(RetouchError::Mismatch { what: "an output whose base image differs from the result".to_owned() });
    }
    let Some(rect) = intersect(rect, Rect::new(0, 0, canvas.width(), canvas.height())) else { return Ok(()) };
    for y in rect.y..rect.y + rect.height {
        let range = rect.x * n..(rect.x + rect.width) * n;
        if let (Some(src), Some(dst)) = (base.row(y).and_then(|r| r.get(range.clone())), canvas.row_mut(y).and_then(|r| r.get_mut(range))) {
            dst.copy_from_slice(src);
        }
    }
    for painted in strokes {
        apply(canvas, *painted, rect)?;
    }
    Ok(())
}
