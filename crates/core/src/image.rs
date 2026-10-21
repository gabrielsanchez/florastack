//! Image buffers: [`Image<T>`] holds `u8`, `u16` or `f32` samples with one to four interleaved channels, a row stride
//! and a [`ColorSpace`] tag; [`AnyImage`] holds one of the three when the sample type is known only at run time.
//!
//! This is the container. Conversions between sample types and encodings ([`crate::convert`]), the tiled view
//! ([`crate::tile`]), the tile cache ([`crate::store`]) and the basic operations ([`crate::ops`]) build on it. Stacking
//! math runs on linear `Image<f32>`, with samples nominally in `[0, 1]`; values outside that range are allowed
//! (extended range).
//!
//! Channels are interleaved: gray, gray and alpha, RGB, or RGBA. The alpha channel (the second of two, the fourth of
//! four) is coverage, never transfer-encoded.
//!
//! Construction validates the dimensions with checked arithmetic and reserves memory with `try_reserve_exact`, so a
//! hostile size returns an [`ImageError`] instead of panicking or aborting. Limits on what a decoder accepts belong to
//! the decoder.

use std::collections::TryReserveError;
use std::fmt;
use std::sync::Arc;

use rayon::prelude::*;

use crate::geometry::Rect;

/// The most interleaved channels an image may have (gray, gray and alpha, RGB, RGBA).
pub const MAX_CHANNELS: usize = 4;

/// The sample types, for code that learns which one an image has at run time (decoders, the tile cache).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    U8,
    U16,
    F32,
}

impl SampleFormat {
    /// The size of one sample in bytes.
    pub const fn bytes(self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::U16 => 2,
            SampleFormat::F32 => 4,
        }
    }
}

/// A sample type an [`Image`] can hold: `u8`, `u16` or `f32`. Sealed: no other type implements it.
///
/// Integer samples are fractions of their largest value: `255u8` and `65535u16` are white, as `1.0f32` is. The
/// normalizing conversions below are exact both ways for every integer sample.
pub trait Sample: Copy + Default + PartialEq + Send + Sync + fmt::Debug + 'static + sealed::Sealed {
    const FORMAT: SampleFormat;
    /// Nominal white: 255, 65535 or 1.0.
    const WHITE: Self;

    /// This sample as a fraction of white: `v / 255` or `v / 65535` for the integer types (so white is exactly 1),
    /// the value itself for `f32`.
    fn to_normalized(self) -> f32;

    /// The sample nearest to `v` times white. The integer types round to nearest and clamp to their range, and NaN
    /// gives 0; `f32` keeps `v` as it is, extended range included.
    fn from_normalized(v: f32) -> Self;
}

impl Sample for u8 {
    const FORMAT: SampleFormat = SampleFormat::U8;
    const WHITE: u8 = u8::MAX;

    fn to_normalized(self) -> f32 {
        // Division, not multiplication by 1/255: it is correctly rounded, so 255 maps to exactly 1.
        f32::from(self) / 255.0
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A saturating cast is the clamp: NaN gives 0.
    fn from_normalized(v: f32) -> u8 {
        (v * 255.0 + 0.5) as u8
    }
}

impl Sample for u16 {
    const FORMAT: SampleFormat = SampleFormat::U16;
    const WHITE: u16 = u16::MAX;

    fn to_normalized(self) -> f32 {
        f32::from(self) / 65535.0
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A saturating cast is the clamp: NaN gives 0.
    fn from_normalized(v: f32) -> u16 {
        (v * 65535.0 + 0.5) as u16
    }
}

impl Sample for f32 {
    const FORMAT: SampleFormat = SampleFormat::F32;
    const WHITE: f32 = 1.0;

    fn to_normalized(self) -> f32 {
        self
    }

    fn from_normalized(v: f32) -> f32 {
        v
    }
}

/// What the crate needs from a sample type beyond [`Sample`]: wrapping in [`AnyImage`], little-endian bytes for the
/// tile cache, and blending for the stitched tiles. Unnameable outside the crate, which seals [`Sample`].
pub(crate) mod sealed {
    use super::{AnyImage, Image, Sample};

    pub trait Sealed: Sized {
        fn wrap(image: Image<Self>) -> AnyImage
        where
            Self: Sample;

