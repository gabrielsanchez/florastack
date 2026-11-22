//! A file's metadata without reading the whole file ([`read_metadata_file`]): an export copies a frame's EXIF, IPTC
//! and XMP, and a frame can be a 50 MB RAW file whose metadata takes a few hundred kilobytes.
//!
//! The file is read sparsely: into a buffer of its size whose pages are read only where the metadata can be (the
//! rest stays zero, which the system doesn't back with memory until it is written), then handed to
//! [`read_metadata`] as if it were the whole file, at the same offsets. What is read is the first megabyte, and
//! beyond it what the container says holds metadata:
//!
//! - TIFF-structured files (TIFF, DNG and the TIFF-based RAW files): every IFD reached from the header (the chains,
//!   sub-IFDs, the EXIF, GPS and interoperability IFDs) with the values its entries point to, and the maker note's
//!   IFD and values, read both relative to the file and to the maker note (a Nikon maker note holds a TIFF of its
//!   own);
//! - JPEG: the segments before the image data; PNG and WebP: every chunk but the image data's; RAF: the embedded
//!   JPEG's segments; CR3 and other ISO media files: every box but the media data's; MRW: its header blocks.
//!
//! Any other file, a small one (a few megabytes), and one whose walk reads more than half of it, is read whole. The
//! walks are bounded (IFDs, entries, chunks, bytes) so that a hostile file can't make them run away.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{Metadata, read_metadata};
use crate::error::CodecError;

/// The bytes read from the start of every file: the headers, and most cameras' EXIF and maker notes.
const PREFIX: u64 = 1 << 20;
/// Files up to this size are read whole.
const SMALL: u64 = 4 << 20;
/// Pages read at a time.
const PAGE: u64 = 4096;
/// Bounds on the walks.
const MAX_IFDS: usize = 256;
const MAX_ENTRIES: u64 = 4096;
const MAX_VALUE: u64 = 32 << 20;
/// The largest value read for a maker note's IFD, which is a guess at where its values are.
const MAX_GUESSED: u64 = 1 << 20;
const MAX_CHUNKS: usize = 100_000;
/// IFD tags that point to further IFDs: sub-IFDs, EXIF, GPS, interoperability.
const IFD_POINTERS: [u16; 4] = [330, 34665, 34853, 40965];
const MAKER_NOTE: u16 = 37500;

/// The metadata of the file at `path`, as [`read_metadata`] reads it from the whole file, reading only the parts that
/// hold it. See the module documentation.
pub fn read_metadata_file(path: &Path) -> Result<Metadata, CodecError> {
    read_sparse(path).map(|(bytes, _)| read_metadata(&bytes))
}

/// The file at `path` read sparsely, and how many of its bytes were read.
pub(crate) fn read_sparse(path: &Path) -> Result<(Vec<u8>, u64), CodecError> {
    let error = |source| CodecError::Read { path: path.to_path_buf(), source };
    let mut file = florastack_core::fs::File::open(path).map_err(error)?;
    let len = file.seek(SeekFrom::End(0)).map_err(error)?;
    let mut sparse = Sparse::new(file, len).map_err(error)?;
    let walked = if len <= SMALL { Ok(false) } else { sparse.walk() };
    match walked {
        Ok(true) if sparse.read <= len / 2 => {}
        Ok(_) => sparse.load(0, len).map_err(error)?,
        Err(e) => return Err(error(e)),
    }
    let read = sparse.read;
    Ok((sparse.bytes, read))
}

/// A file read a page at a time where asked: its bytes, zero where unread.
struct Sparse<R> {
    reader: R,
    len: u64,
    bytes: Vec<u8>,
    pages: Vec<bool>,
    read: u64,
    /// How many maker notes deep the walk is: a maker note's TIFF may hold another, which a hostile file could
    /// repeat without end.
    depth: usize,
}

/// A byte order.
#[derive(Clone, Copy)]
enum Order {
    Little,
    Big,
}

impl<R: Read + Seek> Sparse<R> {
    fn new(reader: R, len: u64) -> std::io::Result<Sparse<R>> {
        let size = usize::try_from(len).map_err(|_| std::io::Error::other("the file is too large to read"))?;
        let pages = usize::try_from(len.div_ceil(PAGE)).map_err(|_| std::io::Error::other("the file is too large to read"))?;
        Ok(Sparse { reader, len, bytes: vec![0; size], pages: vec![false; pages], read: 0, depth: 0 })
    }

