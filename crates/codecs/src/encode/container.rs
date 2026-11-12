//! The TIFF container, written to a sink as the image data arrives: the header, then the image's strips or tiles one
//! after another as they are compressed ([`Container::chunk`]), and last the IFDs with what they point to
//! ([`Container::finish`]). Only the directories are ever held in memory, so a file's size doesn't decide how much
//! memory writing it takes.
//!
//! The layout is the one lightcraft-tiff's `TiffWriter` gives a file it builds in memory (image data, then for each
//! IFD its sub-IFDs, its pointer IFDs, its values too long for an entry and the IFD itself, all at even offsets), and
//! values are serialized by its `encode`: a file that fits in memory comes out byte for byte as `TiffWriter` writes
//! it, which the tests check. IFDs given small images of their own, such as a DNG's preview, carry them as
//! lightcraft-tiff's `ImageData`, written with the directories.
//!
//! Classic TIFF or BigTIFF is chosen before the first byte, from the most the image data can take: offsets are 4 or
//! 8 bytes from the header on.

use std::io::{Seek, SeekFrom, Write};

use lightcraft_tiff::writer::encode;
use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, Value, tags as t};

use crate::error::{BackendError, CodecError};
use crate::format::Format;

/// Classic TIFF's 32-bit offsets reach 4 GiB; past this much image data (leaving room for the IFDs), BigTIFF.
pub(crate) const CLASSIC_LIMIT: u64 = (1 << 32) - (64 << 20);

/// IFDs nest this deep at most (IFD0, a sub-IFD, its pointer IFDs): a bound for the recursion, never reached.
const MAX_DEPTH: usize = 16;

const ORDER: ByteOrder = ByteOrder::Little;

/// How an image's chunks are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    Strips { rows: u32 },
    Tiles { width: u32, height: u32 },
}

/// Where the chunks written so far are, in file order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Chunks {
    offsets: Vec<u64>,
    counts: Vec<u64>,
}

/// A TIFF file being written to `sink`.
pub(crate) struct Container<W> {
    sink: W,
    bigtiff: bool,
    format: Format,
    /// Where the file starts in the sink, and how many bytes of it are written.
    start: u64,
    len: u64,
}

impl<W: Write + Seek> Container<W> {
    /// Writes the header, with the first IFD's offset still to come. `data_bound` is the most the image data can
    /// take: past [`CLASSIC_LIMIT`], the file is BigTIFF.
    pub(crate) fn new(mut sink: W, data_bound: u64, format: Format) -> Result<Container<W>, CodecError> {
        let bigtiff = data_bound > CLASSIC_LIMIT;
        let start = sink.stream_position().map_err(|e| CodecError::encode(format, e))?;
        let mut header = b"II".to_vec();
        if bigtiff {
            ORDER.put_u16(&mut header, 43);
            ORDER.put_u16(&mut header, 8);
            ORDER.put_u16(&mut header, 0);
            ORDER.put_u64(&mut header, 0);
        } else {
            ORDER.put_u16(&mut header, 42);
            ORDER.put_u32(&mut header, 0);
        }
        let mut container = Container { sink, bigtiff, format, start, len: 0 };
        container.put(&header)?;
        Ok(container)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), CodecError> {
        self.sink.write_all(bytes).map_err(|e| CodecError::encode(self.format, e))?;
        self.len += bytes.len() as u64;
        Ok(())
    }

    /// Appends one strip or tile, already compressed, and records where it is.
    pub(crate) fn chunk(&mut self, bytes: &[u8], chunks: &mut Chunks) -> Result<(), CodecError> {
        if self.len % 2 == 1 {
            self.put(&[0])?;
        }
        chunks.offsets.push(self.len);
        chunks.counts.push(bytes.len() as u64);
        self.put(bytes)
    }