        /// The image inside `any`, or `any` back if it holds another sample type.
        fn unwrap(any: AnyImage) -> Result<Image<Self>, AnyImage>
        where
            Self: Sample;

        fn unwrap_ref(any: &AnyImage) -> Option<&Image<Self>>
        where
            Self: Sample;

        fn unwrap_mut(any: &mut AnyImage) -> Option<&mut Image<Self>>
        where
            Self: Sample;

        /// Appends `samples` to `out` as little-endian bytes.
        fn write_le(samples: &[Self], out: &mut Vec<u8>);

        /// Appends the samples of `bytes` (little-endian, a whole number of samples) to `out`.
        fn read_le(bytes: &[u8], out: &mut Vec<Self>);

        /// `a + t (b - a)`, rounded to nearest for the integer types; exactly `a` when `a == b`.
        fn lerp(a: Self, b: Self, t: f32) -> Self;

        /// Decodes sRGB-encoded samples to normalized linear `f32` values, every sample alike (alpha is the caller's).
        fn srgb_decode(samples: &[Self], out: &mut [f32]);
    }

    impl Sealed for u8 {
        fn wrap(image: Image<u8>) -> AnyImage {
            AnyImage::U8(image)
        }

        fn unwrap(any: AnyImage) -> Result<Image<u8>, AnyImage> {
            if let AnyImage::U8(image) = any { Ok(image) } else { Err(any) }
        }

        fn unwrap_ref(any: &AnyImage) -> Option<&Image<u8>> {
            if let AnyImage::U8(image) = any { Some(image) } else { None }
        }

        fn unwrap_mut(any: &mut AnyImage) -> Option<&mut Image<u8>> {
            if let AnyImage::U8(image) = any { Some(image) } else { None }
        }

        fn write_le(samples: &[u8], out: &mut Vec<u8>) {
            out.extend_from_slice(samples);
        }

        fn read_le(bytes: &[u8], out: &mut Vec<u8>) {
            out.extend_from_slice(bytes);
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Between two u8 values: in range.
        fn lerp(a: u8, b: u8, t: f32) -> u8 {
            let a_f = f32::from(a);
            (a_f + t * (f32::from(b) - a_f) + 0.5) as u8
        }

        fn srgb_decode(samples: &[u8], out: &mut [f32]) {
            crate::convert::decode_u8_row(samples, out);
        }
    }

    impl Sealed for u16 {
        fn wrap(image: Image<u16>) -> AnyImage {
            AnyImage::U16(image)
        }

        fn unwrap(any: AnyImage) -> Result<Image<u16>, AnyImage> {
            if let AnyImage::U16(image) = any { Ok(image) } else { Err(any) }
        }

        fn unwrap_ref(any: &AnyImage) -> Option<&Image<u16>> {
            if let AnyImage::U16(image) = any { Some(image) } else { None }
        }

        fn unwrap_mut(any: &mut AnyImage) -> Option<&mut Image<u16>> {
            if let AnyImage::U16(image) = any { Some(image) } else { None }
        }

        fn write_le(samples: &[u16], out: &mut Vec<u8>) {
            out.extend(samples.iter().flat_map(|v| v.to_le_bytes()));
        }

        fn read_le(bytes: &[u8], out: &mut Vec<u16>) {
            out.extend(bytes.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)));
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Between two u16 values: in range.
        fn lerp(a: u16, b: u16, t: f32) -> u16 {
            let a_f = f32::from(a);
            (a_f + t * (f32::from(b) - a_f) + 0.5) as u16
        }

        fn srgb_decode(samples: &[u16], out: &mut [f32]) {
            crate::convert::decode_u16_row(samples, out);
        }
    }

    impl Sealed for f32 {
        fn wrap(image: Image<f32>) -> AnyImage {
            AnyImage::F32(image)
        }

        fn unwrap(any: AnyImage) -> Result<Image<f32>, AnyImage> {
            if let AnyImage::F32(image) = any { Ok(image) } else { Err(any) }
        }

        fn unwrap_ref(any: &AnyImage) -> Option<&Image<f32>> {
            if let AnyImage::F32(image) = any { Some(image) } else { None }
        }

        fn unwrap_mut(any: &mut AnyImage) -> Option<&mut Image<f32>> {
            if let AnyImage::F32(image) = any { Some(image) } else { None }
        }

