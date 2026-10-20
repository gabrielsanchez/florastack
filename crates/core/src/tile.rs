//! The tiled view: an image split into overlapping tiles that can be processed one at a time (or in parallel) and
//! stitched back with feathered seams, so that stacking can bound its memory however large the frames are.
//!
//! A [`TileGrid`] cuts the image into **cores** that partition it: as many columns and rows as `tile_size` needs, of
//! nearly equal sizes (they differ by one pixel at most, so no sliver is left at the edge). Each tile is its core
//! grown by `overlap` pixels on every side, clipped to the image, so neighbors share `2 overlap` pixels around their
//! seam. Processing near a tile's edge thus sees `overlap` pixels of real context beyond its core.
//!
//! [`TileGrid::stitch`] takes every pixel from the tile whose core holds it, except in a band of `overlap` pixels
//! centered on each seam, where it blends the two neighbors with a smoothstep ramp (`3s^2 - 2s^3`, so the weights meet
//! the plain regions without a kink); where a horizontal and a vertical band cross, four tiles blend bilinearly. The
//! outer `overlap / 2` pixels of every tile are context only. Weights are applied as `a + t (b - a)`, which is exactly
//! `a` when the tiles agree, so stitching the unprocessed tiles gives back the image bit for bit.
//! [`TileGrid::stitch_with`] fetches the tiles one row of tiles at a time, so that at most two rows of tiles are in
//! memory besides the output.

use rayon::prelude::*;

use crate::geometry::Rect;
use crate::image::{Image, ImageError, Sample};

/// The most tiles a grid may have: far more than any sensible tile size gives, few enough to list.
pub const MAX_TILES: usize = 1 << 24;

/// Why a grid can't be made or a stitch can't be done.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum TileError {
    #[error("tiles are at least 1 pixel, not {0}")]
    TileSize(usize),
    #[error("an overlap of {overlap} pixels is more than half the tile size of {tile_size}")]
    Overlap { overlap: usize, tile_size: usize },
    #[error("an image of {width}x{height} pixels has nothing to tile")]
    Empty { width: usize, height: usize },
    #[error("tiles of {tile_size} pixels would cut a {width}x{height} image into more than {MAX_TILES} tiles")]
    TooMany { width: usize, height: usize, tile_size: usize },
    #[error("the image is {got_width}x{got_height} but the grid is for {width}x{height}")]
    ImageSize { width: usize, height: usize, got_width: usize, got_height: usize },
    #[error("the grid has {expected} tiles, not {got}")]
    Count { expected: usize, got: usize },
    #[error("tile {index} is {}x{} with {} channels, but it must be {}x{} with {}", .got[0], .got[1], .got[2], .expected[0], .expected[1], .expected[2])]
    TileShape { index: usize, expected: [usize; 3], got: [usize; 3] },
    #[error("there is no tile {index} in a grid of {count}")]
    Index { index: usize, count: usize },
    #[error(transparent)]
    Image(#[from] ImageError),
}

/// One tile of a [`TileGrid`]: its index (row-major), its place in the grid, the core it owns and the region it
/// covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    pub index: usize,
    pub column: usize,
    pub row: usize,
    /// The pixels this tile alone is responsible for; the cores partition the image.
    pub core: Rect,
    /// The core grown by the overlap on every side, clipped to the image: the pixels the tile holds.
    pub outer: Rect,
}

/// A core interval along one axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    len: usize,
}

/// A run of positions along an axis that the same tiles cover: tile `first` alone, or a seam band where `first` and
/// `first + 1` blend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Piece {
    start: usize,
    end: usize,
    first: usize,
    /// Whether this is the band between `first` and `first + 1`.
    band: bool,
}

/// One row of tiles, whose first pixel rows lie on image row `top`.
struct TileRow<'a, T: Sample> {
    tiles: &'a [&'a Image<T>],
    top: usize,
}

impl<'a, T: Sample> TileRow<'a, T> {
    /// `len` samples of image row `y` from pixel `x` on, out of tile column `i`, whose first pixel is image column
    /// `left`, with `c` channels.
    fn segment(&self, i: usize, left: usize, y: usize, x: usize, len: usize, c: usize) -> Option<&'a [T]> {
        let tile: &'a Image<T> = self.tiles.get(i)?;
        let from = x.checked_sub(left)? * c;
        tile.row(y.checked_sub(self.top)?)?.get(from..from + len)
    }
}

