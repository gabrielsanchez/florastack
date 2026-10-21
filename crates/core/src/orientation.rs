//! Image orientation as cameras record it: the EXIF and TIFF `Orientation` tag (tag 274, values 1 to 8; CIPA DC-008
//! Exif 2.32 and TIFF 6.0), and [`Image::oriented`], which turns stored pixels upright.
//!
//! The tag says where the stored rows and columns belong: value 6, for example, means that row 0 is the right edge
//! of the upright image and column 0 its top, so the stored image must be turned 90° clockwise to be viewed. Both the
//! codecs (honoring or ignoring the tag on import, as vertical rigs flip it at random) and the RAW importer use these.

use rayon::prelude::*;

use crate::image::{Image, ImageError, Sample};

/// How the stored pixels relate to the upright image: the eight values of the EXIF and TIFF `Orientation` tag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Orientation {
    /// 1: stored upright.
    #[default]
    Normal,
    /// 2: mirrored left to right.
    FlipHorizontal,
    /// 3: turned half a turn.
    Rotate180,
    /// 4: mirrored top to bottom.
    FlipVertical,
    /// 5: mirrored along the main diagonal (row 0 is the left edge, column 0 the top).
    Transpose,
    /// 6: must be turned 90° clockwise to be upright (row 0 is the right edge, column 0 the top).
    Rotate90,
    /// 7: mirrored along the other diagonal (row 0 is the right edge, column 0 the bottom).
    Transverse,
    /// 8: must be turned 90° counter-clockwise to be upright (row 0 is the left edge, column 0 the bottom).
    Rotate270,
}

impl Orientation {
    /// Every orientation, in the order of their tag values.
    pub const ALL: [Orientation; 8] = [
        Orientation::Normal,
        Orientation::FlipHorizontal,
        Orientation::Rotate180,
        Orientation::FlipVertical,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
    ];

    /// The orientation a tag value names, or `None` outside 1 to 8.
    pub fn from_exif(value: u16) -> Option<Orientation> {
        let index = usize::from(value).checked_sub(1)?;
        Orientation::ALL.get(index).copied()
    }

    /// The tag value, 1 to 8.
    pub fn to_exif(self) -> u16 {
        match self {
            Orientation::Normal => 1,
            Orientation::FlipHorizontal => 2,
            Orientation::Rotate180 => 3,
            Orientation::FlipVertical => 4,
            Orientation::Transpose => 5,
            Orientation::Rotate90 => 6,
            Orientation::Transverse => 7,
            Orientation::Rotate270 => 8,
        }
    }

    /// Whether the upright image has the stored image's width and height swapped.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Orientation::Transpose | Orientation::Rotate90 | Orientation::Transverse | Orientation::Rotate270)
    }

    /// The orientation that undoes this one: storing `image.oriented(o.inverse())` with the tag `o` makes a file
    /// whose upright view is `image`.
    #[must_use]
    pub fn inverse(self) -> Orientation {
        match self {
            Orientation::Rotate90 => Orientation::Rotate270,
            Orientation::Rotate270 => Orientation::Rotate90,
            other => other,
        }
    }

    /// The stored pixel shown at `(x, y)` of the upright image, for a stored image `width` x `height`. The callers
    /// keep `(x, y)` inside the upright image, so the subtractions can't underflow; they saturate all the same.
    fn source(self, x: usize, y: usize, width: usize, height: usize) -> (usize, usize) {
        let last_x = width.saturating_sub(1);
        let last_y = height.saturating_sub(1);
        match self {
            Orientation::Normal => (x, y),
            Orientation::FlipHorizontal => (last_x.saturating_sub(x), y),
            Orientation::Rotate180 => (last_x.saturating_sub(x), last_y.saturating_sub(y)),
            Orientation::FlipVertical => (x, last_y.saturating_sub(y)),
            Orientation::Transpose => (y, x),
            Orientation::Rotate90 => (y, last_y.saturating_sub(x)),
            Orientation::Transverse => (last_x.saturating_sub(y), last_y.saturating_sub(x)),
            Orientation::Rotate270 => (last_x.saturating_sub(y), x),
        }
    }
}