        fn write_le(samples: &[f32], out: &mut Vec<u8>) {
            out.extend(samples.iter().flat_map(|v| v.to_le_bytes()));
        }

        fn read_le(bytes: &[u8], out: &mut Vec<f32>) {
            out.extend(bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        }

        #[allow(clippy::float_cmp)] // Equal samples, infinities included, must blend to themselves exactly.
        fn lerp(a: f32, b: f32, t: f32) -> f32 {
            if a == b { a } else { a + t * (b - a) }
        }

        fn srgb_decode(samples: &[f32], out: &mut [f32]) {
            crate::convert::decode_f32_row(samples, out);
        }
    }
}

/// How sample values relate to light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// Proportional to scene light (scene-referred linear): what stacking computes on.
    #[default]
    Linear,
    /// Encoded with the sRGB transfer curve (IEC 61966-2-1), as most 8-bit files are.
    Srgb,
}

/// The color-space tag of an image: its transfer encoding and, when the file had one, its ICC profile. Without a
/// profile the primaries and white point are those of sRGB (Rec. 709 primaries, D65 white). The encoding always says
/// how the samples are encoded, whatever curve the profile has: the codecs linearize files whose profile curve is
/// neither sRGB's nor linear, so the profile then only describes the primaries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ColorSpace {
    pub encoding: Encoding,
    /// The embedded ICC profile, kept byte for byte so that output can carry it over; shared, so clones are cheap.
    pub icc: Option<Arc<[u8]>>,
}

impl ColorSpace {
    /// Linear light with sRGB primaries and no profile.
    pub const LINEAR: ColorSpace = ColorSpace { encoding: Encoding::Linear, icc: None };
    /// sRGB-encoded with no profile.
    pub const SRGB: ColorSpace = ColorSpace { encoding: Encoding::Srgb, icc: None };
}

/// Why an image can't be created.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    #[error("an image has 1 to {MAX_CHANNELS} channels, not {0}")]
    Channels(usize),
    #[error("an image has at least one pixel, not {width}x{height}")]
    Empty { width: usize, height: usize },
    #[error("a {width}x{height} image with {channels} channels is too large to address")]
    TooLarge { width: usize, height: usize, channels: usize },
    #[error("a row stride of {stride} samples is shorter than a row of {row} samples")]
    Stride { stride: usize, row: usize },
    #[error(
        "the buffer holds {actual} samples, but a {width}x{height} image with {channels} channels and a stride of {stride} needs at least {expected}"
    )]
    BufferLength { width: usize, height: usize, channels: usize, stride: usize, expected: usize, actual: usize },
    #[error("cannot allocate a {width}x{height} image with {channels} channels")]
    Alloc {
        width: usize,
        height: usize,
        channels: usize,
        #[source]
        source: TryReserveError,
    },
    #[error("the region {rect:?} is empty or reaches outside the {width}x{height} image")]
    Region { rect: Rect, width: usize, height: usize },
}

/// The validated shape of an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    width: usize,
    height: usize,
    channels: usize,
    stride: usize,
}

impl Shape {
    /// Checks a shape whose rows are `stride` samples apart (`None`: packed rows) for samples of `sample_size` bytes.
    fn new(width: usize, height: usize, channels: usize, stride: Option<usize>, sample_size: usize) -> Result<Shape, ImageError> {
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(ImageError::Channels(channels));
        }
        if width == 0 || height == 0 {
            return Err(ImageError::Empty { width, height });
        }
        let too_large = ImageError::TooLarge { width, height, channels };
        let row = width.checked_mul(channels).ok_or_else(|| too_large.clone())?;
        let stride = stride.unwrap_or(row);
        if stride < row {
            return Err(ImageError::Stride { stride, row });
        }
        let shape = Shape { width, height, channels, stride };
        // A Vec can't hold more than isize::MAX bytes.
        let bytes = shape.min_len().and_then(|len| len.checked_mul(sample_size)).ok_or_else(|| too_large.clone())?;
        if isize::try_from(bytes).is_err() {
            return Err(too_large);
        }
        Ok(shape)
    }

    fn row_len(self) -> usize {
        self.width * self.channels
    }

    /// The fewest samples a buffer of this shape holds: every row but the last padded to the stride.
    fn min_len(self) -> Option<usize> {
        self.stride.checked_mul(self.height - 1)?.checked_add(self.row_len())
    }
}

