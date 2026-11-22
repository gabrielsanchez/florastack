//! The retouch session's images kept tile by tile: the output, the result and the sources, in a tile cache with a
//! memory budget that spills the tiles least recently used to temporary files ([`florastack_core::TileCache`]), so
//! that retouching a large output holds only the tiles around what is painted or shown. Painting reads a window of
//! the images ([`Tiled::read`]), paints it (florastack-retouch paints windows to the same bits as whole images) and
//! writes back the region it painted ([`Tiled::write`]).

use std::sync::Arc;

use florastack_core::sync::Mutex;
use florastack_core::{ColorSpace, Image, Rect, TempFileStore, TileCache, TileKey};
use florastack_stack::{StackError, Tiling};

use crate::error::CommandError;

/// The tiles of a retouch session's images, shared by the images, which remove theirs when they are dropped.
pub(crate) type Shared = Arc<Mutex<Store>>;

/// A tile cache and the tiles' side.
pub(crate) struct Store {
    cache: TileCache<TempFileStore>,
    side: usize,
    next: u64,
}

/// The smallest share of the memory budget the session's tiles keep in memory.
const MIN_BUDGET: usize = 64 << 20;

impl Store {
    /// A store for a retouch session: tiles of `tiling`'s side, half its memory budget (at least 64 MiB, and the
    /// whole budget when that is less), spilling into its spill folder.
    pub(crate) fn new(tiling: &Tiling) -> Result<Shared, CommandError> {
        let store = match &tiling.spill_dir {
            Some(dir) => TempFileStore::in_dir(dir),
            None => TempFileStore::new(),
        }
        .map_err(StackError::from)?;
        let budget = (tiling.budget / 2).max(MIN_BUDGET.min(tiling.budget));
        Ok(Arc::new(Mutex::new(Store { cache: TileCache::new(store, budget), side: tiling.tile_size.max(16), next: 0 })))
    }

    /// The bytes of tiles in memory, and how many tiles were written out to make room.
    #[cfg_attr(not(test), allow(dead_code, reason = "for tests and logs"))]
    pub(crate) fn resident_bytes(&self) -> usize {
        self.cache.resident_bytes()
    }

    #[cfg_attr(not(test), allow(dead_code, reason = "for tests and logs"))]
    pub(crate) fn spills(&self) -> u64 {
        self.cache.stats().spills
    }
}

fn lock(store: &Shared) -> Result<florastack_core::sync::MutexGuard<'_, Store>, CommandError> {
    store.lock().map_err(|_| CommandError::Internal { command: "retouch".to_owned() })
}

/// An image of the output's size kept tile by tile in a [`Store`]; its tiles go when it is dropped.
pub(crate) struct Tiled {
    store: Shared,
    id: u64,
    width: usize,
    height: usize,
    channels: usize,
    color: ColorSpace,
    side: usize,
}

impl Tiled {
    /// `image`, put into `store` tile by tile.
    pub(crate) fn from_image(store: &Shared, image: &Image<f32>) -> Result<Tiled, CommandError> {
        let (id, side) = {
            let mut s = lock(store)?;
            s.next += 1;
            (s.next, s.side)
        };
        let tiled = Tiled {
            store: Arc::clone(store),
            id,
            width: image.width(),
            height: image.height(),
            channels: image.channels(),
            color: image.color_space().clone(),
            side,
        };
        for (index, rect) in tiled.tiles() {
            let tile = crop(image, rect)?;
            lock(store)?.cache.put(TileKey::new(id, index), tile).map_err(StackError::from)?;
        }
        Ok(tiled)
    }

    /// A copy of the image, tile by tile.
    pub(crate) fn copy(&self) -> Result<Tiled, CommandError> {
        let id = {
            let mut s = lock(&self.store)?;
            s.next += 1;
            s.next
        };
        let copy = Tiled { store: Arc::clone(&self.store), id, color: self.color.clone(), ..*self };
        for (index, _) in self.tiles() {
            let mut s = lock(&self.store)?;
            let tile = s.cache.get::<f32>(TileKey::new(self.id, index)).map_err(StackError::from)?.cloned().ok_or_else(|| missing(index))?;
            s.cache.put(TileKey::new(id, index), tile).map_err(StackError::from)?;
        }
        Ok(copy)
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    /// The side of large regions' pieces: two tiles (1024 pixels with the default tiles).
    pub(crate) fn piece(&self) -> usize {
        2 * self.side
    }

    /// The whole image.
    pub(crate) fn rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// The tiles: each one's index and the rectangle it covers.
    fn tiles(&self) -> impl Iterator<Item = (u64, Rect)> + '_ {
        let columns = self.width.div_ceil(self.side);
        let rows = self.height.div_ceil(self.side);
        (0..rows).flat_map(move |ty| {
            (0..columns).map(move |tx| {
                let (x, y) = (tx * self.side, ty * self.side);
                let rect = Rect::new(x, y, self.side.min(self.width - x), self.side.min(self.height - y));
                ((ty * columns + tx) as u64, rect)
            })
        })
    }

