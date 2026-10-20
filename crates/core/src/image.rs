//! Image buffers: [`Image<T>`] holds `u8`, `u16` or `f32` samples with one to four interleaved channels, a row stride
//! and a [`ColorSpace`] tag.
//!
//! This is the container only. Conversions between sample types and encodings, the tiled view, the disk-backed tile
//! store and the basic operations (resize, blur, gradients, warps) build on it (roadmap step 1.1). Stacking math runs on
//! linear `Image<f32>`, with samples nominally in `[0, 1]`; values outside that range are allowed (extended range).
//!
//! Construction validates the dimensions with checked arithmetic and reserves memory with `try_reserve_exact`, so a
//! hostile size returns an [`ImageError`] instead of panicking or aborting. Limits on what a decoder accepts belong to
//! the decoder.

use std::collections::TryReserveError;
use std::fmt;
use std::sync::Arc;

/// The most interleaved channels an image may have (gray, gray and alpha, RGB, RGBA).
pub const MAX_CHANNELS: usize = 4;

/// A sample type an [`Image`] can hold: `u8`, `u16` or `f32`. Sealed: no other type implements it.
pub trait Sample: Copy + Default + PartialEq + Send + Sync + fmt::Debug + 'static + sealed::Sealed {}

impl Sample for u8 {}
impl Sample for u16 {}
impl Sample for f32 {}

mod sealed {
    pub trait Sealed {}
    impl Sealed for u8 {}
    impl Sealed for u16 {}
    impl Sealed for f32 {}
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
/// profile the primaries and white point are those of sRGB (Rec. 709 primaries, D65 white).
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