/// An image: `height` rows of `width` pixels, each pixel `channels` interleaved samples, rows `stride` samples apart.
///
/// Coordinates are pixel indices: `(0, 0)` is the top-left pixel, and geometry treats pixel `(x, y)` as the point at
/// the pixel's center (see [`crate::geometry`]). Equality compares the shape, the color space and the pixels, not the
/// padding at the end of the rows.
#[derive(Clone)]
pub struct Image<T: Sample> {
    shape: Shape,
    color: ColorSpace,
    data: Vec<T>,
}

impl<T: Sample> Image<T> {
    /// An image with every sample `T::default()` (zero) and packed rows.
    pub fn new(width: usize, height: usize, channels: usize, color: ColorSpace) -> Result<Self, ImageError> {
        let shape = Shape::new(width, height, channels, None, size_of::<T>())?;
        let len = shape.stride * shape.height;
        let mut data = Vec::new();
        data.try_reserve_exact(len).map_err(|source| ImageError::Alloc { width, height, channels, source })?;
        data.resize(len, T::default());
        Ok(Image { shape, color, data })
    }

    /// An image over `data`, whose rows are packed: `width * height * channels` samples.
    pub fn from_vec(width: usize, height: usize, channels: usize, color: ColorSpace, data: Vec<T>) -> Result<Self, ImageError> {
        Self::with_stride(width, height, channels, None, color, data)
    }

    /// An image over `data` whose rows start `stride` samples apart. The buffer holds every row but the last padded
    /// to the stride, and the last row; the padding is kept but is not part of the image.
    pub fn from_vec_with_stride(
        width: usize,
        height: usize,
        channels: usize,
        stride: usize,
        color: ColorSpace,
        data: Vec<T>,
    ) -> Result<Self, ImageError> {
        Self::with_stride(width, height, channels, Some(stride), color, data)
    }

    fn with_stride(width: usize, height: usize, channels: usize, stride: Option<usize>, color: ColorSpace, data: Vec<T>) -> Result<Self, ImageError> {
        let shape = Shape::new(width, height, channels, stride, size_of::<T>())?;
        let expected = shape.min_len().unwrap_or(usize::MAX);
        if data.len() < expected {
            return Err(ImageError::BufferLength { width, height, channels, stride: shape.stride, expected, actual: data.len() });
        }
        Ok(Image { shape, color, data })
    }

    /// An image with packed rows whose sample `(x, y, channel)` is `f(x, y, channel)`, called in memory order.
    pub fn from_fn(
        width: usize,
        height: usize,
        channels: usize,
        color: ColorSpace,
        mut f: impl FnMut(usize, usize, usize) -> T,
    ) -> Result<Self, ImageError> {
        let mut image = Self::new(width, height, channels, color)?;
        for (y, row) in image.rows_mut().enumerate() {
            for (i, sample) in row.iter_mut().enumerate() {
                *sample = f(i / channels, y, i % channels);
            }
        }
        Ok(image)
    }

    pub fn width(&self) -> usize {
        self.shape.width
    }

    pub fn height(&self) -> usize {
        self.shape.height
    }

    pub fn channels(&self) -> usize {
        self.shape.channels
    }

    /// The distance between the starts of two rows, in samples (at least `width * channels`).
    pub fn stride(&self) -> usize {
        self.shape.stride
    }

    /// The number of pixels, `width * height`.
    pub fn pixel_count(&self) -> usize {
        self.shape.width * self.shape.height
    }

    pub fn color_space(&self) -> &ColorSpace {
        &self.color
    }

    pub fn set_color_space(&mut self, color: ColorSpace) {
        self.color = color;
    }

    /// Whether `other` has the same width, height and channel count (whatever its sample type and color space).
    pub fn same_shape<U: Sample>(&self, other: &Image<U>) -> bool {
        (self.width(), self.height(), self.channels()) == (other.width(), other.height(), other.channels())
    }

    /// The `width * channels` samples of row `y`, or `None` below the last row.
    pub fn row(&self, y: usize) -> Option<&[T]> {
        if y >= self.shape.height {
            return None;
        }
        let start = y * self.shape.stride;
        self.data.get(start..start + self.shape.row_len())
    }

