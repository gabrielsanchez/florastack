//! The tiled, streaming framework the methods are built on: [`Tiled`], a [`Stacker`] that keeps a method's
//! accumulators tile by tile in a [`TileCache`] and folds each frame into them one tile at a time, and [`TileKernel`],
//! what a method computes on one tile.
//!
//! # Tiles
//!
//! The canvas is cut by florastack-core's [`TileGrid`] into tiles of [`Tiling::tile_size`] pixels a side (at least four
//! times the overlap), each grown by an overlap of twice the context the kernel needs ([`TileKernel::context`]: the reach of its filters, so that a
//! pixel within `context` pixels of a tile's edge is computed from real neighbors). A frame is read a tile at a time
//! from its [`FrameSource`] (its outer region, overlap included) and folded into that tile's state. At the end, each
//! tile's result is computed from its state, and the tiles are stitched back with feathered seams
//! ([`TileGrid::stitch_with`], which holds two rows of tiles at a time). The stitch takes each pixel from a tile whose
//! outer region reaches at least `context` pixels beyond it, so the result is the one the kernel would give on the
//! whole canvas at once: on a single tile and on many, the same up to the rounding of the box blurs' running sums.
//!
//! # Memory
//!
//! A tile's state is a few planes of `f32` (the weighted average keeps the weighted sum of the frames and the sum of
//! their weights), so the accumulators take a fixed number of bytes per canvas pixel whatever the number of frames. It
//! covers only the tile's inner region, its core grown by half the overlap, the pixels the stitch reads: the kernel
//! computes on the whole outer region for context, and keeps its inner part.
//! They live in a [`TileCache`] with a memory budget ([`Tiling::budget`]): beyond it, the least recently used tiles
//! spill to temporary files ([`TempFileStore`]) and are read back when their turn comes, so that a canvas larger than
//! memory can still be stacked, at the cost of reading and writing the spilled tiles once per frame. Besides the
//! accumulators, a stacker holds the tiles it is working on (as many as rayon has threads) and their scratch planes;
//! the frame itself is the caller's, who reads and drops one at a time.
//!
//! # Determinism, parallelism and cancellation
//!
//! Each frame's tiles are folded in parallel, a batch of as many as rayon has threads at a time. A tile's fold reads
//! only that tile's state and patch, and every per-pixel sum runs over the frames in the order they were added, so the
//! result is the same bit for bit whatever the number of threads. After each batch the stacker reports its progress
//! (tiles done in the frame) to the [`Monitor`] and stops if it was cancelled; the stack in progress is then dropped
//! (see [`Stacker`]).

use std::path::PathBuf;

use florastack_core::{ColorSpace, Image, MemoryStore, Monitor, Rect, StoreError, TempFileStore, Tile, TileCache, TileGrid, TileKey, TileStore};
use rayon::prelude::*;

use crate::error::StackError;
use crate::method::{StackShape, Stacked, Stacker};
use crate::source::{FrameSource, Patch, read_checked};

/// How a [`Tiled`] stacker cuts the canvas and how much memory its accumulators may take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tiling {
    /// The side of a tile's core, in pixels; tiles grow beyond it by the kernel's overlap. At least twice the
    /// overlap is used.
    pub tile_size: usize,
    /// The bytes of accumulator tiles kept in memory; the rest spill to temporary files.
    pub budget: usize,
    /// Where the spilled tiles go (a folder of their own is made in it); `None`: the system's temporary directory.
    pub spill_dir: Option<PathBuf>,
}

impl Tiling {
    /// The default tile size: small enough that a tile's scratch planes, one set per thread, stay a few megabytes.
    pub const DEFAULT_TILE_SIZE: usize = 512;
    /// The default budget: 1 GiB, a 64 MP canvas of RGB accumulators for the weighted average.
    pub const DEFAULT_BUDGET: usize = 1 << 30;
}

impl Default for Tiling {
    fn default() -> Self {
        Tiling { tile_size: Tiling::DEFAULT_TILE_SIZE, budget: Tiling::DEFAULT_BUDGET, spill_dir: None }
    }
}

