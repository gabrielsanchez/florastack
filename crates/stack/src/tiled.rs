//! The tiled, streaming framework the methods are built on: [`Tiled`], a [`Stacker`] that keeps a method's
//! accumulators tile by tile in a [`TileCache`] and folds each frame into them one tile at a time, and [`TileKernel`],
//! what a method computes on one tile.
//!
//! # Tiles
//!
//! The canvas is cut by florastack-core's [`TileGrid`] into tiles of [`Tiling::tile_size`] pixels a side (at least four
//! times the overlap), each grown by an overlap of twice the context the kernel needs ([`TileKernel::context`]: the reach of its filters, so that a
//! pixel within `context` pixels of a tile's edge is computed from real neighbors). A frame is read a tile at a time
//! from its [`FrameSource`] (its outer region, overlap included, or that region grown to the kernel's lattice:
//! [`TileKernel::read_region`]) and folded into that tile's state. At the end, each
//! tile's result is computed from its state, and the tiles are stitched back with feathered seams
//! ([`TileGrid::stitch_with`], which holds two rows of tiles at a time). The stitch takes each pixel from a tile whose
//! outer region reaches at least `context` pixels beyond it, so the result is the one the kernel would give on the
//! whole canvas at once: on a single tile and on many, the same up to the rounding of the box blurs' running sums.
//!
//! # Whole-canvas parts
//!
//! A multi-scale kernel, such as the pyramid method's, computes its coarse levels on the whole canvas at a reduced
//! scale instead of in tiles, whose overlap would otherwise have to grow with the coarsest level's reach. Each tile's
//! fold then hands back its share of the frame's reduced image (its core's samples, [`TileView::core`]); once every
//! tile of a frame is folded, the kernel absorbs the shares in tile order ([`TileKernel::absorb`]), and before the
//! results it prepares what they read from that part ([`TileKernel::prepare`]). The whole-canvas part belongs to the
//! kernel, outside the tile cache and its budget: it is a small fraction of the canvas (1/64 of its pixels per plane
//! for the pyramid method). Kernels without one keep the defaults, which do nothing.
//!
//! # Memory
//!
//! A tile's state is a few planes of `f32`, or of `u16` for compact bounds ([`StatePlane`]): the weighted average keeps
//! the weighted sum of the frames and the sum of their weights, so the accumulators take a fixed number of bytes per
//! canvas pixel whatever the number of frames. It covers only the tile's inner region, its core grown by half the
//! overlap, the pixels the stitch reads: the kernel computes on the whole outer region for context, and keeps its inner
//! part. They live in a [`TileCache`] with a memory budget ([`Tiling::budget`]): beyond it, the least recently used tiles
//! spill to temporary files ([`TempFileStore`]) and are read back when their turn comes, so that a canvas larger than
//! memory can still be stacked, at the cost of reading and writing the spilled tiles once per frame. Besides the
//! accumulators, a stacker holds the tiles it is working on (as many as rayon has threads) and their scratch planes;
//! the frame itself is the caller's, who reads and drops one at a time.
//!
//! # Determinism, parallelism and cancellation
//!
//! Each frame's tiles are folded in parallel, a batch of as many as rayon has threads at a time. A tile's fold reads
//! only that tile's state and patch, every per-pixel sum runs over the frames in the order they were added, and shares
//! of a whole-canvas part are absorbed in tile order, so the result is the same bit for bit whatever the number of
//! threads. After each batch the stacker reports its progress (tiles done in the frame) to the [`Monitor`] and stops if
//! it was cancelled; the stack in progress is then dropped (see [`Stacker`]).

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

/// A plane of a tile's state: `f32` samples, or `u16` ones for bounds that need less precision.
#[derive(Clone, Debug, PartialEq)]
pub enum StatePlane {
    F32(Image<f32>),
    U16(Image<u16>),
}

impl StatePlane {
    /// The plane's bytes in memory.
    pub fn bytes(&self) -> usize {
        match self {
            StatePlane::F32(i) => size_of_val(i.as_raw()),
            StatePlane::U16(i) => size_of_val(i.as_raw()),
        }
    }

    pub fn as_f32(&self) -> Option<&Image<f32>> {
        if let StatePlane::F32(i) = self { Some(i) } else { None }
    }

    pub fn as_f32_mut(&mut self) -> Option<&mut Image<f32>> {
        if let StatePlane::F32(i) = self { Some(i) } else { None }
    }

    pub fn as_u16(&self) -> Option<&Image<u16>> {
        if let StatePlane::U16(i) = self { Some(i) } else { None }
    }

    pub fn as_u16_mut(&mut self) -> Option<&mut Image<u16>> {
        if let StatePlane::U16(i) = self { Some(i) } else { None }
    }
}