    /// The samples of row `y`, mutably.
    pub fn row_mut(&mut self, y: usize) -> Option<&mut [T]> {
        if y >= self.shape.height {
            return None;
        }
        let start = y * self.shape.stride;
        let len = self.shape.row_len();
        self.data.get_mut(start..start + len)
    }

    /// Every row, top to bottom, without the padding.
    pub fn rows(&self) -> impl ExactSizeIterator<Item = &[T]> + DoubleEndedIterator {
        let len = self.shape.row_len();
        // The buffer holds at least `stride * (height - 1) + len` samples, so every row is whole.
        self.data.chunks(self.shape.stride).take(self.shape.height).map(move |row| row.get(..len).unwrap_or(row))
    }

    /// Every row, mutably.
    pub fn rows_mut(&mut self) -> impl ExactSizeIterator<Item = &mut [T]> + DoubleEndedIterator {
        let len = self.shape.row_len();
        self.data.chunks_mut(self.shape.stride).take(self.shape.height).map(move |row| {
            let whole = row.len().min(len);
            &mut row[..whole]
        })
    }

    /// Every row, as a rayon parallel iterator (in order, so `enumerate` gives the row index).
    pub fn par_rows(&self) -> impl IndexedParallelIterator<Item = &[T]> {
        let len = self.shape.row_len();
        self.data.par_chunks(self.shape.stride).take(self.shape.height).map(move |row| row.get(..len).unwrap_or(row))
    }

    /// Every row, mutably, as a rayon parallel iterator.
    pub fn par_rows_mut(&mut self) -> impl IndexedParallelIterator<Item = &mut [T]> {
        let len = self.shape.row_len();
        self.data.par_chunks_mut(self.shape.stride).take(self.shape.height).map(move |row| {
            let whole = row.len().min(len);
            &mut row[..whole]
        })
    }

    /// A copy of the pixels in `rect`, with packed rows and the same color space.
    pub fn crop(&self, rect: Rect) -> Result<Image<T>, ImageError> {
        if rect.is_empty() || !rect.fits_in(self.width(), self.height()) {
            return Err(ImageError::Region { rect, width: self.width(), height: self.height() });
        }
        let c = self.shape.channels;
        let mut out = Image::new(rect.width, rect.height, c, self.color.clone())?;
        let columns = rect.x * c..(rect.x + rect.width) * c;
        for (dst, src) in out.rows_mut().zip(self.rows().skip(rect.y)) {
            if let Some(src) = src.get(columns.clone()) {
                dst.copy_from_slice(src);
            }
        }
        Ok(out)
    }

    /// The samples of pixel `(x, y)`, or `None` outside the image.
    pub fn pixel(&self, x: usize, y: usize) -> Option<&[T]> {
        if x >= self.shape.width {
            return None;
        }
        let c = self.shape.channels;
        self.row(y)?.get(x * c..x * c + c)
    }

    /// The samples of pixel `(x, y)`, mutably.
    pub fn pixel_mut(&mut self, x: usize, y: usize) -> Option<&mut [T]> {
        if x >= self.shape.width {
            return None;
        }
        let c = self.shape.channels;
        self.row_mut(y)?.get_mut(x * c..x * c + c)
    }

    /// Sample `channel` of pixel `(x, y)`, or `None` outside the image.
    pub fn get(&self, x: usize, y: usize, channel: usize) -> Option<T> {
        if channel >= self.shape.channels {
            return None;
        }
        self.pixel(x, y)?.get(channel).copied()
    }

    /// The whole buffer, row padding included.
    pub fn as_raw(&self) -> &[T] {
        &self.data
    }

    /// The buffer, row padding included.
    pub fn into_raw(self) -> Vec<T> {
        self.data
    }
}

impl<T: Sample> PartialEq for Image<T> {
    fn eq(&self, other: &Self) -> bool {
        self.same_shape(other) && self.color == other.color && self.rows().eq(other.rows())
    }
}

impl<T: Sample> fmt::Debug for Image<T> {
    /// The shape and color space; the pixels would drown everything else.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Image")
            .field("sample", &std::any::type_name::<T>())
            .field("width", &self.shape.width)
            .field("height", &self.shape.height)
            .field("channels", &self.shape.channels)
            .field("stride", &self.shape.stride)
            .field("color", &self.color)
            .finish_non_exhaustive()
    }
}