/// What a tiled method computes on one tile. See the module documentation.
///
/// A tile's state is a list of planes (each an image of 1 to 4 channels), made by [`TileKernel::state`] and changed by
/// [`TileKernel::fold`]; the planes' layout is the kernel's own. It covers the tile's *inner* region, its core grown by
/// half the overlap (clipped to the canvas): the pixels the stitch takes from the tile. The frame's patch covers the
/// outer region, the inner one grown by the context, so the kernel computes on all of it and keeps the inner part.
pub trait TileKernel: Send + Sync {
    /// The method's name, as outputs record it.
    fn name(&self) -> &'static str;

    /// The pixels of context the kernel needs around a pixel to compute it as it would on the whole canvas: how far its
    /// filters reach.
    fn context(&self) -> usize;

    /// A tile's empty state, for a `width` x `height` inner region of frames with `channels` channels.
    fn state(&self, width: usize, height: usize, channels: usize) -> Result<Vec<Image<f32>>, StackError>;

    /// Folds `patch`, the frame on the tile's outer region, into the tile's state, whose pixels are those of `inner`, a
    /// rectangle of the patch.
    fn fold(&self, state: &mut [Image<f32>], patch: &Patch, inner: Rect) -> Result<(), StackError>;

    /// Which pixels of the tile's inner region some frame covered: a one-channel image of 1 (covered) and 0 (not).
    fn covered(&self, state: &[Image<f32>]) -> Result<Image<f32>, StackError>;

    /// The stacked pixels of the tile's inner region, with the frames' `channels`; 0 where no frame covered.
    fn result(&self, state: &[Image<f32>], channels: usize) -> Result<Image<f32>, StackError>;
}

/// Where a stack's accumulator tiles go beyond the budget: nowhere, when they all fit, or temporary files.
#[derive(Debug)]
enum Spill {
    Memory(MemoryStore),
    Files(TempFileStore),
}

impl TileStore for Spill {
    fn write(&mut self, key: TileKey, bytes: &[u8]) -> Result<(), StoreError> {
        match self {
            Spill::Memory(s) => s.write(key, bytes),
            Spill::Files(s) => s.write(key, bytes),
        }
    }

    fn read(&mut self, key: TileKey, buf: &mut Vec<u8>) -> Result<bool, StoreError> {
        match self {
            Spill::Memory(s) => s.read(key, buf),
            Spill::Files(s) => s.read(key, buf),
        }
    }

    fn remove(&mut self, key: TileKey) -> Result<(), StoreError> {
        match self {
            Spill::Memory(s) => s.remove(key),
            Spill::Files(s) => s.remove(key),
        }
    }

    fn stored_bytes(&self) -> u64 {
        match self {
            Spill::Memory(s) => s.stored_bytes(),
            Spill::Files(s) => s.stored_bytes(),
        }
    }
}

/// A stack in progress.
struct InProgress {
    shape: StackShape,
    grid: TileGrid,
    cache: TileCache<Spill>,
    /// How many planes each tile's state has.
    planes: usize,
    frames: usize,
    /// The frames' color space, from the first frame's first patch.
    color: Option<ColorSpace>,
}

impl InProgress {
    fn key(plane: usize, tile: usize) -> TileKey {
        TileKey::new(plane as u64, tile as u64)
    }

    /// Takes tile `tile`'s state out of the cache.
    fn take(&mut self, tile: usize) -> Result<Vec<Image<f32>>, StackError> {
        (0..self.planes)
            .map(|plane| {
                self.cache.take::<f32>(InProgress::key(plane, tile))?.ok_or(StackError::Store(StoreError::Lost { key: InProgress::key(plane, tile) }))
            })
            .collect()
    }

    /// Puts tile `tile`'s state back into the cache.
    fn put(&mut self, tile: usize, state: Vec<Image<f32>>) -> Result<(), StackError> {
        for (plane, image) in state.into_iter().enumerate() {
            self.cache.put(InProgress::key(plane, tile), image)?;
        }
        Ok(())
    }
}

/// A [`Stacker`] made of a [`TileKernel`] run tile by tile. See the module documentation.
pub struct Tiled<K: TileKernel> {
    kernel: K,
    tiling: Tiling,
    stack: Option<InProgress>,
}

impl<K: TileKernel> Tiled<K> {
    pub fn new(kernel: K, tiling: Tiling) -> Tiled<K> {
        Tiled { kernel, tiling, stack: None }
    }

    pub fn kernel(&self) -> &K {
        &self.kernel
    }

    pub fn tiling(&self) -> &Tiling {
        &self.tiling
    }

