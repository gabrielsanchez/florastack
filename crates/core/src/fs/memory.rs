//! An in-memory file system with `std::fs`'s functions and types: the browser build's file system (see the module
//! documentation of [`crate::fs`]). Compiled on native targets for its tests.
//!
//! Paths are absolute from `/` (a relative path is taken from `/`, the only working folder), `.` and `..` resolved,
//! and every folder up to a file must exist, as on a disk. A file's contents are shared, not copied: a file opened for
//! reading reads the contents it was opened with, and a file opened for writing writes its own copy, which becomes
//! the file's at `flush`, `sync_all`, `sync_data` and when it is dropped. FloraStack writes a file whole before others
//! read it (to a temporary name, then renamed), so they never see the difference. Locks are advisory and exclusive,
//! like the standard library's, and released when the file that holds one is dropped.
//!
//! Every lock it takes is held for a map operation or a pointer swap, never a copy, and spins on a browser's main
//! thread rather than waiting ([`crate::sync`]).
#![allow(
    clippy::trivially_copy_pass_by_ref,
    clippy::unused_self,
    clippy::unnecessary_wraps,
    reason = "std::fs's signatures, which the code that uses either file system is written against"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sync::{Mutex, MutexGuard};
use crate::time::SystemTime;

/// A file's contents, shared by the handles open on it (and by hard links).
#[derive(Debug)]
struct Data {
    contents: Mutex<Arc<Vec<u8>>>,
    modified: Mutex<SystemTime>,
    locked: AtomicBool,
}

impl Data {
    fn new() -> Arc<Data> {
        Arc::new(Data { contents: Mutex::new(Arc::new(Vec::new())), modified: Mutex::new(SystemTime::now()), locked: AtomicBool::new(false) })
    }

    fn contents(&self) -> Arc<Vec<u8>> {
        Arc::clone(&guard(&self.contents))
    }

    fn modified(&self) -> SystemTime {
        *guard(&self.modified)
    }
}

#[derive(Clone, Debug)]
enum Node {
    Dir { modified: SystemTime },
    File(Arc<Data>),
}

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn error(kind: io::ErrorKind, what: &str, path: &Path) -> io::Error {
    io::Error::new(kind, format!("{what}: {}", path.display()))
}

fn not_found(path: &Path) -> io::Error {
    error(io::ErrorKind::NotFound, "no such file or folder", path)
}

/// `path` from `/`, with `.` and `..` resolved.
pub(super) fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for c in path.components() {
        match c {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
        }
    }
    out
}

fn is_root(path: &Path) -> bool {
    path.parent().is_none()
}

/// An in-memory file system. See the module documentation.
#[derive(Debug, Default)]
pub struct MemoryFs {
    /// Every file and folder but the root, which always exists, by normalized path.
    nodes: Mutex<BTreeMap<PathBuf, Node>>,
}

impl MemoryFs {
    pub const fn new() -> MemoryFs {
        MemoryFs { nodes: Mutex::new(BTreeMap::new()) }
    }