/// An image's division into tiles. See the module documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileGrid {
    width: usize,
    height: usize,
    tile_size: usize,
    overlap: usize,
    columns: Vec<Span>,
    rows: Vec<Span>,
}

/// `len` positions in `parts` nearly equal spans.
fn spans(len: usize, parts: usize) -> Vec<Span> {
    let (base, extra) = (len / parts, len % parts);
    (0..parts).map(|i| Span { start: i * base + i.min(extra), len: base + usize::from(i < extra) }).collect()
}

/// The pieces of an axis: plain runs of one tile and the `overlap`-wide bands centered on each seam.
fn pieces(spans: &[Span], overlap: usize) -> Vec<Piece> {
    let mut out = Vec::with_capacity(2 * spans.len());
    let mut position = 0;
    for (i, span) in spans.iter().enumerate() {
        let end = span.start + span.len;
        if i + 1 < spans.len() && overlap > 0 {
            let band_start = end - overlap / 2;
            out.push(Piece { start: position, end: band_start, first: i, band: false });
            out.push(Piece { start: band_start, end: band_start + overlap, first: i, band: true });
            position = band_start + overlap;
        } else {
            out.push(Piece { start: position, end, first: i, band: false });
            position = end;
        }
    }
    out.retain(|p| p.start < p.end);
    out
}

/// The blend weight of the second tile at `offset` into a band of `overlap` positions: a smoothstep from 0 to 1,
/// sampled at the pixel centers so that it is never exactly 0 or 1.
fn ramp(offset: usize, overlap: usize) -> f32 {
    let s = (offset as f32 + 0.5) / overlap as f32;
    s * s * (3.0 - 2.0 * s)
}

impl TileGrid {
    /// A grid over a `width` x `height` image with cores of at most `tile_size` pixels a side and `overlap` pixels of
    /// context around each, at most half the tile size.
    pub fn new(width: usize, height: usize, tile_size: usize, overlap: usize) -> Result<TileGrid, TileError> {
        if width == 0 || height == 0 {
            return Err(TileError::Empty { width, height });
        }
        if tile_size == 0 {
            return Err(TileError::TileSize(tile_size));
        }
        if overlap > tile_size / 2 {
            return Err(TileError::Overlap { overlap, tile_size });
        }
        let (n, m) = (width.div_ceil(tile_size), height.div_ceil(tile_size));
        if n.checked_mul(m).is_none_or(|tiles| tiles > MAX_TILES) {
            return Err(TileError::TooMany { width, height, tile_size });
        }
        // Every core is then at least `tile_size / 2` long, at least the overlap, so the seam bands never meet.
        Ok(TileGrid { width, height, tile_size, overlap, columns: spans(width, n), rows: spans(height, m) })
    }

    /// A grid over `image`.
    pub fn for_image<T: Sample>(image: &Image<T>, tile_size: usize, overlap: usize) -> Result<TileGrid, TileError> {
        TileGrid::new(image.width(), image.height(), tile_size, overlap)
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn tile_size(&self) -> usize {
        self.tile_size
    }

    pub fn overlap(&self) -> usize {
        self.overlap
    }

    /// The number of tile columns.
    pub fn columns(&self) -> usize {
        self.columns.len()
    }

    /// The number of tile rows.
    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    /// The number of tiles.
    pub fn len(&self) -> usize {
        self.columns.len() * self.rows.len()
    }

    /// Always false: a grid has at least one tile.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Tile `index`, counted row by row.
    pub fn tile(&self, index: usize) -> Option<Tile> {
        (index < self.len()).then(|| self.tile_at(index))
    }

    /// Every tile, row by row.
    pub fn tiles(&self) -> impl ExactSizeIterator<Item = Tile> + '_ {
        (0..self.len()).map(|i| self.tile_at(i))
    }

    /// Tile `index`, which is in the grid.
    fn tile_at(&self, index: usize) -> Tile {
        let n = self.columns.len();
        let (column, row) = (index % n, index / n);
        let span = |spans: &[Span], i: usize| spans.get(i).copied().unwrap_or(Span { start: 0, len: 0 });
        let (cs, rs) = (span(&self.columns, column), span(&self.rows, row));
        let grow = |span: Span, len: usize| {
            let start = span.start.saturating_sub(self.overlap);
            (start, span.start.saturating_add(span.len).saturating_add(self.overlap).min(len) - start)
        };
        let (x, w) = grow(cs, self.width);
        let (y, h) = grow(rs, self.height);
        Tile { index, column, row, core: Rect::new(cs.start, rs.start, cs.len, rs.len), outer: Rect::new(x, y, w, h) }
    }