    /// The tiles that `rect` overlaps.
    fn tiles_in(&self, rect: Rect) -> impl Iterator<Item = (u64, Rect)> + '_ {
        self.tiles().filter(move |(_, t)| florastack_retouch::intersect(*t, rect).is_some())
    }

    /// `rect` of the image (within it), as an image of its own.
    pub(crate) fn read(&self, rect: Rect) -> Result<Image<f32>, CommandError> {
        let rect = florastack_retouch::intersect(rect, self.rect()).ok_or_else(|| outside(rect))?;
        let n = self.channels;
        let mut data = vec![0.0f32; rect.width * rect.height * n];
        let mut s = lock(&self.store)?;
        for (index, t) in self.tiles_in(rect) {
            let tile = s.cache.get::<f32>(TileKey::new(self.id, index)).map_err(StackError::from)?.ok_or_else(|| missing(index))?;
            let Some(part) = florastack_retouch::intersect(t, rect) else { continue };
            for y in part.y..part.y + part.height {
                let src = tile.row(y - t.y).and_then(|r| r.get((part.x - t.x) * n..(part.x - t.x + part.width) * n));
                let at = ((y - rect.y) * rect.width + (part.x - rect.x)) * n;
                if let (Some(src), Some(dst)) = (src, data.get_mut(at..at + part.width * n)) {
                    dst.copy_from_slice(src);
                }
            }
        }
        Ok(Image::from_vec(rect.width, rect.height, n, self.color.clone(), data).map_err(StackError::from)?)
    }

    /// Writes `rect` of the output from `window`, an image of the output's `at`, which holds it.
    pub(crate) fn write(&self, window: &Image<f32>, at: Rect, rect: Rect) -> Result<(), CommandError> {
        let n = self.channels;
        let Some(rect) = florastack_retouch::intersect(rect, self.rect()) else { return Ok(()) };
        let held = florastack_retouch::intersect(rect, Rect::new(at.x, at.y, window.width(), window.height()));
        if held != Some(rect) || window.channels() != n {
            return Err(outside(rect));
        }
        let mut s = lock(&self.store)?;
        for (index, t) in self.tiles_in(rect) {
            let Some(part) = florastack_retouch::intersect(t, rect) else { continue };
            let tile = s.cache.get_mut::<f32>(TileKey::new(self.id, index)).map_err(StackError::from)?.ok_or_else(|| missing(index))?;
            for y in part.y..part.y + part.height {
                let src = window.row(y - at.y).and_then(|r| r.get((part.x - at.x) * n..(part.x - at.x + part.width) * n));
                let dst = tile.row_mut(y - t.y).and_then(|r| r.get_mut((part.x - t.x) * n..(part.x - t.x + part.width) * n));
                if let (Some(src), Some(dst)) = (src, dst) {
                    dst.copy_from_slice(src);
                }
            }
        }
        Ok(())
    }

    /// The whole image, as one: for writing it to a file.
    pub(crate) fn whole(&self) -> Result<Image<f32>, CommandError> {
        self.read(self.rect())
    }
}

impl Drop for Tiled {
    fn drop(&mut self) {
        let Ok(mut s) = self.store.lock() else { return };
        let columns = self.width.div_ceil(self.side) as u64;
        let rows = self.height.div_ceil(self.side) as u64;
        for index in 0..columns * rows {
            s.cache.remove(TileKey::new(self.id, index));
        }
    }
}

/// `rect` of `image`, as an image of its own.
pub(crate) fn crop(image: &Image<f32>, rect: Rect) -> Result<Image<f32>, CommandError> {
    let n = image.channels();
    let mut data = Vec::with_capacity(rect.width * rect.height * n);
    for y in rect.y..rect.y + rect.height {
        let row = image.row(y).and_then(|r| r.get(rect.x * n..(rect.x + rect.width) * n)).ok_or_else(|| outside(rect))?;
        data.extend_from_slice(row);
    }
    Ok(Image::from_vec(rect.width, rect.height, n, image.color_space().clone(), data).map_err(StackError::from)?)
}

fn missing(index: u64) -> CommandError {
    tracing::error!("tile {index} of a retouch image is missing");
    CommandError::Internal { command: "retouch".to_owned() }
}

fn outside(rect: Rect) -> CommandError {
    tracing::error!("a retouch image was asked for {rect:?}, outside it");
    CommandError::Internal { command: "retouch".to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: usize, h: usize) -> Image<f32> {
        let data = (0..w * h * 3).map(|i| i as f32 * 0.25).collect();
        Image::from_vec(w, h, 3, ColorSpace::LINEAR, data).unwrap()
    }

    #[test]
    fn a_tiled_image_reads_and_writes_any_region_through_a_small_budget() {
        let tiling = Tiling { tile_size: 16, budget: 4096, spill_dir: None };
        let store = Store::new(&tiling).unwrap();
        let whole = image(53, 37);
        let tiled = Tiled::from_image(&store, &whole).unwrap();
        assert!(tiled.whole().unwrap() == whole, "read back whole");
        let rect = Rect::new(9, 13, 30, 20);
        assert!(tiled.read(rect).unwrap() == crop(&whole, rect).unwrap(), "a region across tiles");
        // A window written back in part: only that part changes.
        let at = Rect::new(5, 5, 40, 30);
        let read = tiled.read(at).unwrap();
        let window = Image::from_vec(read.width(), read.height(), 3, ColorSpace::LINEAR, vec![-1.0; read.as_raw().len()]).unwrap();
        let part = Rect::new(10, 12, 25, 9);
        tiled.write(&window, at, part).unwrap();
        let after = tiled.whole().unwrap();
        for y in 0..37 {
            for x in 0..53 {
                let inside = (10..35).contains(&x) && (12..21).contains(&y);
                assert_eq!(after.get(x, y, 0) == Some(-1.0), inside, "({x}, {y})");
            }
        }
        let copy = tiled.copy().unwrap();
        assert!(copy.whole().unwrap() == after, "the copy holds the same pixels");
        assert!(lock(&store).unwrap().spills() > 0, "the tiles spilled beyond the budget");
        drop((tiled, copy));
        assert_eq!(lock(&store).unwrap().resident_bytes(), 0, "dropped images take their tiles");
    }
}
