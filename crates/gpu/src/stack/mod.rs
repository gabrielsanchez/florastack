//! The stacking methods on the GPU ([`GpuStacker`]): the weighted average, the pyramid method, and the depth map's
//! two passes (its focus statistics and its composite), each a port of florastack-stack's tile kernel, run tile by
//! tile as florastack-stack's `Tiled` runs it. florastack-stack is the reference ([`Method`] says which kernel each
//! mirrors): the agreement tests stack the same frames both ways and compare the results.
//!
//! # Streaming
//!
//! A stacker holds one stack's accumulators, never its frames: each frame comes as a [`Frame`], folded in and
//! dropped. A frame to warp (the engine's decoded frames, with their alignment) is uploaded a block at a time, the
//! block of the frame each tile's warp reads, and warped on the GPU ([`crate::ops::WarpGeometry`]); a frame already on
//! the canvas (a slab) is uploaded a tile at a time. The accumulators stay on the GPU between frames.
//!
//! # Tiles and memory
//!
//! The canvas is cut into tiles by florastack-core's [`TileGrid`], with the overlap the CPU's kernels need (twice
//! their reach), so that every tile computes its pixels as the whole canvas would. Tiles are larger than the CPU's
//! (many threads share one GPU tile), and small enough that every plane fits the device's binding limit (128 MiB by
//! WebGPU's defaults). Each tile's accumulators live in VRAM within [`GpuTiling::budget`]; beyond it, the least recently used
//! tiles are read back to the host, into florastack-core's [`TileCache`] (which spills to temporary files beyond its
//! own budget), and uploaded again when their turn comes. The tiles of a frame are visited in alternating order, so
//! that the tiles a frame ends with are still resident when the next one starts. Besides the accumulators, a tile in
//! flight takes its patch and the method's scratch planes: a few dozen bytes a pixel of the tile.
//!
//! # Determinism
//!
//! Each output sample is computed by one invocation, in a fixed order of taps and frames, so a stack is the same on
//! the same device for the same frames in the same order whatever the tile order. Devices differ from one another and
//! from the CPU in the last bits of their floating point; the agreement tests bound by how much.
//!
//! # Waiting
//!
//! Readbacks wait for the GPU, which native targets can do; in a browser nothing may block, so the stackers need the
//! web build's jobs to await them (roadmap 7.3), and until then the engine stacks there on the CPU.

mod average;
mod depth;
mod pyramid;

pub use pyramid::levels_for;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use florastack_core::{ColorSpace, Image, MemoryStore, Monitor, Rect, StoreError, TempFileStore, Tile, TileCache, TileGrid, TileKey, TileStore};

use crate::batch::Batch;
use crate::device::Gpu;
use crate::error::GpuError;
use crate::ops::WarpGeometry;
use crate::plane::Plane;

/// A stacking method on the GPU, with the settings its kernels take. The engine makes them from florastack-stack's
/// settings, whose checks they share.
#[derive(Clone, Debug, PartialEq)]
pub enum Method {
    /// The weighted average: florastack-stack's `WeightedAverage`, with its `radius`, `smoothing` and `power`.
    Average { radius: f64, smoothing: f64, power: f64 },
    /// The pyramid method: florastack-stack's `Pyramid`, with its `levels` setting (0 chooses from the canvas, as
    /// [`levels_for`] does), the finest level's blend power (`1 / grit`; `None` picks), decisions on every channel or
    /// on luminance, and the range limit (0 off).
    Pyramid { levels: u32, blend: Option<f32>, channel_decisions: bool, range_limit: f32 },
    /// The depth map's pass 1: florastack-stack's `FocusKernel` with its estimation `radius`. Its result has three
    /// channels: each pixel's interpolated depth, its confidence and its guide luminance.
    Focus { radius: f64 },
    /// The depth map's pass 2: florastack-stack's `CompositeKernel`, compositing `frames` frames from `depth`.
    Composite { depth: Arc<Image<f32>>, frames: usize },
}

impl Method {
    /// The method's name, as florastack-stack records it.
    pub fn name(&self) -> &'static str {
        match self {
            Method::Average { .. } => "average",
            Method::Pyramid { .. } => "pyramid",
            Method::Focus { .. } | Method::Composite { .. } => "depth-map",
        }
    }
}

