//! The tile cache: tiles kept in memory up to a byte budget, with the least recently used ones spilled to a
//! [`TileStore`] and read back on demand, so that stacks of hundreds of 60 MP frames never need to be resident at once.
//!
//! - [`TileCache`] holds tiles of any sample type ([`AnyImage`]) by [`TileKey`]. When the tiles in memory exceed the
//!   budget, it writes the least recently used ones to its store and drops them from memory; using a tile again
//!   reads it back. A tile read back and left unchanged isn't written again when it is evicted the next time.
//! - [`TileStore`] is where the evicted tiles go, as plain bytes by key. [`TempFileStore`] keeps one file per tile in a
//!   temporary folder of its own, with ordinary file I/O, and removes the folder when it is dropped; [`MemoryStore`]
//!   keeps them in memory, for tests. The browser build can implement the trait over the origin-private file system.
//!
//! Only the samples go to the store, little-endian with packed rows; each tile's shape and color space (with its ICC
//! profile, shared) stay in the cache. Every failure is an error the caller can act on, and the cache stays
//! consistent: a tile that can't be written out stays in memory, over budget, rather than being lost.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::image::{AnyImage, ColorSpace, Image, ImageError, Sample, SampleFormat};

/// A tile's name in a cache or store: which image (a frame, an accumulator, a pyramid level: the caller's numbering)
/// and which tile of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub image: u64,
    pub tile: u64,
}

impl TileKey {
    pub const fn new(image: u64, tile: u64) -> TileKey {
        TileKey { image, tile }
    }
}

impl fmt::Display for TileKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tile {} of image {}", self.tile, self.image)
    }
}

