//! Writing a file a band of rows at a time ([`crate::Codecs::write_to`]), so that neither the converted image nor
//! the finished file is ever whole in memory: what gives the bands ([`RowSource`]), where the file goes ([`Sink`]),
//! and what an encoder that streams receives ([`Bands`]).
//!
//! TIFF and DNG stream. Their writers ask for bands of about [`BAND_BYTES`] of stored samples, from the top, convert
//! each one to the output's depth and profile, compress its strips or tiles in parallel and write them before they
//! ask for the next. The other formats' encoders need the whole image: for them the source is asked for every row
//! at once.

use std::io::{Seek, Write};

use florastack_core::{AnyImage, Image, Rect, Sample};

use crate::error::{BackendError, CodecError, Warning};
use crate::format::Format;
use crate::options::BitDepth;

/// About how many bytes of stored samples a streaming writer holds at a time: enough rows for every thread to have
/// strips to compress, small beside any image worth streaming.
pub const BAND_BYTES: usize = 32 << 20;

/// An image handed out a band of rows at a time, top to bottom.
///
/// Every band has the image's width, channels, sample type and color space. A streaming writer first asks for row 0
/// alone, to learn those, and then for consecutive bands from row 0 down; a writer that doesn't stream asks once, for
/// every row.
pub trait RowSource {
    fn width(&self) -> usize;

    fn height(&self) -> usize;

    /// The rows `y..y + rows` (inside the image), as an image `rows` high.
    fn rows(&mut self, y: usize, rows: usize) -> Result<AnyImage, CodecError>;
}

/// An image in memory as a [`RowSource`].
pub struct ImageRows<'a>(pub &'a AnyImage);

impl RowSource for ImageRows<'_> {
    fn width(&self) -> usize {
        self.0.width()
    }

    fn height(&self) -> usize {
        self.0.height()
    }

    fn rows(&mut self, y: usize, rows: usize) -> Result<AnyImage, CodecError> {
        band(self.0, y, rows)
    }
}

/// The rows `y..y + rows` of `image`.
pub(crate) fn band(image: &AnyImage, y: usize, rows: usize) -> Result<AnyImage, CodecError> {
    fn cut<T: Sample>(image: &Image<T>, y: usize, rows: usize) -> Result<Image<T>, CodecError> {
        Ok(image.crop(Rect::new(0, y, image.width(), rows))?)
    }
    Ok(match image {
        AnyImage::U8(i) => AnyImage::U8(cut(i, y, rows)?),
        AnyImage::U16(i) => AnyImage::U16(cut(i, y, rows)?),
        AnyImage::F32(i) => AnyImage::F32(cut(i, y, rows)?),
    })
}

/// Where a streamed file goes: anything that writes and seeks (the writers go back once, to the header).
pub trait Sink: Write + Seek {}

impl<T: Write + Seek + ?Sized> Sink for T {}

/// The image an [`crate::ImageEncoder`] that streams writes: bands already in the output's depth and encoded for its
/// profile.
pub trait Bands {
    fn width(&self) -> usize;

    fn height(&self) -> usize;

    /// The channels of every band.
    fn channels(&self) -> usize;

    /// The sample type of every band.
    fn depth(&self) -> BitDepth;

    /// The rows `y..y + rows`. Encoders ask for consecutive bands from row 0 down.
    fn rows(&mut self, y: usize, rows: usize) -> Result<AnyImage, CodecError>;
}

/// A prepared image in memory as [`Bands`]: how the in-memory [`crate::ImageEncoder::encode`] of a streaming encoder
/// runs the same code.
pub(crate) struct WholeBands<'a>(pub &'a AnyImage);

impl Bands for WholeBands<'_> {
    fn width(&self) -> usize {
        self.0.width()
    }

    fn height(&self) -> usize {
        self.0.height()
    }

    fn channels(&self) -> usize {
        self.0.channels()
    }

    fn depth(&self) -> BitDepth {
        depth_of(self.0)
    }

    fn rows(&mut self, y: usize, rows: usize) -> Result<AnyImage, CodecError> {
        band(self.0, y, rows)
    }
}

pub(crate) fn depth_of(image: &AnyImage) -> BitDepth {
    match image {
        AnyImage::U8(_) => BitDepth::U8,
        AnyImage::U16(_) => BitDepth::U16,
        AnyImage::F32(_) => BitDepth::F32,
    }
}

/// How many rows a band holds: whole `unit`s (a strip's rows, a tile's height) of rows `row_bytes` long, about
/// [`BAND_BYTES`] in all and at least one unit.
pub(crate) fn band_rows(row_bytes: usize, unit: usize) -> usize {
    let unit = unit.max(1);
    let units = BAND_BYTES / row_bytes.max(1).saturating_mul(unit).max(1);
    unit.saturating_mul(units.max(1))
}

/// Checks that a band is what was asked for: a source that hands out something else would make a damaged file.
pub(crate) fn check_band(band: &AnyImage, width: usize, rows: usize, channels: usize, depth: BitDepth, format: Format) -> Result<(), CodecError> {
    if band.width() == width && band.height() == rows && band.channels() == channels && depth_of(band) == depth {
        Ok(())
    } else {
        Err(CodecError::encode(format, BackendError::message("a band of rows doesn't have the image's shape")))
    }
}

/// A file, written to a sink.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// The file's length.
    pub bytes: u64,
    pub warnings: Vec<Warning>,
}

#[cfg(test)]
mod tests {
    use florastack_core::ColorSpace;

    use super::*;

    #[test]
    fn bands_are_whole_units_near_the_budget() {
        assert_eq!(band_rows(1 << 20, 1), 32);
        assert_eq!(band_rows(1 << 20, 5), 30, "whole strips of 5 rows");
        assert_eq!(band_rows(usize::MAX, 256), 256, "at least one unit, however long the rows");
        assert_eq!(band_rows(0, 0), BAND_BYTES);
    }

    #[test]
    fn an_image_hands_out_its_rows() {
        let image = AnyImage::U16(Image::from_fn(3, 5, 2, ColorSpace::LINEAR, |x, y, c| u16::try_from(y * 100 + x * 10 + c).unwrap()).unwrap());
        let mut rows = ImageRows(&image);
        let AnyImage::U16(band) = rows.rows(3, 2).unwrap() else { panic!("16-bit") };
        assert_eq!((band.width(), band.height(), band.channels()), (3, 2, 2));
        assert_eq!(band.pixel(1, 0).unwrap(), [310, 311]);
        assert_eq!(band.color_space(), &ColorSpace::LINEAR);
        assert!(rows.rows(4, 2).is_err(), "past the last row");
        check_band(&AnyImage::U16(band.clone()), 3, 2, 2, BitDepth::U16, Format::Tiff).unwrap();
        assert!(check_band(&AnyImage::U16(band), 3, 3, 2, BitDepth::U16, Format::Tiff).is_err());
    }
}