    /// The tile grid of the stack in progress.
    pub fn grid(&self) -> Option<&TileGrid> {
        self.stack.as_ref().map(|s| &s.grid)
    }

    /// The grid a stack of `shape` is cut into: tiles of the tiling's size, at least four times the overlap, which is
    /// twice the kernel's context, so that a tile computes on at most four times its core's pixels and keeps at most
    /// about one and a half times them.
    fn grid_for(&self, shape: StackShape) -> Result<TileGrid, StackError> {
        let overlap = self.kernel.context().saturating_mul(2);
        let tile_size = self.tiling.tile_size.max(overlap.saturating_mul(4)).max(1);
        Ok(TileGrid::new(shape.width, shape.height, tile_size, overlap)?)
    }

    fn fold_frame(&self, stack: &mut InProgress, frame: &dyn FrameSource, monitor: &dyn Monitor) -> Result<(), StackError> {
        let tiles: Vec<Tile> = stack.grid.tiles().collect();
        let total = tiles.len() as u64;
        let batch = rayon::current_num_threads().max(1);
        let grid = &stack.grid.clone();
        let mut done = 0;
        for chunk in tiles.chunks(batch) {
            monitor.checkpoint()?;
            let mut work = Vec::with_capacity(chunk.len());
            for tile in chunk {
                work.push((*tile, stack.take(tile.index)?));
            }
            let kernel = &self.kernel;
            let folded: Vec<(Tile, Vec<Image<f32>>, Option<ColorSpace>)> = work
                .into_par_iter()
                .map(|(tile, mut state)| {
                    let patch = read_checked(frame, tile.outer)?;
                    kernel.fold(&mut state, &patch, relative(inner(grid, tile), tile.outer))?;
                    Ok((tile, state, (tile.index == 0).then(|| patch.image.color_space().clone())))
                })
                .collect::<Result<_, StackError>>()?;
            for (tile, state, color) in folded {
                if stack.color.is_none() {
                    stack.color = color;
                }
                stack.put(tile.index, state)?;
            }
            done += chunk.len() as u64;
            monitor.step(done, total)?;
        }
        stack.frames += 1;
        Ok(())
    }

    fn finish_stack(&self, mut stack: InProgress, monitor: &dyn Monitor) -> Result<Stacked, StackError> {
        if stack.frames == 0 {
            return Err(StackError::NoFrames);
        }
        let c = stack.shape.channels;
        let n = stack.grid.len();
        // Count the pixels that no frame covered, in the tiles' cores, which partition the canvas.
        let mut uncovered = 0;
        for index in 0..n {
            monitor.checkpoint()?;
            let state = stack.take(index)?;
            let covered = self.kernel.covered(&state)?;
            if let Some(tile) = stack.grid.tile(index) {
                uncovered += count_uncovered(&covered, tile, inner(&stack.grid, tile));
            }
            stack.put(index, state)?;
        }
        let alpha_too = uncovered > 0;
        let passes = if alpha_too { 3 } else { 2 };
        let total = (n * passes) as u64;
        let step = |done: usize| monitor.step(done as u64, total);
        step(n)?;
        let mut done = n;
        let grid = stack.grid.clone();
        let mut image = grid.stitch_with(|index| {
            let state = stack.take(index)?;
            let result = outer_sized(&grid, index, self.kernel.result(&state, c)?)?;
            if alpha_too {
                stack.put(index, state)?;
            }
            done += 1;
            step(done)?;
            Ok::<_, StackError>(result)
        })?;
        let alpha = if alpha_too {
            Some(grid.stitch_with(|index| {
                let covered = outer_sized(&grid, index, self.kernel.covered(&stack.take(index)?)?)?;
                done += 1;
                step(done)?;
                Ok::<_, StackError>(covered)
            })?)
        } else {
            None
        };
        image.set_color_space(stack.color.take().unwrap_or(ColorSpace::LINEAR));
        Ok(Stacked { image, alpha, frames: stack.frames, uncovered })
    }
}

/// A tile's inner region: its core grown by half the grid's overlap, clipped to the canvas. The stitch takes the tile's
/// pixels there and nowhere else.
fn inner(grid: &TileGrid, tile: Tile) -> Rect {
    let half = grid.overlap() / 2;
    let grow = |start: usize, len: usize, side: usize| {
        let from = start.saturating_sub(half);
        (from, start.saturating_add(len).saturating_add(half).min(side) - from)
    };
    let (x, width) = grow(tile.core.x, tile.core.width, grid.width());
    let (y, height) = grow(tile.core.y, tile.core.height, grid.height());
    Rect::new(x, y, width, height)
}

