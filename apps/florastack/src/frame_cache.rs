//! The display images of the frames around the selection ([`FrameCache`]), so that moving back and forth through
//! the frames shows them at once instead of decoding each file again (a RAW frame takes about a second). The app
//! reads the selected frame's neighbors ahead ([`FrameCache::ahead`]), one at a time, after the selected one; the
//! cache keeps what fits in its budget and lets the least recently used go first.
//!
//! An image is kept with the file's size and modification time, and a file that changed since is read again.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use florastack_engine::view::DisplayImage;

/// How much the cache keeps, in bytes of pixels: five 24-megapixel frames' display images.
pub const BUDGET: usize = 1 << 30;
/// How many frames on each side of the selected one are read ahead.
pub const AHEAD: usize = 2;

/// What a file was when it was read: its size and modification time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl Stamp {
    /// The file's stamp now; none when it can't be read.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn of(path: &Path) -> Option<Stamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Stamp { len: meta.len(), modified: meta.modified().ok() })
    }

    /// In a browser the frames are the page's own files, which don't change.
    #[cfg(target_arch = "wasm32")]
    pub fn of(_path: &Path) -> Option<Stamp> {
        Some(Stamp { len: 0, modified: None })
    }
}

struct Entry {
    path: PathBuf,
    max_side: usize,
    stamp: Stamp,
    image: Arc<DisplayImage>,
    used: u64,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.image.rgba.len().saturating_mul(2)
    }
}

/// See the module documentation.
pub struct FrameCache {
    entries: Vec<Entry>,
    budget: usize,
    /// Counts the uses, for the least recently used.
    clock: u64,
}

impl Default for FrameCache {
    fn default() -> FrameCache {
        FrameCache::new(BUDGET)
    }
}

impl FrameCache {
    pub fn new(budget: usize) -> FrameCache {
        FrameCache { entries: Vec::new(), budget, clock: 0 }
    }

    /// The image of `path` read at `max_side`, when it is kept and the file is as it was (`stamp`).
    pub fn get(&mut self, path: &Path, max_side: usize, stamp: Option<Stamp>) -> Option<Arc<DisplayImage>> {
        let i = self.entries.iter().position(|e| e.path == path && e.max_side == max_side)?;
        if stamp != Some(self.entries[i].stamp) {
            self.entries.swap_remove(i);
            return None;
        }
        self.clock += 1;
        let entry = self.entries.get_mut(i)?;
        entry.used = self.clock;
        Some(Arc::clone(&entry.image))
    }

    /// Whether the image of `path` at `max_side` is kept, whatever the file is now.
    pub fn has(&self, path: &Path, max_side: usize) -> bool {
        self.entries.iter().any(|e| e.path == path && e.max_side == max_side)
    }

    /// Keeps `image`, read from `path` (as `stamp` says it was) at `max_side`, letting the least recently used go
    /// until the rest fits the budget. An image larger than the whole budget isn't kept.
    pub fn insert(&mut self, path: &Path, max_side: usize, stamp: Stamp, image: Arc<DisplayImage>) {
        self.entries.retain(|e| !(e.path == path && e.max_side == max_side));
        self.clock += 1;
        let entry = Entry { path: path.to_path_buf(), max_side, stamp, image, used: self.clock };
        if entry.bytes() > self.budget {
            return;
        }
        let mut total = entry.bytes() + self.bytes();
        while total > self.budget {
            let Some((i, _)) = self.entries.iter().enumerate().min_by_key(|(_, e)| e.used) else { break };
            total = total.saturating_sub(self.entries.swap_remove(i).bytes());
        }
        self.entries.push(entry);
    }

    /// The bytes of pixels kept.
    pub fn bytes(&self) -> usize {
        self.entries.iter().map(Entry::bytes).sum()
    }

    /// Lets every image go.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The frames to read ahead around the one at `index` of `frames`, nearest first and the next before the
    /// previous at the same distance, leaving out the ones kept.
    pub fn ahead(&self, frames: &[PathBuf], index: usize, max_side: usize) -> Vec<PathBuf> {
        let mut wanted = Vec::new();
        for distance in 1..=AHEAD {
            let next = index.checked_add(distance).and_then(|i| frames.get(i));
            let previous = index.checked_sub(distance).and_then(|i| frames.get(i));
            for path in [next, previous].into_iter().flatten() {
                if !self.has(path, max_side) && !wanted.contains(path) {
                    wanted.push(path.clone());
                }
            }
        }
        wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(pixels: usize) -> Arc<DisplayImage> {
        Arc::new(DisplayImage { width: pixels, height: 1, rgba: vec![0; pixels * 4], alpha: false })
    }

    const STAMP: Stamp = Stamp { len: 1, modified: None };

    #[test]
    fn the_least_recently_used_go_first_and_the_budget_holds() {
        // 8 bytes a pixel: room for three images of 100 pixels.
        let mut cache = FrameCache::new(2400);
        for name in ["a", "b", "c"] {
            cache.insert(Path::new(name), 64, STAMP, image(100));
        }
        assert!(cache.get(Path::new("a"), 64, Some(STAMP)).is_some());
        cache.insert(Path::new("d"), 64, STAMP, image(100));
        assert!(cache.has(Path::new("a"), 64) && !cache.has(Path::new("b"), 64), "b was the least recently used");
        assert_eq!(cache.bytes(), 2400);
        // The two least recently used (c, then a) make way for a larger one; one larger than the budget isn't kept.
        cache.insert(Path::new("e"), 64, STAMP, image(200));
        assert_eq!(cache.bytes(), 2400);
        assert!(cache.has(Path::new("e"), 64) && cache.has(Path::new("d"), 64) && !cache.has(Path::new("a"), 64));
        cache.insert(Path::new("f"), 64, STAMP, image(400));
        assert!(!cache.has(Path::new("f"), 64));
        // Another size is another image.
        assert!(cache.get(Path::new("d"), 128, Some(STAMP)).is_none());
    }

    #[test]
    fn a_changed_file_is_read_again() {
        let mut cache = FrameCache::new(BUDGET);
        cache.insert(Path::new("a"), 64, STAMP, image(10));
        assert!(cache.get(Path::new("a"), 64, Some(Stamp { len: 2, modified: None })).is_none());
        assert!(!cache.has(Path::new("a"), 64), "dropped");
        cache.insert(Path::new("a"), 64, STAMP, image(10));
        assert!(cache.get(Path::new("a"), 64, None).is_none(), "a file that can't be read now");
    }

    #[test]
    fn reading_ahead_takes_the_nearest_frames_not_kept() {
        let frames: Vec<PathBuf> = (0..6).map(|i| PathBuf::from(format!("{i}"))).collect();
        let mut cache = FrameCache::new(BUDGET);
        let names = |paths: Vec<PathBuf>| paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
        assert_eq!(names(cache.ahead(&frames, 2, 64)), ["3", "1", "4", "0"]);
        assert_eq!(names(cache.ahead(&frames, 0, 64)), ["1", "2"]);
        assert_eq!(names(cache.ahead(&frames, 5, 64)), ["4", "3"]);
        cache.insert(Path::new("3"), 64, STAMP, image(1));
        assert_eq!(names(cache.ahead(&frames, 2, 64)), ["1", "4", "0"]);
        assert_eq!(names(cache.ahead(&frames, 2, 32)), ["3", "1", "4", "0"]);
    }
}