/// How a [`GpuStacker`] cuts the canvas and how much memory it may take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuTiling {
    /// The side of a tile's core, in pixels: at least four times the overlap is used, and less where the device's
    /// binding limit asks for it.
    pub tile_size: usize,
    /// The bytes of accumulator tiles kept on the GPU; beyond, tiles are read back to the host.
    pub budget: usize,
    /// The bytes of read-back tiles kept in host memory; beyond, they spill to temporary files.
    pub host_budget: usize,
    /// Where those files go; `None`: the system's temporary directory.
    pub spill_dir: Option<PathBuf>,
}

impl GpuTiling {
    pub const DEFAULT_TILE_SIZE: usize = 1536;
    /// 1 GiB: a 24 MP canvas of any method's accumulators, on a GPU with 4 GB.
    pub const DEFAULT_BUDGET: usize = 1 << 30;
    pub const DEFAULT_HOST_BUDGET: usize = 1 << 30;
}

impl Default for GpuTiling {
    fn default() -> Self {
        GpuTiling {
            tile_size: GpuTiling::DEFAULT_TILE_SIZE,
            budget: GpuTiling::DEFAULT_BUDGET,
            host_budget: GpuTiling::DEFAULT_HOST_BUDGET,
            spill_dir: None,
        }
    }
}

/// A frame as a [`GpuStacker`] reads it. See the module documentation.
#[derive(Clone, Copy, Debug)]
pub enum Frame<'a> {
    /// A decoded frame, warped onto the canvas on the GPU as `geometry` says (its frame size is the image's).
    Warp { image: &'a Image<f32>, geometry: WarpGeometry },
    /// A frame already on the canvas, with its coverage (one channel) where it doesn't cover all of it.
    Canvas { image: &'a Image<f32>, coverage: Option<&'a Image<f32>> },
}

/// What a GPU stack gives: florastack-stack's `Stacked`, field for field.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuStacked {
    /// The stacked image, linear, 0 where no frame covers.
    pub image: Image<f32>,
    /// 1 where some frame covers, 0 where none does; `None` when frames cover every pixel.
    pub alpha: Option<Image<f32>>,
    pub frames: usize,
    pub uncovered: usize,
}

/// The canvas of a stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) channels: usize,
}

/// Where a tile lies, as a method's fold sees it (florastack-stack's `TileView`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct View {
    /// What the patch covers.
    pub(crate) region: Rect,
    /// What the tile's state covers: its core grown by half the overlap, clipped.
    pub(crate) inner: Rect,
    pub(crate) core: Rect,
    /// The frame's place in the stack, from 0.
    pub(crate) frame: usize,
}

impl View {
    /// The inner region's offset in the patch.
    pub(crate) fn offset(&self) -> (usize, usize) {
        (self.inner.x - self.region.x, self.inner.y - self.region.y)
    }
}

/// A frame's pixels on a tile's region, on the GPU, with its coverage when it has one.
pub(crate) struct Patch {
    pub(crate) image: Plane,
    pub(crate) coverage: Option<Plane>,
}

/// What a method computes on one tile on the GPU: florastack-stack's `TileKernel`, on planes. See the module
/// documentation.
/// `Send` on native targets, where stacks run on job threads; in a browser wgpu's resources stay on their thread.
pub(crate) trait TileMethod: wgpu::WasmNotSend {
    /// The pixels of context around a pixel the method's filters read.
    fn context(&self) -> usize;

    /// The region a tile whose outer region is `outer` reads.
    fn read_region(&self, outer: Rect, width: usize, height: usize) -> Rect {
        let _ = (width, height);
        outer
    }

    /// Starts a stack on `shape`, dropping any other.
    fn start(&mut self, gpu: &Gpu, shape: Shape) -> Result<(), GpuError> {
        let _ = (gpu, shape);
        Ok(())
    }