    /// Reads the pages that `n` bytes from `start` lie on (within the file), those not read yet.
    fn load(&mut self, start: u64, n: u64) -> std::io::Result<()> {
        let end = start.saturating_add(n).min(self.len);
        if start >= end {
            return Ok(());
        }
        let (first, last) = (start / PAGE, (end - 1) / PAGE);
        let mut page = first;
        while page <= last {
            if self.pages.get(usize::try_from(page).unwrap_or(usize::MAX)).copied().unwrap_or(true) {
                page += 1;
                continue;
            }
            // A run of unread pages, read at once.
            let mut stop = page;
            while stop < last && !self.pages.get(usize::try_from(stop + 1).unwrap_or(usize::MAX)).copied().unwrap_or(true) {
                stop += 1;
            }
            let (from, to) = (page * PAGE, ((stop + 1) * PAGE).min(self.len));
            let (a, b) = (usize::try_from(from).unwrap_or(usize::MAX), usize::try_from(to).unwrap_or(usize::MAX));
            let Some(target) = self.bytes.get_mut(a..b) else { return Ok(()) };
            self.reader.seek(SeekFrom::Start(from))?;
            self.reader.read_exact(target)?;
            for p in page..=stop {
                if let Some(flag) = self.pages.get_mut(usize::try_from(p).unwrap_or(usize::MAX)) {
                    *flag = true;
                }
            }
            self.read += to - from;
            page = stop + 1;
        }
        Ok(())
    }

    /// `n` bytes at `offset`, read first; `None` beyond the file.
    fn get(&mut self, offset: u64, n: u64) -> std::io::Result<Option<&[u8]>> {
        self.load(offset, n)?;
        let end = offset.saturating_add(n);
        if end > self.len {
            return Ok(None);
        }
        let (a, b) = (usize::try_from(offset).unwrap_or(usize::MAX), usize::try_from(end).unwrap_or(usize::MAX));
        Ok(self.bytes.get(a..b))
    }

    fn uint(&mut self, offset: u64, n: u64, order: Order) -> std::io::Result<Option<u64>> {
        Ok(self.get(offset, n)?.map(|b| {
            let fold = |acc: u64, &byte: &u8| (acc << 8) | u64::from(byte);
            match order {
                Order::Big => b.iter().fold(0, fold),
                Order::Little => b.iter().rev().fold(0, fold),
            }
        }))
    }