/// Why the cache or its store failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot create a folder for tiles in {}", .path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {key} to {}", .path.display())]
    Write {
        key: TileKey,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read {key} from {}", .path.display())]
    Read {
        key: TileKey,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot remove {} from {}", .key.map_or_else(|| String::from("the tile folder"), |k| k.to_string()), .path.display())]
    Remove {
        key: Option<TileKey>,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{key} came back from the store with {got} bytes, but it has {expected}")]
    Damaged { key: TileKey, expected: usize, got: usize },
    #[error("{key} is missing from the store it was written to")]
    Lost { key: TileKey },
    #[error("{key} holds {stored:?} samples, not {requested:?}")]
    SampleFormat { key: TileKey, stored: SampleFormat, requested: SampleFormat },
    #[error(transparent)]
    Image(#[from] ImageError),
}

/// Where a [`TileCache`] keeps the tiles it evicts: whole tiles as bytes, by key.
pub trait TileStore {
    /// Stores `bytes` under `key`, replacing what was there.
    fn write(&mut self, key: TileKey, bytes: &[u8]) -> Result<(), StoreError>;

    /// Replaces the contents of `buf` with the bytes under `key`; `false` if there are none.
    fn read(&mut self, key: TileKey, buf: &mut Vec<u8>) -> Result<bool, StoreError>;

    /// Removes the bytes under `key`, if there are any.
    fn remove(&mut self, key: TileKey) -> Result<(), StoreError>;

    /// The bytes stored, in total.
    fn stored_bytes(&self) -> u64;
}

/// A [`TileStore`] in memory: for tests, and for jobs small enough not to need a disk.
#[derive(Debug, Default)]
pub struct MemoryStore {
    tiles: HashMap<TileKey, Vec<u8>>,
}

impl MemoryStore {
    pub fn new() -> MemoryStore {
        MemoryStore::default()
    }

    /// The number of tiles stored.
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
}

impl TileStore for MemoryStore {
    fn write(&mut self, key: TileKey, bytes: &[u8]) -> Result<(), StoreError> {
        self.tiles.insert(key, bytes.to_vec());
        Ok(())
    }

    fn read(&mut self, key: TileKey, buf: &mut Vec<u8>) -> Result<bool, StoreError> {
        buf.clear();
        match self.tiles.get(&key) {
            Some(bytes) => {
                buf.extend_from_slice(bytes);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn remove(&mut self, key: TileKey) -> Result<(), StoreError> {
        self.tiles.remove(&key);
        Ok(())
    }

    fn stored_bytes(&self) -> u64 {
        self.tiles.values().map(|v| v.len() as u64).sum()
    }
}

/// A [`TileStore`] of temporary files: one file per tile in a folder of its own, created for it and removed with
/// everything in it when the store is dropped (or by [`TempFileStore::close`], which reports failure). Plain file
/// I/O, nothing mapped.
#[derive(Debug)]
pub struct TempFileStore {
    dir: PathBuf,
    sizes: HashMap<TileKey, u64>,
    total: u64,
    removed: bool,
}

impl TempFileStore {
    /// A store in a new folder in the system's temporary directory.
    pub fn new() -> Result<TempFileStore, StoreError> {
        TempFileStore::in_dir(&std::env::temp_dir())
    }

    /// A store in a new folder in `parent`, such as a cache location from the preferences.
    pub fn in_dir(parent: &Path) -> Result<TempFileStore, StoreError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let mut last_error = None;
        // `create_dir` fails when the folder exists, so a name taken by another process is skipped, never shared.
        for _ in 0..64 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!("{}-tiles-{}-{nanos:08x}-{n}", crate::branding::GUI_BINARY, std::process::id());
            let dir = parent.join(name);
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(TempFileStore { dir, sizes: HashMap::new(), total: 0, removed: false }),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => last_error = Some(e),
                Err(source) => return Err(StoreError::CreateDir { path: parent.to_path_buf(), source }),
            }
        }
        let source = last_error.unwrap_or_else(|| std::io::Error::from(ErrorKind::AlreadyExists));
        Err(StoreError::CreateDir { path: parent.to_path_buf(), source })
    }

    /// The store's folder.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// The number of tiles stored.
    pub fn len(&self) -> usize {
        self.sizes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty()
    }

    /// Removes the folder and its files now, reporting a failure that dropping the store would only log.
    pub fn close(mut self) -> Result<(), StoreError> {
        self.removed = true;
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Remove { key: None, path: self.dir.clone(), source }),
        }
    }

    fn file(&self, key: TileKey) -> PathBuf {
        self.dir.join(format!("{:016x}-{:016x}.tile", key.image, key.tile))
    }
}

impl TileStore for TempFileStore {
    fn write(&mut self, key: TileKey, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.file(key);
        std::fs::write(&path, bytes).map_err(|source| StoreError::Write { key, path, source })?;
        let size = bytes.len() as u64;
        let old = self.sizes.insert(key, size).unwrap_or(0);
        self.total = self.total - old + size;
        Ok(())
    }

    fn read(&mut self, key: TileKey, buf: &mut Vec<u8>) -> Result<bool, StoreError> {
        buf.clear();
        let Some(&size) = self.sizes.get(&key) else {
            return Ok(false);
        };
        let path = self.file(key);
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(StoreError::Read { key, path, source }),
        };
        // At most one byte more than was written: a file that grew since is reported as damaged, not read whole.
        file.take(size.saturating_add(1)).read_to_end(buf).map_err(|source| StoreError::Read { key, path, source })?;
        Ok(true)
    }

    fn remove(&mut self, key: TileKey) -> Result<(), StoreError> {
        let path = self.file(key);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(source) => return Err(StoreError::Remove { key: Some(key), path, source }),
        }
        self.total -= self.sizes.remove(&key).unwrap_or(0);
        Ok(())
    }

    fn stored_bytes(&self) -> u64 {
        self.total
    }
}

impl Drop for TempFileStore {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("cannot remove the tile folder {}: {e}", self.dir.display()),
        }
    }
}

/// What a [`TileCache`] has done, for tests, logs and tuning the budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Accesses to tiles that were in memory.
    pub hits: u64,
    /// Accesses that had to read the tile back from the store.
    pub loads: u64,
    /// Tiles written to the store on eviction.
    pub spills: u64,
    /// Evictions of tiles the store already held unchanged, which needed no write.
    pub clean_evictions: u64,
}

/// The shape and color of a tile, kept while its samples are in the store.
#[derive(Clone, Debug)]
struct Meta {
    format: SampleFormat,
    width: usize,
    height: usize,
    channels: usize,
    color: ColorSpace,
}

impl Meta {
    fn of(image: &AnyImage) -> Meta {
        Meta { format: image.format(), width: image.width(), height: image.height(), channels: image.channels(), color: image.color_space().clone() }
    }

    /// The bytes of the tile's samples with packed rows.
    fn packed_bytes(&self) -> usize {
        self.width * self.height * self.channels * self.format.bytes()
    }
}

