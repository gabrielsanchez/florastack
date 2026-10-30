//! Where the report crops every output: up to three square regions, chosen on the first outputs of a stack (the three
//! methods') and the truth when there is one, so that the crops show what tells the methods apart.
//!
//! The choice works on lightness planes reduced by a whole factor (a 20 MP output to about 640 pixels across), scored
//! by window sums over integral images, among windows a quarter of a window apart, in a fixed order, so that it is the
//! same on every run: first where the outputs' detail differs most (the standard deviation across outputs of each one's
//! lightness less its local mean, which halos, veils over edges and lost sharpness change, and a broad shift of tone
//! doesn't), then where the worst of them is furthest from the truth (or, with no truth, where their tones differ most),
//! then where the truth or their mean has the most detail (its gradient). Windows don't overlap, and none holds a pixel
//! some output leaves uncovered.

use florastack_core::Image;
use serde::Serialize;

/// A region of an output, in its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Window {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Window {
    fn overlaps(&self, other: &Window) -> bool {
        self.x < other.x + other.width && other.x < self.x + self.width && self.y < other.y + other.height && other.y < self.y + self.height
    }
}

/// The factor a `width` x `height` image is reduced by to choose regions: about 640 pixels across.
pub fn reduction(width: usize, height: usize) -> usize {
    width.max(height).div_ceil(640).max(1)
}

/// The side of the square regions, in output pixels, for a `width` x `height` image reduced by `k`: a third of the
/// short side, from 64 to 384 pixels, a multiple of `k`.
pub fn side(width: usize, height: usize, k: usize) -> usize {
    let side = (width.min(height) / 3).clamp(64, 384).min(width.min(height));
    (side / k).max(1) * k
}

/// An image's lightness, reduced by `k`: the mean luminance of each `k` x `k` block, raised to 1/2.2 so that dark
/// regions count about as they look; NaN where any pixel of the block is uncovered (alpha 0).
pub struct Plane {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

impl Plane {
    pub fn of(image: &Image<f32>, k: usize) -> Plane {
        let (width, height) = (image.width() / k, image.height() / k);
        let channels = image.channels();
        let alpha = (channels == 2 || channels == 4).then(|| channels - 1);
        let mut data = vec![0.0f32; width * height];
        for (by, out) in data.chunks_mut(width.max(1)).enumerate().take(height) {
            let mut sums = vec![0.0f64; width];
            let mut uncovered = vec![false; width];
            for y in by * k..(by + 1) * k {
                let Some(row) = image.row(y) else { continue };
                for (bx, (sum, hole)) in sums.iter_mut().zip(&mut uncovered).enumerate() {
                    for x in bx * k..(bx + 1) * k {
                        let Some(pixel) = row.get(x * channels..(x + 1) * channels) else { continue };
                        if alpha.and_then(|a| pixel.get(a)).is_some_and(|&a| a <= 0.0) {
                            *hole = true;
                        }
                        *sum += f64::from(match pixel {
                            [r, g, b, ..] => 0.2126 * r + 0.7152 * g + 0.0722 * b,
                            [v, ..] => *v,
                            [] => 0.0,
                        });
                    }
                }
            }
            let n = (k * k) as f64;
            for ((v, sum), hole) in out.iter_mut().zip(sums).zip(uncovered) {
                #[allow(clippy::cast_possible_truncation)] // A mean lightness, about 0 to 1.
                let light = (sum / n).max(0.0).powf(1.0 / 2.2) as f32;
                *v = if hole { f32::NAN } else { light };
            }
        }
        Plane { width, height, data }
    }
}

/// Up to three regions of `side` output pixels for outputs whose planes (reduced by `k`) are `planes`, with the truth's
/// plane when there is one, each with what it shows.
pub fn choose(planes: &[Plane], truth: Option<&Plane>, k: usize, side: usize) -> Vec<(Window, &'static str)> {
    let Some(first) = planes.first() else { return Vec::new() };
    let (w, h) = (first.width, first.height);
    let span = side / k;
    if span == 0 || span > w || span > h || planes.iter().chain(truth).any(|p| p.width != w || p.height != h) {
        return Vec::new();
    }
    let n = w * h;
    let values = |p: &Plane| -> Vec<f64> { p.data.iter().map(|&v| if v.is_nan() { 0.0 } else { f64::from(v) }).collect() };
    let all: Vec<Vec<f64>> = planes.iter().map(values).collect();
    // Each plane's detail: its lightness less its mean over a sixteenth of a region around.
    let radius = (span / 16).max(1);
    let fine: Vec<Vec<f64>> = all.iter().map(|v| high_pass(v, w, h, radius)).collect();
    let truth_values = truth.map(values);
    let mut tone = vec![0.0f64; n];
    let mut difference = vec![0.0f64; n];
    let mut error = vec![0.0f64; n];
    let mut invalid = vec![0.0f64; n];
    let mut mean = vec![0.0f64; n];
    for i in 0..n {
        if planes.iter().any(|p| p.data[i].is_nan()) {
            invalid[i] = 1.0;
            continue;
        }
        let at: Vec<f64> = all.iter().map(|v| v[i]).collect();
        (mean[i], tone[i]) = mean_and_deviation(&at);
        difference[i] = mean_and_deviation(&fine.iter().map(|v| v[i]).collect::<Vec<_>>()).1;
        if let Some(t) = &truth_values {
            error[i] = at.iter().map(|v| (v - t[i]).abs()).fold(0.0, f64::max);
        }
    }
    let base = truth_values.unwrap_or(mean);
    let mut detail = vec![0.0f64; n];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let at = |x: usize, y: usize| base[y * w + x];
            detail[y * w + x] = (at(x + 1, y) - at(x - 1, y)).abs() + (at(x, y + 1) - at(x, y - 1)).abs();
        }
    }
    let [tone, difference, error, detail, invalid] = [tone, difference, error, detail, invalid].map(|map| Integral::new(&map, w, h));
    let mut chosen: Vec<(Window, &'static str)> = Vec::new();
    let mut pick = |score: &Integral, why: &'static str| {
        let step = (span / 4).max(1);
        let positions = |len: usize| {
            let last = len - span;
            let mut p: Vec<usize> = (0..=last).step_by(step).collect();
            if p.last() != Some(&last) {
                p.push(last);
            }
            p
        };
        let mut best: Option<(f64, Window)> = None;
        for y in positions(h) {
            for x in positions(w) {
                let window = Window { x: x * k, y: y * k, width: span * k, height: span * k };
                if invalid.sum(x, y, span) > 0.0 || chosen.iter().any(|(c, _)| c.overlaps(&window)) {
                    continue;
                }
                let s = score.sum(x, y, span);
                if best.is_none_or(|(b, _)| s > b) {
                    best = Some((s, window));
                }
            }
        }
        if let Some((_, window)) = best {
            chosen.push((window, why));
        }
    };
    pick(&difference, "where the methods' detail differs most");
    if truth.is_some() {
        pick(&error, "where the worst result is furthest from the truth");
    } else {
        pick(&tone, "where the methods' tones differ most");
    }
    pick(&detail, if truth.is_some() { "where the truth has the most detail" } else { "where the results have the most detail" });
    chosen
}

