//! Spilled tiles in a browser: the browser build's [`TempFileStore`], over the origin-private file system (OPFS).
//!
//! The origin-private file system is the page's own disk space. Its synchronous file access, which a tile store
//! needs, exists only in workers, and opening a file there is asynchronous, so one worker, `florastack-storage`,
//! holds every store's file and runs an event loop that serves their reads and writes; the threads that stack
//! (workers too, which may block) send it requests and wait for the answers. Each store is one file in the
//! `florastack-tiles` folder, its tiles at offsets in it, a removed tile's space reused by the next tile that fits.
//! A file's synchronous access handle locks it against every other page of the site, so when the storage worker
//! starts it removes every file in the folder it can: those are what pages that closed or crashed left behind.
//!
//! On the page's main thread, which may not block, and where the origin-private file system isn't available (some
//! private windows), a store keeps its tiles in memory instead, and says so in the log.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemSyncAccessHandle};

use crate::store::{StoreError, TileKey, TileStore};
use crate::sync::OnceLock;

/// The folder of the origin-private file system that holds the stores' files.
const FOLDER: &str = "florastack-tiles";

type Reply<T> = mpsc::Sender<Result<T, String>>;

enum Request {
    Open {
        name: String,
        reply: Reply<u32>,
    },
    Write {
        file: u32,
        at: u64,
        bytes: Vec<u8>,
        reply: Reply<()>,
    },
    Read {
        file: u32,
        at: u64,
        len: usize,
        reply: Reply<Vec<u8>>,
    },
    /// Closes the file and removes it.
    Close {
        file: u32,
    },
}

static STORAGE: OnceLock<Option<UnboundedSender<Request>>> = OnceLock::new();

/// Starts the storage worker, if it hasn't started. The browser build's app calls it at startup on the page's main
/// thread, so that the worker is running before the first stack needs it; otherwise the first store starts it.
/// `false` when no worker can start.
pub fn start() -> bool {
    storage().is_some()
}

fn storage() -> Option<&'static UnboundedSender<Request>> {
    STORAGE
        .get_or_init(|| {
            let (sender, receiver) = unbounded();
            match crate::thread::spawn_async("florastack-storage", move || serve(receiver)) {
                Ok(()) => Some(sender),
                Err(e) => {
                    tracing::warn!("no storage worker ({e}): spilled tiles stay in memory");
                    None
                }
            }
        })
        .as_ref()
}

/// Sends a request to the storage worker and waits for its answer: on a thread that may block only.
fn call<T>(make: impl FnOnce(Reply<T>) -> Request) -> Result<T, String> {
    let sender = storage().ok_or("no storage worker")?;
    let (reply, answer) = mpsc::channel();
    sender.unbounded_send(make(reply)).map_err(|_| "the storage worker has stopped".to_owned())?;
    answer.recv().map_err(|_| "the storage worker has stopped".to_owned())?
}