    fn check_image<T: Sample>(&self, image: &Image<T>) -> Result<(), TileError> {
        if (image.width(), image.height()) == (self.width, self.height) {
            Ok(())
        } else {
            Err(TileError::ImageSize { width: self.width, height: self.height, got_width: image.width(), got_height: image.height() })
        }
    }

    /// A copy of tile `index`'s pixels (its outer region) from `image`.
    pub fn extract<T: Sample>(&self, image: &Image<T>, index: usize) -> Result<Image<T>, TileError> {
        self.check_image(image)?;
        let tile = self.tile(index).ok_or(TileError::Index { index, count: self.len() })?;
        Ok(image.crop(tile.outer)?)
    }

    /// Every tile's pixels from `image`, in tile order, copied in parallel.
    pub fn split<T: Sample>(&self, image: &Image<T>) -> Result<Vec<Image<T>>, TileError> {
        self.check_image(image)?;
        (0..self.len()).into_par_iter().map(|i| self.extract(image, i)).collect()
    }

    /// Splits `image`, runs `f` on every tile in parallel, and stitches the results, which must have their tile's
    /// size (any channel count, the same for all). Every tile and result is in memory at once; [`TileGrid::stitch_with`]
    /// bounds that.
    pub fn map<T, U, E, F>(&self, image: &Image<T>, f: F) -> Result<Image<U>, E>
    where
        T: Sample,
        U: Sample,
        E: From<TileError> + Send,
        F: Fn(Tile, Image<T>) -> Result<Image<U>, E> + Sync,
    {
        self.check_image(image)?;
        let results: Vec<Image<U>> = (0..self.len())
            .into_par_iter()
            .map(|i| {
                let tile = self.tile(i).ok_or(TileError::Index { index: i, count: self.len() })?;
                f(tile, image.crop(tile.outer).map_err(TileError::from)?)
            })
            .collect::<Result<_, E>>()?;
        Ok(self.stitch(&results)?)
    }

    /// The image whose tiles are `tiles` (in tile order, each of its tile's outer size, with one channel count),
    /// blended across the seams. Its color space is the first tile's.
    pub fn stitch<T: Sample>(&self, tiles: &[Image<T>]) -> Result<Image<T>, TileError> {
        if tiles.len() != self.len() {
            return Err(TileError::Count { expected: self.len(), got: tiles.len() });
        }
        let first = tiles.first().ok_or(TileError::Count { expected: self.len(), got: 0 })?;
        let channels = first.channels();
        for (index, tile) in tiles.iter().enumerate() {
            self.check_tile(index, tile, channels)?;
        }
        let mut out = Image::new(self.width, self.height, channels, first.color_space().clone())?;
        let n = self.columns.len();
        let row_tiles = |row: usize| -> Vec<&Image<T>> { tiles.iter().skip(row * n).take(n).collect() };
        for piece in pieces(&self.rows, self.overlap) {
            let upper = row_tiles(piece.first);
            let lower = if piece.band { Some(row_tiles(piece.first + 1)) } else { None };
            self.stitch_rows(&mut out, piece, &upper, lower.as_deref());
        }
        Ok(out)
    }