    fn nodes(&self) -> MutexGuard<'_, BTreeMap<PathBuf, Node>> {
        guard(&self.nodes)
    }

    fn node(nodes: &BTreeMap<PathBuf, Node>, path: &Path) -> Option<Node> {
        if is_root(path) { Some(Node::Dir { modified: SystemTime::UNIX_EPOCH }) } else { nodes.get(path).cloned() }
    }

    /// Checks that `path`'s parent is a folder.
    fn parent_is_dir(nodes: &BTreeMap<PathBuf, Node>, path: &Path) -> io::Result<()> {
        let Some(parent) = path.parent() else { return Err(error(io::ErrorKind::AlreadyExists, "the root folder", path)) };
        match MemoryFs::node(nodes, parent) {
            Some(Node::Dir { .. }) => Ok(()),
            Some(Node::File(_)) => Err(error(io::ErrorKind::NotADirectory, "not a folder", parent)),
            None => Err(not_found(parent)),
        }
    }

    /// The paths at and under `path`, which come together in the map's order.
    fn subtree(nodes: &BTreeMap<PathBuf, Node>, path: &Path) -> Vec<PathBuf> {
        nodes.range(path.to_path_buf()..).map(|(p, _)| p).take_while(|p| p.starts_with(path)).cloned().collect()
    }

    pub fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let path = normal(path);
        match MemoryFs::node(&self.nodes(), &path) {
            Some(Node::File(data)) => Ok(data.contents().as_ref().clone()),
            Some(Node::Dir { .. }) => Err(error(io::ErrorKind::IsADirectory, "a folder", &path)),
            None => Err(not_found(&path)),
        }
    }

    /// The contents of the file at `path`, shared rather than copied.
    pub fn read_shared(&self, path: &Path) -> io::Result<Arc<Vec<u8>>> {
        let path = normal(path);
        match MemoryFs::node(&self.nodes(), &path) {
            Some(Node::File(data)) => Ok(data.contents()),
            Some(Node::Dir { .. }) => Err(error(io::ErrorKind::IsADirectory, "a folder", &path)),
            None => Err(not_found(&path)),
        }
    }

    pub fn read_to_string(&self, path: &Path) -> io::Result<String> {
        String::from_utf8(self.read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        self.write_shared(path, Arc::new(contents.to_vec()))
    }

    /// Makes `contents` the file at `path`, creating or replacing it, without copying them.
    pub fn write_shared(&self, path: &Path, contents: Arc<Vec<u8>>) -> io::Result<()> {
        let path = normal(path);
        let data = {
            let mut nodes = self.nodes();
            match MemoryFs::node(&nodes, &path) {
                Some(Node::File(data)) => data,
                Some(Node::Dir { .. }) => return Err(error(io::ErrorKind::IsADirectory, "a folder", &path)),
                None => {
                    MemoryFs::parent_is_dir(&nodes, &path)?;
                    let data = Data::new();
                    nodes.insert(path, Node::File(Arc::clone(&data)));
                    data
                }
            }
        };
        *guard(&data.contents) = contents;
        *guard(&data.modified) = SystemTime::now();
        Ok(())
    }

    pub fn create_dir(&self, path: &Path) -> io::Result<()> {
        let path = normal(path);
        let mut nodes = self.nodes();
        if MemoryFs::node(&nodes, &path).is_some() {
            return Err(error(io::ErrorKind::AlreadyExists, "already exists", &path));
        }
        MemoryFs::parent_is_dir(&nodes, &path)?;
        nodes.insert(path, Node::Dir { modified: SystemTime::now() });
        Ok(())
    }

    pub fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let path = normal(path);
        let mut nodes = self.nodes();
        let mut missing = Vec::new();
        let mut at = Some(path.as_path());
        while let Some(p) = at {
            match MemoryFs::node(&nodes, p) {
                Some(Node::Dir { .. }) => break,
                Some(Node::File(_)) => return Err(error(io::ErrorKind::NotADirectory, "not a folder", p)),
                None => missing.push(p.to_path_buf()),
            }
            at = p.parent();
        }
        let now = SystemTime::now();
        for p in missing.into_iter().rev() {
            nodes.insert(p, Node::Dir { modified: now });
        }
        Ok(())
    }

    pub fn remove_file(&self, path: &Path) -> io::Result<()> {
        let path = normal(path);
        let mut nodes = self.nodes();
        match MemoryFs::node(&nodes, &path) {
            Some(Node::File(_)) => {
                nodes.remove(&path);
                Ok(())
            }
            Some(Node::Dir { .. }) => Err(error(io::ErrorKind::IsADirectory, "a folder", &path)),
            None => Err(not_found(&path)),
        }
    }

    pub fn remove_dir(&self, path: &Path) -> io::Result<()> {
        let path = normal(path);
        let mut nodes = self.nodes();
        match MemoryFs::node(&nodes, &path) {
            Some(Node::Dir { .. }) if is_root(&path) => Err(error(io::ErrorKind::PermissionDenied, "the root folder", &path)),
            Some(Node::Dir { .. }) if MemoryFs::subtree(&nodes, &path).len() > 1 => Err(error(io::ErrorKind::DirectoryNotEmpty, "not empty", &path)),
            Some(Node::Dir { .. }) => {
                nodes.remove(&path);
                Ok(())
            }
            Some(Node::File(_)) => Err(error(io::ErrorKind::NotADirectory, "not a folder", &path)),
            None => Err(not_found(&path)),
        }
    }

    pub fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        let path = normal(path);
        let mut nodes = self.nodes();
        match MemoryFs::node(&nodes, &path) {
            Some(Node::Dir { .. }) if is_root(&path) => Err(error(io::ErrorKind::PermissionDenied, "the root folder", &path)),
            Some(Node::Dir { .. }) => {
                for p in MemoryFs::subtree(&nodes, &path) {
                    nodes.remove(&p);
                }
                Ok(())
            }
            Some(Node::File(_)) => Err(error(io::ErrorKind::NotADirectory, "not a folder", &path)),
            None => Err(not_found(&path)),
        }
    }

    /// Moves a file or a folder with everything in it. A file replaces a file at `to`, a folder an empty folder.
    pub fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (from, to) = (normal(from), normal(to));
        let mut nodes = self.nodes();
        let Some(node) = MemoryFs::node(&nodes, &from) else { return Err(not_found(&from)) };
        if from == to {
            return Ok(());
        }
        if is_root(&from) || to.starts_with(&from) {
            return Err(error(io::ErrorKind::InvalidInput, "can't move a folder into itself", &to));
        }
        MemoryFs::parent_is_dir(&nodes, &to)?;
        match (&node, MemoryFs::node(&nodes, &to)) {
            (Node::File(_), Some(Node::Dir { .. })) => return Err(error(io::ErrorKind::IsADirectory, "a folder", &to)),
            (Node::Dir { .. }, Some(Node::File(_))) => return Err(error(io::ErrorKind::NotADirectory, "not a folder", &to)),
            (Node::Dir { .. }, Some(Node::Dir { .. })) if MemoryFs::subtree(&nodes, &to).len() > 1 => {
                return Err(error(io::ErrorKind::DirectoryNotEmpty, "not empty", &to));
            }
            _ => {}
        }
        for p in MemoryFs::subtree(&nodes, &from) {
            if let Some(moved) = nodes.remove(&p)
                && let Ok(rest) = p.strip_prefix(&from)
            {
                let target = if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) };
                nodes.insert(target, moved);
            }
        }
        Ok(())
    }

    /// Copies a file's contents (shared until either is written) and returns their length.
    pub fn copy(&self, from: &Path, to: &Path) -> io::Result<u64> {
        let contents = self.read_shared(from)?;
        let len = contents.len() as u64;
        self.write_shared(to, contents)?;
        Ok(len)
    }

    /// A second name for the file at `from`, which shares its contents from then on.
    pub fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (from, to) = (normal(from), normal(to));
        let mut nodes = self.nodes();
        let data = match MemoryFs::node(&nodes, &from) {
            Some(Node::File(data)) => data,
            Some(Node::Dir { .. }) => return Err(error(io::ErrorKind::IsADirectory, "a folder", &from)),
            None => return Err(not_found(&from)),
        };
        if MemoryFs::node(&nodes, &to).is_some() {
            return Err(error(io::ErrorKind::AlreadyExists, "already exists", &to));
        }
        MemoryFs::parent_is_dir(&nodes, &to)?;
        nodes.insert(to, Node::File(data));
        Ok(())
    }

    pub fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let path = normal(path);
        MemoryFs::node(&self.nodes(), &path).map(|n| Metadata::of(&n)).ok_or_else(|| not_found(&path))
    }

    pub fn exists(&self, path: &Path) -> io::Result<bool> {
        Ok(MemoryFs::node(&self.nodes(), &normal(path)).is_some())
    }

    pub fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let path = normal(path);
        if MemoryFs::node(&self.nodes(), &path).is_some() { Ok(path) } else { Err(not_found(&path)) }
    }

    pub fn read_dir(&self, path: &Path) -> io::Result<ReadDir> {
        let path = normal(path);
        let nodes = self.nodes();
        match MemoryFs::node(&nodes, &path) {
            Some(Node::Dir { .. }) => {}
            Some(Node::File(_)) => return Err(error(io::ErrorKind::NotADirectory, "not a folder", &path)),
            None => return Err(not_found(&path)),
        }
        let entries: Vec<_> = nodes
            .range(path.clone()..)
            .take_while(|(p, _)| p.starts_with(&path))
            .filter(|(p, _)| p.parent() == Some(path.as_path()))
            .map(|(p, n)| Ok(DirEntry { path: p.clone(), metadata: Metadata::of(n) }))
            .collect();
        Ok(ReadDir { entries: entries.into_iter() })
    }

    /// The bytes of every file's contents, each counted once however many names it has.
    pub fn stored_bytes(&self) -> u64 {
        let nodes = self.nodes();
        let mut seen = std::collections::HashSet::new();
        nodes
            .values()
            .filter_map(|n| match n {
                Node::File(data) if seen.insert(Arc::as_ptr(data)) => Some(data.contents().len() as u64),
                _ => None,
            })
            .sum()
    }

    fn open(&self, path: &Path, options: &OpenOptions) -> io::Result<File> {
        let path = normal(path);
        let writes = options.write || options.append;
        let mut nodes = self.nodes();
        let data = match MemoryFs::node(&nodes, &path) {
            Some(Node::Dir { .. }) if writes => return Err(error(io::ErrorKind::IsADirectory, "a folder", &path)),
            // A folder opens for reading (to sync it), and reads nothing.
            Some(Node::Dir { .. }) => return Ok(File::new(Data::new(), Arc::new(Vec::new()), 0, options, false)),
            Some(Node::File(_)) if options.create_new => return Err(error(io::ErrorKind::AlreadyExists, "already exists", &path)),
            Some(Node::File(data)) => data,
            None if writes && (options.create || options.create_new) => {
                MemoryFs::parent_is_dir(&nodes, &path)?;
                let data = Data::new();
                nodes.insert(path, Node::File(Arc::clone(&data)));
                data
            }
            None => return Err(not_found(&path)),
        };
        drop(nodes);
        let truncate = options.truncate && writes;
        let contents = if truncate { Arc::new(Vec::new()) } else { data.contents() };
        let position = if options.append { contents.len() as u64 } else { 0 };
        Ok(File::new(data, contents, position, options, truncate))
    }
}