    /// Reads what holds the metadata of the file's container; false when the container isn't one walked here.
    fn walk(&mut self) -> std::io::Result<bool> {
        self.load(0, PREFIX)?;
        let head: Vec<u8> = self.bytes.get(..16).unwrap_or_default().to_vec();
        let starts = |m: &[u8]| head.starts_with(m);
        if [&b"II"[..], b"MM"].iter().any(|m| starts(m)) {
            self.tiff(0, 0)?;
        } else if starts(&[0xff, 0xd8]) {
            self.jpeg(0)?;
        } else if starts(b"\x89PNG\r\n\x1a\n") {
            self.chunks(8, Chunks::Png)?;
        } else if starts(b"RIFF") && head.get(8..12) == Some(&b"WEBP"[..]) {
            self.chunks(12, Chunks::Webp)?;
        } else if starts(b"FUJIFILMCCD-RAW") {
            // The embedded JPEG's offset and length, big-endian at byte 84.
            if let Some(jpeg) = self.uint(84, 4, Order::Big)? {
                self.jpeg(jpeg)?;
            }
        } else if head.get(4..8) == Some(&b"ftyp"[..]) {
            self.chunks(0, Chunks::Boxes)?;
        } else if starts(b"\0MRM") {
            if let Some(header) = self.uint(4, 4, Order::Big)? {
                self.load(0, 8 + header)?;
            }
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    /// The IFDs of the TIFF whose header is at `header`, its offsets counted from `base`.
    fn tiff(&mut self, header: u64, base: u64) -> std::io::Result<()> {
        let Some(mark) = self.get(header, 2)?.map(<[u8]>::to_vec) else { return Ok(()) };
        let order = if mark == b"MM" { Order::Big } else { Order::Little };
        let big = self.uint(header + 2, 2, order)? == Some(43);
        let first = if big { self.uint(header + 8, 8, order)? } else { self.uint(header + 4, 4, order)? };
        let mut queue: Vec<u64> = first.into_iter().collect();
        let mut seen = Vec::new();
        while let Some(offset) = queue.pop() {
            if offset == 0 || seen.contains(&offset) || seen.len() >= MAX_IFDS {
                continue;
            }
            seen.push(offset);
            let more = self.ifd(base + offset, base, order, big, MAX_VALUE)?;
            queue.extend(more);
        }
        Ok(())
    }

    /// The IFD at `at` and the values its entries point to (offsets counted from `base`, values read up to
    /// `max_value` bytes); returns the IFDs it points to. What doesn't look like an IFD (no entries, too many, a type
    /// TIFF doesn't have) is left alone.
    fn ifd(&mut self, at: u64, base: u64, order: Order, big: bool, max_value: u64) -> std::io::Result<Vec<u64>> {
        let (count_size, entry_size, inline) = if big { (8, 20, 8) } else { (2, 12, 4) };
        let Some(count) = self.uint(at, count_size, order)? else { return Ok(Vec::new()) };
        if count == 0 || count > MAX_ENTRIES {
            return Ok(Vec::new());
        }
        let entries = at + count_size;
        self.load(entries, count * entry_size + inline)?;
        for i in 0..count {
            if !self.uint(entries + i * entry_size + 2, 2, order)?.is_some_and(|kind| (1..=18).contains(&kind)) {
                return Ok(Vec::new());
            }
        }
        let mut next = Vec::new();
        for i in 0..count {
            let e = entries + i * entry_size;
            let (Some(tag), Some(kind)) = (self.uint(e, 2, order)?, self.uint(e + 2, 2, order)?) else { break };
            let n = if big { self.uint(e + 4, 8, order)? } else { self.uint(e + 4, 4, order)? }.unwrap_or(0);
            let value_at = e + if big { 12 } else { 8 };
            let size = n.saturating_mul(type_size(kind));
            let data = if size > inline { self.uint(value_at, inline, order)?.map(|o| base + o) } else { Some(value_at) };
            let Some(data) = data else { continue };
            if size > inline {
                self.load(data, size.min(max_value))?;
            }
            let tag = u16::try_from(tag).unwrap_or(0);
            if IFD_POINTERS.contains(&tag) {
                let width = if type_size(kind) == 8 { 8 } else { 4 };
                for k in 0..n.min(64) {
                    if let Some(p) = self.uint(data + k * width, width, order)? {
                        next.push(p);
                    }
                }
            } else if tag == MAKER_NOTE && size > inline && self.depth < 2 {
                self.depth += 1;
                let walked = self.maker_note(data, size, base, order);
                self.depth -= 1;
                walked?;
            }
        }
        let after = entries + count * entry_size;
        if let Some(p) = self.uint(after, inline, order)? {
            next.push(p);
        }
        Ok(next)
    }

    /// A maker note of `size` bytes at `at`: a TIFF of its own (Nikon), or an IFD after a short signature, its values
    /// at offsets from the file's TIFF or from the maker note; both are read, the walk being bounded.
    fn maker_note(&mut self, at: u64, size: u64, base: u64, order: Order) -> std::io::Result<()> {
        let Some(head) = self.get(at, 16.min(size))?.map(<[u8]>::to_vec) else { return Ok(()) };
        if head.starts_with(b"Nikon\0") {
            return self.tiff(at + 10, at + 10);
        }
        for skip in [0, 8, 10, 12, 14] {
            for relative in [base, at] {
                self.ifd(at + skip, relative, order, false, MAX_GUESSED)?;
            }
        }
        Ok(())
    }

    /// The JPEG at `at`: its segments up to the image data.
    fn jpeg(&mut self, at: u64) -> std::io::Result<()> {
        let mut pos = at + 2;
        for _ in 0..MAX_CHUNKS {
            let Some(marker) = self.get(pos, 2)?.map(<[u8]>::to_vec) else { break };
            if marker.first() != Some(&0xff) {
                break;
            }
            match marker.get(1) {
                // Fill bytes.
                Some(0xff) => pos += 1,
                // Markers without a length.
                Some(0x01 | 0xd0..=0xd8) => pos += 2,
                // The image data, or the end.
                Some(0xda | 0xd9) | None => break,
                Some(_) => {
                    let Some(len) = self.uint(pos + 2, 2, Order::Big)? else { break };
                    self.load(pos, 2 + len)?;
                    pos += 2 + len;
                }
            }
        }
        Ok(())
    }

    /// The chunks or boxes from `pos`, all but the image data's.
    fn chunks(&mut self, mut pos: u64, kind: Chunks) -> std::io::Result<()> {
        for _ in 0..MAX_CHUNKS {
            let (header, size, name) = match kind {
                Chunks::Png => {
                    let (Some(len), Some(name)) = (self.uint(pos, 4, Order::Big)?, self.get(pos + 4, 4)?.map(<[u8]>::to_vec)) else { break };
                    (8, len + 4, name)
                }
                Chunks::Webp => {
                    let (Some(name), Some(len)) = (self.get(pos, 4)?.map(<[u8]>::to_vec), self.uint(pos + 4, 4, Order::Little)?) else { break };
                    (8, len + (len & 1), name)
                }
                Chunks::Boxes => {
                    let (Some(len), Some(name)) = (self.uint(pos, 4, Order::Big)?, self.get(pos + 4, 4)?.map(<[u8]>::to_vec)) else { break };
                    match len {
                        // To the end of the file.
                        0 => (8, self.len.saturating_sub(pos + 8), name),
                        1 => {
                            let Some(large) = self.uint(pos + 8, 8, Order::Big)? else { break };
                            (16, large.saturating_sub(16), name)
                        }
                        n => (8, n.saturating_sub(8), name),
                    }
                }
            };
            let image = [&b"IDAT"[..], b"fdAT", b"VP8 ", b"VP8L", b"ALPH", b"ANMF", b"mdat"].contains(&name.as_slice());
            if !image {
                self.load(pos, header + size.min(MAX_VALUE))?;
            }
            if name == b"IEND" {
                break;
            }
            let Some(after) = pos.checked_add(header).and_then(|p| p.checked_add(size)) else { break };
            if after <= pos || after >= self.len {
                break;
            }
            pos = after;
        }
        Ok(())
    }
}

/// The kinds of chunked containers walked.
#[derive(Clone, Copy)]
enum Chunks {
    Png,
    Webp,
    Boxes,
}

/// The size of one value of TIFF type `kind`; 1 for a type this doesn't know.
fn type_size(kind: u64) -> u64 {
    match kind {
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16..=18 => 8,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixture_gives_the_same_metadata_read_sparsely() {
        let mut checked = 0;
        for set in ["raw", "tiff", "pngsuite", "webp", "openexr"] {
            for path in florastack_testkit::fixtures(set) {
                let whole = std::fs::read(&path).unwrap();
                let (sparse, read) = read_sparse(&path).unwrap();
                assert_eq!(read_metadata(&sparse), read_metadata(&whole), "{}", path.display());
                let len = whole.len() as u64;
                if set == "raw" && len > SMALL {
                    assert!(read <= len / 2, "{}: {read} of {len} bytes read", path.display());
                }
                checked += 1;
            }
        }
        if checked == 0 {
            florastack_testkit::skip!(Corpus, "no format fixtures: run `cargo xtask corpus --fixtures`");
        }
    }

    #[test]
    fn hostile_containers_end_their_walks() {
        // An IFD that points to itself, and a chunk whose length wraps around.
        let dir = std::env::temp_dir().join(format!("florastack-sparse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut tiff = b"II*\0\x08\0\0\0\x01\0\x69\x87\x04\0\x01\0\0\0\x08\0\0\0\x08\0\0\0".to_vec();
        tiff.resize(6 << 20, 0);
        let mut png = b"\x89PNG\r\n\x1a\n\xff\xff\xff\xfftEXt".to_vec();
        png.resize(6 << 20, 7);
        // Nikon maker notes, each holding a TIFF whose IFD's maker note is the next one: 170,000 deep, unbounded.
        let mut nested = b"MM\0*\0\0\0\x08\0\x01\x92\x7c\0\x07\0\0\x01\0\0\0\0\x1a\0\0\0\0".to_vec();
        while nested.len() < 6 << 20 {
            nested.extend_from_slice(b"Nikon\0\x02\x10\0\0MM\0*\0\0\0\x08\0\x01\x92\x7c\0\x07\0\0\x01\0\0\0\0\x1a\0\0\0\0");
        }
        for (name, bytes) in [("loop.tif", tiff), ("wrap.png", png), ("nested.nef", nested)] {
            let path = dir.join(name);
            std::fs::write(&path, &bytes).unwrap();
            let (sparse, _) = read_sparse(&path).unwrap();
            assert_eq!(read_metadata(&sparse), read_metadata(&bytes), "{name}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