/// `rect` relative to `within`, which holds it.
fn relative(rect: Rect, within: Rect) -> Rect {
    Rect::new(rect.x - within.x, rect.y - within.y, rect.width, rect.height)
}

/// Tile `index`'s result on its inner region, placed in an image of its outer region's size, as the stitch takes
/// tiles; the rest, which the stitch never reads, is 0.
fn outer_sized(grid: &TileGrid, index: usize, result: Image<f32>) -> Result<Image<f32>, StackError> {
    let Some(tile) = grid.tile(index) else { return Ok(result) };
    let at = relative(inner(grid, tile), tile.outer);
    if (at.width, at.height) == (tile.outer.width, tile.outer.height) {
        return Ok(result);
    }
    let c = result.channels();
    let mut out = Image::new(tile.outer.width, tile.outer.height, c, result.color_space().clone())?;
    for (j, src) in result.rows().enumerate() {
        if let Some(dst) = out.row_mut(at.y + j).and_then(|row| row.get_mut(at.x * c..(at.x + at.width) * c)) {
            dst.copy_from_slice(src);
        }
    }
    Ok(out)
}

/// How many pixels of `tile`'s core are 0 in `covered`, a plane of the tile's `inner` region.
fn count_uncovered(covered: &Image<f32>, tile: Tile, inner: Rect) -> usize {
    let core = relative(tile.core, inner);
    (core.y..core.y + core.height)
        .filter_map(|y| covered.row(y))
        .map(|row| row.iter().skip(core.x).take(core.width).filter(|&&v| v <= 0.0).count())
        .sum()
}

impl<K: TileKernel> Stacker for Tiled<K> {
    fn method(&self) -> &'static str {
        self.kernel.name()
    }

    fn begin(&mut self, shape: StackShape) -> Result<(), StackError> {
        self.stack = None;
        shape.check()?;
        let grid = self.grid_for(shape)?;
        // The accumulators' size, from the bytes per pixel of a one-pixel state.
        let pixel = self.kernel.state(1, 1, shape.channels)?;
        let planes = pixel.len();
        let per_pixel: usize = pixel.iter().map(|p| p.channels() * size_of::<f32>()).sum();
        let bytes = grid.tiles().fold(0_usize, |sum, t| {
            let r = inner(&grid, t);
            sum.saturating_add(r.width.saturating_mul(r.height).saturating_mul(per_pixel))
        });
        let store = if bytes > self.tiling.budget {
            let dir = self.tiling.spill_dir.clone().unwrap_or_else(std::env::temp_dir);
            tracing::info!(
                "stacking a {}x{} canvas: its accumulators ({} MiB) exceed the {} MiB budget, so tiles spill to temporary files in {}",
                shape.width,
                shape.height,
                bytes >> 20,
                self.tiling.budget >> 20,
                dir.display()
            );
            Spill::Files(TempFileStore::in_dir(&dir)?)
        } else {
            Spill::Memory(MemoryStore::new())
        };
        let mut stack = InProgress { shape, grid: grid.clone(), cache: TileCache::new(store, self.tiling.budget), planes, frames: 0, color: None };
        // Each tile's state is made and handed to the cache, which spills the oldest beyond the budget.
        for tile in grid.tiles() {
            let r = inner(&grid, tile);
            stack.put(tile.index, self.kernel.state(r.width, r.height, shape.channels)?)?;
        }
        self.stack = Some(stack);
        Ok(())
    }

    fn add_frame(&mut self, frame: &dyn FrameSource, monitor: &dyn Monitor) -> Result<(), StackError> {
        // A frame of the wrong shape is refused before anything is folded, so the stack goes on.
        self.stack.as_ref().ok_or(StackError::NotStarted)?.shape.check_frame(frame)?;
        let mut stack = self.stack.take().ok_or(StackError::NotStarted)?;
        self.fold_frame(&mut stack, frame, monitor)?;
        self.stack = Some(stack);
        Ok(())
    }

    fn finish(&mut self, monitor: &dyn Monitor) -> Result<Stacked, StackError> {
        let stack = self.stack.take().ok_or(StackError::NotStarted)?;
        self.finish_stack(stack, monitor)
    }
}