/// An image whose sample type is known only at run time: what a decoder returns, and what the tile cache holds.
#[derive(Clone, Debug, PartialEq)]
pub enum AnyImage {
    U8(Image<u8>),
    U16(Image<u16>),
    F32(Image<f32>),
}

impl AnyImage {
    pub fn from_image<T: Sample>(image: Image<T>) -> AnyImage {
        T::wrap(image)
    }

    /// The image inside, if its samples are `T`; otherwise `self` back.
    pub fn into_image<T: Sample>(self) -> Result<Image<T>, AnyImage> {
        T::unwrap(self)
    }

    /// The image inside, if its samples are `T`.
    pub fn as_image<T: Sample>(&self) -> Option<&Image<T>> {
        T::unwrap_ref(self)
    }

    /// The image inside, mutably, if its samples are `T`.
    pub fn as_image_mut<T: Sample>(&mut self) -> Option<&mut Image<T>> {
        T::unwrap_mut(self)
    }

    pub fn format(&self) -> SampleFormat {
        match self {
            AnyImage::U8(_) => SampleFormat::U8,
            AnyImage::U16(_) => SampleFormat::U16,
            AnyImage::F32(_) => SampleFormat::F32,
        }
    }

    pub fn width(&self) -> usize {
        match self {
            AnyImage::U8(i) => i.width(),
            AnyImage::U16(i) => i.width(),
            AnyImage::F32(i) => i.width(),
        }
    }

    pub fn height(&self) -> usize {
        match self {
            AnyImage::U8(i) => i.height(),
            AnyImage::U16(i) => i.height(),
            AnyImage::F32(i) => i.height(),
        }
    }

    pub fn channels(&self) -> usize {
        match self {
            AnyImage::U8(i) => i.channels(),
            AnyImage::U16(i) => i.channels(),
            AnyImage::F32(i) => i.channels(),
        }
    }

    pub fn color_space(&self) -> &ColorSpace {
        match self {
            AnyImage::U8(i) => i.color_space(),
            AnyImage::U16(i) => i.color_space(),
            AnyImage::F32(i) => i.color_space(),
        }
    }

    /// The memory the buffer holds, in bytes (row padding included).
    pub fn buffer_bytes(&self) -> usize {
        match self {
            AnyImage::U8(i) => i.as_raw().len(),
            AnyImage::U16(i) => i.as_raw().len() * 2,
            AnyImage::F32(i) => i.as_raw().len() * 4,
        }
    }
}