    /// The entries that say where `chunks` are, set on the IFD of the image they hold.
    pub(crate) fn place(&self, ifd: &mut IfdBuilder, layout: Layout, chunks: Chunks) -> Result<(), CodecError> {
        let (offsets, counts) = match layout {
            Layout::Strips { rows } => {
                ifd.set(t::ROWS_PER_STRIP, Value::Long(vec![rows]));
                (t::STRIP_OFFSETS, t::STRIP_BYTE_COUNTS)
            }
            Layout::Tiles { width, height } => {
                ifd.set(t::TILE_WIDTH, Value::Long(vec![width]));
                ifd.set(t::TILE_LENGTH, Value::Long(vec![height]));
                (t::TILE_OFFSETS, t::TILE_BYTE_COUNTS)
            }
        };
        ifd.set(offsets, self.offsets(chunks.offsets)?);
        ifd.set(counts, self.offsets(chunks.counts)?);
        Ok(())
    }

    fn too_large(&self) -> CodecError {
        CodecError::encode(self.format, BackendError::message("the file outgrew classic TIFF's 4 GiB"))
    }

    fn offsets(&self, values: Vec<u64>) -> Result<Value, CodecError> {
        if self.bigtiff {
            return Ok(Value::Long8(values));
        }
        let narrow: Option<Vec<u32>> = values.into_iter().map(|v| u32::try_from(v).ok()).collect();
        narrow.map(Value::Long).ok_or_else(|| self.too_large())
    }

    /// Writes `chain` (IFD0, IFD1 and so on) with everything its IFDs hold, points the header at it and returns the
    /// sink and the file's length.
    pub(crate) fn finish(mut self, chain: &[IfdBuilder]) -> Result<(W, u64), CodecError> {
        if self.len % 2 == 1 {
            self.put(&[0])?;
        }
        // The directories, serialized as the bytes that follow the image data: offsets count from the file's start.
        let base = self.len;
        let mut tail = Vec::new();
        // Where the offset of the next IFD of the chain goes: in the header first (a position in the file), then in
        // the IFD before (a position in `tail`).
        let mut first = None;
        let mut link: Option<usize> = None;
        for ifd in chain {
            let (offset, next) = self.ifd(&mut tail, base, ifd, 0)?;
            match link {
                None => first = Some(offset),
                Some(at) => self.patch(&mut tail, at, offset)?,
            }
            link = Some(next);
        }
        let first = first.ok_or_else(|| CodecError::encode(self.format, BackendError::message("no IFD to write")))?;
        self.put(&tail)?;
        let mut pointer = Vec::with_capacity(8);
        let at = if self.bigtiff {
            ORDER.put_u64(&mut pointer, first);
            8
        } else {
            ORDER.put_u32(&mut pointer, u32::try_from(first).map_err(|_| self.too_large())?);
            4
        };
        let format = self.format;
        let failed = |e: std::io::Error| CodecError::encode(format, e);
        self.sink.seek(SeekFrom::Start(self.start + at)).map_err(failed)?;
        self.sink.write_all(&pointer).map_err(failed)?;
        self.sink.seek(SeekFrom::Start(self.start + self.len)).map_err(failed)?;
        self.sink.flush().map_err(failed)?;
        Ok((self.sink, self.len))
    }

    fn patch(&self, tail: &mut [u8], at: usize, offset: u64) -> Result<(), CodecError> {
        let mut bytes = Vec::with_capacity(8);
        if self.bigtiff {
            ORDER.put_u64(&mut bytes, offset);
        } else {
            ORDER.put_u32(&mut bytes, u32::try_from(offset).map_err(|_| self.too_large())?);
        }
        let slot = at.checked_add(bytes.len()).and_then(|end| tail.get_mut(at..end));
        let slot = slot.ok_or_else(|| CodecError::encode(self.format, BackendError::message("an IFD's link is out of place")))?;
        slot.copy_from_slice(&bytes);
        Ok(())
    }