    /// A tile's empty state for its inner region, its initial values recorded into `batch`.
    fn state(&self, batch: &mut Batch<'_>, view: &View, shape: Shape) -> Result<Vec<Plane>, GpuError>;

    /// Records the fold of `patch` into a tile's `state`.
    fn fold(&self, batch: &mut Batch<'_>, state: &[Plane], patch: &Patch, view: &View, shape: Shape) -> Result<(), GpuError>;

    /// Records what follows a frame's tiles: the pyramid's whole-canvas levels.
    fn end_frame(&mut self, batch: &mut Batch<'_>) -> Result<(), GpuError> {
        let _ = batch;
        Ok(())
    }

    /// Records what the results read once every frame is in.
    fn prepare(&mut self, batch: &mut Batch<'_>) -> Result<(), GpuError> {
        let _ = batch;
        Ok(())
    }

    /// Records a tile's result over its inner region and which of its pixels are covered (one channel of 0 and 1).
    fn result(&self, batch: &mut Batch<'_>, state: &[Plane], view: &View, shape: Shape) -> Result<(Plane, Plane), GpuError>;
}

/// How long the GPU may lag behind the uploads, in bytes of patches and blocks submitted, before the host waits.
const IN_FLIGHT: u64 = 512 << 20;

/// Where read-back tiles go: memory, or temporary files beyond the host budget.
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

/// The tiles' states: the ones on the GPU, least recently used first, and the ones read back to the host.
struct Residency {
    resident: HashMap<usize, Vec<Plane>>,
    order: VecDeque<usize>,
    bytes: u64,
    budget: u64,
    /// The planes' shapes of every tile read back, by tile.
    shapes: HashMap<usize, Vec<(usize, usize, usize)>>,
    host: TileCache<Spill>,
    /// How many tiles were read back and uploaded again.
    evictions: usize,
}

fn key(plane: usize, tile: usize) -> TileKey {
    TileKey::new(plane as u64, tile as u64)
}

impl Residency {
    fn new(tiling: &GpuTiling, total: u64) -> Result<Residency, GpuError> {
        let store = if total > tiling.budget as u64 + tiling.host_budget as u64 {
            let dir = tiling.spill_dir.clone().unwrap_or_else(std::env::temp_dir);
            Spill::Files(TempFileStore::in_dir(&dir)?)
        } else {
            Spill::Memory(MemoryStore::new())
        };
        Ok(Residency {
            resident: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            budget: tiling.budget as u64,
            shapes: HashMap::new(),
            host: TileCache::new(store, tiling.host_budget),
            evictions: 0,
        })
    }

    /// Makes room for `bytes` more on the GPU, reading back the least recently used tiles but `keep`.
    fn make_room(&mut self, gpu: &Gpu, batch: &mut Batch<'_>, bytes: u64, keep: usize) -> Result<(), GpuError> {
        while self.bytes + bytes > self.budget {
            let Some(pos) = self.order.iter().position(|&t| t != keep) else { return Ok(()) };
            let Some(victim) = self.order.remove(pos) else { return Ok(()) };
            let Some(planes) = self.resident.remove(&victim) else { continue };
            // The tile's recorded work must be done before it is read.
            batch.submit();
            let mut shapes = Vec::with_capacity(planes.len());
            for (i, plane) in planes.iter().enumerate() {
                let words = gpu.read_words(plane, "a tile read back")?;
                let samples = words.into_iter().map(f32::from_bits).collect();
                // One channel of `width * channels` samples a row: images hold at most 4 channels, a record 8.
                let image = Image::from_vec(plane.width() * plane.channels(), plane.height(), 1, ColorSpace::LINEAR, samples)?;
                self.host.put(key(i, victim), image)?;
                shapes.push((plane.width(), plane.height(), plane.channels()));
                self.bytes = self.bytes.saturating_sub(plane.bytes());
            }
            self.shapes.insert(victim, shapes);
            self.evictions += 1;
        }
        Ok(())
    }

    /// Tile `tile`'s state on the GPU: resident, read back and uploaded again, or made by `make`.
    fn get<'p>(
        &'p mut self,
        gpu: &Gpu,
        batch: &mut Batch<'_>,
        tile: usize,
        make: impl FnOnce(&mut Batch<'_>) -> Result<Vec<Plane>, GpuError>,
    ) -> Result<&'p [Plane], GpuError> {
        if self.resident.contains_key(&tile) {
            self.order.retain(|&t| t != tile);
            self.order.push_back(tile);
        } else {
            let planes = if let Some(shapes) = self.shapes.remove(&tile) {
                let bytes = shapes.iter().map(|&(w, h, c)| (w * h * c) as u64 * 4).sum();
                self.make_room(gpu, batch, bytes, tile)?;
                let mut planes = Vec::with_capacity(shapes.len());
                for (i, (w, h, c)) in shapes.into_iter().enumerate() {
                    let image = self.host.take::<f32>(key(i, tile))?.ok_or(StoreError::Lost { key: key(i, tile) })?;
                    let plane = gpu.plane("a tile's state", w, h, c)?;
                    let words: Vec<u32> = image.rows().flatten().map(|v| v.to_bits()).collect();
                    gpu.write_words(&plane, &words);
                    planes.push(plane);
                }
                planes
            } else {
                let planes = make(batch)?;
                let bytes = planes.iter().map(Plane::bytes).sum();
                self.make_room(gpu, batch, bytes, tile)?;
                planes
            };
            self.bytes += planes.iter().map(Plane::bytes).sum::<u64>();
            self.resident.insert(tile, planes);
            self.order.push_back(tile);
        }
        self.resident.get(&tile).map(Vec::as_slice).ok_or(GpuError::Readback { what: "a tile's state" })
    }
}

/// A stack in progress.
struct InProgress {
    shape: Shape,
    grid: TileGrid,
    residency: Residency,
    frames: usize,
    color: Option<ColorSpace>,
}

/// A stacking method run on the GPU, one frame at a time. See the module documentation.
pub struct GpuStacker {
    gpu: Gpu,
    method: Box<dyn TileMethod>,
    name: &'static str,
    tiling: GpuTiling,
    stack: Option<InProgress>,
}

/// A tile's inner region: its core grown by half the grid's overlap, clipped (florastack-stack's `tiled::inner`).
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

/// Tile `tile`'s result on its inner region placed in an image of its outer region's size, as the stitch takes tiles.
fn outer_sized(tile: Tile, inner: Rect, result: Image<f32>) -> Result<Image<f32>, GpuError> {
    if (inner.width, inner.height) == (tile.outer.width, tile.outer.height) {
        return Ok(result);
    }
    let (x, y) = (inner.x - tile.outer.x, inner.y - tile.outer.y);
    let c = result.channels();
    let mut out = Image::new(tile.outer.width, tile.outer.height, c, result.color_space().clone())?;
    for (j, src) in result.rows().enumerate() {
        if let Some(dst) = out.row_mut(y + j).and_then(|row| row.get_mut(x * c..(x + inner.width) * c)) {
            dst.copy_from_slice(src);
        }
    }
    Ok(out)
}

impl GpuStacker {
    /// A stacker of `method` on `gpu`, its settings checked as florastack-stack checks them.
    pub fn new(gpu: &Gpu, method: Method, tiling: GpuTiling) -> Result<GpuStacker, GpuError> {
        let name = method.name();
        let method: Box<dyn TileMethod> = match method {
            Method::Average { radius, smoothing, power } => Box::new(average::Average::new(gpu, radius, smoothing, power)?),
            Method::Pyramid { levels, blend, channel_decisions, range_limit } => {
                Box::new(pyramid::Pyramid::new(levels, blend, channel_decisions, range_limit)?)
            }
            Method::Focus { radius } => Box::new(depth::Focus::new(gpu, radius)?),
            Method::Composite { depth, frames } => Box::new(depth::Composite::new(depth, frames)?),
        };
        Ok(GpuStacker { gpu: gpu.clone(), method, name, tiling, stack: None })
    }

    pub fn method(&self) -> &'static str {
        self.name
    }

    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// How many tiles were read back to the host and uploaded again in the stack in progress, for the tests that
    /// force it with a small budget.
    pub fn evictions(&self) -> usize {
        self.stack.as_ref().map_or(0, |s| s.residency.evictions)
    }

    /// The grid of a `width` x `height` canvas: florastack-stack's overlap (twice the method's context), tiles at
    /// least four times it, halved until a tile's largest plane (48 bytes a pixel bounds them) fits the device.
    fn grid(&self, width: usize, height: usize) -> Result<TileGrid, GpuError> {
        let overlap = self.method.context().saturating_mul(2);
        let mut tile = self.tiling.tile_size.max(1);
        let fits = |tile: usize| {
            let side = (tile + 2 * overlap) as u64;
            side * side * 48 <= self.gpu.max_binding()
        };
        while tile > 4 * overlap.max(16) && !fits(tile) {
            tile /= 2;
        }
        Ok(TileGrid::new(width, height, tile.max(overlap.saturating_mul(4)).max(1), overlap)?)
    }

    /// Starts a stack on a `width` x `height` canvas of frames of `channels` channels (1 to 4), dropping any other.
    pub fn begin(&mut self, width: usize, height: usize, channels: usize) -> Result<(), GpuError> {
        self.stack = None;
        if width == 0 || height == 0 || !(1..=4).contains(&channels) {
            return Err(GpuError::shape("a canvas", format!("{width}x{height} with {channels} channels"), "at least 1x1 with 1 to 4"));
        }
        let shape = Shape { width, height, channels };
        let grid = self.grid(width, height)?;
        self.method.start(&self.gpu, shape)?;
        // The accumulators' size, from the first tile's: for the host store's choice.
        let total = (width * height * channels * 4 * 12) as u64;
        let residency = Residency::new(&self.tiling, total)?;
        self.stack = Some(InProgress { shape, grid, residency, frames: 0, color: None });
        Ok(())
    }

    /// `frame`'s pixels on `region` on the GPU.
    fn patch(gpu: &Gpu, batch: &mut Batch<'_>, frame: &Frame<'_>, region: Rect) -> Result<(Patch, u64), GpuError> {
        match *frame {
            Frame::Warp { image, geometry } => {
                let geometry = WarpGeometry { frame_width: image.width(), frame_height: image.height(), ..geometry };
                let block_rect = geometry.block(region);
                let block = gpu.upload_rect(image, block_rect)?;
                let warped = gpu.plane("a warped patch", region.width, region.height, image.channels())?;
                let coverage = gpu.plane("a patch's coverage", region.width, region.height, 1)?;
                geometry.record(batch, &block, block_rect, region, &warped, &coverage)?;
                let bytes = block.bytes() + warped.bytes();
                Ok((Patch { image: warped, coverage: Some(coverage) }, bytes))
            }
            Frame::Canvas { image, coverage } => {
                let plane = gpu.upload_rect(image, region)?;
                let coverage = coverage.map(|k| gpu.upload_rect(k, region)).transpose()?;
                let bytes = plane.bytes() + coverage.as_ref().map_or(0, Plane::bytes);
                Ok((Patch { image: plane, coverage }, bytes))
            }
        }
    }

    /// Checks that `frame` fits the canvas.
    fn check_frame(shape: Shape, frame: &Frame<'_>) -> Result<(), GpuError> {
        let (got, ok) = match *frame {
            Frame::Warp { image, geometry } => {
                let g = WarpGeometry { frame_width: image.width(), frame_height: image.height(), ..geometry };
                g.check()?;
                (image.channels(), image.channels() == shape.channels)
            }
            Frame::Canvas { image, coverage } => {
                let fits = (image.width(), image.height(), image.channels()) == (shape.width, shape.height, shape.channels);
                let cov = coverage.is_none_or(|k| (k.width(), k.height(), k.channels()) == (shape.width, shape.height, 1));
                (image.channels(), fits && cov)
            }
        };
        if ok {
            Ok(())
        } else {
            Err(GpuError::shape(
                "a frame",
                format!("{got} channels"),
                format!("the canvas's {}x{} with {}", shape.width, shape.height, shape.channels),
            ))
        }
    }

    /// Folds the next frame into the stack. `monitor` hears the progress within the frame (in tiles) and cancels at
    /// the next tile; a cancelled or failed frame ends the stack.
    pub fn add_frame(&mut self, frame: &Frame<'_>, monitor: &dyn Monitor) -> Result<(), GpuError> {
        let shape = self.stack.as_ref().ok_or(GpuError::Unsupported { what: "a frame before `begin`" })?.shape;
        Self::check_frame(shape, frame)?;
        let mut stack = self.stack.take().ok_or(GpuError::Unsupported { what: "a frame before `begin`" })?;
        self.fold_frame(&mut stack, frame, monitor)?;
        self.stack = Some(stack);
        Ok(())
    }

    fn fold_frame(&mut self, stack: &mut InProgress, frame: &Frame<'_>, monitor: &dyn Monitor) -> Result<(), GpuError> {
        let gpu = self.gpu.clone();
        let tiles: Vec<Tile> = stack.grid.tiles().collect();
        let n = tiles.len();
        // Alternate the order, so that the tiles a frame ends with are still on the GPU when the next one starts.
        let order: Vec<Tile> = if stack.frames.is_multiple_of(2) { tiles } else { tiles.into_iter().rev().collect() };
        let (w, h) = (stack.shape.width, stack.shape.height);
        let mut batch = Batch::new(&gpu);
        let mut in_flight = 0;
        for (done, tile) in order.into_iter().enumerate() {
            monitor.checkpoint()?;
            let region = self.method.read_region(tile.outer, w, h);
            let view = View { region, inner: inner(&stack.grid, tile), core: tile.core, frame: stack.frames };
            let (patch, bytes) = Self::patch(&gpu, &mut batch, frame, region)?;
            let method = &self.method;
            let state = stack.residency.get(&gpu, &mut batch, tile.index, |b| method.state(b, &view, stack.shape))?;
            method.fold(&mut batch, state, &patch, &view, stack.shape)?;
            batch.submit();
            in_flight += bytes;
            if in_flight > IN_FLIGHT {
                gpu.wait()?;
                in_flight = 0;
            }
            monitor.step(done as u64 + 1, n as u64)?;
        }
        self.method.end_frame(&mut batch)?;
        batch.finish()?;
        if stack.color.is_none() {
            stack.color = Some(match frame {
                Frame::Warp { image, .. } | Frame::Canvas { image, .. } => image.color_space().clone(),
            });
        }
        stack.frames += 1;
        Ok(())
    }

    /// The tile's result and coverage, read back.
    fn tile_result(&self, stack: &mut InProgress, tile: Tile, image: bool) -> Result<Image<f32>, GpuError> {
        let gpu = self.gpu.clone();
        let view = View {
            region: self.method.read_region(tile.outer, stack.shape.width, stack.shape.height),
            inner: inner(&stack.grid, tile),
            core: tile.core,
            frame: stack.frames,
        };
        let mut batch = Batch::new(&gpu);
        let method = &self.method;
        let state = stack.residency.get(&gpu, &mut batch, tile.index, |b| method.state(b, &view, stack.shape))?;
        let (result, covered) = method.result(&mut batch, state, &view, stack.shape)?;
        batch.submit();
        let mut out = gpu.download(if image { &result } else { &covered })?;
        if image {
            out.set_color_space(stack.color.clone().unwrap_or(ColorSpace::LINEAR));
        }
        Ok(out)
    }

    /// The stack of the frames added since `begin`, which ends it. `monitor` hears the progress (in tiles).
    pub fn finish(&mut self, monitor: &dyn Monitor) -> Result<GpuStacked, GpuError> {
        let mut stack = self.stack.take().ok_or(GpuError::Unsupported { what: "finishing before `begin`" })?;
        if stack.frames == 0 {
            return Err(GpuError::Unsupported { what: "a stack of no frames" });
        }
        let gpu = self.gpu.clone();
        let mut batch = Batch::new(&gpu);
        self.method.prepare(&mut batch)?;
        batch.finish()?;
        let grid = stack.grid.clone();
        let n = grid.len();
        // Count the pixels no frame covered, in the tiles' cores, which partition the canvas.
        let mut uncovered = 0;
        for (done, tile) in grid.tiles().enumerate() {
            monitor.checkpoint()?;
            let covered = self.tile_result(&mut stack, tile, false)?;
            let inner = inner(&grid, tile);
            let (x0, y0) = (tile.core.x - inner.x, tile.core.y - inner.y);
            uncovered += (y0..y0 + tile.core.height)
                .filter_map(|y| covered.row(y))
                .map(|row| row.iter().skip(x0).take(tile.core.width).filter(|&&v| v <= 0.0).count())
                .sum::<usize>();
            monitor.progress(done as u64 + 1, 3 * n as u64);
        }
        let mut done = n;
        let image = grid.stitch_with(|index| {
            monitor.checkpoint()?;
            let tile = grid.tile(index).ok_or(GpuError::Readback { what: "a tile" })?;
            let result = self.tile_result(&mut stack, tile, true)?;
            done += 1;
            monitor.progress(done as u64, 3 * n as u64);
            outer_sized(tile, inner(&grid, tile), result)
        })?;
        let alpha = if uncovered > 0 {
            Some(grid.stitch_with(|index| {
                monitor.checkpoint()?;
                let tile = grid.tile(index).ok_or(GpuError::Readback { what: "a tile" })?;
                let covered = self.tile_result(&mut stack, tile, false)?;
                done += 1;
                monitor.progress(done as u64, 3 * n as u64);
                outer_sized(tile, inner(&grid, tile), covered)
            })?)
        } else {
            None
        };
        monitor.progress(3 * n as u64, 3 * n as u64);
        Ok(GpuStacked { image, alpha, frames: stack.frames, uncovered })
    }
}
