//! Single-channel `f64` planes and the filters the metrics share.

use florastack_core::Image;

use crate::MetricsError;

/// Rec. 709 luminance weights (ITU-R BT.709), applied to the samples as they are.
pub(crate) const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// One channel of an image, or a quantity derived from one, in `f64`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Plane {
    pub w: usize,
    pub h: usize,
    /// `w * h` values, row by row.
    pub data: Vec<f64>,
}

impl Plane {
    fn from_pixels(image: &Image<f32>, f: impl Fn(&[f32]) -> f64) -> Result<Plane, MetricsError> {
        let c = image.channels();
        let mut data = Vec::with_capacity(image.pixel_count());
        for row in image.rows() {
            for pixel in row.chunks_exact(c) {
                let v = f(pixel);
                if !v.is_finite() {
                    return Err(MetricsError::NonFinite);
                }
                data.push(v);
            }
        }
        Ok(Plane { w: image.width(), h: image.height(), data })
    }

    /// Channel `c` of `image`.
    pub fn channel(image: &Image<f32>, c: usize) -> Result<Plane, MetricsError> {
        Self::from_pixels(image, |pixel| pixel.get(c).copied().map_or(f64::NAN, f64::from))
    }

    /// The luminance of `image`: Rec. 709 weights over RGB, the gray channel otherwise.
    pub fn luminance(image: &Image<f32>) -> Result<Plane, MetricsError> {
        if image.channels() >= 3 {
            Self::from_pixels(image, |p| p.iter().zip(LUMA).map(|(&v, w)| w * f64::from(v)).sum())
        } else {
            Self::channel(image, 0)
        }
    }

    /// The value at `(x, y)`, with coordinates clamped to the plane (the edge repeats).
    pub fn at(&self, x: usize, y: usize) -> f64 {
        let (x, y) = (x.min(self.w - 1), y.min(self.h - 1));
        self.data.get(y * self.w + x).copied().unwrap_or(0.0)
    }

    /// The value at `(x + dx, y + dy)` for offsets in `-1..=1`, clamped to the plane.
    fn around(&self, x: usize, y: usize, dx: isize, dy: isize) -> f64 {
        self.at(x.saturating_add_signed(dx), y.saturating_add_signed(dy))
    }

    /// The Sobel gradient magnitude (I. Sobel and G. Feldman, 1968), divided by 4 so that a step edge of contrast `h`
    /// gives `h` at the pixels on both sides of the step.
    pub fn sobel(&self) -> Plane {
        let mut data = Vec::with_capacity(self.data.len());
        for y in 0..self.h {
            for x in 0..self.w {
                let p = |dx, dy| self.around(x, y, dx, dy);
                let gx = (p(1, -1) + 2.0 * p(1, 0) + p(1, 1)) - (p(-1, -1) + 2.0 * p(-1, 0) + p(-1, 1));
                let gy = (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1)) - (p(-1, -1) + 2.0 * p(0, -1) + p(1, -1));
                data.push(gx.hypot(gy) / 4.0);
            }
        }
        Plane { w: self.w, h: self.h, data }
    }

    /// The response to `kernel` across and then down, without padding: the result is `w - k + 1` by `h - k + 1`
    /// for a kernel of `k` taps, which must not exceed either side.
    pub fn filter_valid(&self, kernel: &[f64]) -> Plane {
        let k = kernel.len();
        let (w, h) = ((self.w + 1).saturating_sub(k), (self.h + 1).saturating_sub(k));
        if w == 0 || h == 0 || k == 0 {
            return Plane { w, h, data: Vec::new() };
        }
        let mut across = Vec::with_capacity(w * self.h);
        for row in self.data.chunks_exact(self.w) {
            across.extend(row.windows(k).map(|window| window.iter().zip(kernel).map(|(v, c)| v * c).sum::<f64>()));
        }
        let mut data = vec![0.0; w * h];
        for (i, &c) in kernel.iter().enumerate() {
            for (out, src) in data.chunks_exact_mut(w).zip(across.chunks_exact(w).skip(i)) {
                for (o, s) in out.iter_mut().zip(src) {
                    *o += c * s;
                }
            }
        }
        Plane { w, h, data }
    }
}

#[cfg(test)]
mod tests {
    use florastack_core::ColorSpace;

    use super::*;

    #[test]
    fn sobel_gives_the_contrast_of_a_step_and_zero_on_flat_ground() {
        let step = Image::from_fn(8, 4, 1, ColorSpace::LINEAR, |x, _, _| if x < 4 { 0.2 } else { 0.7 }).unwrap();
        let g = Plane::luminance(&step).unwrap().sobel();
        assert!((g.at(3, 2) - 0.5).abs() < 1e-6 && (g.at(4, 2) - 0.5).abs() < 1e-6);
        assert!(g.at(1, 2).abs() < 1e-12 && g.at(6, 0).abs() < 1e-12);
    }

    #[test]
    fn valid_filtering_matches_a_direct_sum() {
        let image = Image::from_fn(7, 5, 1, ColorSpace::LINEAR, |x, y, _| (x * x + 3 * y) as f32 * 0.01).unwrap();
        let plane = Plane::channel(&image, 0).unwrap();
        let kernel = [0.25, 0.5, 0.25];
        let out = plane.filter_valid(&kernel);
        assert_eq!((out.w, out.h), (5, 3));
        for y in 0..3 {
            for x in 0..5 {
                let direct: f64 = (0..3).flat_map(|j| (0..3).map(move |i| (i, j))).map(|(i, j)| kernel[i] * kernel[j] * plane.at(x + i, y + j)).sum();
                assert!((out.data[y * 5 + x] - direct).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn non_finite_samples_are_rejected() {
        let image = Image::from_fn(2, 2, 3, ColorSpace::LINEAR, |x, _, _| if x == 1 { f32::INFINITY } else { 0.5 }).unwrap();
        assert_eq!(Plane::luminance(&image), Err(MetricsError::NonFinite));
        assert!(Plane::channel(&image, 0).is_err());
    }
}