/// What a file or folder is ([`std::fs::FileType`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType {
    dir: bool,
}

impl FileType {
    pub fn is_dir(&self) -> bool {
        self.dir
    }
    pub fn is_file(&self) -> bool {
        !self.dir
    }
    pub fn is_symlink(&self) -> bool {
        false
    }
}

/// A file's or a folder's metadata ([`std::fs::Metadata`]).
#[derive(Clone, Debug)]
pub struct Metadata {
    kind: FileType,
    len: u64,
    modified: SystemTime,
}

impl Metadata {
    fn of(node: &Node) -> Metadata {
        match node {
            Node::Dir { modified } => Metadata { kind: FileType { dir: true }, len: 0, modified: *modified },
            Node::File(data) => Metadata { kind: FileType { dir: false }, len: data.contents().len() as u64, modified: data.modified() },
        }
    }

    #[allow(clippy::len_without_is_empty, reason = "std::fs::Metadata's methods, which have no is_empty")]
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_dir(&self) -> bool {
        self.kind.dir
    }
    pub fn is_file(&self) -> bool {
        !self.kind.dir
    }
    pub fn is_symlink(&self) -> bool {
        false
    }
    pub fn file_type(&self) -> FileType {
        self.kind
    }
    pub fn modified(&self) -> io::Result<SystemTime> {
        Ok(self.modified)
    }
    pub fn accessed(&self) -> io::Result<SystemTime> {
        Ok(self.modified)
    }
    pub fn created(&self) -> io::Result<SystemTime> {
        Ok(self.modified)
    }
}