/// The mean of `values` and their standard deviation.
fn mean_and_deviation(values: &[f64]) -> (f64, f64) {
    let n = values.len().max(1) as f64;
    let mean = values.iter().sum::<f64>() / n;
    (mean, (values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n).sqrt())
}

/// `map` less its mean over the square of `radius` around each pixel (cut at the edges).
fn high_pass(map: &[f64], w: usize, h: usize, radius: usize) -> Vec<f64> {
    let sums = Integral::new(map, w, h);
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(radius), (y + radius + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(radius), (x + radius + 1).min(w));
            let area = ((x1 - x0) * (y1 - y0)) as f64;
            out[y * w + x] = map[y * w + x] - sums.rect(x0, y0, x1, y1) / area;
        }
    }
    out
}

/// Sums over a map's square windows, in constant time each.
struct Integral {
    width: usize,
    sums: Vec<f64>,
}

impl Integral {
    fn new(map: &[f64], width: usize, height: usize) -> Integral {
        let stride = width + 1;
        let mut sums = vec![0.0; stride * (height + 1)];
        for y in 0..height {
            let mut row = 0.0;
            for x in 0..width {
                row += map[y * width + x];
                sums[(y + 1) * stride + x + 1] = sums[y * stride + x + 1] + row;
            }
        }
        Integral { width, sums }
    }

    /// The sum over the `size` x `size` window at `(x, y)`.
    fn sum(&self, x: usize, y: usize, size: usize) -> f64 {
        self.rect(x, y, x + size, y + size)
    }

    /// The sum over the pixels from `(x0, y0)` to before `(x1, y1)`.
    fn rect(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> f64 {
        let stride = self.width + 1;
        let at = |x: usize, y: usize| self.sums[y * stride + x];
        at(x1, y1) - at(x0, y1) - at(x1, y0) + at(x0, y0)
    }
}
