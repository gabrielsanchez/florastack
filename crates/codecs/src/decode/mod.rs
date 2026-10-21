//! The decoders: one per format, each turning a file's bytes into its stored samples, what it says about their
//! color, its orientation and its metadata ([`crate::Stored`]). They check the image size before allocating, and
//! guard the libraries they call against panics.

mod bmp;
mod exr;
pub(crate) mod fallback;
mod jpeg;
mod png;
mod tiff;
mod webp;

use florastack_core::{ColorSpace, Image, Orientation, Sample};

pub(crate) use self::exr::{XMP_ATTRIBUTE as EXR_XMP_ATTRIBUTE, read_metadata as exr_metadata};

use crate::codec::ImageDecoder;
use crate::error::CodecError;
use crate::metadata::Metadata;

/// Every built-in decoder.
pub(crate) fn builtin() -> Vec<Box<dyn ImageDecoder>> {
    vec![
        Box::new(jpeg::JpegDecoder),
        Box::new(png::PngDecoder),
        Box::new(tiff::TiffDecoder),
        Box::new(exr::ExrDecoder),
        Box::new(webp::WebPDecoder),
        Box::new(bmp::BmpDecoder),
    ]
}

/// Refuses an image larger than `max_pixels` (or with no pixels) before anything is allocated for it.
pub(crate) fn check_size(width: u64, height: u64, max_pixels: u64) -> Result<(), CodecError> {
    if width == 0 || height == 0 || width.checked_mul(height).is_none_or(|n| n > max_pixels) {
        return Err(CodecError::TooLarge { width, height, max: max_pixels });
    }
    Ok(())
}

/// A byte budget for a decoder's own allocations: `max_pixels` pixels of up to 4 channels of 8 bytes, plus room for
/// compressed data and metadata.
pub(crate) fn byte_budget(max_pixels: u64) -> usize {
    usize::try_from(max_pixels.saturating_mul(32).saturating_add(64 << 20)).unwrap_or(usize::MAX)
}

/// An image over decoded samples, with packed rows; the read tags it with its color space afterwards.
pub(crate) fn image<T: Sample>(width: u32, height: u32, channels: usize, samples: Vec<T>) -> Result<Image<T>, CodecError> {
    let (w, h) = (usize::try_from(width).unwrap_or(usize::MAX), usize::try_from(height).unwrap_or(usize::MAX));
    Ok(Image::from_vec(w, h, channels, ColorSpace::LINEAR, samples)?)
}

/// The orientation an EXIF block in `metadata` records, or [`Orientation::Normal`].
pub(crate) fn exif_orientation(metadata: &Metadata) -> Orientation {
    metadata.exif().and_then(crate::metadata::exif::orientation).unwrap_or_default()
}

/// Converts associated (premultiplied) alpha to straight alpha, in place: every color sample divided by its pixel's
/// alpha. A pixel with no coverage keeps zeros.
pub(crate) fn unpremultiply<T: Sample>(samples: &mut [T], channels: usize) {
    if channels != 2 && channels != 4 {
        return;
    }
    for pixel in samples.chunks_exact_mut(channels) {
        let Some((alpha, color)) = pixel.split_last_mut() else { continue };
        let a = alpha.to_normalized();
        for c in color {
            *c = if a > 0.0 { T::from_normalized(c.to_normalized() / a) } else { T::default() };
        }
    }
}

/// Planar samples (all of channel 0, then all of channel 1, ...) interleaved, for `pixels` pixels of `channels`.
pub(crate) fn interleave<T: Copy + Default>(planar: &[T], pixels: usize, channels: usize) -> Vec<T> {
    let mut out = vec![T::default(); pixels.saturating_mul(channels)];
    for (c, plane) in planar.chunks(pixels).take(channels).enumerate() {
        for (i, &v) in plane.iter().enumerate() {
            if let Some(o) = out.get_mut(i * channels + c) {
                *o = v;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_checked_without_overflow() {
        assert!(check_size(100, 100, 10_000).is_ok());
        assert!(matches!(check_size(100, 101, 10_000), Err(CodecError::TooLarge { .. })));
        assert!(check_size(u64::MAX, u64::MAX, u64::MAX).is_err());
        assert!(check_size(0, 5, 100).is_err());
    }

    #[test]
    fn premultiplied_alpha_becomes_straight() {
        let mut px = [64u8, 128, 0, 0];
        unpremultiply(&mut px, 2);
        assert_eq!(px, [128, 128, 0, 0], "half covered: the color doubles; no coverage: zero");
        let mut f = [0.25f32, 0.5, 0.5, 0.5];
        unpremultiply(&mut f, 4);
        assert_eq!(f, [0.5, 1.0, 1.0, 0.5]);
        let mut rgb = [10u16, 20, 30];
        unpremultiply(&mut rgb, 3);
        assert_eq!(rgb, [10, 20, 30], "no alpha, no change");
    }

    #[test]
    fn planes_interleave() {
        assert_eq!(interleave(&[1, 2, 3, 10, 20, 30], 3, 2), [1, 10, 2, 20, 3, 30]);
        assert_eq!(interleave(&[1, 2], 3, 2), [1, 0, 2, 0, 0, 0], "short input leaves zeros, never panics");
    }
}