/// An entry of [`ReadDir`] ([`std::fs::DirEntry`]).
#[derive(Clone, Debug)]
pub struct DirEntry {
    path: PathBuf,
    metadata: Metadata,
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn file_name(&self) -> OsString {
        self.path.file_name().map_or_else(OsString::new, ToOwned::to_owned)
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        Ok(self.metadata.clone())
    }
    pub fn file_type(&self) -> io::Result<FileType> {
        Ok(self.metadata.kind)
    }
}

/// A folder's entries when it was read ([`std::fs::ReadDir`]).
#[derive(Debug)]
pub struct ReadDir {
    entries: std::vec::IntoIter<io::Result<DirEntry>>,
}

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        self.entries.next()
    }
}

/// How to open a file ([`std::fs::OpenOptions`]).
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // std's options, one flag each.
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

impl OpenOptions {
    pub fn new() -> OpenOptions {
        OpenOptions::default()
    }
    pub fn read(&mut self, read: bool) -> &mut OpenOptions {
        self.read = read;
        self
    }
    pub fn write(&mut self, write: bool) -> &mut OpenOptions {
        self.write = write;
        self
    }
    pub fn append(&mut self, append: bool) -> &mut OpenOptions {
        self.append = append;
        self
    }
    pub fn truncate(&mut self, truncate: bool) -> &mut OpenOptions {
        self.truncate = truncate;
        self
    }
    pub fn create(&mut self, create: bool) -> &mut OpenOptions {
        self.create = create;
        self
    }
    pub fn create_new(&mut self, create_new: bool) -> &mut OpenOptions {
        self.create_new = create_new;
        self
    }
    /// Opens `path` in the browser's in-memory file system.
    #[cfg(target_arch = "wasm32")]
    pub fn open(&self, path: impl AsRef<Path>) -> io::Result<File> {
        global::FS.open(path.as_ref(), self)
    }
    /// Opens `path` in `fs`.
    pub fn open_in(&self, fs: &MemoryFs, path: impl AsRef<Path>) -> io::Result<File> {
        fs.open(path.as_ref(), self)
    }
}