    /// [`TileGrid::stitch`] with the tiles fetched on demand, row of tiles by row of tiles: `fetch(index)` is called
    /// once per tile, in order, and at most two rows of tiles are held at a time. The fetched tiles may come from a
    /// [`crate::store::TileCache`], for instance.
    pub fn stitch_with<T, E>(&self, mut fetch: impl FnMut(usize) -> Result<Image<T>, E>) -> Result<Image<T>, E>
    where
        T: Sample,
        E: From<TileError>,
    {
        let n = self.columns.len();
        let mut fetch_row = |row: usize, channels: Option<usize>| -> Result<Vec<Image<T>>, E> {
            let mut tiles = Vec::with_capacity(n);
            for index in row * n..(row + 1) * n {
                let tile = fetch(index)?;
                self.check_tile(index, &tile, channels.unwrap_or(tile.channels()))?;
                tiles.push(tile);
            }
            Ok(tiles)
        };
        let mut upper = fetch_row(0, None)?;
        let (channels, color) = match upper.first() {
            Some(t) => (t.channels(), t.color_space().clone()),
            None => return Err(TileError::Count { expected: self.len(), got: 0 }.into()),
        };
        let mut out = Image::new(self.width, self.height, channels, color).map_err(TileError::from)?;
        let mut lower: Option<Vec<Image<T>>> = None;
        let mut current = 0;
        for piece in pieces(&self.rows, self.overlap) {
            if piece.first != current {
                // The previous band fetched this row as its lower half.
                upper = lower.take().map_or_else(|| fetch_row(piece.first, Some(channels)), Ok)?;
                current = piece.first;
            }
            if piece.band && lower.is_none() {
                lower = Some(fetch_row(piece.first + 1, Some(channels))?);
            }
            let upper_refs: Vec<&Image<T>> = upper.iter().collect();
            let lower_refs: Option<Vec<&Image<T>>> = if piece.band { lower.as_ref().map(|row| row.iter().collect()) } else { None };
            self.stitch_rows(&mut out, piece, &upper_refs, lower_refs.as_deref());
        }
        Ok(out)
    }

    fn check_tile<T: Sample>(&self, index: usize, tile: &Image<T>, channels: usize) -> Result<(), TileError> {
        let outer = self.tile(index).ok_or(TileError::Index { index, count: self.len() })?.outer;
        let got = [tile.width(), tile.height(), tile.channels()];
        let expected = [outer.width, outer.height, channels];
        if got == expected { Ok(()) } else { Err(TileError::TileShape { index, expected, got }) }
    }