    /// Appends `ifd` and what it references to `tail`, whose first byte is at `base` in the file. Returns the IFD's
    /// offset in the file and the position in `tail` of its next-IFD pointer.
    fn ifd(&self, tail: &mut Vec<u8>, base: u64, ifd: &IfdBuilder, depth: usize) -> Result<(u64, usize), CodecError> {
        if depth > MAX_DEPTH {
            return Err(CodecError::encode(self.format, BackendError::message("the IFDs nest too deep")));
        }
        let here = |tail: &Vec<u8>| base + tail.len() as u64;
        let mut entries = ifd.entries.clone();
        if let Some(image) = &ifd.image {
            let (chunks, offsets_tag, counts_tag) = match image {
                ImageData::Strips { rows_per_strip, strips } => {
                    entries.insert(t::ROWS_PER_STRIP, Value::Long(vec![*rows_per_strip]));
                    (strips, t::STRIP_OFFSETS, t::STRIP_BYTE_COUNTS)
                }
                ImageData::Tiles { tile_width, tile_height, tiles } => {
                    entries.insert(t::TILE_WIDTH, Value::Long(vec![*tile_width]));
                    entries.insert(t::TILE_LENGTH, Value::Long(vec![*tile_height]));
                    (tiles, t::TILE_OFFSETS, t::TILE_BYTE_COUNTS)
                }
            };
            let (mut offsets, mut counts) = (Vec::with_capacity(chunks.len()), Vec::with_capacity(chunks.len()));
            for chunk in chunks {
                align(tail);
                offsets.push(here(tail));
                counts.push(chunk.len() as u64);
                tail.extend_from_slice(chunk);
            }
            entries.insert(offsets_tag, self.offsets(offsets)?);
            entries.insert(counts_tag, self.offsets(counts)?);
        }
        if !ifd.sub_ifds.is_empty() {
            let mut offsets = Vec::with_capacity(ifd.sub_ifds.len());
            for sub in &ifd.sub_ifds {
                offsets.push(self.ifd(tail, base, sub, depth + 1)?.0);
            }
            entries.insert(t::SUB_IFDS, self.offsets(offsets)?);
        }
        for (&tag, child) in &ifd.children {
            let offset = self.ifd(tail, base, child, depth + 1)?.0;
            entries.insert(tag, self.offsets(vec![offset])?);
        }
        // Values too long for their entry go before the IFD, which then points at them.
        let inline = if self.bigtiff { 8 } else { 4 };
        let mut encoded = Vec::with_capacity(entries.len());
        for (&tag, value) in &entries {
            let bytes = encode(ORDER, value);
            let at = if bytes.len() > inline {
                align(tail);
                let at = here(tail);
                tail.extend_from_slice(&bytes);
                Some(at)
            } else {
                None
            };
            encoded.push((tag, value, bytes, at));
        }
        align(tail);
        let offset = here(tail);
        if self.bigtiff {
            ORDER.put_u64(tail, encoded.len() as u64);
        } else {
            let count =
                u16::try_from(encoded.len()).map_err(|_| CodecError::encode(self.format, BackendError::message("an IFD has too many entries")))?;
            ORDER.put_u16(tail, count);
        }
        for (tag, value, bytes, at) in &encoded {
            ORDER.put_u16(tail, *tag);
            ORDER.put_u16(tail, value.field_type() as u16);
            if self.bigtiff {
                ORDER.put_u64(tail, value.count() as u64);
            } else {
                let count =
                    u32::try_from(value.count()).map_err(|_| CodecError::encode(self.format, BackendError::message("a value is too long")))?;
                ORDER.put_u32(tail, count);
            }
            match at {
                Some(at) if self.bigtiff => ORDER.put_u64(tail, *at),
                Some(at) => ORDER.put_u32(tail, u32::try_from(*at).map_err(|_| self.too_large())?),
                None => {
                    tail.extend_from_slice(bytes);
                    tail.extend(std::iter::repeat_n(0u8, inline.saturating_sub(bytes.len())));
                }
            }
        }
        let next = tail.len();
        if self.bigtiff {
            ORDER.put_u64(tail, 0);
        } else {
            ORDER.put_u32(tail, 0);
        }
        Ok((offset, next))
    }
}

