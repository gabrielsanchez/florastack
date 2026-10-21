//! The encoders: one per output format. Each receives an image already in the output's depth and profile
//! ([`crate::Codecs::write`] converts it) and the output's metadata.

mod dng;
mod exr;
mod jpeg;
mod png;
pub(crate) mod strips;
mod tiff;

use std::borrow::Cow;

use florastack_core::{Image, Sample};

use crate::codec::ImageEncoder;

/// Every built-in encoder.
pub(crate) fn builtin() -> Vec<Box<dyn ImageEncoder>> {
    vec![Box::new(jpeg::JpegEncoder), Box::new(png::PngEncoder), Box::new(tiff::TiffEncoder), Box::new(exr::ExrEncoder), Box::new(dng::DngEncoder)]
}

/// The image's samples with packed rows (`width * channels` samples a row).
pub(crate) fn packed<T: Sample>(image: &Image<T>) -> Cow<'_, [T]> {
    let row = image.width() * image.channels();
    if image.stride() == row {
        Cow::Borrowed(image.as_raw().get(..row * image.height()).unwrap_or(image.as_raw()))
    } else {
        Cow::Owned(image.rows().flatten().copied().collect())
    }
}

/// The samples of the first `keep` channels of every pixel, packed.
pub(crate) fn first_channels<T: Sample>(image: &Image<T>, keep: usize) -> Vec<T> {
    let c = image.channels();
    image.rows().flat_map(|row| row.chunks_exact(c).flat_map(move |px| px.iter().take(keep).copied())).collect()
}

/// A dimension as the 32 bits file headers hold; images that large never get this far ([`Image`] can't address
/// them on 32-bit targets, and the formats' own limits come first).
pub(crate) fn dim32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}
