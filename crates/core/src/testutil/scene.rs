//! Procedural test scenes: a sharp, linear RGB image and its depth map, made from a seed, to feed
//! [`generate`](super::synth::generate).
//!
//! Depth runs from 0 (nearest) to 1 (farthest). The scene has detail everywhere, so that alignment has something to
//! lock onto, and the structures that are hard to stack: high-contrast edges between depth layers (where halos form)
//! and thin bristles that cross over everything else.

use super::ops::{to_f32, to_index};
use super::rng::Rng;
use crate::image::{ColorSpace, Image, ImageError};

/// A smooth random texture in `[0, 1]`: value noise, bilinear between random lattice values every `cell` pixels.
struct ValueNoise {
    cell: f64,
    columns: usize,
    lattice: Vec<f64>,
}

impl ValueNoise {
    fn new(width: usize, height: usize, cell: usize, rng: &mut Rng) -> ValueNoise {
        let columns = width / cell + 2;
        let rows = height / cell + 2;
        ValueNoise { cell: cell as f64, columns, lattice: (0..columns * rows).map(|_| rng.uniform()).collect() }
    }

    fn at(&self, x: f64, y: f64) -> f64 {
        let (gx, gy) = (x / self.cell, y / self.cell);
        let (ix, iy) = (to_index(gx), to_index(gy));
        // Smoothstep between lattice points hides the grid.
        let smooth = |t: f64| t * t * (3.0 - 2.0 * t);
        let (tx, ty) = (smooth(gx.fract()), smooth(gy.fract()));
        let v = |i: usize, j: usize| self.lattice.get(j * self.columns + i).copied().unwrap_or(0.5);
        let top = v(ix, iy) * (1.0 - tx) + v(ix + 1, iy) * tx;
        let bottom = v(ix, iy + 1) * (1.0 - tx) + v(ix + 1, iy + 1) * tx;
        top * (1.0 - ty) + bottom * ty
    }
}

/// A flat-colored disc at one depth.
struct Disc {
    center: (f64, f64),
    radius: f64,
    color: [f64; 3],
    depth: f64,
}

/// A thin straight bristle at one depth.
struct Bristle {
    from: (f64, f64),
    to: (f64, f64),
    half_width: f64,
    color: [f64; 3],
    depth: f64,
}

impl Bristle {
    fn distance(&self, p: (f64, f64)) -> f64 {
        let (dx, dy) = (self.to.0 - self.from.0, self.to.1 - self.from.1);
        let len2 = (dx * dx + dy * dy).max(f64::MIN_POSITIVE);
        let t = (((p.0 - self.from.0) * dx + (p.1 - self.from.1) * dy) / len2).clamp(0.0, 1.0);
        ((p.0 - self.from.0 - t * dx).powi(2) + (p.1 - self.from.1 - t * dy).powi(2)).sqrt()
    }
}

/// A `width` x `height` scene: a textured background plane receding from 0.6 (top) to 1.0 (bottom), three
/// flat discs at mid depth (0.3 to 0.5) with high-contrast edges, and six thin bright bristles near the camera (0.0 to
/// 0.2) crossing over both. Returns the sharp linear RGB image and its one-channel depth map.
pub fn textured(width: usize, height: usize, seed: u64) -> Result<(Image<f32>, Image<f32>), ImageError> {
    let mut rng = Rng::new(seed);
    let (w, h) = (width as f64, height as f64);
    let scale = w.min(h);
    let octaves: Vec<[ValueNoise; 3]> =
        [16, 6, 3].iter().map(|&cell| std::array::from_fn(|_| ValueNoise::new(width, height, cell, &mut rng))).collect();
    let discs: Vec<Disc> = (0..3)
        .map(|i| Disc {
            center: (rng.range(0.2, 0.8) * w, rng.range(0.2, 0.8) * h),
            radius: rng.range(0.08, 0.16) * scale,
            // Alternately dark and bright against the mid-gray background.
            color: if i % 2 == 0 { [0.03, 0.04, 0.05] } else { [0.85, 0.75, 0.6] },
            depth: rng.range(0.3, 0.5),
        })
        .collect();
    let bristles: Vec<Bristle> = (0..6)
        .map(|_| {
            let center = (rng.range(0.1, 0.9) * w, rng.range(0.1, 0.9) * h);
            let angle = rng.range(0.0, std::f64::consts::PI);
            let half_length = rng.range(0.2, 0.4) * scale;
            let (dx, dy) = (angle.cos() * half_length, angle.sin() * half_length);
            Bristle {
                from: (center.0 - dx, center.1 - dy),
                to: (center.0 + dx, center.1 + dy),
                half_width: rng.range(0.5, 1.0),
                color: [0.95, 0.9, 0.8],
                depth: rng.range(0.0, 0.2),
            }
        })
        .collect();

    let mut sharp = Image::<f32>::new(width, height, 3, ColorSpace::LINEAR)?;
    let mut depth = Image::<f32>::new(width, height, 1, ColorSpace::LINEAR)?;
    for y in 0..height {
        for x in 0..width {
            let p = (x as f64, y as f64);
            let mut color: [f64; 3] = std::array::from_fn(|c| {
                let texture: f64 = octaves.iter().zip([0.5, 0.3, 0.2]).map(|(noise, weight)| weight * noise[c].at(p.0, p.1)).sum();
                0.15 + 0.6 * texture
            });
            let mut z = 0.6 + 0.4 * p.1 / (h - 1.0).max(1.0);
            // Paint far to near, with one pixel of antialiasing; the depth is the nearest object covering the pixel's center.
            let mut cover = |color: &mut [f64; 3], coverage: f64, paint: [f64; 3], paint_depth: f64| {
                let alpha = coverage.clamp(0.0, 1.0);
                for (c, p) in color.iter_mut().zip(paint) {
                    *c += alpha * (p - *c);
                }
                if alpha >= 0.5 {
                    z = paint_depth;
                }
            };
            for disc in &discs {
                let distance = ((p.0 - disc.center.0).powi(2) + (p.1 - disc.center.1).powi(2)).sqrt();
                cover(&mut color, disc.radius + 0.5 - distance, disc.color, disc.depth);
            }
            for bristle in &bristles {
                cover(&mut color, bristle.half_width + 0.5 - bristle.distance(p), bristle.color, bristle.depth);
            }
            if let Some(pixel) = sharp.pixel_mut(x, y) {
                for (out, c) in pixel.iter_mut().zip(color) {
                    *out = to_f32(c);
                }
            }
            if let Some(d) = depth.pixel_mut(x, y) {
                d[0] = to_f32(z);
            }
        }
    }
    Ok((sharp, depth))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenes_are_reproducible_and_span_the_depth_range() {
        let (sharp, depth) = textured(64, 48, 3).unwrap();
        assert_eq!((sharp.width(), sharp.height(), sharp.channels()), (64, 48, 3));
        assert!(depth.same_shape(&Image::<f32>::new(64, 48, 1, ColorSpace::LINEAR).unwrap()));
        assert_eq!(textured(64, 48, 3).unwrap(), (sharp.clone(), depth.clone()));
        assert_ne!(textured(64, 48, 4).unwrap().0, sharp);
        assert!(sharp.as_raw().iter().all(|v| (0.0..=1.0).contains(v)));
        let (min, max) = depth.as_raw().iter().fold((f32::MAX, f32::MIN), |(lo, hi), &d| (lo.min(d), hi.max(d)));
        assert!(min < 0.2 && max > 0.95, "{min} {max}");
    }
}