/// Whether the store holds a copy of a tile, and whether that copy is current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stored {
    No,
    Current,
    Stale,
}

#[derive(Debug)]
struct Entry {
    meta: Meta,
    /// The tile, when it is in memory.
    image: Option<AnyImage>,
    /// When it was last used, its key in the recency index while it is in memory.
    tick: u64,
    stored: Stored,
}

/// Tiles in memory up to a budget, the rest in a [`TileStore`]. See the module documentation.
#[derive(Debug)]
pub struct TileCache<S: TileStore> {
    store: S,
    budget: usize,
    resident: usize,
    clock: u64,
    entries: HashMap<TileKey, Entry>,
    /// The tiles in memory, least recently used first.
    recency: BTreeMap<u64, TileKey>,
    scratch: Vec<u8>,
    stats: CacheStats,
}

impl TileCache<TempFileStore> {
    /// A cache that spills to temporary files in the system's temporary directory.
    pub fn with_temp_files(budget_bytes: usize) -> Result<TileCache<TempFileStore>, StoreError> {
        Ok(TileCache::new(TempFileStore::new()?, budget_bytes))
    }
}

impl<S: TileStore> TileCache<S> {
    /// A cache that keeps up to `budget_bytes` of tiles in memory and spills the rest to `store`.
    pub fn new(store: S, budget_bytes: usize) -> TileCache<S> {
        TileCache {
            store,
            budget: budget_bytes,
            resident: 0,
            clock: 0,
            entries: HashMap::new(),
            recency: BTreeMap::new(),
            scratch: Vec::new(),
            stats: CacheStats::default(),
        }
    }

    /// The memory budget for tiles, in bytes. The tile in use stays in memory even when it alone exceeds it.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Changes the budget, evicting tiles to meet it.
    pub fn set_budget(&mut self, budget_bytes: usize) -> Result<(), StoreError> {
        self.budget = budget_bytes;
        self.evict(None)
    }

    /// The bytes of the tiles in memory.
    pub fn resident_bytes(&self) -> usize {
        self.resident
    }

    /// The number of tiles, in memory or in the store.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, key: TileKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// Whether the tile is in memory now.
    pub fn is_resident(&self, key: TileKey) -> bool {
        self.entries.get(&key).is_some_and(|e| e.image.is_some())
    }

    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// Stores `image` under `key`, replacing any tile there, and evicts others to meet the budget. On an error the
    /// tile is in the cache anyway; only the eviction failed.
    pub fn put<T: Sample>(&mut self, key: TileKey, image: Image<T>) -> Result<(), StoreError> {
        let image = AnyImage::from(image);
        let bytes = image.buffer_bytes();
        let meta = Meta::of(&image);
        let tick = self.tick();
        let stored = match self.entries.remove(&key) {
            Some(old) => {
                self.forget_resident(&old);
                if old.stored == Stored::No { Stored::No } else { Stored::Stale }
            }
            None => Stored::No,
        };
        self.entries.insert(key, Entry { meta, image: Some(image), tick, stored });
        self.recency.insert(tick, key);
        self.resident += bytes;
        self.evict(Some(key))
    }

    /// The tile under `key`, read back from the store if it was evicted, or `None` if there is no such tile. Using it
    /// makes it the most recently used and may evict others.
    pub fn get<T: Sample>(&mut self, key: TileKey) -> Result<Option<&Image<T>>, StoreError> {
        if !self.load::<T>(key)? {
            return Ok(None);
        }
        let image = self.entries.get(&key).and_then(|e| e.image.as_ref()).and_then(AnyImage::as_image::<T>);
        Ok(image)
    }

    /// The tile under `key`, mutably: like [`TileCache::get`], and the change will be written out when it is evicted.
    pub fn get_mut<T: Sample>(&mut self, key: TileKey) -> Result<Option<&mut Image<T>>, StoreError> {
        if !self.load::<T>(key)? {
            return Ok(None);
        }
        let Some(entry) = self.entries.get_mut(&key) else {
            return Ok(None);
        };
        if entry.stored == Stored::Current {
            entry.stored = Stored::Stale;
        }
        Ok(entry.image.as_mut().and_then(AnyImage::as_image_mut::<T>))
    }