fn js_error(e: &JsValue) -> String {
    if let Some(error) = e.dyn_ref::<js_sys::Error>() {
        return String::from(error.message());
    }
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

async fn folder() -> Result<FileSystemDirectoryHandle, String> {
    let scope: web_sys::WorkerGlobalScope = js_sys::global().dyn_into().map_err(|_| "not in a worker".to_owned())?;
    let root = JsFuture::from(scope.navigator().storage().get_directory()).await.map_err(|e| js_error(&e))?;
    let root: FileSystemDirectoryHandle = root.dyn_into().map_err(|e| js_error(&e))?;
    let options = web_sys::FileSystemGetDirectoryOptions::new();
    options.set_create(true);
    let dir = JsFuture::from(root.get_directory_handle_with_options(FOLDER, &options)).await.map_err(|e| js_error(&e))?;
    dir.dyn_into().map_err(|e| js_error(&e))
}

/// Removes the files in `dir` that no page holds open (see the module documentation).
async fn remove_abandoned(dir: &FileSystemDirectoryHandle) {
    let keys = dir.keys();
    let mut names = Vec::new();
    loop {
        let Ok(next) = keys.next() else { break };
        let Ok(item) = JsFuture::from(next).await else { break };
        if js_sys::Reflect::get(&item, &"done".into()).is_ok_and(|d| d.is_truthy()) {
            break;
        }
        if let Some(name) = js_sys::Reflect::get(&item, &"value".into()).ok().and_then(|v| v.as_string()) {
            names.push(name);
        }
    }
    let mut removed = 0;
    for name in names {
        // A file another page holds open can't be removed: it is still in use.
        if JsFuture::from(dir.remove_entry(&name)).await.is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!("removed {removed} tile files that closed pages left behind");
    }
}

async fn open(dir: &FileSystemDirectoryHandle, name: &str) -> Result<FileSystemSyncAccessHandle, String> {
    let options = web_sys::FileSystemGetFileOptions::new();
    options.set_create(true);
    let file = JsFuture::from(dir.get_file_handle_with_options(name, &options)).await.map_err(|e| js_error(&e))?;
    let file: FileSystemFileHandle = file.dyn_into().map_err(|e| js_error(&e))?;
    let handle = JsFuture::from(file.create_sync_access_handle()).await.map_err(|e| js_error(&e))?;
    let handle: FileSystemSyncAccessHandle = handle.dyn_into().map_err(|e| js_error(&e))?;
    handle.truncate_with_f64(0.0).map_err(|e| js_error(&e))?;
    Ok(handle)
}

fn at(offset: u64) -> web_sys::FileSystemReadWriteOptions {
    let options = web_sys::FileSystemReadWriteOptions::new();
    #[allow(clippy::cast_precision_loss)] // Offsets are far below 2^53.
    options.set_at(offset as f64);
    options
}

fn length(len: usize) -> Result<u32, String> {
    u32::try_from(len).map_err(|_| "a tile over 4 GiB".to_owned())
}

/// Writes through a buffer of the page's own: the module's memory is shared, which file access may refuse.
fn write(handle: &FileSystemSyncAccessHandle, offset: u64, bytes: &[u8]) -> Result<(), String> {
    let buffer = js_sys::Uint8Array::new_with_length(length(bytes.len())?);
    buffer.copy_from(bytes);
    let written = handle.write_with_js_u8_array_and_options(&buffer, &at(offset)).map_err(|e| js_error(&e))?;
    #[allow(clippy::cast_precision_loss)] // As above.
    if written < bytes.len() as f64 { Err(format!("{written} of {} bytes written", bytes.len())) } else { Ok(()) }
}

fn read(handle: &FileSystemSyncAccessHandle, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let buffer = js_sys::Uint8Array::new_with_length(length(len)?);
    let got = handle.read_with_js_u8_array_and_options(&buffer, &at(offset)).map_err(|e| js_error(&e))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // At most `len`, a u32.
    let got = (got.max(0.0) as u32).min(buffer.length());
    let mut out = vec![0; got as usize];
    buffer.subarray(0, got).copy_to(&mut out);
    Ok(out)
}

async fn serve(mut requests: UnboundedReceiver<Request>) {
    let dir = match folder().await {
        Ok(dir) => {
            remove_abandoned(&dir).await;
            Some(dir)
        }
        Err(e) => {
            tracing::warn!("the browser's private file system isn't available ({e}): spilled tiles stay in memory");
            None
        }
    };
    let mut files: HashMap<u32, (String, FileSystemSyncAccessHandle)> = HashMap::new();
    let mut next = 1_u32;
    while let Some(request) = std::future::poll_fn(|cx| futures_core::Stream::poll_next(Pin::new(&mut requests), cx)).await {
        match request {
            Request::Open { name, reply } => {
                let opened = match &dir {
                    Some(dir) => open(dir, &name).await,
                    None => Err("the browser's private file system isn't available".to_owned()),
                };
                let _ = reply.send(opened.map(|handle| {
                    let id = next;
                    next = next.wrapping_add(1);
                    files.insert(id, (name, handle));
                    id
                }));
            }
            Request::Write { file, at, bytes, reply } => {
                let done = files.get(&file).ok_or_else(|| "the store's file is closed".to_owned()).and_then(|(_, h)| write(h, at, &bytes));
                let _ = reply.send(done);
            }
            Request::Read { file, at, len, reply } => {
                let done = files.get(&file).ok_or_else(|| "the store's file is closed".to_owned()).and_then(|(_, h)| read(h, at, len));
                let _ = reply.send(done);
            }
            Request::Close { file } => {
                if let Some((name, handle)) = files.remove(&file) {
                    handle.close();
                    if let Some(dir) = &dir {
                        let _ = JsFuture::from(dir.remove_entry(&name)).await;
                    }
                }
            }
        }
    }
}

/// Where a tile is in a store's file: its offset, the space it may use there, and its length.
#[derive(Clone, Copy, Debug)]
struct Slot {
    at: u64,
    room: u64,
    len: u64,
}

#[derive(Debug)]
enum Backend {
    /// The storage worker's file of this number.
    File(u32),
    Memory(HashMap<TileKey, Vec<u8>>),
}

/// The browser build's tile store: tiles in the origin-private file system (see the module documentation), with the
/// native store's API.
#[derive(Debug)]
pub struct TempFileStore {
    path: PathBuf,
    backend: Backend,
    slots: HashMap<TileKey, Slot>,
    free: Vec<Slot>,
    end: u64,
    total: u64,
}

impl TempFileStore {
    /// The native store's lock file's name; a browser's files lock themselves.
    pub const LOCK_FILE: &'static str = ".lock";

    pub fn new() -> Result<TempFileStore, StoreError> {
        TempFileStore::in_dir(Path::new("/"))
    }

    /// A store in a file of its own; `_parent` is a native folder, which the browser doesn't have.
    pub fn in_dir(_parent: &Path) -> Result<TempFileStore, StoreError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let name = format!("{}-tiles-{}-{}", crate::branding::GUI_BINARY, crate::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        let path = PathBuf::from(format!("/{FOLDER}/{name}"));
        let backend = if crate::sync::can_block() {
            match call(|reply| Request::Open { name, reply }) {
                Ok(file) => Backend::File(file),
                Err(e) => {
                    tracing::warn!("spilled tiles stay in memory: {e}");
                    Backend::Memory(HashMap::new())
                }
            }
        } else {
            Backend::Memory(HashMap::new())
        };
        Ok(TempFileStore { path, backend, slots: HashMap::new(), free: Vec::new(), end: 0, total: 0 })
    }

    /// The store's file, in the origin-private file system.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        match &self.backend {
            Backend::File(_) => self.slots.len(),
            Backend::Memory(tiles) => tiles.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn close(self) -> Result<(), StoreError> {
        Ok(())
    }

    /// The storage worker removes what closed pages left behind when it starts (see the module documentation).
    pub fn remove_abandoned(_parent: &Path) -> Result<Vec<PathBuf>, StoreError> {
        Ok(Vec::new())
    }

    fn failed(&self, e: &str) -> io::Error {
        io::Error::other(format!("{}: {e}", self.path.display()))
    }

    /// Where a tile of `len` bytes goes, `key`'s place if it fits there.
    fn place(&mut self, key: TileKey, len: u64) -> Slot {
        if let Some(old) = self.slots.remove(&key) {
            if old.room >= len {
                return Slot { len, ..old };
            }
            self.free.push(old);
        }
        if let Some(i) = self.free.iter().position(|s| s.room >= len) {
            return Slot { len, ..self.free.swap_remove(i) };
        }
        let slot = Slot { at: self.end, room: len, len };
        self.end += len;
        slot
    }
}

impl TileStore for TempFileStore {
    fn write(&mut self, key: TileKey, bytes: &[u8]) -> Result<(), StoreError> {
        let len = bytes.len() as u64;
        let old = match &mut self.backend {
            Backend::Memory(tiles) => tiles.insert(key, bytes.to_vec()).map_or(0, |b| b.len() as u64),
            Backend::File(file) => {
                let file = *file;
                let old = self.slots.get(&key).map_or(0, |s| s.len);
                let slot = self.place(key, len);
                let bytes = bytes.to_vec();
                if let Err(e) = call(|reply| Request::Write { file, at: slot.at, bytes, reply }) {
                    self.free.push(slot);
                    self.total -= old;
                    return Err(StoreError::Write { key, path: self.path.clone(), source: self.failed(&e) });
                }
                self.slots.insert(key, slot);
                old
            }
        };
        self.total = self.total - old + len;
        Ok(())
    }

    fn read(&mut self, key: TileKey, buf: &mut Vec<u8>) -> Result<bool, StoreError> {
        buf.clear();
        match &self.backend {
            Backend::Memory(tiles) => match tiles.get(&key) {
                Some(bytes) => {
                    buf.extend_from_slice(bytes);
                    Ok(true)
                }
                None => Ok(false),
            },
            Backend::File(file) => {
                let file = *file;
                let Some(slot) = self.slots.get(&key).copied() else { return Ok(false) };
                let len = usize::try_from(slot.len).map_err(|_| StoreError::Read {
                    key,
                    path: self.path.clone(),
                    source: io::Error::other("too large"),
                })?;
                let bytes = call(|reply| Request::Read { file, at: slot.at, len, reply }).map_err(|e| StoreError::Read {
                    key,
                    path: self.path.clone(),
                    source: self.failed(&e),
                })?;
                *buf = bytes;
                Ok(true)
            }
        }
    }

    fn remove(&mut self, key: TileKey) -> Result<(), StoreError> {
        let len = match &mut self.backend {
            Backend::Memory(tiles) => tiles.remove(&key).map_or(0, |b| b.len() as u64),
            Backend::File(_) => match self.slots.remove(&key) {
                Some(slot) => {
                    self.free.push(slot);
                    slot.len
                }
                None => 0,
            },
        };
        self.total -= len;
        Ok(())
    }

    fn stored_bytes(&self) -> u64 {
        self.total
    }
}

impl Drop for TempFileStore {
    fn drop(&mut self) {
        if let Backend::File(file) = self.backend
            && let Some(storage) = storage()
        {
            let _ = storage.unbounded_send(Request::Close { file });
        }
    }
}