    /// Fills the output rows of `piece` (a piece of the row axis) from the tiles of its upper row of tiles and, in a
    /// band, its lower one. The tiles' shapes have been checked, so every segment read below exists.
    fn stitch_rows<T: Sample>(&self, out: &mut Image<T>, piece: Piece, upper: &[&Image<T>], lower: Option<&[&Image<T>]>) {
        let columns = pieces(&self.columns, self.overlap);
        let c = out.channels();
        let overlap = self.overlap;
        // Where each tile column's and each row of tiles' pixels start in the image.
        let x_origin: Vec<usize> = self.columns.iter().map(|span| span.start.saturating_sub(overlap)).collect();
        let y_origin = |row: usize| self.rows.get(row).map_or(0, |span| span.start.saturating_sub(overlap));
        let upper = TileRow { tiles: upper, top: y_origin(piece.first) };
        let lower = lower.map(|tiles| TileRow { tiles, top: y_origin(piece.first + 1) });
        let left = |i: usize| x_origin.get(i).copied().unwrap_or(usize::MAX);
        out.par_rows_mut().enumerate().skip(piece.start).take(piece.end - piece.start).for_each(|(y, dst)| {
            let ty = ramp(y - piece.start, overlap.max(1));
            for p in &columns {
                let Some(dst) = dst.get_mut(p.start * c..p.end * c) else { continue };
                let len = dst.len();
                let i = p.first;
                let Some(a) = upper.segment(i, left(i), y, p.start, len, c) else { continue };
                let below = |i: usize| lower.as_ref().and_then(|lower| lower.segment(i, left(i), y, p.start, len, c));
                if !p.band {
                    match below(i) {
                        None => dst.copy_from_slice(a),
                        Some(b) => {
                            for ((d, a), b) in dst.iter_mut().zip(a).zip(b) {
                                *d = T::lerp(*a, *b, ty);
                            }
                        }
                    }
                    continue;
                }
                let Some(right) = upper.segment(i + 1, left(i + 1), y, p.start, len, c) else { continue };
                let lower_pair = below(i).zip(below(i + 1));
                for (k, ((d, a), r)) in dst.chunks_exact_mut(c).zip(a.chunks_exact(c)).zip(right.chunks_exact(c)).enumerate() {
                    let tx = ramp(k, overlap);
                    for (ch, ((d, a), r)) in d.iter_mut().zip(a).zip(r).enumerate() {
                        let top = T::lerp(*a, *r, tx);
                        *d = match lower_pair {
                            Some((bl, br)) => match (bl.get(k * c + ch), br.get(k * c + ch)) {
                                (Some(bl), Some(br)) => T::lerp(top, T::lerp(*bl, *br, tx), ty),
                                _ => top,
                            },
                            None => top,
                        };
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ColorSpace;
    use crate::ops::{BlurMethod, gaussian_blur_in_place};

    fn pattern<T: Sample>(w: usize, h: usize, c: usize) -> Image<T> {
        Image::from_fn(w, h, c, ColorSpace::SRGB, |x, y, ch| T::from_normalized(((x * 37 + y * 101 + ch * 13) % 251) as f32 / 251.0)).unwrap()
    }

    /// A tile-sized image of one value per tile: tile `i` holds `i`.
    fn constant_tiles(grid: &TileGrid) -> Vec<Image<f32>> {
        grid.tiles().map(|t| Image::from_fn(t.outer.width, t.outer.height, 1, ColorSpace::LINEAR, |_, _, _| t.index as f32).unwrap()).collect()
    }

    #[test]
    fn cores_partition_the_image_in_nearly_equal_parts() {
        let grid = TileGrid::new(1000, 513, 256, 32).unwrap();
        assert_eq!((grid.columns(), grid.rows(), grid.len()), (4, 3, 12));
        let tiles: Vec<Tile> = grid.tiles().collect();
        let widths: Vec<usize> = tiles.iter().take(4).map(|t| t.core.width).collect();
        assert_eq!(widths, [250, 250, 250, 250]);
        let heights: Vec<usize> = tiles.iter().step_by(4).map(|t| t.core.height).collect();
        assert_eq!(heights, [171, 171, 171]);
        let mut covered = vec![0u8; 1000 * 513];
        for t in &tiles {
            for y in t.core.y..t.core.y + t.core.height {
                for x in t.core.x..t.core.x + t.core.width {
                    covered[y * 1000 + x] += 1;
                }
            }
            assert!(t.outer.fits_in(1000, 513));
            assert_eq!(t.outer.x, t.core.x.saturating_sub(32));
            assert_eq!(t.outer.right(), Some((t.core.x + t.core.width + 32).min(1000)));
        }
        assert!(covered.iter().all(|&n| n == 1), "every pixel in exactly one core");
        let expected = Tile { index: 5, column: 1, row: 1, core: Rect::new(250, 171, 250, 171), outer: Rect::new(218, 139, 314, 235) };
        assert_eq!(grid.tile(5).unwrap(), expected);
        assert_eq!(grid.tile(12), None);
        assert_eq!(TileGrid::new(100, 50, 512, 64).unwrap().len(), 1, "a small image is one tile");
    }

    #[test]
    fn bad_grids_are_errors() {
        assert_eq!(TileGrid::new(10, 10, 0, 0), Err(TileError::TileSize(0)));
        assert_eq!(TileGrid::new(10, 10, 8, 5), Err(TileError::Overlap { overlap: 5, tile_size: 8 }));
        assert_eq!(TileGrid::new(0, 10, 8, 2), Err(TileError::Empty { width: 0, height: 10 }));
        assert!(TileGrid::new(10, 10, 8, 4).is_ok());
        // A hostile size can't make the grid allocate without bound.
        assert!(matches!(TileGrid::new(usize::MAX, usize::MAX, 1, 0), Err(TileError::TooMany { .. })));
        assert!(matches!(TileGrid::new(1 << 13, 1 << 12, 1, 0), Err(TileError::TooMany { .. })));
        let wide = TileGrid::new(usize::MAX, 1, usize::MAX / 2, 2).unwrap();
        assert_eq!(wide.tiles().map(|t| t.outer.right()).last(), Some(Some(usize::MAX)));
    }

    #[test]
    fn split_then_stitch_gives_the_image_back_exactly() {
        for (w, h, size, overlap) in [(40, 30, 16, 8), (40, 30, 16, 0), (17, 9, 4, 2), (5, 5, 64, 32), (33, 20, 7, 3), (8, 1, 3, 1)] {
            let grid = TileGrid::new(w, h, size, overlap).unwrap();
            let u8_image = pattern::<u8>(w, h, 3);
            assert_eq!(grid.stitch(&grid.split(&u8_image).unwrap()).unwrap(), u8_image, "{w}x{h} {size}/{overlap}");
            let u16_image = pattern::<u16>(w, h, 2);
            assert_eq!(grid.stitch(&grid.split(&u16_image).unwrap()).unwrap(), u16_image);
            let f32_image = pattern::<f32>(w, h, 4);
            let tiles = grid.split(&f32_image).unwrap();
            assert_eq!(grid.stitch(&tiles).unwrap(), f32_image);
            assert_eq!(grid.stitch_with(|i| Ok::<_, TileError>(tiles[i].clone())).unwrap(), f32_image);
        }
    }

    #[test]
    #[allow(clippy::float_cmp)] // Away from the bands a pixel is copied from its tile.
    fn seams_blend_smoothly_between_disagreeing_tiles() {
        let grid = TileGrid::new(20, 1, 10, 4).unwrap();
        let out = grid.stitch(&constant_tiles(&grid)).unwrap();
        let row = out.row(0).unwrap();
        // The seam is at x = 10 and the band covers 8..12.
        assert_eq!(&row[..8], &[0.0; 8]);
        assert_eq!(&row[12..], &[1.0; 8]);
        assert!(row[8] > 0.0 && row[8] < row[9] && row[9] < 0.5 && row[10] > 0.5 && row[10] < row[11] && row[11] < 1.0, "{row:?}");
        assert!((row[9] + row[10] - 1.0).abs() < 1e-6, "symmetric about the seam");
        // Where bands cross, the four tiles blend bilinearly.
        let grid = TileGrid::new(20, 20, 10, 4).unwrap();
        let out = grid.stitch(&constant_tiles(&grid)).unwrap();
        let (tx, ty) = (ramp(1, 4), ramp(2, 4));
        let expected = (1.0 - ty) * tx + ty * (2.0 + tx);
        assert!((out.get(9, 10, 0).unwrap() - expected).abs() < 1e-6);
        assert_eq!(out.get(3, 15, 0), Some(2.0));
    }

    #[test]
    fn stitch_with_fetches_each_tile_once_in_order() {
        let image = pattern::<f32>(30, 50, 1);
        for overlap in [4, 0] {
            let grid = TileGrid::new(30, 50, 10, overlap).unwrap();
            let tiles = grid.split(&image).unwrap();
            let mut order = Vec::new();
            let out = grid
                .stitch_with(|i| {
                    order.push(i);
                    Ok::<_, TileError>(tiles[i].clone())
                })
                .unwrap();
            assert_eq!(out, image);
            assert_eq!(order, (0..grid.len()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn mapping_a_local_operation_over_tiles_matches_the_whole_image() {
        // A blur whose radius fits in the context (ceil(3 * 1.2) = 4, at most half the overlap) sees the same pixels in
        // a tile as in the whole image, so the tiled result is the same bit for bit.
        let image = pattern::<f32>(70, 45, 3);
        let grid = TileGrid::new(70, 45, 20, 8).unwrap();
        let tiled = grid
            .map(&image, |_, mut tile| {
                gaussian_blur_in_place(&mut tile, 1.2, BlurMethod::Exact).unwrap();
                Ok::<_, TileError>(tile)
            })
            .unwrap();
        let mut whole = image.clone();
        gaussian_blur_in_place(&mut whole, 1.2, BlurMethod::Exact).unwrap();
        assert_eq!(tiled, whole);
        // The results may have another sample type.
        let bytes: Image<u8> = grid.map(&image, |_, tile| Ok::<_, TileError>(tile.convert::<u8>()?)).unwrap();
        assert_eq!(bytes, image.convert::<u8>().unwrap());
    }

    #[test]
    fn mismatched_tiles_are_errors() {
        let grid = TileGrid::new(20, 20, 10, 2).unwrap();
        let image = pattern::<u8>(20, 20, 1);
        let mut tiles = grid.split(&image).unwrap();
        assert!(matches!(grid.stitch(&tiles[1..]), Err(TileError::Count { expected: 4, got: 3 })));
        tiles[2] = Image::new(3, 3, 1, ColorSpace::SRGB).unwrap();
        assert!(matches!(grid.stitch(&tiles), Err(TileError::TileShape { index: 2, .. })));
        tiles[2] = Image::new(12, 12, 2, ColorSpace::SRGB).unwrap();
        assert!(matches!(grid.stitch(&tiles), Err(TileError::TileShape { index: 2, .. })), "channels must agree");
        assert!(matches!(grid.split(&pattern::<u8>(21, 20, 1)), Err(TileError::ImageSize { .. })));
        assert!(matches!(grid.extract(&image, 9), Err(TileError::Index { index: 9, count: 4 })));
        let failing = grid
            .stitch_with(|i| if i < 2 { Ok(image.crop(grid.tile(i).unwrap().outer).unwrap()) } else { Err(TileError::Index { index: i, count: 0 }) });
        assert!(matches!(failing, Err(TileError::Index { index: 2, .. })));
    }
}