/// Values and IFDs start at even offsets. The image data before `tail` ends at one, so `tail`'s length decides.
fn align(tail: &mut Vec<u8>) {
    if tail.len() % 2 == 1 {
        tail.push(0);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use lightcraft_tiff::TiffWriter;

    use super::*;

    fn ifd(strips: &[Vec<u8>]) -> IfdBuilder {
        let mut ifd = IfdBuilder::new();
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![3]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![u32::try_from(strips.len()).unwrap()]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8]));
        ifd.set(t::SOFTWARE, Value::Ascii("a name too long for an entry".to_owned()));
        let mut exif = IfdBuilder::new();
        exif.set(t::EXPOSURE_TIME, Value::Rational(vec![(1, 250)]));
        ifd.set_child(t::EXIF_IFD, exif);
        ifd
    }

    #[test]
    fn a_streamed_file_is_the_one_the_in_memory_writer_makes() {
        // Odd lengths, so that chunks need padding to even offsets.
        let strips = vec![vec![1u8; 9], vec![2; 4], vec![3; 7]];
        for bigtiff in [false, true] {
            let mut whole = ifd(&strips);
            whole.set_image(ImageData::Strips { rows_per_strip: 1, strips: strips.clone() });
            let expected = TiffWriter::new(ByteOrder::Little, bigtiff).write(&[whole]).unwrap();

            let bound = if bigtiff { CLASSIC_LIMIT + 1 } else { 20 };
            let mut container = Container::new(Cursor::new(Vec::new()), bound, Format::Tiff).unwrap();
            let mut chunks = Chunks::default();
            for strip in &strips {
                container.chunk(strip, &mut chunks).unwrap();
            }
            let mut streamed = ifd(&strips);
            container.place(&mut streamed, Layout::Strips { rows: 1 }, chunks).unwrap();
            let (sink, len) = container.finish(&[streamed]).unwrap();
            let bytes = sink.into_inner();
            assert_eq!(len, bytes.len() as u64);
            assert_eq!(bytes, expected, "bigtiff: {bigtiff}");
        }
    }

    #[test]
    fn sub_ifds_chains_and_images_held_by_an_ifd_match_too() {
        let tiles = vec![vec![7u8; 5], vec![8; 6]];
        let build = || {
            let mut preview = ifd(&[vec![0; 3]]);
            preview.set_image(ImageData::Strips { rows_per_strip: 1, strips: vec![vec![9u8; 3]] });
            let mut raw = IfdBuilder::new();
            raw.set(t::IMAGE_WIDTH, Value::Long(vec![2]));
            raw.set_image(ImageData::Tiles { tile_width: 16, tile_height: 16, tiles: tiles.clone() });
            preview.add_sub_ifd(raw);
            let mut second = IfdBuilder::new();
            second.set(t::IMAGE_WIDTH, Value::Long(vec![1]));
            vec![preview, second]
        };
        let expected = TiffWriter::new(ByteOrder::Little, false).write(&build()).unwrap();
        let container = Container::new(Cursor::new(Vec::new()), 0, Format::Dng).unwrap();
        let (sink, _) = container.finish(&build()).unwrap();
        assert_eq!(sink.into_inner(), expected);
    }

    #[test]
    fn the_file_starts_where_the_sink_stands() {
        let mut sink = Cursor::new(vec![0xAAu8; 3]);
        sink.set_position(3);
        let mut container = Container::new(sink, 0, Format::Tiff).unwrap();
        let mut chunks = Chunks::default();
        container.chunk(&[5, 6], &mut chunks).unwrap();
        let mut image = IfdBuilder::new();
        container.place(&mut image, Layout::Strips { rows: 1 }, chunks).unwrap();
        let (sink, len) = container.finish(&[image]).unwrap();
        let bytes = sink.into_inner();
        assert_eq!(&bytes[..3], [0xAA; 3]);
        let file = &bytes[3..];
        assert_eq!(len, file.len() as u64);
        let parsed = lightcraft_tiff::Tiff::parse(file).unwrap();
        assert_eq!(parsed.ifds[0].u32(t::STRIP_OFFSETS), Some(8), "offsets count from the file's start, not the sink's");
    }

    #[test]
    fn classic_offsets_past_4_gib_are_an_error() {
        let container = Container::new(Cursor::new(Vec::new()), 0, Format::Tiff).unwrap();
        assert!(container.offsets(vec![u64::from(u32::MAX) + 1]).is_err());
        assert!(matches!(container.offsets(vec![7]).unwrap(), Value::Long(v) if v == [7]));
    }
}