    /// Removes the tile under `key` from the cache and its store and returns it.
    pub fn take<T: Sample>(&mut self, key: TileKey) -> Result<Option<Image<T>>, StoreError> {
        let Some(entry) = self.entries.get(&key) else {
            return Ok(None);
        };
        check_format::<T>(key, &entry.meta)?;
        let image = match &entry.image {
            Some(_) => None,
            None => Some(self.read_back::<T>(key)?),
        };
        let Some(entry) = self.entries.remove(&key) else {
            return Ok(None);
        };
        self.forget_resident(&entry);
        self.drop_stored(key, entry.stored);
        Ok(image.or_else(|| entry.image.and_then(|i| i.into_image::<T>().ok())))
    }

    /// Removes the tile under `key`; `false` if there was none.
    pub fn remove(&mut self, key: TileKey) -> bool {
        let Some(entry) = self.entries.remove(&key) else {
            return false;
        };
        self.forget_resident(&entry);
        self.drop_stored(key, entry.stored);
        true
    }

    /// Removes every tile.
    pub fn clear(&mut self) {
        let keys: Vec<TileKey> = self.entries.keys().copied().collect();
        for key in keys {
            self.remove(key);
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Takes a tile's memory and recency out of the books, before its entry is dropped or replaced.
    fn forget_resident(&mut self, entry: &Entry) {
        if let Some(image) = &entry.image {
            self.resident -= image.buffer_bytes();
            self.recency.remove(&entry.tick);
        }
    }

    /// Deletes a removed tile's copy in the store. A copy that can't be deleted is only orphaned: the store's folder
    /// goes with the store, so this logs instead of failing the removal.
    fn drop_stored(&mut self, key: TileKey, stored: Stored) {
        if stored != Stored::No
            && let Err(e) = self.store.remove(key)
        {
            tracing::warn!("{e}");
        }
    }

    /// Makes the tile under `key` resident and most recently used, then evicts others; `false` if there is none.
    fn load<T: Sample>(&mut self, key: TileKey) -> Result<bool, StoreError> {
        let Some(entry) = self.entries.get(&key) else {
            return Ok(false);
        };
        check_format::<T>(key, &entry.meta)?;
        if entry.image.is_some() {
            self.stats.hits += 1;
        } else {
            let image = AnyImage::from(self.read_back::<T>(key)?);
            self.resident += image.buffer_bytes();
            self.stats.loads += 1;
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.image = Some(image);
            }
        }
        let tick = self.tick();
        if let Some(entry) = self.entries.get_mut(&key) {
            self.recency.remove(&entry.tick);
            entry.tick = tick;
            self.recency.insert(tick, key);
        }
        self.evict(Some(key))?;
        Ok(true)
    }

    /// The tile under `key` read from the store (which must hold it).
    fn read_back<T: Sample>(&mut self, key: TileKey) -> Result<Image<T>, StoreError> {
        let Some(meta) = self.entries.get(&key).map(|e| e.meta.clone()) else {
            return Err(StoreError::Lost { key });
        };
        if !self.store.read(key, &mut self.scratch)? {
            return Err(StoreError::Lost { key });
        }
        let expected = meta.packed_bytes();
        if self.scratch.len() != expected {
            return Err(StoreError::Damaged { key, expected, got: self.scratch.len() });
        }
        let mut data = Vec::new();
        data.try_reserve_exact(meta.width * meta.height * meta.channels).map_err(|source| ImageError::Alloc {
            width: meta.width,
            height: meta.height,
            channels: meta.channels,
            source,
        })?;
        T::read_le(&self.scratch, &mut data);
        Ok(Image::from_vec(meta.width, meta.height, meta.channels, meta.color, data)?)
    }

    /// Evicts the least recently used tiles other than `keep` until the budget is met, writing out those the store
    /// doesn't hold unchanged. A tile that fails to be written stays in memory and the error is returned.
    fn evict(&mut self, keep: Option<TileKey>) -> Result<(), StoreError> {
        while self.resident > self.budget {
            let Some(victim) = self.recency.values().copied().find(|k| Some(*k) != keep) else {
                return Ok(());
            };
            let Some(entry) = self.entries.get_mut(&victim) else {
                return Ok(());
            };
            let Some(image) = entry.image.take() else {
                // Not in memory after all: only the index was out of date.
                self.recency.remove(&entry.tick);
                continue;
            };
            if entry.stored == Stored::Current {
                self.stats.clean_evictions += 1;
            } else {
                pack(&image, &mut self.scratch);
                if let Err(e) = self.store.write(victim, &self.scratch) {
                    entry.image = Some(image);
                    return Err(e);
                }
                entry.stored = Stored::Current;
                self.stats.spills += 1;
            }
            self.resident -= image.buffer_bytes();
            self.recency.remove(&entry.tick);
        }
        Ok(())
    }
}

fn check_format<T: Sample>(key: TileKey, meta: &Meta) -> Result<(), StoreError> {
    if meta.format == T::FORMAT { Ok(()) } else { Err(StoreError::SampleFormat { key, stored: meta.format, requested: T::FORMAT }) }
}

/// The samples of `image`, little-endian with packed rows, in `out`.
fn pack(image: &AnyImage, out: &mut Vec<u8>) {
    out.clear();
    match image {
        AnyImage::U8(i) => pack_rows(i, out),
        AnyImage::U16(i) => pack_rows(i, out),
        AnyImage::F32(i) => pack_rows(i, out),
    }
}

fn pack_rows<T: Sample>(image: &Image<T>, out: &mut Vec<u8>) {
    for row in image.rows() {
        T::write_le(row, out);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::image::Encoding;

    /// A 12x10 RGB tile, 1440 bytes, whose samples depend on `seed`.
    fn tile(seed: u16) -> Image<f32> {
        Image::from_fn(12, 10, 3, ColorSpace::LINEAR, |x, y, c| f32::from(seed) * 1000.0 + (x + 7 * y + 3 * c) as f32).unwrap()
    }

    const TILE_BYTES: usize = 12 * 10 * 3 * 4;

    fn key(n: u64) -> TileKey {
        TileKey::new(7, n)
    }

    /// A store in memory that counts writes and can be made to fail them.
    #[derive(Default)]
    struct Recording {
        inner: MemoryStore,
        writes: usize,
        fail_writes: bool,
    }

    impl TileStore for Recording {
        fn write(&mut self, key: TileKey, bytes: &[u8]) -> Result<(), StoreError> {
            if self.fail_writes {
                let source = std::io::Error::new(ErrorKind::StorageFull, "disk full");
                return Err(StoreError::Write { key, path: PathBuf::from("test"), source });
            }
            self.writes += 1;
            self.inner.write(key, bytes)
        }

        fn read(&mut self, key: TileKey, buf: &mut Vec<u8>) -> Result<bool, StoreError> {
            self.inner.read(key, buf)
        }

        fn remove(&mut self, key: TileKey) -> Result<(), StoreError> {
            self.inner.remove(key)
        }

        fn stored_bytes(&self) -> u64 {
            self.inner.stored_bytes()
        }
    }

    #[test]
    fn tiles_of_every_sample_type_come_back_with_their_color_space() {
        let mut cache = TileCache::new(MemoryStore::new(), 0);
        let icc: Arc<[u8]> = Arc::from(&b"icc profile"[..]);
        let color = ColorSpace { encoding: Encoding::Srgb, icc: Some(icc) };
        // Strided rows come back packed: the padding isn't part of the image.
        let bytes = Image::from_vec_with_stride(3, 2, 1, 5, color.clone(), vec![1u8, 2, 3, 0, 0, 4, 5, 6]).unwrap();
        let words = Image::from_fn(5, 4, 2, color, |x, y, c| u16::try_from(x * 1000 + y * 10 + c).unwrap()).unwrap();
        let floats = tile(3);
        cache.put(key(1), bytes.clone()).unwrap();
        cache.put(key(2), words.clone()).unwrap();
        cache.put(key(3), floats.clone()).unwrap();
        assert!(!cache.is_resident(key(1)) && !cache.is_resident(key(2)) && cache.is_resident(key(3)), "a budget of 0 keeps only the tile in use");
        assert_eq!(cache.get::<u8>(key(1)).unwrap(), Some(&bytes));
        assert_eq!(cache.get::<u16>(key(2)).unwrap(), Some(&words));
        assert_eq!(cache.get::<f32>(key(3)).unwrap(), Some(&floats));
        assert_eq!(cache.get::<f32>(key(4)).unwrap(), None);
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.store().len(), 3);
    }

    #[test]
    fn the_least_recently_used_tile_is_evicted_first() {
        let mut cache = TileCache::new(MemoryStore::new(), 2 * TILE_BYTES);
        cache.put(key(0), tile(0)).unwrap();
        cache.put(key(1), tile(1)).unwrap();
        assert_eq!(cache.resident_bytes(), 2 * TILE_BYTES);
        cache.put(key(2), tile(2)).unwrap();
        let resident = |cache: &TileCache<MemoryStore>| (0..4).filter(|&n| cache.is_resident(key(n))).collect::<Vec<_>>();
        assert_eq!(resident(&cache), [1, 2]);
        cache.get::<f32>(key(1)).unwrap();
        cache.put(key(3), tile(3)).unwrap();
        assert_eq!(resident(&cache), [1, 3], "tile 2 was used longest ago");
        assert_eq!(cache.get::<f32>(key(0)).unwrap(), Some(&tile(0)));
        assert_eq!(resident(&cache), [0, 3]);
        assert_eq!(cache.stats(), CacheStats { hits: 1, loads: 1, spills: 3, clean_evictions: 0 });
        assert!(cache.resident_bytes() <= cache.budget());
        cache.set_budget(0).unwrap();
        assert_eq!(resident(&cache), Vec::<u64>::new());
    }

    #[test]
    fn unchanged_tiles_are_not_written_twice_and_changed_ones_are() {
        let mut cache = TileCache::new(Recording::default(), TILE_BYTES);
        cache.put(key(0), tile(0)).unwrap();
        cache.put(key(1), tile(1)).unwrap();
        assert_eq!(cache.store().writes, 1);
        cache.get::<f32>(key(0)).unwrap();
        assert_eq!(cache.store().writes, 2);
        cache.get::<f32>(key(1)).unwrap();
        assert_eq!((cache.store().writes, cache.stats().clean_evictions), (2, 1), "tile 0 was stored unchanged");
        cache.get_mut::<f32>(key(0)).unwrap().unwrap().pixel_mut(0, 0).unwrap()[0] = -5.0;
        assert_eq!((cache.store().writes, cache.stats().clean_evictions), (2, 2));
        cache.get::<f32>(key(1)).unwrap();
        assert_eq!(cache.store().writes, 3, "the changed tile 0 is written out");
        assert_eq!(cache.get::<f32>(key(0)).unwrap().unwrap().get(0, 0, 0), Some(-5.0));
    }

    #[test]
    fn a_replaced_tile_is_the_one_read_back() {
        let mut cache = TileCache::new(MemoryStore::new(), 0);
        cache.put(key(0), tile(0)).unwrap();
        cache.put(key(1), tile(1)).unwrap();
        cache.put(key(0), tile(5)).unwrap();
        cache.put(key(1), tile(6)).unwrap();
        assert_eq!(cache.get::<f32>(key(0)).unwrap(), Some(&tile(5)));
        assert_eq!(cache.get::<f32>(key(1)).unwrap(), Some(&tile(6)));
        // A tile may come back with another sample type.
        cache.put(key(0), Image::<u8>::new(2, 2, 1, ColorSpace::LINEAR).unwrap()).unwrap();
        assert!(cache.get::<u8>(key(0)).unwrap().is_some());
    }

    #[test]
    fn take_and_remove_drop_the_stored_copies() {
        let mut cache = TileCache::new(MemoryStore::new(), 0);
        for n in 0..3 {
            cache.put(key(n), tile(u16::try_from(n).unwrap())).unwrap();
        }
        assert_eq!(cache.store().len(), 2);
        assert_eq!(cache.take::<f32>(key(0)).unwrap(), Some(tile(0)), "taken from the store");
        assert_eq!(cache.take::<f32>(key(2)).unwrap(), Some(tile(2)), "taken from memory");
        assert_eq!(cache.take::<f32>(key(2)).unwrap(), None);
        assert!(cache.remove(key(1)));
        assert!(!cache.remove(key(1)));
        assert!(cache.is_empty() && cache.store().is_empty());
        assert_eq!(cache.resident_bytes(), 0);
        cache.put(key(9), tile(9)).unwrap();
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn the_wrong_sample_type_is_an_error_and_changes_nothing() {
        let mut cache = TileCache::new(MemoryStore::new(), 0);
        cache.put(key(0), tile(0)).unwrap();
        cache.put(key(1), tile(1)).unwrap();
        let e = cache.get::<u16>(key(0)).unwrap_err();
        assert!(matches!(e, StoreError::SampleFormat { stored: SampleFormat::F32, requested: SampleFormat::U16, .. }), "{e}");
        assert!(cache.take::<u8>(key(1)).is_err());
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.take::<f32>(key(1)).unwrap(), Some(tile(1)));
        assert_eq!(e.to_string(), "tile 0 of image 7 holds F32 samples, not U16");
    }

    #[test]
    fn a_tile_that_cannot_be_written_out_stays_in_memory() {
        let mut cache = TileCache::new(Recording::default(), 0);
        cache.put(key(0), tile(0)).unwrap();
        cache.store.fail_writes = true;
        let e = cache.put(key(1), tile(1)).unwrap_err();
        assert!(matches!(e, StoreError::Write { .. }), "{e}");
        assert!(cache.is_resident(key(0)) && cache.is_resident(key(1)), "nothing lost, over budget");
        cache.store.fail_writes = false;
        assert_eq!(cache.get::<f32>(key(0)).unwrap(), Some(&tile(0)));
        assert_eq!(cache.get::<f32>(key(1)).unwrap(), Some(&tile(1)));
    }

    /// A folder of its own in the system's temporary directory for one test, removed by the test.
    fn test_parent(name: &str) -> PathBuf {
        let parent = std::env::temp_dir().join(format!("florastack-core-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&parent).unwrap();
        parent
    }

    #[test]
    fn temporary_files_hold_the_tiles_and_go_away_with_the_store() {
        let parent = test_parent("store");
        let mut cache = TileCache::new(TempFileStore::in_dir(&parent).unwrap(), TILE_BYTES);
        let dir = cache.store().path().to_path_buf();
        assert!(dir.starts_with(&parent) && dir.is_dir());
        for n in 0..4 {
            cache.put(key(n), tile(u16::try_from(n).unwrap())).unwrap();
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
        assert_eq!(cache.store().stored_bytes(), 3 * TILE_BYTES as u64);
        // Reading tile 1 back evicts tile 3 to a file of its own; taking tile 2 deletes its file.
        assert_eq!(cache.get::<f32>(key(1)).unwrap(), Some(&tile(1)));
        assert_eq!(cache.take::<f32>(key(2)).unwrap(), Some(tile(2)));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
        drop(cache);
        assert!(!dir.exists(), "dropping the store removes its folder");
        let store = TempFileStore::in_dir(&parent).unwrap();
        let other = TempFileStore::in_dir(&parent).unwrap();
        assert_ne!(store.path(), other.path());
        let dir = store.path().to_path_buf();
        store.close().unwrap();
        assert!(!dir.exists());
        drop(other);
        std::fs::remove_dir(&parent).unwrap();
    }

    #[test]
    fn a_damaged_or_missing_file_is_an_error() {
        let parent = test_parent("damaged");
        let mut cache = TileCache::new(TempFileStore::in_dir(&parent).unwrap(), 0);
        cache.put(key(0), tile(0)).unwrap();
        cache.put(key(1), tile(1)).unwrap();
        let file = std::fs::read_dir(cache.store().path()).unwrap().next().unwrap().unwrap().path();
        std::fs::write(&file, [0u8; 3]).unwrap();
        let e = cache.get::<f32>(key(0)).unwrap_err();
        assert!(matches!(e, StoreError::Damaged { expected: TILE_BYTES, got: 3, .. }), "{e}");
        // A file that grew is read no further than one byte past what was written.
        std::fs::write(&file, vec![0u8; 10 * TILE_BYTES]).unwrap();
        let e = cache.get::<f32>(key(0)).unwrap_err();
        assert!(matches!(e, StoreError::Damaged { got, .. } if got == TILE_BYTES + 1), "{e}");
        std::fs::remove_file(&file).unwrap();
        assert!(matches!(cache.get::<f32>(key(0)), Err(StoreError::Lost { .. })));
        assert_eq!(cache.get::<f32>(key(1)).unwrap(), Some(&tile(1)), "the other tiles are fine");
        assert!(matches!(TempFileStore::in_dir(&parent.join("missing").join("deeper")), Err(StoreError::CreateDir { .. })));
        drop(cache);
        std::fs::remove_dir(&parent).unwrap();
    }
}