impl<T: Sample> From<Image<T>> for AnyImage {
    fn from(image: Image<T>) -> AnyImage {
        T::wrap(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_images_are_zeroed_and_packed() {
        let image = Image::<u16>::new(3, 2, 4, ColorSpace::SRGB).unwrap();
        assert_eq!((image.width(), image.height(), image.channels(), image.stride()), (3, 2, 4, 12));
        assert_eq!(image.pixel_count(), 6);
        assert_eq!(image.as_raw(), &[0; 24]);
        assert_eq!(image.color_space().encoding, Encoding::Srgb);
    }

    #[test]
    fn invalid_shapes_are_errors() {
        assert_eq!(Image::<u8>::new(2, 2, 0, ColorSpace::LINEAR).unwrap_err(), ImageError::Channels(0));
        assert_eq!(Image::<u8>::new(2, 2, 5, ColorSpace::LINEAR).unwrap_err(), ImageError::Channels(5));
        assert_eq!(Image::<u8>::new(0, 2, 1, ColorSpace::LINEAR).unwrap_err(), ImageError::Empty { width: 0, height: 2 });
        assert!(matches!(Image::<f32>::new(usize::MAX, 2, 3, ColorSpace::LINEAR), Err(ImageError::TooLarge { .. })));
        assert!(matches!(Image::<f32>::new(1 << 40, 1 << 40, 1, ColorSpace::LINEAR), Err(ImageError::TooLarge { .. })));
        assert_eq!(Image::from_vec_with_stride(4, 2, 1, 3, ColorSpace::LINEAR, vec![0u8; 8]).unwrap_err(), ImageError::Stride { stride: 3, row: 4 });
        assert!(matches!(
            Image::from_vec(2, 2, 3, ColorSpace::LINEAR, vec![0u8; 11]),
            Err(ImageError::BufferLength { expected: 12, actual: 11, .. })
        ));
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn a_huge_but_addressable_image_fails_to_allocate_instead_of_aborting() {
        // A quarter of the address space is addressable, but no machine can reserve it.
        let side = 1 << 30;
        let e = Image::<u8>::new(side, (usize::MAX / 4) / side, 1, ColorSpace::LINEAR).unwrap_err();
        assert!(matches!(e, ImageError::Alloc { .. } | ImageError::TooLarge { .. }), "{e}");
    }

    #[test]
    fn strided_rows_skip_the_padding() {
        // 2x3 gray image with rows 4 samples apart; the last row needs no padding.
        let data = vec![1u8, 2, 99, 99, 3, 4, 99, 99, 5, 6];
        let image = Image::from_vec_with_stride(2, 3, 1, 4, ColorSpace::LINEAR, data).unwrap();
        assert_eq!(image.rows().collect::<Vec<_>>(), [&[1, 2][..], &[3, 4], &[5, 6]]);
        assert_eq!(image.row(2), Some(&[5, 6][..]));
        assert_eq!(image.row(3), None);
        assert_eq!(image.get(1, 1, 0), Some(4));
        assert_eq!(image.get(2, 1, 0), None);
        assert_eq!(image.get(1, 1, 1), None);
        let packed = Image::from_vec(2, 3, 1, ColorSpace::LINEAR, vec![1u8, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(image, packed, "padding is not part of the image");
    }

    #[test]
    fn pixels_and_samples_are_addressed_by_x_y_and_channel() {
        let mut image = Image::from_fn(3, 2, 3, ColorSpace::LINEAR, |x, y, c| (100 * y + 10 * x + c) as f32).unwrap();
        assert_eq!(image.pixel(2, 1), Some(&[120.0, 121.0, 122.0][..]));
        assert_eq!(image.pixel(3, 0), None);
        image.pixel_mut(0, 1).unwrap()[2] = -1.0;
        assert_eq!(image.get(0, 1, 2), Some(-1.0));
        for row in image.rows_mut() {
            row.fill(0.5);
        }
        assert!(image.as_raw().iter().all(|&v| v.to_bits() == 0.5f32.to_bits()));
    }

    #[test]
    fn normalizing_is_exact_for_every_integer_sample() {
        for v in 0..=u8::MAX {
            let n = v.to_normalized();
            assert!((0.0..=1.0).contains(&n));
            assert_eq!(u8::from_normalized(n), v);
        }
        for v in 0..=u16::MAX {
            assert_eq!(u16::from_normalized(v.to_normalized()), v);
        }
        assert_eq!((u8::WHITE.to_normalized(), u16::WHITE.to_normalized(), f32::WHITE), (1.0, 1.0, 1.0));
        assert_eq!((u8::from_normalized(-0.5), u8::from_normalized(7.0), u8::from_normalized(f32::NAN)), (0, 255, 0));
        assert_eq!((u16::from_normalized(f32::INFINITY), u16::from_normalized(f32::NEG_INFINITY)), (65535, 0));
        assert_eq!(f32::from_normalized(-2.5), -2.5, "f32 keeps extended range");
        assert_eq!(u8::from_normalized(0.5), 128, "ties round up");
        assert_eq!([u8::FORMAT.bytes(), u16::FORMAT.bytes(), f32::FORMAT.bytes()], [1, 2, 4]);
    }

    #[test]
    fn little_endian_bytes_round_trip() {
        use sealed::Sealed;
        let mut bytes = Vec::new();
        u16::write_le(&[1, 0xabcd], &mut bytes);
        assert_eq!(bytes, [1, 0, 0xcd, 0xab]);
        let mut back = Vec::new();
        u16::read_le(&bytes, &mut back);
        assert_eq!(back, [1, 0xabcd]);
        let floats = [0.0, -1.5, f32::MAX, f32::MIN_POSITIVE];
        let mut bytes = Vec::new();
        f32::write_le(&floats, &mut bytes);
        let mut back = Vec::new();
        f32::read_le(&bytes, &mut back);
        assert_eq!(back, floats);
    }

    #[test]
    #[allow(clippy::float_cmp)] // Blending equal samples must give them back exactly.
    fn lerp_keeps_equal_samples_and_rounds_integers() {
        use sealed::Sealed;
        assert_eq!(u8::lerp(0, 255, 0.5), 128);
        assert_eq!(u16::lerp(100, 200, 0.25), 125);
        assert_eq!(f32::lerp(1.0, 3.0, 0.25), 1.5);
        for t in [0.0, 0.3, 1.0] {
            assert_eq!(u8::lerp(77, 77, t), 77);
            assert_eq!(f32::lerp(f32::INFINITY, f32::INFINITY, t), f32::INFINITY);
            assert_eq!(f32::lerp(0.1, 0.1, t), 0.1);
        }
    }

    #[test]
    fn crop_copies_a_region_and_rejects_bad_ones() {
        let image = Image::from_fn(5, 4, 2, ColorSpace::SRGB, |x, y, c| u16::try_from(100 * y + 10 * x + c).unwrap()).unwrap();
        let crop = image.crop(Rect::new(1, 2, 3, 2)).unwrap();
        assert_eq!((crop.width(), crop.height(), crop.channels(), crop.stride()), (3, 2, 2, 6));
        assert_eq!(crop.pixel(0, 0), Some(&[210, 211][..]));
        assert_eq!(crop.pixel(2, 1), Some(&[330, 331][..]));
        assert_eq!(crop.color_space(), image.color_space());
        assert_eq!(image.crop(Rect::new(0, 0, 5, 4)).unwrap(), image);
        for bad in [Rect::new(3, 0, 3, 1), Rect::new(0, 0, 0, 1), Rect::new(0, 4, 1, 1), Rect::new(usize::MAX, 0, 2, 1)] {
            assert!(matches!(image.crop(bad), Err(ImageError::Region { .. })), "{bad:?}");
        }
    }

    #[test]
    fn parallel_rows_match_the_sequential_ones() {
        let data: Vec<u8> = (0..27).collect();
        let mut image = Image::from_vec_with_stride(3, 4, 2, 7, ColorSpace::LINEAR, data).unwrap();
        let sequential: Vec<Vec<u8>> = image.rows().map(<[u8]>::to_vec).collect();
        let parallel: Vec<Vec<u8>> = image.par_rows().map(<[u8]>::to_vec).collect();
        assert_eq!(sequential, parallel);
        image.par_rows_mut().enumerate().for_each(|(y, row)| row.fill(u8::try_from(y).unwrap()));
        assert_eq!(image.get(2, 3, 1), Some(3));
        assert_eq!(image.as_raw()[6], 6, "the padding is left alone");
    }

    #[test]
    fn any_image_wraps_and_unwraps_by_sample_type() {
        let image = Image::<u16>::new(3, 2, 1, ColorSpace::LINEAR).unwrap();
        let mut any = AnyImage::from(image.clone());
        assert_eq!((any.format(), any.width(), any.height(), any.channels(), any.buffer_bytes()), (SampleFormat::U16, 3, 2, 1, 12));
        assert_eq!(any.as_image::<u16>(), Some(&image));
        assert!(any.as_image::<f32>().is_none() && any.as_image_mut::<u8>().is_none());
        any.as_image_mut::<u16>().unwrap().pixel_mut(0, 0).unwrap()[0] = 9;
        let any = any.into_image::<f32>().unwrap_err();
        assert_eq!(any.into_image::<u16>().unwrap().get(0, 0, 0), Some(9));
        assert_eq!(AnyImage::from_image(Image::<f32>::new(1, 1, 4, ColorSpace::SRGB).unwrap()).color_space(), &ColorSpace::SRGB);
    }

    #[test]
    fn equality_includes_the_color_space_and_debug_omits_the_pixels() {
        let a = Image::<u8>::new(2, 2, 1, ColorSpace::LINEAR).unwrap();
        let mut b = a.clone();
        assert_eq!(a, b);
        b.set_color_space(ColorSpace { encoding: Encoding::Linear, icc: Some(Arc::from(&b"icc"[..])) });
        assert_ne!(a, b);
        let text = format!("{a:?}");
        assert!(text.contains("width: 2") && text.contains("u8") && !text.contains("data"), "{text}");
    }
}