/// Where a tile lies on the canvas, as its fold sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileView {
    /// The region the frame's patch covers ([`TileKernel::read_region`] of the tile's outer region).
    pub region: Rect,
    /// The tile's inner region, which its state covers: its core grown by half the overlap, clipped to the canvas.
    pub inner: Rect,
    /// The tile's core: the cores partition the canvas.
    pub core: Rect,
    /// The frame's place in the stack: 0 for the first frame added since [`Stacker::begin`].
    pub frame: usize,
}

impl TileView {
    /// The inner region as a rectangle of the patch.
    pub fn inner_in_patch(&self) -> Rect {
        relative(self.inner, self.region)
    }
}

/// A tile's share of a frame's whole-canvas part: planes of the kernel's own layout. See the module documentation.
pub type Share = Vec<Image<f32>>;

/// What a tiled method computes on one tile. See the module documentation.
///
/// A tile's state is a list of planes, made by [`TileKernel::state`] and changed by [`TileKernel::fold`]; the planes'
/// layout is the kernel's own. It covers the tile's *inner* region, its core grown by half the overlap (clipped to the
/// canvas): the pixels the stitch takes from the tile. The frame's patch covers the outer region, the inner one grown
/// by the context, so the kernel computes on all of it and keeps the inner part.
pub trait TileKernel: Send + Sync {
    /// The method's name, as outputs record it.
    fn name(&self) -> &'static str;

    /// The pixels of context the kernel needs around a pixel to compute it as it would on the whole canvas: how far its
    /// filters reach.
    fn context(&self) -> usize;

    /// The region of a `width` x `height` canvas a tile whose outer region is `outer` reads: `outer` itself, or for a
    /// kernel that decimates, `outer` grown so that its origin lies on the kernel's lattice. It must hold `outer` and
    /// lie inside the canvas.
    fn read_region(&self, outer: Rect, width: usize, height: usize) -> Rect {
        let _ = (width, height);
        outer
    }

    /// A tile's empty state, for its `inner` region (on the canvas) of frames with `channels` channels.
    fn state(&self, inner: Rect, channels: usize) -> Result<Vec<StatePlane>, StackError>;

    /// Folds `patch`, the frame on `view.region`, into the tile's state, and returns the tile's share of the frame's
    /// whole-canvas part if the kernel has one.
    fn fold(&self, state: &mut [StatePlane], patch: &Patch, view: &TileView) -> Result<Option<Share>, StackError>;

    /// Starts the whole-canvas part of a stack of `shape`, dropping any other. The default does nothing.
    fn start(&mut self, shape: StackShape) -> Result<(), StackError> {
        let _ = shape;
        Ok(())
    }

    /// Folds a frame's shares, in tile order, into the whole-canvas part. The default does nothing.
    fn absorb(&mut self, shares: Vec<(Tile, Share)>) -> Result<(), StackError> {
        let _ = shares;
        Ok(())
    }

    /// Readies what the tiles' results read from the whole-canvas part, once every frame is folded. The default does
    /// nothing.
    fn prepare(&mut self, monitor: &dyn Monitor) -> Result<(), StackError> {
        let _ = monitor;
        Ok(())
    }

    /// Which pixels of the tile's inner region some frame covered: a one-channel image of 1 (covered) and 0 (not).
    fn covered(&self, state: &[StatePlane]) -> Result<Image<f32>, StackError>;

    /// The stacked pixels of the tile's `inner` region (on the canvas), with the frames' `channels`; 0 where no frame
    /// covered.
    fn result(&self, state: &[StatePlane], inner: Rect, channels: usize) -> Result<Image<f32>, StackError>;
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

/// The sample type of each plane of a tile's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    F32,
    U16,
}

impl Kind {
    fn of(plane: &StatePlane) -> Kind {
        match plane {
            StatePlane::F32(_) => Kind::F32,
            StatePlane::U16(_) => Kind::U16,
        }
    }
}

/// A stack in progress.
struct InProgress {
    shape: StackShape,
    grid: TileGrid,
    cache: TileCache<Spill>,
    /// The sample type of each plane of a tile's state.
    kinds: Vec<Kind>,
    frames: usize,
    /// The frames' color space, from the first frame's first patch.
    color: Option<ColorSpace>,
}

impl InProgress {
    fn key(plane: usize, tile: usize) -> TileKey {
        TileKey::new(plane as u64, tile as u64)
    }

    /// Takes tile `tile`'s state out of the cache.
    fn take(&mut self, tile: usize) -> Result<Vec<StatePlane>, StackError> {
        let kinds = self.kinds.clone();
        kinds
            .iter()
            .enumerate()
            .map(|(plane, kind)| {
                let key = InProgress::key(plane, tile);
                let lost = || StackError::Store(StoreError::Lost { key });
                Ok(match kind {
                    Kind::F32 => StatePlane::F32(self.cache.take::<f32>(key)?.ok_or_else(lost)?),
                    Kind::U16 => StatePlane::U16(self.cache.take::<u16>(key)?.ok_or_else(lost)?),
                })
            })
            .collect()
    }