/// An open file ([`std::fs::File`]). See the module documentation.
#[derive(Debug)]
pub struct File {
    data: Arc<Data>,
    /// What this file reads, and writes once it has a copy of its own.
    contents: Arc<Vec<u8>>,
    position: u64,
    options: OpenOptions,
    /// Whether `contents` has changes the file doesn't have yet.
    dirty: AtomicBool,
    /// Whether this handle holds the file's lock.
    locks: AtomicBool,
}

impl File {
    #[cfg(target_arch = "wasm32")]
    pub fn open(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().read(true).open(path)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn create(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().write(true).create(true).truncate(true).open(path)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn create_new(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).create_new(true).open(path)
    }

    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }

    fn new(data: Arc<Data>, contents: Arc<Vec<u8>>, position: u64, options: &OpenOptions, dirty: bool) -> File {
        File { data, contents, position, options: *options, dirty: AtomicBool::new(dirty), locks: AtomicBool::new(false) }
    }

    fn writable(&self) -> io::Result<()> {
        if self.options.write || self.options.append {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "the file is open for reading"))
        }
    }

    /// Gives the file this handle's changes.
    fn commit(&self) {
        if self.dirty.swap(false, Ordering::AcqRel) {
            *guard(&self.data.contents) = Arc::clone(&self.contents);
            *guard(&self.data.modified) = SystemTime::now();
        }
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        let len = if self.dirty.load(Ordering::Acquire) { self.contents.len() } else { self.data.contents().len() };
        Ok(Metadata { kind: FileType { dir: false }, len: len as u64, modified: self.data.modified() })
    }

    pub fn set_len(&mut self, size: u64) -> io::Result<()> {
        self.writable()?;
        let size = usize::try_from(size).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too large"))?;
        Arc::make_mut(&mut self.contents).resize(size, 0);
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    pub fn sync_all(&self) -> io::Result<()> {
        self.commit();
        Ok(())
    }

    pub fn sync_data(&self) -> io::Result<()> {
        self.commit();
        Ok(())
    }

    pub fn set_modified(&self, time: SystemTime) -> io::Result<()> {
        *guard(&self.data.modified) = time;
        Ok(())
    }

    /// Takes the file's exclusive lock, unless another handle holds it.
    pub fn try_lock(&self) -> Result<(), std::fs::TryLockError> {
        if self.locks.load(Ordering::Acquire) || self.data.locked.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            self.locks.store(true, Ordering::Release);
            Ok(())
        } else {
            Err(std::fs::TryLockError::WouldBlock)
        }
    }

    pub fn unlock(&self) -> io::Result<()> {
        if self.locks.swap(false, Ordering::AcqRel) {
            self.data.locked.store(false, Ordering::Release);
        }
        Ok(())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        self.commit();
        let _ = self.unlock();
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(self.position).unwrap_or(usize::MAX).min(self.contents.len());
        let rest = self.contents.get(start..).unwrap_or_default();
        let n = rest.len().min(buf.len());
        buf.get_mut(..n).unwrap_or_default().copy_from_slice(rest.get(..n).unwrap_or_default());
        self.position += n as u64;
        Ok(n)
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writable()?;
        let contents = Arc::make_mut(&mut self.contents);
        let start = if self.options.append {
            contents.len()
        } else {
            usize::try_from(self.position).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?
        };
        let end = start.checked_add(buf.len()).ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        if contents.len() < end {
            contents.resize(end, 0);
        }
        if let Some(target) = contents.get_mut(start..end) {
            target.copy_from_slice(buf);
        }
        self.position = end as u64;
        self.dirty.store(true, Ordering::Release);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.commit();
        Ok(())
    }
}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let len = self.contents.len() as u64;
        let target = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => len.checked_add_signed(d),
            SeekFrom::Current(d) => self.position.checked_add_signed(d),
        };
        self.position = target.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.position)
    }
}