impl<T: Sample> Image<T> {
    /// The upright image for stored pixels with this `orientation`: a new image with packed rows, the width and height
    /// swapped when the orientation turns the image a quarter turn, and the same color space. Rows are filled in
    /// parallel; every output sample is a copy, so the result doesn't depend on scheduling.
    pub fn oriented(&self, orientation: Orientation) -> Result<Image<T>, ImageError> {
        let (width, height) = (self.width(), self.height());
        let (out_w, out_h) = if orientation.swaps_axes() { (height, width) } else { (width, height) };
        let channels = self.channels();
        let mut out = Image::new(out_w, out_h, channels, self.color_space().clone())?;
        if orientation == Orientation::Normal {
            for (dst, src) in out.rows_mut().zip(self.rows()) {
                dst.copy_from_slice(src);
            }
            return Ok(out);
        }
        out.par_rows_mut().enumerate().for_each(|(y, row)| {
            for (x, pixel) in row.chunks_exact_mut(channels).enumerate() {
                let (sx, sy) = orientation.source(x, y, width, height);
                if let Some(src) = self.pixel(sx, sy) {
                    pixel.copy_from_slice(src);
                }
            }
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ColorSpace;

    /// A 3x2 image whose pixels are numbered 1..=6 in reading order, with two channels each (the number and ten times
    /// it), so that a test sees channels move with their pixel.
    fn numbered() -> Image<u8> {
        Image::from_fn(3, 2, 2, ColorSpace::SRGB, |x, y, c| u8::try_from((1 + x + 3 * y) * if c == 0 { 1 } else { 10 }).unwrap()).unwrap()
    }

    /// The first channel of every pixel, row by row.
    fn grid(image: &Image<u8>) -> Vec<Vec<u8>> {
        image.rows().map(|row| row.chunks(image.channels()).map(|p| p[0]).collect()).collect()
    }

    #[test]
    fn tag_values_round_trip_and_reject_the_rest() {
        for (i, o) in Orientation::ALL.iter().enumerate() {
            assert_eq!(usize::from(o.to_exif()), i + 1);
            assert_eq!(Orientation::from_exif(o.to_exif()), Some(*o));
        }
        assert_eq!(Orientation::from_exif(0), None);
        assert_eq!(Orientation::from_exif(9), None);
        assert_eq!(Orientation::from_exif(u16::MAX), None);
        assert_eq!(Orientation::default(), Orientation::Normal);
    }

    #[test]
    fn each_orientation_turns_the_stored_pixels_upright() {
        // Stored:  1 2 3
        //          4 5 6
        let expected: [(Orientation, Vec<Vec<u8>>); 8] = [
            (Orientation::Normal, vec![vec![1, 2, 3], vec![4, 5, 6]]),
            (Orientation::FlipHorizontal, vec![vec![3, 2, 1], vec![6, 5, 4]]),
            (Orientation::Rotate180, vec![vec![6, 5, 4], vec![3, 2, 1]]),
            (Orientation::FlipVertical, vec![vec![4, 5, 6], vec![1, 2, 3]]),
            // Row 0 is the left edge and column 0 the top.
            (Orientation::Transpose, vec![vec![1, 4], vec![2, 5], vec![3, 6]]),
            // Turned a quarter turn clockwise.
            (Orientation::Rotate90, vec![vec![4, 1], vec![5, 2], vec![6, 3]]),
            (Orientation::Transverse, vec![vec![6, 3], vec![5, 2], vec![4, 1]]),
            // Turned a quarter turn counter-clockwise.
            (Orientation::Rotate270, vec![vec![3, 6], vec![2, 5], vec![1, 4]]),
        ];
        let image = numbered();
        for (o, want) in expected {
            let upright = image.oriented(o).unwrap();
            assert_eq!(grid(&upright), want, "{o:?}");
            assert_eq!(upright.channels(), 2);
            assert!(upright.rows().all(|row| row.chunks(2).all(|p| p[1] == 10 * p[0])), "{o:?}: channels stay with their pixel");
            assert_eq!(upright.color_space(), image.color_space());
        }
    }

    #[test]
    fn the_inverse_undoes_each_orientation() {
        let image = Image::from_fn(7, 4, 3, ColorSpace::LINEAR, |x, y, c| (x * 100 + y * 10 + c) as f32).unwrap();
        for o in Orientation::ALL {
            let back = image.oriented(o).unwrap().oriented(o.inverse()).unwrap();
            assert_eq!(back, image, "{o:?}");
        }
    }

    #[test]
    fn strided_input_comes_out_packed() {
        let data = vec![1u16, 2, 0, 3, 4, 0];
        let image = Image::from_vec_with_stride(2, 2, 1, 3, ColorSpace::LINEAR, data).unwrap();
        let turned = image.oriented(Orientation::Rotate90).unwrap();
        assert_eq!(turned.stride(), 2);
        assert_eq!(turned.as_raw(), &[3, 1, 4, 2]);
    }
}