    /// Puts tile `tile`'s state back into the cache.
    fn put(&mut self, tile: usize, state: Vec<StatePlane>) -> Result<(), StackError> {
        if state.len() != self.kinds.len() || state.iter().zip(&self.kinds).any(|(p, k)| Kind::of(p) != *k) {
            return Err(StackError::Shape { width: 0, height: 0, channels: state.len() });
        }
        for (plane, image) in state.into_iter().enumerate() {
            let key = InProgress::key(plane, tile);
            match image {
                StatePlane::F32(i) => self.cache.put(key, i)?,
                StatePlane::U16(i) => self.cache.put(key, i)?,
            }
        }
        Ok(())
    }
}

/// A tile folded: its state, its share of a whole-canvas part, and the frame's color space (from the first tile).
type Folded = (Tile, Vec<StatePlane>, Option<Share>, Option<ColorSpace>);

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

    /// Folds `frame` into every tile, and returns the tiles' shares of the frame's whole-canvas part, in tile order.
    fn fold_frame(&self, stack: &mut InProgress, frame: &dyn FrameSource, monitor: &dyn Monitor) -> Result<Vec<(Tile, Share)>, StackError> {
        let tiles: Vec<Tile> = stack.grid.tiles().collect();
        let total = tiles.len() as u64;
        let batch = rayon::current_num_threads().max(1);
        let grid = &stack.grid.clone();
        let (w, h) = (stack.shape.width, stack.shape.height);
        let index = stack.frames;
        let mut shares = Vec::new();
        let mut done = 0;
        for chunk in tiles.chunks(batch) {
            monitor.checkpoint()?;
            let mut work = Vec::with_capacity(chunk.len());
            for tile in chunk {
                work.push((*tile, stack.take(tile.index)?));
            }
            let kernel = &self.kernel;
            let folded: Vec<Folded> = work
                .into_par_iter()
                .map(|(tile, mut state)| {
                    let region = kernel.read_region(tile.outer, w, h);
                    let view = TileView { region, inner: inner(grid, tile), core: tile.core, frame: index };
                    let patch = read_checked(frame, region)?;
                    let share = kernel.fold(&mut state, &patch, &view)?;
                    Ok((tile, state, share, (tile.index == 0).then(|| patch.image.color_space().clone())))
                })
                .collect::<Result<_, StackError>>()?;
            for (tile, state, share, color) in folded {
                if stack.color.is_none() {
                    stack.color = color;
                }
                stack.put(tile.index, state)?;
                if let Some(share) = share {
                    shares.push((tile, share));
                }
            }
            done += chunk.len() as u64;
            monitor.step(done, total)?;
        }
        stack.frames += 1;
        Ok(shares)
    }

    fn finish_stack(&self, mut stack: InProgress, monitor: &dyn Monitor) -> Result<Stacked, StackError> {
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
            let at = grid.tile(index).map_or(Rect::new(0, 0, grid.width(), grid.height()), |t| inner(&grid, t));
            let result = outer_sized(&grid, index, self.kernel.result(&state, at, c)?)?;
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
    Rect::new(rect.x.saturating_sub(within.x), rect.y.saturating_sub(within.y), rect.width, rect.height)
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
        self.kernel.start(shape)?;
        // The accumulators' size: every tile's state, made and dropped to be measured.
        let mut kinds = None;
        let mut bytes = 0_usize;
        for tile in grid.tiles() {
            let state = self.kernel.state(inner(&grid, tile), shape.channels)?;
            bytes = bytes.saturating_add(state.iter().map(StatePlane::bytes).sum());
            kinds.get_or_insert_with(|| state.iter().map(Kind::of).collect::<Vec<_>>());
        }
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
        let mut stack = InProgress {
            shape,
            grid: grid.clone(),
            cache: TileCache::new(store, self.tiling.budget),
            kinds: kinds.unwrap_or_default(),
            frames: 0,
            color: None,
        };
        // Each tile's state is made and handed to the cache, which spills the oldest beyond the budget.
        for tile in grid.tiles() {
            stack.put(tile.index, self.kernel.state(inner(&grid, tile), shape.channels)?)?;
        }
        self.stack = Some(stack);
        Ok(())
    }

    fn add_frame(&mut self, frame: &dyn FrameSource, monitor: &dyn Monitor) -> Result<(), StackError> {
        // A frame of the wrong shape is refused before anything is folded, so the stack goes on.
        self.stack.as_ref().ok_or(StackError::NotStarted)?.shape.check_frame(frame)?;
        let mut stack = self.stack.take().ok_or(StackError::NotStarted)?;
        let shares = self.fold_frame(&mut stack, frame, monitor)?;
        self.kernel.absorb(shares)?;
        self.stack = Some(stack);
        Ok(())
    }

    fn finish(&mut self, monitor: &dyn Monitor) -> Result<Stacked, StackError> {
        let stack = self.stack.take().ok_or(StackError::NotStarted)?;
        if stack.frames == 0 {
            return Err(StackError::NoFrames);
        }
        self.kernel.prepare(monitor)?;
        self.finish_stack(stack, monitor)
    }
}