/// The browser build's file system: `std::fs`'s functions over one [`MemoryFs`] for the page.
#[cfg(target_arch = "wasm32")]
pub mod global {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{MemoryFs, Metadata, ReadDir};

    pub(super) static FS: MemoryFs = MemoryFs::new();

    pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        FS.read(path.as_ref())
    }
    /// The contents of the file at `path`, shared with the file system rather than copied.
    pub fn read_shared(path: impl AsRef<Path>) -> io::Result<Arc<Vec<u8>>> {
        FS.read_shared(path.as_ref())
    }
    pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
        FS.read_to_string(path.as_ref())
    }
    pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
        FS.write(path.as_ref(), contents.as_ref())
    }
    /// Makes `contents` the file at `path` without copying them.
    pub fn write_shared(path: impl AsRef<Path>, contents: Arc<Vec<u8>>) -> io::Result<()> {
        FS.write_shared(path.as_ref(), contents)
    }
    pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
        FS.create_dir(path.as_ref())
    }
    pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
        FS.create_dir_all(path.as_ref())
    }
    pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
        FS.remove_file(path.as_ref())
    }
    pub fn remove_dir(path: impl AsRef<Path>) -> io::Result<()> {
        FS.remove_dir(path.as_ref())
    }
    pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
        FS.remove_dir_all(path.as_ref())
    }
    pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
        FS.rename(from.as_ref(), to.as_ref())
    }
    pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
        FS.copy(from.as_ref(), to.as_ref())
    }
    pub fn hard_link(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
        FS.hard_link(from.as_ref(), to.as_ref())
    }
    pub fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
        FS.metadata(path.as_ref())
    }
    pub fn symlink_metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
        FS.metadata(path.as_ref())
    }
    pub fn exists(path: impl AsRef<Path>) -> io::Result<bool> {
        FS.exists(path.as_ref())
    }
    pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
        FS.canonicalize(path.as_ref())
    }
    pub fn read_dir(path: impl AsRef<Path>) -> io::Result<ReadDir> {
        FS.read_dir(path.as_ref())
    }
    pub(crate) fn stored_bytes() -> u64 {
        FS.stored_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn create(fs: &MemoryFs, path: &str) -> File {
        OpenOptions::new().write(true).create(true).truncate(true).open_in(fs, path).unwrap()
    }

    #[test]
    fn files_are_written_read_and_listed_in_folders() {
        let fs = MemoryFs::new();
        assert_eq!(fs.write(&p("/a/b.txt"), b"x").unwrap_err().kind(), io::ErrorKind::NotFound, "the folder must exist");
        fs.create_dir_all(&p("/a/c")).unwrap();
        fs.write(&p("/a/b.txt"), b"hello").unwrap();
        assert_eq!(fs.read(&p("/a/./c/../b.txt")).unwrap(), b"hello");
        assert_eq!(fs.read_to_string(&p("a/b.txt")).unwrap(), "hello", "relative paths start at the root");
        let mut names: Vec<_> = fs.read_dir(&p("/a")).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names, ["b.txt", "c"]);
        let meta = fs.metadata(&p("/a/b.txt")).unwrap();
        assert!(meta.is_file() && meta.len() == 5);
        assert!(fs.metadata(&p("/a/c")).unwrap().is_dir());
        assert!(fs.metadata(&p("/")).unwrap().is_dir());
        assert_eq!(fs.create_dir(&p("/a")).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs.create_dir_all(&p("/a/b.txt/d")).unwrap_err().kind(), io::ErrorKind::NotADirectory);
        assert_eq!(fs.stored_bytes(), 5);
    }

    #[test]
    fn a_written_file_changes_when_flushed_or_dropped() {
        let fs = MemoryFs::new();
        let mut file = create(&fs, "/f");
        file.write_all(b"abc").unwrap();
        assert_eq!(fs.read(&p("/f")).unwrap(), b"", "not before it is flushed");
        file.sync_all().unwrap();
        assert_eq!(fs.read(&p("/f")).unwrap(), b"abc");
        file.seek(SeekFrom::Start(1)).unwrap();
        file.write_all(b"XYZ").unwrap();
        drop(file);
        assert_eq!(fs.read(&p("/f")).unwrap(), b"aXYZ");
        let mut appended = OpenOptions::new().append(true).open_in(&fs, "/f").unwrap();
        appended.write_all(b"!").unwrap();
        drop(appended);
        let mut reader = OpenOptions::new().read(true).open_in(&fs, "/f").unwrap();
        let mut text = String::new();
        reader.read_to_string(&mut text).unwrap();
        assert_eq!(text, "aXYZ!");
        assert!(reader.write(b"no").is_err(), "a file open for reading doesn't write");
        assert_eq!(OpenOptions::new().write(true).create_new(true).open_in(&fs, "/f").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(OpenOptions::new().read(true).open_in(&fs, "/missing").unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn rename_moves_files_and_folders_with_their_contents() {
        let fs = MemoryFs::new();
        fs.create_dir_all(&p("/p/cache")).unwrap();
        fs.write(&p("/p/cache/x"), b"1").unwrap();
        fs.write(&p("/p/project.json.tmp"), b"{}").unwrap();
        fs.rename(&p("/p/project.json.tmp"), &p("/p/project.json")).unwrap();
        assert!(fs.exists(&p("/p/project.json")).unwrap() && !fs.exists(&p("/p/project.json.tmp")).unwrap());
        fs.create_dir(&p("/q")).unwrap();
        fs.rename(&p("/p"), &p("/q")).unwrap();
        assert_eq!(fs.read(&p("/q/cache/x")).unwrap(), b"1");
        assert!(!fs.exists(&p("/p")).unwrap());
        assert_eq!(fs.rename(&p("/q"), &p("/q/inner")).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        fs.create_dir(&p("/r")).unwrap();
        fs.write(&p("/r/full"), b"").unwrap();
        assert_eq!(fs.rename(&p("/q"), &p("/r")).unwrap_err().kind(), io::ErrorKind::DirectoryNotEmpty);
        assert_eq!(fs.remove_dir(&p("/r")).unwrap_err().kind(), io::ErrorKind::DirectoryNotEmpty);
        fs.remove_dir_all(&p("/r")).unwrap();
        assert!(!fs.exists(&p("/r/full")).unwrap());
        assert_eq!(fs.copy(&p("/q/cache/x"), &p("/q/y")).unwrap(), 1);
        fs.write(&p("/q/y"), b"22").unwrap();
        assert_eq!(fs.read(&p("/q/cache/x")).unwrap(), b"1", "a copy is independent");
        fs.hard_link(&p("/q/y"), &p("/q/z")).unwrap();
        fs.write(&p("/q/y"), b"333").unwrap();
        assert_eq!(fs.read(&p("/q/z")).unwrap(), b"333", "a link shares");
        assert_eq!(fs.stored_bytes(), 1 + 2 + 3, "the linked contents count once");
    }

    #[test]
    fn locks_are_exclusive_until_dropped() {
        let fs = MemoryFs::new();
        let options = *OpenOptions::new().read(true).write(true).create(true).truncate(false);
        let first = options.open_in(&fs, "/.lock").unwrap();
        let second = options.open_in(&fs, "/.lock").unwrap();
        first.try_lock().unwrap();
        assert!(matches!(second.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        drop(first);
        second.try_lock().unwrap();
    }
}
